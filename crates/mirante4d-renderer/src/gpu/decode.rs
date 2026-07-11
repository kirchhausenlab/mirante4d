use crate::{
    CameraRenderMode, CameraRenderModeF32, DvrRgbaFrame, IsoSurfaceFrameF32, IsoSurfaceFrameU16,
    IsoSurfaceNormal, PixelCoverage, RenderError,
};

const GPU_OUTPUT_COVERED_FLAG: u32 = 0x8000_0000;
const GPU_OUTPUT_MISSING_MASK: u32 = 0x7fff;
pub(super) const GPU_SURFACE_OUTPUT_U16_FIELDS: usize = 6;
pub(super) const GPU_SURFACE_OUTPUT_F32_FIELDS: usize = 8;

pub(super) fn gpu_output_value_u16(packed: u32) -> u16 {
    (packed & 0xffff) as u16
}

pub(super) fn mode_uses_iso_u16(mode: CameraRenderMode) -> bool {
    matches!(mode, CameraRenderMode::Isosurface { .. })
}

pub(super) fn mode_uses_iso_f32(mode: CameraRenderModeF32) -> bool {
    matches!(mode, CameraRenderModeF32::Isosurface { .. })
}

pub(super) fn mode_uses_dvr_u16(mode: CameraRenderMode) -> bool {
    matches!(mode, CameraRenderMode::Dvr { .. })
}

pub(super) fn mode_uses_dvr_f32(mode: CameraRenderModeF32) -> bool {
    matches!(mode, CameraRenderModeF32::Dvr { .. })
}

pub(super) fn gpu_output_missing_samples(packed: u32) -> u64 {
    u64::from((packed >> 16) & GPU_OUTPUT_MISSING_MASK)
}

pub(super) fn gpu_output_covered(packed: u32) -> bool {
    (packed & GPU_OUTPUT_COVERED_FLAG) != 0
}

pub(super) fn gpu_output_f32_missing_samples(marker: f32) -> u64 {
    if marker < 0.0 {
        (-marker - 1.0).max(0.0).round() as u64
    } else {
        marker.max(0.0).round() as u64
    }
}

pub(super) fn gpu_output_f32_covered(marker: f32) -> bool {
    marker < 0.0
}

pub(super) fn decode_gpu_iso_surface_u16(
    width: u64,
    height: u64,
    output_words: &[u32],
    enabled: bool,
) -> Result<Option<IsoSurfaceFrameU16>, RenderError> {
    if !enabled {
        return Ok(None);
    }
    let pixel_count = output_words.len() / GPU_SURFACE_OUTPUT_U16_FIELDS;
    let mut source_values = Vec::with_capacity(pixel_count);
    let mut display_scalars = Vec::with_capacity(pixel_count);
    let mut material_scalars = Vec::with_capacity(pixel_count);
    let mut hit_depth = Vec::with_capacity(pixel_count);
    let mut normals = Vec::with_capacity(pixel_count);
    let mut diffuse_lighting = Vec::with_capacity(pixel_count);
    let mut specular_lighting = Vec::with_capacity(pixel_count);
    let mut coverage = Vec::with_capacity(pixel_count);

    for record in output_words.chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS) {
        source_values.push(decode_low_u16(record[1]));
        display_scalars.push(gpu_output_value_u16(record[0]));
        material_scalars.push(decode_high_u16(record[1]));
        hit_depth.push(f32::from_bits(record[2]));
        normals.push(decode_gpu_surface_normal(record[3], record[4]));
        diffuse_lighting.push(decode_high_u16(record[4]));
        specular_lighting.push(decode_low_u16(record[5]));
        coverage.push(u8::from(gpu_output_covered(record[0])));
    }

    Ok(Some(IsoSurfaceFrameU16::try_new(
        width,
        height,
        source_values,
        display_scalars,
        material_scalars,
        hit_depth,
        normals,
        diffuse_lighting,
        specular_lighting,
        PixelCoverage::Mask(coverage),
    )?))
}

pub(super) fn decode_gpu_iso_surface_f32(
    width: u64,
    height: u64,
    output_words: &[u32],
    enabled: bool,
) -> Result<Option<IsoSurfaceFrameF32>, RenderError> {
    if !enabled {
        return Ok(None);
    }
    let pixel_count = output_words.len() / GPU_SURFACE_OUTPUT_F32_FIELDS;
    let mut source_values = Vec::with_capacity(pixel_count);
    let mut display_scalars = Vec::with_capacity(pixel_count);
    let mut material_scalars = Vec::with_capacity(pixel_count);
    let mut hit_depth = Vec::with_capacity(pixel_count);
    let mut normals = Vec::with_capacity(pixel_count);
    let mut diffuse_lighting = Vec::with_capacity(pixel_count);
    let mut specular_lighting = Vec::with_capacity(pixel_count);
    let mut coverage = Vec::with_capacity(pixel_count);

    for record in output_words.chunks_exact(GPU_SURFACE_OUTPUT_F32_FIELDS) {
        let marker = f32::from_bits(record[1]);
        source_values.push(f32::from_bits(record[2]));
        display_scalars.push(f32::from_bits(record[0]));
        material_scalars.push(f32::from_bits(record[3]));
        hit_depth.push(f32::from_bits(record[4]));
        normals.push(decode_gpu_surface_normal(record[5], record[6]));
        diffuse_lighting.push(decode_high_u16(record[6]));
        specular_lighting.push(decode_low_u16(record[7]));
        coverage.push(u8::from(gpu_output_f32_covered(marker)));
    }

    Ok(Some(IsoSurfaceFrameF32::try_new(
        width,
        height,
        source_values,
        display_scalars,
        material_scalars,
        hit_depth,
        normals,
        diffuse_lighting,
        specular_lighting,
        PixelCoverage::Mask(coverage),
    )?))
}

pub(super) fn decode_gpu_dvr_rgba_u16(
    width: u64,
    height: u64,
    output_words: &[u32],
    enabled: bool,
) -> Result<Option<DvrRgbaFrame>, RenderError> {
    if !enabled {
        return Ok(None);
    }
    let pixel_count = output_words.len() / GPU_SURFACE_OUTPUT_U16_FIELDS;
    let mut premultiplied_rgba = Vec::with_capacity(pixel_count);
    let mut coverage = Vec::with_capacity(pixel_count);
    for record in output_words.chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS) {
        premultiplied_rgba.push([
            decode_gpu_dvr_component(record[1]),
            decode_gpu_dvr_component(record[2]),
            decode_gpu_dvr_component(record[3]),
            decode_gpu_dvr_component(record[4]),
        ]);
        coverage.push(u8::from(gpu_output_covered(record[0])));
    }
    Ok(Some(DvrRgbaFrame::try_new(
        width,
        height,
        premultiplied_rgba,
        PixelCoverage::Mask(coverage),
    )?))
}

pub(super) fn decode_gpu_dvr_rgba_f32(
    width: u64,
    height: u64,
    output_words: &[u32],
    enabled: bool,
) -> Result<Option<DvrRgbaFrame>, RenderError> {
    if !enabled {
        return Ok(None);
    }
    let pixel_count = output_words.len() / GPU_SURFACE_OUTPUT_F32_FIELDS;
    let mut premultiplied_rgba = Vec::with_capacity(pixel_count);
    let mut coverage = Vec::with_capacity(pixel_count);
    for record in output_words.chunks_exact(GPU_SURFACE_OUTPUT_F32_FIELDS) {
        premultiplied_rgba.push([
            decode_gpu_dvr_component(record[2]),
            decode_gpu_dvr_component(record[3]),
            decode_gpu_dvr_component(record[4]),
            decode_gpu_dvr_component(record[5]),
        ]);
        coverage.push(u8::from(gpu_output_f32_covered(f32::from_bits(record[1]))));
    }
    Ok(Some(DvrRgbaFrame::try_new(
        width,
        height,
        premultiplied_rgba,
        PixelCoverage::Mask(coverage),
    )?))
}

fn decode_low_u16(word: u32) -> u16 {
    (word & 0xffff) as u16
}

fn decode_high_u16(word: u32) -> u16 {
    ((word >> 16) & 0xffff) as u16
}

fn decode_normal_component(word: u32) -> i16 {
    decode_low_u16(word) as i16
}

fn decode_gpu_surface_normal(normal_xy: u32, normal_z_diffuse: u32) -> IsoSurfaceNormal {
    IsoSurfaceNormal {
        x: decode_normal_component(normal_xy),
        y: decode_normal_component(normal_xy >> 16),
        z: decode_normal_component(normal_z_diffuse),
    }
}

fn decode_gpu_dvr_component(word: u32) -> f32 {
    let value = f32::from_bits(word);
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
