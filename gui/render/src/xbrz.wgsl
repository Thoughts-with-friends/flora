// xBRZ as a fragment shader, at any scale ("freescale"). The algorithm is Zenju's
// xBRZ (GPL-3.0); this follows the single-pass form of the DeSmuME and libretro shaders.
//
// Every output pixel looks at its source pixel E and the neighbours around it:
//
//    -|-2|-1|+1|+2      each corner of E decides on its own whether an edge runs
//    --+--+--+--+--     through it (the two diagonals of the 2x2 block are compared),
//       | A| B| C|      and if so blends a neighbour's colour into that corner along
//       | D| E| F|      the line of the edge: 45 degrees, or shallower or steeper
//       | G| H| I|      when the pixels further along say so.

const NONE: i32 = 0;
const NORMAL: i32 = 1;
const DOMINANT: i32 = 2;
const EQUAL_COLOR_TOLERANCE: f32 = 30.0 / 255.0;
const STEEP_DIRECTION_THRESHOLD: f32 = 2.2;
const DOMINANT_DIRECTION_THRESHOLD: f32 = 3.6;
const HALF_SQRT2: f32 = 0.70710678;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var source: texture_2d<f32>;

var<private> center: vec2<i32>;
var<private> last: vec2<i32>;

// The source pixel at an offset from E; the image's edge repeats outwards.
fn p(x: i32, y: i32) -> vec3<f32> {
    return textureLoad(source, clamp(center + vec2<i32>(x, y), vec2<i32>(0), last), 0).rgb;
}

// Perceived difference of two colours (Rec. 2020 YCbCr).
fn dist(a: vec3<f32>, b: vec3<f32>) -> f32 {
    let weights = vec3<f32>(0.2627, 0.6780, 0.0593);
    let difference = a - b;
    let y = dot(difference, weights);
    let cb = (0.5 / (1.0 - weights.b)) * (difference.b - y);
    let cr = (0.5 / (1.0 - weights.r)) * (difference.r - y);
    return sqrt(y * y + cb * cb + cr * cr);
}

fn similar(a: vec3<f32>, b: vec3<f32>) -> bool {
    return dist(a, b) < EQUAL_COLOR_TOLERANCE;
}

fn same(a: vec3<f32>, b: vec3<f32>) -> bool {
    return all(a == b);
}

fn differ(a: vec3<f32>, b: vec3<f32>) -> bool {
    return any(a != b);
}

// How far `at` lies to the left of the line, with one output pixel of smoothing.
fn left_ratio(at: vec2<f32>, origin: vec2<f32>, direction: vec2<f32>, scale: vec2<f32>) -> f32 {
    let from_origin = at - origin;
    let projected = direction * (dot(from_origin, direction) / dot(direction, direction));
    let off_line = from_origin - projected;
    let normal = vec2<f32>(-direction.y, direction.x);
    let side = sign(dot(from_origin, normal));
    return smoothstep(-HALF_SQRT2, HALF_SQRT2, side * length(off_line * scale));
}

@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    last = size - vec2<i32>(1);
    let texel = input.uv * vec2<f32>(size);
    // One output pixel in source pixels: the scale needs no uniform.
    let scale = vec2<f32>(1.0 / abs(dpdx(texel.x)));
    center = vec2<i32>(floor(texel));
    let pos = fract(texel) - vec2<f32>(0.5);

    let a = p(-1, -1);
    let b = p(0, -1);
    let c = p(1, -1);
    let d = p(-1, 0);
    let e = p(0, 0);
    let f = p(1, 0);
    let g = p(-1, 1);
    let h = p(0, 1);
    let i = p(1, 1);

    // The blend of each corner of E: top left, top right, bottom right, bottom left.
    var top_left = NONE;
    var top_right = NONE;
    var bottom_right = NONE;
    var bottom_left = NONE;

    if !((same(e, f) && same(h, i)) || (same(e, h) && same(f, i))) {
        let along = dist(g, e) + dist(e, c) + dist(p(0, 2), i) + dist(i, p(2, 0)) + 4.0 * dist(h, f);
        let across = dist(d, h) + dist(h, p(1, 2)) + dist(b, f) + dist(f, p(2, 1)) + 4.0 * dist(e, i);
        if along < across && differ(e, f) && differ(e, h) {
            bottom_right = select(NORMAL, DOMINANT, DOMINANT_DIRECTION_THRESHOLD * along < across);
        }
    }
    if !((same(d, e) && same(g, h)) || (same(d, g) && same(e, h))) {
        let along = dist(p(-2, 1), d) + dist(d, b) + dist(p(-1, 2), h) + dist(h, f) + 4.0 * dist(g, e);
        let across = dist(p(-2, 0), g) + dist(g, p(0, 2)) + dist(a, e) + dist(e, i) + 4.0 * dist(d, h);
        if along > across && differ(e, d) && differ(e, h) {
            bottom_left = select(NORMAL, DOMINANT, DOMINANT_DIRECTION_THRESHOLD * across < along);
        }
    }
    if !((same(b, c) && same(e, f)) || (same(b, e) && same(c, f))) {
        let along = dist(d, b) + dist(b, p(1, -2)) + dist(h, f) + dist(f, p(2, -1)) + 4.0 * dist(e, c);
        let across = dist(a, e) + dist(e, i) + dist(p(0, -2), c) + dist(c, p(2, 0)) + 4.0 * dist(b, f);
        if along > across && differ(e, b) && differ(e, f) {
            top_right = select(NORMAL, DOMINANT, DOMINANT_DIRECTION_THRESHOLD * across < along);
        }
    }
    if !((same(a, b) && same(d, e)) || (same(a, d) && same(b, e))) {
        let along = dist(p(-2, 0), a) + dist(a, p(0, -2)) + dist(g, e) + dist(e, c) + 4.0 * dist(d, b);
        let across = dist(p(-2, -1), d) + dist(d, h) + dist(p(-1, -2), b) + dist(b, f) + 4.0 * dist(a, e);
        if along < across && differ(e, d) && differ(e, b) {
            top_left = select(NORMAL, DOMINANT, DOMINANT_DIRECTION_THRESHOLD * along < across);
        }
    }

    var result = e;

    if bottom_right != NONE {
        let dist_f_g = dist(f, g);
        let dist_h_c = dist(h, c);
        let line = bottom_right == DOMINANT
            || !((top_right != NONE && !similar(e, g))
                || (bottom_left != NONE && !similar(e, c))
                || (similar(g, h) && similar(h, i) && similar(i, f) && similar(f, c) && !similar(e, i)));
        var origin = vec2<f32>(0.0, HALF_SQRT2);
        var direction = vec2<f32>(1.0, -1.0);
        if line {
            let shallow = STEEP_DIRECTION_THRESHOLD * dist_f_g <= dist_h_c && differ(e, g) && differ(d, g);
            let steep = STEEP_DIRECTION_THRESHOLD * dist_h_c <= dist_f_g && differ(e, c) && differ(b, c);
            origin = select(vec2<f32>(0.0, 0.5), vec2<f32>(0.0, 0.25), shallow);
            direction.x += select(0.0, 1.0, shallow);
            direction.y -= select(0.0, 1.0, steep);
        }
        let colour = mix(h, f, step(dist(e, f), dist(e, h)));
        result = mix(result, colour, left_ratio(pos, origin, direction, scale));
    }

    if bottom_left != NONE {
        let dist_h_a = dist(h, a);
        let dist_d_i = dist(d, i);
        let line = bottom_left == DOMINANT
            || !((bottom_right != NONE && !similar(e, a))
                || (top_left != NONE && !similar(e, i))
                || (similar(a, d) && similar(d, g) && similar(g, h) && similar(h, i) && !similar(e, g)));
        var origin = vec2<f32>(-HALF_SQRT2, 0.0);
        var direction = vec2<f32>(1.0, 1.0);
        if line {
            let shallow = STEEP_DIRECTION_THRESHOLD * dist_h_a <= dist_d_i && differ(e, a) && differ(b, a);
            let steep = STEEP_DIRECTION_THRESHOLD * dist_d_i <= dist_h_a && differ(e, i) && differ(f, i);
            origin = select(vec2<f32>(-0.5, 0.0), vec2<f32>(-0.25, 0.0), shallow);
            direction.y += select(0.0, 1.0, shallow);
            direction.x += select(0.0, 1.0, steep);
        }
        let colour = mix(h, d, step(dist(e, d), dist(e, h)));
        result = mix(result, colour, left_ratio(pos, origin, direction, scale));
    }

    if top_right != NONE {
        let dist_b_i = dist(b, i);
        let dist_f_a = dist(f, a);
        let line = top_right == DOMINANT
            || !((top_left != NONE && !similar(e, i))
                || (bottom_right != NONE && !similar(e, a))
                || (similar(i, f) && similar(f, c) && similar(c, b) && similar(b, a) && !similar(e, c)));
        var origin = vec2<f32>(HALF_SQRT2, 0.0);
        var direction = vec2<f32>(-1.0, -1.0);
        if line {
            let shallow = STEEP_DIRECTION_THRESHOLD * dist_b_i <= dist_f_a && differ(e, i) && differ(h, i);
            let steep = STEEP_DIRECTION_THRESHOLD * dist_f_a <= dist_b_i && differ(e, a) && differ(d, a);
            origin = select(vec2<f32>(0.5, 0.0), vec2<f32>(0.25, 0.0), shallow);
            direction.y -= select(0.0, 1.0, shallow);
            direction.x -= select(0.0, 1.0, steep);
        }
        let colour = mix(f, b, step(dist(e, b), dist(e, f)));
        result = mix(result, colour, left_ratio(pos, origin, direction, scale));
    }

    if top_left != NONE {
        let dist_d_c = dist(d, c);
        let dist_b_g = dist(b, g);
        let line = top_left == DOMINANT
            || !((bottom_left != NONE && !similar(e, c))
                || (top_right != NONE && !similar(e, g))
                || (similar(c, b) && similar(b, a) && similar(a, d) && similar(d, g) && !similar(e, a)));
        var origin = vec2<f32>(0.0, -HALF_SQRT2);
        var direction = vec2<f32>(-1.0, 1.0);
        if line {
            let shallow = STEEP_DIRECTION_THRESHOLD * dist_d_c <= dist_b_g && differ(e, c) && differ(f, c);
            let steep = STEEP_DIRECTION_THRESHOLD * dist_b_g <= dist_d_c && differ(e, g) && differ(h, g);
            origin = select(vec2<f32>(0.0, -0.5), vec2<f32>(0.0, -0.25), shallow);
            direction.x -= select(0.0, 1.0, shallow);
            direction.y += select(0.0, 1.0, steep);
        }
        let colour = mix(d, b, step(dist(e, b), dist(e, d)));
        result = mix(result, colour, left_ratio(pos, origin, direction, scale));
    }

    return vec4<f32>(result, 1.0);
}
