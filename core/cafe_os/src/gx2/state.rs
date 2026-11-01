//! Fixed-function state (port of Cemu's GX2_State): every setter ends up as a
//! `SET_CONTEXT_REG` packet, `Init*Reg` variants store the same words in a
//! guest structure for a later `Set*Reg`.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::too_many_arguments)]

use wiiu_gpu::regs::context as reg;
use wiiu_hle_macros::hle_export;

use super::Gx2State;
use crate::hle::{HleContext, HleError, HleExport};

fn store(ctx: &mut HleContext<'_>, address: u32, words: &[u32]) -> Result<(), HleError> {
    for (i, word) in words.iter().enumerate() {
        ctx.memory.write_u32(address + 4 * i as u32, *word)?;
    }
    Ok(())
}

fn load<const N: usize>(ctx: &HleContext<'_>, address: u32) -> Result<[u32; N], HleError> {
    let mut words = [0; N];
    for (i, word) in words.iter_mut().enumerate() {
        *word = ctx.memory.read_u32(address + 4 * i as u32)?;
    }
    Ok(words)
}

// ---- Alpha test ------------------------------------------------------------

const fn alpha_test(enable: u32, function: u32, reference: f32) -> [u32; 2] {
    [(function & 7) | ((enable & 1) << 3), reference.to_bits()]
}

fn set_alpha_test_words(gx2: &mut Gx2State, words: [u32; 2]) {
    gx2.set_context(reg::SX_ALPHA_TEST_CONTROL, &[words[0]]);
    gx2.set_context(reg::SX_ALPHA_REF, &[words[1]]);
}

#[hle_export(lib = "gx2", name = "GX2InitAlphaTestReg")]
fn gx2_init_alpha_test_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    enable: u32,
    function: u32,
    reference: f32,
) -> Result<(), HleError> {
    store(ctx, out, &alpha_test(enable, function, reference))
}

#[hle_export(lib = "gx2", name = "GX2SetAlphaTestReg")]
fn gx2_set_alpha_test_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<2>(ctx, source)?;
    set_alpha_test_words(&mut ctx.os.gx2, words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetAlphaTest")]
fn gx2_set_alpha_test(ctx: &mut HleContext<'_>, enable: u32, function: u32, reference: f32) {
    set_alpha_test_words(&mut ctx.os.gx2, alpha_test(enable, function, reference));
}

/// `SPECIAL_OP` 0 is normal rendering, 2 (`DISABLE`) keeps the color buffers untouched.
const fn color_control(logic_op: u32, blend_mask: u32, multiwrite: u32, color_enable: u32) -> u32 {
    let special = if color_enable == 0 { 4 } else { 0 };
    ((multiwrite & 1) << 1)
        | (special << 4)
        | ((blend_mask & 0xFF) << 8)
        | ((logic_op & 0xFF) << 16)
}

#[hle_export(lib = "gx2", name = "GX2InitColorControlReg")]
fn gx2_init_color_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    logic_op: u32,
    blend_mask: u32,
    multiwrite: u32,
    color_enable: u32,
) -> Result<(), HleError> {
    store(ctx, out, &[color_control(logic_op, blend_mask, multiwrite, color_enable)])
}

#[hle_export(lib = "gx2", name = "GX2SetColorControlReg")]
fn gx2_set_color_control_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_COLOR_CONTROL, &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetColorControl")]
fn gx2_set_color_control(
    ctx: &mut HleContext<'_>,
    logic_op: u32,
    blend_mask: u32,
    multiwrite: u32,
    color_enable: u32,
) {
    ctx.os.gx2.set_context(
        reg::CB_COLOR_CONTROL,
        &[color_control(logic_op, blend_mask, multiwrite, color_enable)],
    );
}

// ---- Polygon state ---------------------------------------------------------

const fn polygon_control(
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
    polygon_mode: u32,
    front_mode: u32,
    back_mode: u32,
    offset_front: u32,
    offset_back: u32,
    offset_para: u32,
) -> u32 {
    (cull_front & 1)
        | ((cull_back & 1) << 1)
        | ((front_face & 1) << 2)
        | ((polygon_mode & 3) << 3)
        | ((front_mode & 7) << 5)
        | ((back_mode & 7) << 8)
        | ((offset_front & 1) << 11)
        | ((offset_back & 1) << 12)
        | ((offset_para & 1) << 13)
}

#[hle_export(lib = "gx2", name = "GX2InitPolygonControlReg")]
fn gx2_init_polygon_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
    polygon_mode: u32,
    front_mode: u32,
    back_mode: u32,
    offset_front: u32,
    offset_back: u32,
    offset_para: u32,
) -> Result<(), HleError> {
    let word = polygon_control(
        front_face,
        cull_front,
        cull_back,
        polygon_mode,
        front_mode,
        back_mode,
        offset_front,
        offset_back,
        offset_para,
    );
    store(ctx, out, &[word])
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonControlReg")]
fn gx2_set_polygon_control_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonControl")]
fn gx2_set_polygon_control(
    ctx: &mut HleContext<'_>,
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
    polygon_mode: u32,
    front_mode: u32,
    back_mode: u32,
    offset_front: u32,
    offset_back: u32,
    offset_para: u32,
) {
    let word = polygon_control(
        front_face,
        cull_front,
        cull_back,
        polygon_mode,
        front_mode,
        back_mode,
        offset_front,
        offset_back,
        offset_para,
    );
    ctx.os.gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[word]);
}

#[hle_export(lib = "gx2", name = "GX2SetCullOnlyControl")]
fn gx2_set_cull_only_control(
    ctx: &mut HleContext<'_>,
    front_face: u32,
    cull_front: u32,
    cull_back: u32,
) {
    let word = polygon_control(front_face, cull_front, cull_back, 0, 0, 0, 0, 0, 0);
    ctx.os.gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[word]);
}

fn polygon_offset(
    front_offset: f32,
    front_scale: f32,
    back_offset: f32,
    back_scale: f32,
    clamp: f32,
) -> [u32; 5] {
    [
        (front_scale * 16.0).to_bits(),
        front_offset.to_bits(),
        (back_scale * 16.0).to_bits(),
        back_offset.to_bits(),
        clamp.to_bits(),
    ]
}

fn set_polygon_offset_words(gx2: &mut Gx2State, words: [u32; 5]) {
    gx2.set_context(reg::PA_SU_POLY_OFFSET_FRONT_SCALE, &words[..4]);
    gx2.set_context(reg::PA_SU_POLY_OFFSET_CLAMP, &words[4..]);
}

#[hle_export(lib = "gx2", name = "GX2InitPolygonOffsetReg")]
fn gx2_init_polygon_offset_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    front_offset: f32,
    front_scale: f32,
    back_offset: f32,
    back_scale: f32,
    clamp: f32,
) -> Result<(), HleError> {
    store(ctx, out, &polygon_offset(front_offset, front_scale, back_offset, back_scale, clamp))
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonOffsetReg")]
fn gx2_set_polygon_offset_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<5>(ctx, source)?;
    set_polygon_offset_words(&mut ctx.os.gx2, words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPolygonOffset")]
fn gx2_set_polygon_offset(
    ctx: &mut HleContext<'_>,
    front_offset: f32,
    front_scale: f32,
    back_offset: f32,
    back_scale: f32,
    clamp: f32,
) {
    set_polygon_offset_words(
        &mut ctx.os.gx2,
        polygon_offset(front_offset, front_scale, back_offset, back_scale, clamp),
    );
}

/// `PA_CL_CLIP_CNTL`: DX linear attribute clipping is always on.
fn set_clip_control(gx2: &mut Gx2State, rasterizer: u32, z_clip: u32, half_z: u32) {
    let mut word = 1 << 24;
    if z_clip == 0 {
        word |= (1 << 26) | (1 << 27);
    }
    if rasterizer == 0 {
        word |= 1 << 22;
    }
    if half_z != 0 {
        word |= 1 << 19;
    }
    gx2.set_context(reg::PA_CL_CLIP_CNTL, &[word]);
}

#[hle_export(lib = "gx2", name = "GX2SetRasterizerClipControl")]
fn gx2_set_rasterizer_clip_control(ctx: &mut HleContext<'_>, rasterizer: u32, z_clip: u32) {
    set_clip_control(&mut ctx.os.gx2, rasterizer, z_clip, 0);
}

#[hle_export(lib = "gx2", name = "GX2SetRasterizerClipControlHalfZ")]
fn gx2_set_rasterizer_clip_control_half_z(
    ctx: &mut HleContext<'_>,
    rasterizer: u32,
    z_clip: u32,
    half_z: u32,
) {
    set_clip_control(&mut ctx.os.gx2, rasterizer, z_clip, half_z);
}

#[hle_export(lib = "gx2", name = "GX2SetRasterizerClipControlEx")]
fn gx2_set_rasterizer_clip_control_ex(
    ctx: &mut HleContext<'_>,
    rasterizer: u32,
    z_clip: u32,
    half_z: u32,
) {
    set_clip_control(&mut ctx.os.gx2, rasterizer, z_clip, half_z);
}

// ---- Viewport and scissor --------------------------------------------------

fn viewport(x: f32, y: f32, width: f32, height: f32, near: f32, far: f32) -> [u32; 6] {
    [
        width * 0.5,
        width.mul_add(0.5, x),
        height * -0.5,
        height.mul_add(0.5, y),
        (far - near) * 0.5,
        (near + far) * 0.5,
    ]
    .map(f32::to_bits)
}

#[hle_export(lib = "gx2", name = "GX2InitViewportReg")]
fn gx2_init_viewport_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    near: f32,
    far: f32,
) -> Result<(), HleError> {
    store(ctx, out, &viewport(x, y, width, height, near, far))
}

#[hle_export(lib = "gx2", name = "GX2SetViewportReg")]
fn gx2_set_viewport_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<6>(ctx, source)?;
    ctx.os.gx2.set_context(reg::PA_CL_VPORT_XSCALE, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetViewport")]
fn gx2_set_viewport(
    ctx: &mut HleContext<'_>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    near: f32,
    far: f32,
) {
    ctx.os.gx2.set_context(reg::PA_CL_VPORT_XSCALE, &viewport(x, y, width, height, near, far));
}

fn scissor(x: u32, y: u32, width: u32, height: u32) -> [u32; 2] {
    let clamp = |v: u32| v.min(8192);
    let (tlx, tly, brx, bry) =
        (clamp(x), clamp(y), clamp(x.saturating_add(width)), clamp(y.saturating_add(height)));
    [tlx | (tly << 16) | (1 << 31), brx | (bry << 16)]
}

#[hle_export(lib = "gx2", name = "GX2InitScissorReg")]
fn gx2_init_scissor_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), HleError> {
    store(ctx, out, &scissor(x, y, width, height))
}

#[hle_export(lib = "gx2", name = "GX2SetScissorReg")]
fn gx2_set_scissor_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<2>(ctx, source)?;
    ctx.os.gx2.set_context(reg::PA_SC_GENERIC_SCISSOR_TL, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2GetScissorReg")]
fn gx2_get_scissor_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), HleError> {
    let [tl, br] = load::<2>(ctx, source)?;
    let (tlx, tly, brx, bry) = (tl & 0x7FFF, (tl >> 16) & 0x7FFF, br & 0x7FFF, (br >> 16) & 0x7FFF);
    for (out, value) in
        [(x, tlx), (y, tly), (width, brx.wrapping_sub(tlx)), (height, bry.wrapping_sub(tly))]
    {
        ctx.memory.write_u32(out, value)?;
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetScissor")]
fn gx2_set_scissor(ctx: &mut HleContext<'_>, x: u32, y: u32, width: u32, height: u32) {
    ctx.os.gx2.set_context(reg::PA_SC_GENERIC_SCISSOR_TL, &scissor(x, y, width, height));
}

// ---- Depth and stencil -----------------------------------------------------

const fn depth_stencil(
    depth_test: u32,
    depth_write: u32,
    depth_function: u32,
    stencil_test: u32,
    back_stencil_test: u32,
    front_function: u32,
    front_z_pass: u32,
    front_z_fail: u32,
    front_fail: u32,
    back_function: u32,
    back_z_pass: u32,
    back_z_fail: u32,
    back_fail: u32,
) -> u32 {
    (stencil_test & 1)
        | ((depth_test & 1) << 1)
        | ((depth_write & 1) << 2)
        | ((depth_function & 7) << 4)
        | ((back_stencil_test & 1) << 7)
        | ((front_function & 7) << 8)
        | ((front_fail & 7) << 11)
        | ((front_z_pass & 7) << 14)
        | ((front_z_fail & 7) << 17)
        | ((back_function & 7) << 20)
        | ((back_fail & 7) << 23)
        | ((back_z_pass & 7) << 26)
        | ((back_z_fail & 7) << 29)
}

#[hle_export(lib = "gx2", name = "GX2SetDepthOnlyControl")]
fn gx2_set_depth_only_control(ctx: &mut HleContext<'_>, test: u32, write: u32, function: u32) {
    ctx.os.gx2.set_context(
        reg::DB_DEPTH_CONTROL,
        &[depth_stencil(test, write, function, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetDepthStencilControl")]
fn gx2_set_depth_stencil_control(
    ctx: &mut HleContext<'_>,
    depth_test: u32,
    depth_write: u32,
    depth_function: u32,
    stencil_test: u32,
    back_stencil_test: u32,
    front_function: u32,
    front_z_pass: u32,
    front_z_fail: u32,
    front_fail: u32,
    back_function: u32,
    back_z_pass: u32,
    back_z_fail: u32,
    back_fail: u32,
) {
    let word = depth_stencil(
        depth_test,
        depth_write,
        depth_function,
        stencil_test,
        back_stencil_test,
        front_function,
        front_z_pass,
        front_z_fail,
        front_fail,
        back_function,
        back_z_pass,
        back_z_fail,
        back_fail,
    );
    ctx.os.gx2.set_context(reg::DB_DEPTH_CONTROL, &[word]);
}

#[hle_export(lib = "gx2", name = "GX2InitDepthStencilControlReg")]
fn gx2_init_depth_stencil_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    depth_test: u32,
    depth_write: u32,
    depth_function: u32,
    stencil_test: u32,
    back_stencil_test: u32,
    front_function: u32,
    front_z_pass: u32,
    front_z_fail: u32,
    front_fail: u32,
    back_function: u32,
    back_z_pass: u32,
    back_z_fail: u32,
    back_fail: u32,
) -> Result<(), HleError> {
    let word = depth_stencil(
        depth_test,
        depth_write,
        depth_function,
        stencil_test,
        back_stencil_test,
        front_function,
        front_z_pass,
        front_z_fail,
        front_fail,
        back_function,
        back_z_pass,
        back_z_fail,
        back_fail,
    );
    store(ctx, out, &[word])
}

#[hle_export(lib = "gx2", name = "GX2SetDepthStencilControlReg")]
fn gx2_set_depth_stencil_control_reg(
    ctx: &mut HleContext<'_>,
    source: u32,
) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::DB_DEPTH_CONTROL, &[word]);
    Ok(())
}

const fn stencil_mask(compare: u32, write: u32, reference: u32) -> u32 {
    (reference & 0xFF) | ((compare & 0xFF) << 8) | ((write & 0xFF) << 16)
}

#[hle_export(lib = "gx2", name = "GX2InitStencilMaskReg")]
fn gx2_init_stencil_mask_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    front_compare: u32,
    front_write: u32,
    front_ref: u32,
    back_compare: u32,
    back_write: u32,
    back_ref: u32,
) -> Result<(), HleError> {
    store(
        ctx,
        out,
        &[
            stencil_mask(front_compare, front_write, front_ref),
            stencil_mask(back_compare, back_write, back_ref),
        ],
    )
}

#[hle_export(lib = "gx2", name = "GX2SetStencilMask")]
fn gx2_set_stencil_mask(
    ctx: &mut HleContext<'_>,
    front_compare: u32,
    front_write: u32,
    front_ref: u32,
    back_compare: u32,
    back_write: u32,
    back_ref: u32,
) {
    ctx.os.gx2.set_context(
        reg::DB_STENCILREFMASK,
        &[
            stencil_mask(front_compare, front_write, front_ref),
            stencil_mask(back_compare, back_write, back_ref),
        ],
    );
}

#[hle_export(lib = "gx2", name = "GX2SetStencilMaskReg")]
fn gx2_set_stencil_mask_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<2>(ctx, source)?;
    ctx.os.gx2.set_context(reg::DB_STENCILREFMASK, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetPrimitiveRestartIndex")]
fn gx2_set_primitive_restart_index(ctx: &mut HleContext<'_>, index: u32) {
    ctx.os.gx2.set_context(reg::VGT_MULTI_PRIM_IB_RESET_INDX, &[index]);
}

// ---- Color targets ---------------------------------------------------------

const fn channel_masks(t: [u32; 8]) -> u32 {
    (t[0] & 0xF)
        | ((t[1] & 0xF) << 4)
        | ((t[2] & 0xF) << 8)
        | ((t[3] & 0xF) << 12)
        | ((t[4] & 0xF) << 16)
        | ((t[5] & 0xF) << 20)
        | ((t[6] & 0xF) << 24)
        | ((t[7] & 0xF) << 28)
}

#[hle_export(lib = "gx2", name = "GX2InitTargetChannelMasksReg")]
fn gx2_init_target_channel_masks_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    t0: u32,
    t1: u32,
    t2: u32,
    t3: u32,
    t4: u32,
    t5: u32,
    t6: u32,
    t7: u32,
) -> Result<(), HleError> {
    store(ctx, out, &[channel_masks([t0, t1, t2, t3, t4, t5, t6, t7])])
}

#[hle_export(lib = "gx2", name = "GX2SetTargetChannelMasksReg")]
fn gx2_set_target_channel_masks_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let [word] = load::<1>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_TARGET_MASK, &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetTargetChannelMasks")]
fn gx2_set_target_channel_masks(
    ctx: &mut HleContext<'_>,
    t0: u32,
    t1: u32,
    t2: u32,
    t3: u32,
    t4: u32,
    t5: u32,
    t6: u32,
    t7: u32,
) {
    ctx.os.gx2.set_context(reg::CB_TARGET_MASK, &[channel_masks([t0, t1, t2, t3, t4, t5, t6, t7])]);
}

const fn blend_control(
    color_src: u32,
    color_dst: u32,
    color_fn: u32,
    separate: u32,
    alpha_src: u32,
    alpha_dst: u32,
    alpha_fn: u32,
) -> u32 {
    (color_src & 0x1F)
        | ((color_fn & 7) << 5)
        | ((color_dst & 0x1F) << 8)
        | ((alpha_src & 0x1F) << 16)
        | ((alpha_fn & 7) << 21)
        | ((alpha_dst & 0x1F) << 24)
        | (((separate != 0) as u32) << 29)
}

#[hle_export(lib = "gx2", name = "GX2InitBlendControlReg")]
fn gx2_init_blend_control_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    target: u32,
    color_src: u32,
    color_dst: u32,
    color_fn: u32,
    separate: u32,
    alpha_src: u32,
    alpha_dst: u32,
    alpha_fn: u32,
) -> Result<(), HleError> {
    store(
        ctx,
        out,
        &[
            target,
            blend_control(color_src, color_dst, color_fn, separate, alpha_src, alpha_dst, alpha_fn),
        ],
    )
}

#[hle_export(lib = "gx2", name = "GX2SetBlendControlReg")]
fn gx2_set_blend_control_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let [target, word] = load::<2>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_BLEND0_CONTROL + (target & 7), &[word]);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetBlendControl")]
fn gx2_set_blend_control(
    ctx: &mut HleContext<'_>,
    target: u32,
    color_src: u32,
    color_dst: u32,
    color_fn: u32,
    separate: u32,
    alpha_src: u32,
    alpha_dst: u32,
    alpha_fn: u32,
) {
    let word =
        blend_control(color_src, color_dst, color_fn, separate, alpha_src, alpha_dst, alpha_fn);
    ctx.os.gx2.set_context(reg::CB_BLEND0_CONTROL + (target & 7), &[word]);
}

#[hle_export(lib = "gx2", name = "GX2InitBlendConstantColorReg")]
fn gx2_init_blend_constant_color_reg(
    ctx: &mut HleContext<'_>,
    out: u32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) -> Result<(), HleError> {
    store(ctx, out, &[red, green, blue, alpha].map(f32::to_bits))
}

#[hle_export(lib = "gx2", name = "GX2SetBlendConstantColorReg")]
fn gx2_set_blend_constant_color_reg(ctx: &mut HleContext<'_>, source: u32) -> Result<(), HleError> {
    let words = load::<4>(ctx, source)?;
    ctx.os.gx2.set_context(reg::CB_BLEND_RED, &words);
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2SetBlendConstantColor")]
fn gx2_set_blend_constant_color(
    ctx: &mut HleContext<'_>,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    ctx.os.gx2.set_context(reg::CB_BLEND_RED, &[red, green, blue, alpha].map(f32::to_bits));
}

// ---- Points ----------------------------------------------------------------

fn point_pair(first: f32, second: f32) -> u32 {
    let fixed = |v: f32| ((v.max(0.0) * 8.0) as u32).min(0xFFFF);
    fixed(first) | (fixed(second) << 16)
}

#[hle_export(lib = "gx2", name = "GX2SetPointSize")]
fn gx2_set_point_size(ctx: &mut HleContext<'_>, width: f32, height: f32) {
    ctx.os.gx2.set_context(reg::PA_SU_POINT_SIZE, &[point_pair(width, height)]);
}

#[hle_export(lib = "gx2", name = "GX2SetPointLimits")]
fn gx2_set_point_limits(ctx: &mut HleContext<'_>, min: f32, max: f32) {
    ctx.os.gx2.set_context(reg::PA_SU_POINT_MINMAX, &[point_pair(min, max)]);
}

#[hle_export(lib = "gx2", name = "GX2SetLineWidth")]
fn gx2_set_line_width(_ctx: &mut HleContext<'_>, _width: f32) {}

// ---- Special state and defaults --------------------------------------------

const VTE_DEFAULT: u32 = 0x43F;
/// `VTX_XY_FMT | VTX_Z_FMT`: vertices arrive in window coordinates.
const VTE_FAST_CLEAR: u32 = 0x300;

#[hle_export(lib = "gx2", name = "GX2SetSpecialState")]
fn gx2_set_special_state(ctx: &mut HleContext<'_>, state: u32, enable: u32) {
    if state != 0 {
        return;
    }
    if enable != 0 {
        ctx.os.gx2.set_context(reg::PA_CL_VTE_CNTL, &[VTE_FAST_CLEAR]);
        ctx.os.gx2.set_context(reg::PA_CL_CLIP_CNTL, &[0x49_0000]);
    } else {
        ctx.os.gx2.set_context(reg::PA_CL_VTE_CNTL, &[VTE_DEFAULT]);
        set_clip_control(&mut ctx.os.gx2, 1, 1, 0);
    }
}

/// What `GX2SetDefaultState` programs.
pub(super) fn set_default_state(gx2: &mut Gx2State) {
    super::draw::set_shader_mode(gx2, 0);
    gx2.set_context(reg::PA_CL_VTE_CNTL, &[VTE_DEFAULT]);
    gx2.set_context(reg::DB_DEPTH_CONTROL, &[depth_stencil(1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)]);
    set_alpha_test_words(gx2, alpha_test(0, 1, 0.0));
    // CCW front face, triangles.
    gx2.set_context(reg::PA_SU_SC_MODE_CNTL, &[polygon_control(0, 0, 0, 0, 2, 2, 0, 0, 0)]);
    set_polygon_offset_words(gx2, [0; 5]);
    gx2.set_context(reg::VGT_MULTI_PRIM_IB_RESET_INDX, &[u32::MAX]);
    gx2.set_context(reg::CB_TARGET_MASK, &[u32::MAX]);
    gx2.set_context(reg::CB_BLEND_RED, &[0; 4]);
    gx2.set_context(reg::PA_SU_POINT_SIZE, &[point_pair(1.0, 1.0)]);
    gx2.set_context(reg::PA_SU_POINT_MINMAX, &[point_pair(1.0, 1.0)]);
    gx2.set_context(reg::CB_COLOR_CONTROL, &[color_control(0xCC, 0, 0, 1)]);
    set_clip_control(gx2, 1, 1, 0);
    gx2.set_context(reg::DB_DEPTH_CLEAR, &[1.0_f32.to_bits()]);
}

pub const EXPORTS: &[HleExport] = &[
    GX2_INIT_ALPHA_TEST_REG,
    GX2_SET_ALPHA_TEST_REG,
    GX2_SET_ALPHA_TEST,
    GX2_INIT_COLOR_CONTROL_REG,
    GX2_SET_COLOR_CONTROL_REG,
    GX2_SET_COLOR_CONTROL,
    GX2_INIT_POLYGON_CONTROL_REG,
    GX2_SET_POLYGON_CONTROL_REG,
    GX2_SET_POLYGON_CONTROL,
    GX2_SET_CULL_ONLY_CONTROL,
    GX2_INIT_POLYGON_OFFSET_REG,
    GX2_SET_POLYGON_OFFSET_REG,
    GX2_SET_POLYGON_OFFSET,
    GX2_SET_RASTERIZER_CLIP_CONTROL,
    GX2_SET_RASTERIZER_CLIP_CONTROL_HALF_Z,
    GX2_SET_RASTERIZER_CLIP_CONTROL_EX,
    GX2_INIT_VIEWPORT_REG,
    GX2_SET_VIEWPORT_REG,
    GX2_SET_VIEWPORT,
    GX2_INIT_SCISSOR_REG,
    GX2_SET_SCISSOR_REG,
    GX2_GET_SCISSOR_REG,
    GX2_SET_SCISSOR,
    GX2_SET_DEPTH_ONLY_CONTROL,
    GX2_SET_DEPTH_STENCIL_CONTROL,
    GX2_INIT_DEPTH_STENCIL_CONTROL_REG,
    GX2_SET_DEPTH_STENCIL_CONTROL_REG,
    GX2_INIT_STENCIL_MASK_REG,
    GX2_SET_STENCIL_MASK,
    GX2_SET_STENCIL_MASK_REG,
    GX2_SET_PRIMITIVE_RESTART_INDEX,
    GX2_INIT_TARGET_CHANNEL_MASKS_REG,
    GX2_SET_TARGET_CHANNEL_MASKS_REG,
    GX2_SET_TARGET_CHANNEL_MASKS,
    GX2_INIT_BLEND_CONTROL_REG,
    GX2_SET_BLEND_CONTROL_REG,
    GX2_SET_BLEND_CONTROL,
    GX2_INIT_BLEND_CONSTANT_COLOR_REG,
    GX2_SET_BLEND_CONSTANT_COLOR_REG,
    GX2_SET_BLEND_CONSTANT_COLOR,
    GX2_SET_POINT_SIZE,
    GX2_SET_POINT_LIMITS,
    GX2_SET_LINE_WIDTH,
    GX2_SET_SPECIAL_STATE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_encodings_match_cemu() {
        // Depth test on, write on, LESS.
        assert_eq!(depth_stencil(1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0), 0b1_0110);
        assert_eq!(channel_masks([0xF, 0, 0, 0, 0, 0, 0, 0xA]), 0xA000_000F);
        assert_eq!(blend_control(4, 5, 0, 0, 1, 0, 0) & 0x1F, 4);
        assert_eq!(scissor(1, 2, 3, 4), [1 | (2 << 16) | (1 << 31), 4 | (6 << 16)]);
    }
}
