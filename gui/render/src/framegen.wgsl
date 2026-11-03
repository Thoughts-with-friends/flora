// Frame generation: a picture halfway between two finished frames.
//
// flow:      for each block, the motion d that makes prev(p - d/2) look like cur(p + d/2).
//            Searching around the point in between (bilateral matching) needs no second
//            step to fill holes: every output pixel gets a motion of its own.
// smooth:    a median over the neighbours throws out single wrong blocks.
// composite: fetches both frames along the motion; where they disagree the motion is
//            wrong (something appeared or left), and a plain blend hides that best.

struct VsOut { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var prev_frame: texture_2d<f32>;
@group(0) @binding(1) var cur_frame: texture_2d<f32>;
@group(0) @binding(2) var flow_in: texture_2d<f32>;
@group(0) @binding(3) var linear_clamp: sampler;

/// A motion must fit the picture this much better than none to count: flat areas stay still.
const MOTION_PENALTY: f32 = 0.004;

fn rgb_at(texture: texture_2d<f32>, uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(texture, linear_clamp, uv, 0.0).rgb;
}

/// How badly the frames match along motion `d` (in pixels) around `uv`, over a window of five by
/// five points `spacing` pixels apart. Inside a plain area a small window fits any motion: the
/// coarse steps of the search look far enough to see the area's edges.
fn mismatch(uv: vec2<f32>, d: vec2<f32>, pixel: vec2<f32>, spacing: f32) -> f32 {
    var sum = 0.0;
    let half_motion = d * 0.5 * pixel;
    for (var y = -2; y <= 2; y += 1) {
        for (var x = -2; x <= 2; x += 1) {
            let at = uv + vec2<f32>(f32(x), f32(y)) * spacing * pixel;
            let difference = rgb_at(prev_frame, at - half_motion) - rgb_at(cur_frame, at + half_motion);
            sum += abs(difference.r) + abs(difference.g) + abs(difference.b);
        }
    }
    return sum;
}

@fragment fn fs_flow(input: VsOut) -> @location(0) vec4<f32> {
    let pixel = 1.0 / vec2<f32>(textureDimensions(cur_frame));
    var best = vec2<f32>(0.0);
    // Coarse to fine: each step only looks around the best motion so far.
    var step = 32.0;
    for (var pass_index = 0; pass_index < 6; pass_index += 1) {
        let spacing = clamp(step * 0.5, 5.0, 12.0);
        // Costs of different windows do not compare: the last best is measured again.
        var best_cost = mismatch(input.uv, best, pixel, spacing) + MOTION_PENALTY * length(best);
        let center = best;
        for (var y = -1; y <= 1; y += 1) {
            for (var x = -1; x <= 1; x += 1) {
                if (x == 0 && y == 0) { continue; }
                let candidate = center + vec2<f32>(f32(x), f32(y)) * step;
                let cost = mismatch(input.uv, candidate, pixel, spacing) + MOTION_PENALTY * length(candidate);
                if (cost < best_cost) {
                    best_cost = cost;
                    best = candidate;
                }
            }
        }
        step *= 0.5;
    }
    return vec4<f32>(best, 0.0, 1.0);
}

@fragment fn fs_smooth(input: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(flow_in));
    let center = vec2<i32>(input.uv * vec2<f32>(size));
    var xs: array<f32, 9>;
    var ys: array<f32, 9>;
    var count = 0;
    for (var y = -1; y <= 1; y += 1) {
        for (var x = -1; x <= 1; x += 1) {
            let at = clamp(center + vec2<i32>(x, y), vec2<i32>(0), size - vec2<i32>(1));
            let flow = textureLoad(flow_in, at, 0).xy;
            xs[count] = flow.x;
            ys[count] = flow.y;
            count += 1;
        }
    }
    // Insertion sort: nine values are few.
    for (var i = 1; i < 9; i += 1) {
        let a = xs[i];
        let b = ys[i];
        var j = i - 1;
        loop {
            if (j < 0 || xs[j] <= a) { break; }
            xs[j + 1] = xs[j];
            j -= 1;
        }
        xs[j + 1] = a;
        j = i - 1;
        loop {
            if (j < 0 || ys[j] <= b) { break; }
            ys[j + 1] = ys[j];
            j -= 1;
        }
        ys[j + 1] = b;
    }
    return vec4<f32>(xs[4], ys[4], 0.0, 1.0);
}

@fragment fn fs_composite(input: VsOut) -> @location(0) vec4<f32> {
    let pixel = 1.0 / vec2<f32>(textureDimensions(cur_frame));
    let motion = textureSampleLevel(flow_in, linear_clamp, input.uv, 0.0).xy * pixel * 0.5;
    let before = rgb_at(prev_frame, input.uv - motion);
    let after = rgb_at(cur_frame, input.uv + motion);
    let difference = abs(before - after);
    let disagreement = max(difference.r, max(difference.g, difference.b));
    let still = (rgb_at(prev_frame, input.uv) + rgb_at(cur_frame, input.uv)) * 0.5;
    let moved = (before + after) * 0.5;
    return vec4<f32>(mix(moved, still, smoothstep(0.10, 0.35, disagreement)), 1.0);
}
