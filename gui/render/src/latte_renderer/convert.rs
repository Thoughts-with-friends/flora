//! Latte's register values as wgpu's.
use super::*;

pub fn wgpu_format(format: HostFormat) -> wgpu::TextureFormat {
    use HostFormat as H;
    use wgpu::{AstcBlock as _Unused, TextureFormat as T};
    let _ = std::marker::PhantomData::<_Unused>;
    match format {
        H::R8Unorm => T::R8Unorm,
        H::R8Snorm => T::R8Snorm,
        H::R8Uint => T::R8Uint,
        H::R8Sint => T::R8Sint,
        H::Rg8Unorm => T::Rg8Unorm,
        H::Rg8Snorm => T::Rg8Snorm,
        H::Rg8Uint => T::Rg8Uint,
        H::Rg8Sint => T::Rg8Sint,
        H::Rgba8Unorm => T::Rgba8Unorm,
        H::Rgba8Srgb => T::Rgba8UnormSrgb,
        H::Rgba8Snorm => T::Rgba8Snorm,
        H::Rgba8Uint => T::Rgba8Uint,
        H::Rgba8Sint => T::Rgba8Sint,
        H::R16Uint => T::R16Uint,
        H::R16Sint => T::R16Sint,
        H::R16Float => T::R16Float,
        H::R16Unorm => T::R16Unorm,
        H::Rg16Uint => T::Rg16Uint,
        H::Rg16Sint => T::Rg16Sint,
        H::Rg16Float => T::Rg16Float,
        H::Rg16Unorm => T::Rg16Unorm,
        H::Rgba16Uint => T::Rgba16Uint,
        H::Rgba16Sint => T::Rgba16Sint,
        H::Rgba16Float => T::Rgba16Float,
        H::Rgba16Unorm => T::Rgba16Unorm,
        H::Rgba16Snorm => T::Rgba16Snorm,
        H::R32Uint => T::R32Uint,
        H::R32Sint => T::R32Sint,
        H::R32Float => T::R32Float,
        H::Rg32Uint => T::Rg32Uint,
        H::Rg32Sint => T::Rg32Sint,
        H::Rg32Float => T::Rg32Float,
        H::Rgba32Uint => T::Rgba32Uint,
        H::Rgba32Sint => T::Rgba32Sint,
        H::Rgba32Float => T::Rgba32Float,
        H::Rgb10a2Unorm => T::Rgb10a2Unorm,
        H::Rg11b10Float => T::Rg11b10Ufloat,
        H::Bc1Unorm => T::Bc1RgbaUnorm,
        H::Bc1Srgb => T::Bc1RgbaUnormSrgb,
        H::Bc2Unorm => T::Bc2RgbaUnorm,
        H::Bc2Srgb => T::Bc2RgbaUnormSrgb,
        H::Bc3Unorm => T::Bc3RgbaUnorm,
        H::Bc3Srgb => T::Bc3RgbaUnormSrgb,
        H::Bc4Unorm => T::Bc4RUnorm,
        H::Bc4Snorm => T::Bc4RSnorm,
        H::Bc5Unorm => T::Bc5RgUnorm,
        H::Bc5Snorm => T::Bc5RgSnorm,
        H::Depth16Unorm => T::Depth16Unorm,
        H::Depth24PlusStencil8 => T::Depth24PlusStencil8,
        H::Depth32Float => T::Depth32Float,
        H::Depth32FloatStencil8 => T::Depth32FloatStencil8,
    }
}

pub(super) const fn compare_function(value: u8) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match value & 7 {
        0 => C::Never,
        1 => C::Less,
        2 => C::Equal,
        3 => C::LessEqual,
        4 => C::Greater,
        5 => C::NotEqual,
        6 => C::GreaterEqual,
        _ => C::Always,
    }
}

pub(super) const fn stencil_op(value: u8) -> wgpu::StencilOperation {
    use wgpu::StencilOperation as S;
    match value & 7 {
        0 => S::Keep,
        1 => S::Zero,
        2 => S::Replace,
        3 => S::IncrementClamp,
        4 => S::DecrementClamp,
        5 => S::Invert,
        6 => S::IncrementWrap,
        _ => S::DecrementWrap,
    }
}

const fn blend_factor(value: u8) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match value {
        0x00 => B::Zero,
        0x01 => B::One,
        0x02 => B::Src,
        0x03 => B::OneMinusSrc,
        0x04 | 0x0B => B::SrcAlpha,
        0x05 | 0x0C => B::OneMinusSrcAlpha,
        0x06 => B::DstAlpha,
        0x07 => B::OneMinusDstAlpha,
        0x08 => B::Dst,
        0x09 => B::OneMinusDst,
        0x0A => B::SrcAlphaSaturated,
        0x0D | 0x13 => B::Constant,
        0x0E | 0x14 => B::OneMinusConstant,
        _ => B::One,
    }
}

const fn blend_operation(value: u8) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as B;
    match value & 7 {
        0 => B::Add,
        1 => B::Subtract,
        2 => B::Min,
        3 => B::Max,
        _ => B::ReverseSubtract,
    }
}

pub(super) const fn address_mode(mode: AddressMode, border: bool) -> wgpu::AddressMode {
    match mode {
        AddressMode::Repeat => wgpu::AddressMode::Repeat,
        AddressMode::Mirror => wgpu::AddressMode::MirrorRepeat,
        AddressMode::Border if border => wgpu::AddressMode::ClampToBorder,
        AddressMode::Clamp | AddressMode::Border => wgpu::AddressMode::ClampToEdge,
    }
}

/// `MIN` and `MAX` ignore the blend factors, and the host insists they say so.
pub(super) const fn blend_component(src: u8, dst: u8, operation: u8) -> wgpu::BlendComponent {
    let operation = blend_operation(operation);
    if matches!(operation, wgpu::BlendOperation::Min | wgpu::BlendOperation::Max) {
        wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation,
        }
    } else {
        wgpu::BlendComponent {
            src_factor: blend_factor(src),
            dst_factor: blend_factor(dst),
            operation,
        }
    }
}

pub(super) fn view_dimension(dim: TextureDim) -> wgpu::TextureViewDimension {
    match dim {
        TextureDim::D2 => wgpu::TextureViewDimension::D2,
        TextureDim::D2Array => wgpu::TextureViewDimension::D2Array,
        TextureDim::D3 => wgpu::TextureViewDimension::D3,
        TextureDim::Cube => wgpu::TextureViewDimension::Cube,
    }
}
