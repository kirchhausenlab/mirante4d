use super::*;

pub fn render_camera_mip(
    volume: &DenseVolumeU16,
    camera: CameraState,
    viewport: RenderViewport,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    render_camera(volume, camera, viewport, CameraRenderMode::Mip)
}

pub fn render_camera_mip_f32(
    volume: &DenseVolumeF32,
    camera: CameraState,
    viewport: RenderViewport,
) -> Result<(MipImageF32, FrameDiagnosticsF32), RenderError> {
    render_camera_f32(volume, camera, viewport, CameraRenderModeF32::Mip)
}

pub fn render_camera(
    volume: &DenseVolumeU16,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderMode,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    render_camera_with_quality(
        volume,
        camera,
        viewport,
        mode,
        CameraRenderQuality::voxel_exact(),
    )
}

pub fn render_camera_with_quality(
    volume: &DenseVolumeU16,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let world_to_grid = volume.grid_to_world.inverse()?;
    let pixel_count = (viewport.width * viewport.height) as usize;
    let mut pixels = vec![0u16; pixel_count];
    let mut coverage = vec![false; pixel_count];
    let mut iso_surface =
        mode_uses_iso_u16(mode).then(|| IsoSurfaceFramePartsU16::new(pixel_count));
    let mut dvr_rgba = mode_uses_dvr_u16(mode).then(|| DvrRgbaFrameParts::new(pixel_count));

    for row in 0..viewport.height {
        for col in 0..viewport.width {
            let world_ray = camera.ray_for_render_pixel(
                col as f64,
                row as f64,
                viewport.width as f64,
                viewport.height as f64,
            );
            let grid_ray = GridRay {
                origin: world_to_grid.transform_point(world_ray.origin),
                direction: world_to_grid.transform_vector(world_ray.direction),
            };
            let pixel_index = (row * viewport.width + col) as usize;
            let sample = project_along_grid_ray(volume, grid_ray, mode, quality);
            pixels[pixel_index] = sample.value;
            coverage[pixel_index] = sample.covered;
            if let (Some(surface), Some(sample)) = (iso_surface.as_mut(), sample.iso_surface) {
                surface.set(pixel_index, sample);
            }
            if let (Some(dvr_rgba), Some(sample)) = (dvr_rgba.as_mut(), sample.dvr_rgba) {
                dvr_rgba.set(pixel_index, sample);
            }
        }
    }

    let diagnostics = frame_diagnostics(volume.render_valid_voxel_count(), &pixels);
    let coverage = PixelCoverage::from_bool_mask(coverage);
    let iso_surface = iso_surface
        .map(|surface| surface.into_frame(viewport.width, viewport.height, coverage.clone()))
        .transpose()?;
    let dvr_rgba = dvr_rgba
        .map(|frame| frame.into_frame(viewport.width, viewport.height, coverage.clone()))
        .transpose()?;
    Ok((
        MipImageU16::try_new_with_mode_frames(
            viewport.width,
            viewport.height,
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
        )?,
        diagnostics,
    ))
}

pub fn render_camera_u8(
    volume: &DenseVolumeU8,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderMode,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    render_camera_u8_with_quality(
        volume,
        camera,
        viewport,
        mode,
        CameraRenderQuality::voxel_exact(),
    )
}

pub fn render_camera_u8_with_quality(
    volume: &DenseVolumeU8,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let world_to_grid = volume.grid_to_world.inverse()?;
    let pixel_count = (viewport.width * viewport.height) as usize;
    let mut pixels = vec![0u16; pixel_count];
    let mut coverage = vec![false; pixel_count];
    let mut iso_surface =
        mode_uses_iso_u16(mode).then(|| IsoSurfaceFramePartsU16::new(pixel_count));
    let mut dvr_rgba = mode_uses_dvr_u16(mode).then(|| DvrRgbaFrameParts::new(pixel_count));

    for row in 0..viewport.height {
        for col in 0..viewport.width {
            let world_ray = camera.ray_for_render_pixel(
                col as f64,
                row as f64,
                viewport.width as f64,
                viewport.height as f64,
            );
            let grid_ray = GridRay {
                origin: world_to_grid.transform_point(world_ray.origin),
                direction: world_to_grid.transform_vector(world_ray.direction),
            };
            let pixel_index = (row * viewport.width + col) as usize;
            let sample = project_along_grid_ray_u8(volume, grid_ray, mode, quality);
            pixels[pixel_index] = sample.value;
            coverage[pixel_index] = sample.covered;
            if let (Some(surface), Some(sample)) = (iso_surface.as_mut(), sample.iso_surface) {
                surface.set(pixel_index, sample);
            }
            if let (Some(dvr_rgba), Some(sample)) = (dvr_rgba.as_mut(), sample.dvr_rgba) {
                dvr_rgba.set(pixel_index, sample);
            }
        }
    }

    let diagnostics = frame_diagnostics(volume.render_valid_voxel_count(), &pixels);
    let coverage = PixelCoverage::from_bool_mask(coverage);
    let iso_surface = iso_surface
        .map(|surface| surface.into_frame(viewport.width, viewport.height, coverage.clone()))
        .transpose()?;
    let dvr_rgba = dvr_rgba
        .map(|frame| frame.into_frame(viewport.width, viewport.height, coverage.clone()))
        .transpose()?;
    Ok((
        MipImageU16::try_new_with_mode_frames(
            viewport.width,
            viewport.height,
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
        )?,
        diagnostics,
    ))
}

pub fn render_camera_f32(
    volume: &DenseVolumeF32,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderModeF32,
) -> Result<(MipImageF32, FrameDiagnosticsF32), RenderError> {
    render_camera_f32_with_quality(
        volume,
        camera,
        viewport,
        mode,
        CameraRenderQuality::voxel_exact(),
    )
}

pub fn render_camera_f32_with_quality(
    volume: &DenseVolumeF32,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderModeF32,
    quality: CameraRenderQuality,
) -> Result<(MipImageF32, FrameDiagnosticsF32), RenderError> {
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let world_to_grid = volume.grid_to_world.inverse()?;
    let pixel_count = (viewport.width * viewport.height) as usize;
    let mut pixels = vec![0.0f32; pixel_count];
    let mut coverage = vec![false; pixel_count];
    let mut iso_surface =
        mode_uses_iso_f32(mode).then(|| IsoSurfaceFramePartsF32::new(pixel_count));
    let mut dvr_rgba = mode_uses_dvr_f32(mode).then(|| DvrRgbaFrameParts::new(pixel_count));

    for row in 0..viewport.height {
        for col in 0..viewport.width {
            let world_ray = camera.ray_for_render_pixel(
                col as f64,
                row as f64,
                viewport.width as f64,
                viewport.height as f64,
            );
            let grid_ray = GridRay {
                origin: world_to_grid.transform_point(world_ray.origin),
                direction: world_to_grid.transform_vector(world_ray.direction),
            };
            let pixel_index = (row * viewport.width + col) as usize;
            let sample = project_along_grid_ray_f32(volume, grid_ray, mode, quality);
            pixels[pixel_index] = sample.value;
            coverage[pixel_index] = sample.covered;
            if let (Some(surface), Some(sample)) = (iso_surface.as_mut(), sample.iso_surface) {
                surface.set(pixel_index, sample);
            }
            if let (Some(dvr_rgba), Some(sample)) = (dvr_rgba.as_mut(), sample.dvr_rgba) {
                dvr_rgba.set(pixel_index, sample);
            }
        }
    }

    let diagnostics = frame_diagnostics_f32(volume.render_valid_voxel_count(), &pixels);
    let coverage = PixelCoverage::from_bool_mask(coverage);
    let iso_surface = iso_surface
        .map(|surface| surface.into_frame(viewport.width, viewport.height, coverage.clone()))
        .transpose()?;
    let dvr_rgba = dvr_rgba
        .map(|frame| frame.into_frame(viewport.width, viewport.height, coverage.clone()))
        .transpose()?;
    Ok((
        MipImageF32::try_new_with_mode_frames(
            viewport.width,
            viewport.height,
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
        )?,
        diagnostics,
    ))
}

pub fn render_dvr_channels_with_quality(
    channels: &[DvrVolumeChannel<'_>],
    camera: CameraState,
    viewport: RenderViewport,
    quality: CameraRenderQuality,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    let Some(first) = channels.first().copied() else {
        return Err(RenderError::InvalidDvrChannelSet(
            "at least one visible channel is required",
        ));
    };
    let shape = first.shape();
    if shape.element_count()? == 0 {
        return Err(RenderError::EmptyVolume);
    }
    let grid_to_world = first.grid_to_world();
    for channel in channels {
        if channel.shape() != shape {
            return Err(RenderError::InvalidDvrChannelSet(
                "all DVR channels must share one grid shape",
            ));
        }
        if channel.grid_to_world() != grid_to_world {
            return Err(RenderError::InvalidDvrChannelSet(
                "all DVR channels must share one grid transform",
            ));
        }
    }

    let world_to_grid = grid_to_world.inverse()?;
    let pixel_count = (viewport.width * viewport.height) as usize;
    let mut pixels = vec![0u16; pixel_count];
    let mut coverage = vec![false; pixel_count];
    let mut dvr_rgba = DvrRgbaFrameParts::new(pixel_count);

    for row in 0..viewport.height {
        for col in 0..viewport.width {
            let world_ray = camera.ray_for_render_pixel(
                col as f64,
                row as f64,
                viewport.width as f64,
                viewport.height as f64,
            );
            let grid_ray = GridRay {
                origin: world_to_grid.transform_point(world_ray.origin),
                direction: world_to_grid.transform_vector(world_ray.direction),
            };
            let sample = project_dvr_channels_along_grid_ray(
                channels,
                shape,
                grid_to_world,
                grid_ray,
                quality,
            );
            let pixel_index = (row * viewport.width + col) as usize;
            pixels[pixel_index] = sample.value;
            coverage[pixel_index] = sample.covered;
            dvr_rgba.set(pixel_index, sample.dvr_rgba);
        }
    }

    let diagnostics = frame_diagnostics(
        channels
            .iter()
            .map(|channel| channel.render_valid_voxel_count())
            .sum(),
        &pixels,
    );
    let coverage = PixelCoverage::from_bool_mask(coverage);
    let dvr_rgba = dvr_rgba.into_frame(viewport.width, viewport.height, coverage.clone())?;
    Ok((
        MipImageU16::try_new_with_mode_frames(
            viewport.width,
            viewport.height,
            pixels,
            coverage,
            None,
            Some(dvr_rgba),
        )?,
        diagnostics,
    ))
}

pub fn pick_camera_volume(
    volume: &DenseVolumeU16,
    camera: CameraState,
    viewport: RenderViewport,
    pixel_x: u64,
    pixel_y: u64,
    mode: CameraRenderMode,
) -> Result<CameraVolumePick, RenderError> {
    if pixel_x >= viewport.width || pixel_y >= viewport.height {
        return Err(RenderError::InvalidReadoutPixel {
            x: pixel_x,
            y: pixel_y,
            width: viewport.width,
            height: viewport.height,
        });
    }
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let world_to_grid = volume.grid_to_world.inverse()?;
    let world_ray = camera.ray_for_render_pixel(
        pixel_x as f64,
        pixel_y as f64,
        viewport.width as f64,
        viewport.height as f64,
    );
    let grid_ray = GridRay {
        origin: world_to_grid.transform_point(world_ray.origin),
        direction: world_to_grid.transform_vector(world_ray.direction),
    };

    Ok(pick_along_grid_ray(volume, grid_ray, mode))
}

pub fn pick_camera_volume_u8(
    volume: &DenseVolumeU8,
    camera: CameraState,
    viewport: RenderViewport,
    pixel_x: u64,
    pixel_y: u64,
    mode: CameraRenderMode,
) -> Result<CameraVolumePickU8, RenderError> {
    if pixel_x >= viewport.width || pixel_y >= viewport.height {
        return Err(RenderError::InvalidReadoutPixel {
            x: pixel_x,
            y: pixel_y,
            width: viewport.width,
            height: viewport.height,
        });
    }
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let world_to_grid = volume.grid_to_world.inverse()?;
    let world_ray = camera.ray_for_render_pixel(
        pixel_x as f64,
        pixel_y as f64,
        viewport.width as f64,
        viewport.height as f64,
    );
    let grid_ray = GridRay {
        origin: world_to_grid.transform_point(world_ray.origin),
        direction: world_to_grid.transform_vector(world_ray.direction),
    };

    Ok(pick_along_grid_ray_u8(volume, grid_ray, mode))
}

pub fn pick_camera_volume_f32(
    volume: &DenseVolumeF32,
    camera: CameraState,
    viewport: RenderViewport,
    pixel_x: u64,
    pixel_y: u64,
    mode: CameraRenderModeF32,
) -> Result<CameraVolumePickF32, RenderError> {
    if pixel_x >= viewport.width || pixel_y >= viewport.height {
        return Err(RenderError::InvalidReadoutPixel {
            x: pixel_x,
            y: pixel_y,
            width: viewport.width,
            height: viewport.height,
        });
    }
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let world_to_grid = volume.grid_to_world.inverse()?;
    let world_ray = camera.ray_for_render_pixel(
        pixel_x as f64,
        pixel_y as f64,
        viewport.width as f64,
        viewport.height as f64,
    );
    let grid_ray = GridRay {
        origin: world_to_grid.transform_point(world_ray.origin),
        direction: world_to_grid.transform_vector(world_ray.direction),
    };

    Ok(pick_along_grid_ray_f32(volume, grid_ray, mode))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PickSample {
    intensity: u16,
    z: u64,
    y: u64,
    x: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PickSampleU8 {
    intensity: u8,
    z: u64,
    y: u64,
    x: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PickSampleF32 {
    intensity: f32,
    z: u64,
    y: u64,
    x: u64,
}

#[derive(Debug, Clone, Copy)]
struct GridRay {
    origin: DVec3,
    direction: DVec3,
}

#[derive(Debug, Clone, Copy)]
struct RayBoxHit {
    enter: f64,
    exit: f64,
}

#[derive(Debug, Clone, Copy)]
struct AxisTraversal {
    index: i64,
    step: i64,
    next_t: f64,
    delta_t: f64,
    limit: i64,
}

impl AxisTraversal {
    fn new(entry_coordinate: f64, direction: f64, entry_t: f64, limit: u64) -> Self {
        let limit = limit as i64;
        let index = initial_voxel_index(entry_coordinate, direction, limit);
        if direction > EPSILON {
            let next_boundary = index as f64 + 0.5;
            Self {
                index,
                step: 1,
                next_t: entry_t + ((next_boundary - entry_coordinate) / direction).max(0.0),
                delta_t: 1.0 / direction,
                limit,
            }
        } else if direction < -EPSILON {
            let next_boundary = index as f64 - 0.5;
            Self {
                index,
                step: -1,
                next_t: entry_t + ((next_boundary - entry_coordinate) / direction).max(0.0),
                delta_t: -1.0 / direction,
                limit,
            }
        } else {
            Self {
                index,
                step: 0,
                next_t: f64::INFINITY,
                delta_t: f64::INFINITY,
                limit,
            }
        }
    }

    fn is_inside(self) -> bool {
        self.index >= 0 && self.index < self.limit
    }

    fn advance(&mut self) {
        if self.step == 0 {
            return;
        }
        self.index += self.step;
        self.next_t += self.delta_t;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RenderedSampleU16 {
    value: u16,
    covered: bool,
    iso_surface: Option<IsoRaySurfaceSampleU16>,
    dvr_rgba: Option<[f32; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RenderedSampleF32 {
    value: f32,
    covered: bool,
    iso_surface: Option<IsoRaySurfaceSampleF32>,
    dvr_rgba: Option<[f32; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DvrChannelSample {
    value: u16,
    covered: bool,
    dvr_rgba: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
struct DvrStepContribution {
    tau: f64,
    rgb: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct IsoRaySurfaceSampleU16 {
    source_value: u16,
    display_scalar: u16,
    material_scalar: u16,
    hit_depth: f32,
    normal: IsoSurfaceNormal,
    diffuse_lighting: u16,
    specular_lighting: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct IsoRaySurfaceSampleF32 {
    source_value: f32,
    display_scalar: f32,
    material_scalar: f32,
    hit_depth: f32,
    normal: IsoSurfaceNormal,
    diffuse_lighting: u16,
    specular_lighting: u16,
}

struct IsoSurfaceFramePartsU16 {
    source_values: Vec<u16>,
    display_scalars: Vec<u16>,
    material_scalars: Vec<u16>,
    hit_depth: Vec<f32>,
    normals: Vec<IsoSurfaceNormal>,
    diffuse_lighting: Vec<u16>,
    specular_lighting: Vec<u16>,
}

struct IsoSurfaceFramePartsF32 {
    source_values: Vec<f32>,
    display_scalars: Vec<f32>,
    material_scalars: Vec<f32>,
    hit_depth: Vec<f32>,
    normals: Vec<IsoSurfaceNormal>,
    diffuse_lighting: Vec<u16>,
    specular_lighting: Vec<u16>,
}

struct DvrRgbaFrameParts {
    premultiplied_rgba: Vec<[f32; 4]>,
}

fn mode_uses_iso_u16(mode: CameraRenderMode) -> bool {
    matches!(mode, CameraRenderMode::Isosurface { .. })
}

fn mode_uses_iso_f32(mode: CameraRenderModeF32) -> bool {
    matches!(mode, CameraRenderModeF32::Isosurface { .. })
}

fn mode_uses_dvr_u16(mode: CameraRenderMode) -> bool {
    matches!(mode, CameraRenderMode::Dvr { .. })
}

fn mode_uses_dvr_f32(mode: CameraRenderModeF32) -> bool {
    matches!(mode, CameraRenderModeF32::Dvr { .. })
}

fn accumulate_dvr_sample(
    out: &mut [f64; 4],
    accumulated_alpha: &mut f64,
    source_value: f64,
    parameters: DvrRenderParameters,
    step_factor: f64,
) -> bool {
    let Some(contribution) = dvr_step_contribution(source_value, parameters, step_factor) else {
        return false;
    };
    accumulate_dvr_step(out, accumulated_alpha, &[contribution])
}

fn dvr_step_contribution(
    source_value: f64,
    parameters: DvrRenderParameters,
    step_factor: f64,
) -> Option<DvrStepContribution> {
    if !parameters.visible() {
        return None;
    }
    let opacity_scalar = parameters.opacity_scalar(source_value);
    if opacity_scalar <= EPSILON {
        return None;
    }
    let color_scalar = parameters.color_scalar(source_value);
    let [red, green, blue, color_alpha] = parameters.color_rgba;
    let density = opacity_scalar
        * f64::from(parameters.channel_opacity)
        * f64::from(color_alpha.clamp(0.0, 1.0))
        * parameters.density_scale;
    if density <= EPSILON {
        return None;
    }
    let tau = density * step_factor.max(EPSILON);
    if tau <= EPSILON {
        return None;
    }
    Some(DvrStepContribution {
        tau,
        rgb: [
            color_scalar * f64::from(red.clamp(0.0, 1.0)),
            color_scalar * f64::from(green.clamp(0.0, 1.0)),
            color_scalar * f64::from(blue.clamp(0.0, 1.0)),
        ],
    })
}

fn accumulate_dvr_step(
    out: &mut [f64; 4],
    accumulated_alpha: &mut f64,
    contributions: &[DvrStepContribution],
) -> bool {
    let tau_total: f64 = contributions
        .iter()
        .map(|contribution| contribution.tau)
        .sum();
    if tau_total <= EPSILON {
        return false;
    }
    let alpha = 1.0 - (-tau_total).exp();
    if alpha <= EPSILON {
        return false;
    }
    let mut rgb = [0.0; 3];
    for contribution in contributions {
        let weight = contribution.tau / tau_total;
        rgb[0] += contribution.rgb[0] * weight;
        rgb[1] += contribution.rgb[1] * weight;
        rgb[2] += contribution.rgb[2] * weight;
    }
    let transmittance = 1.0 - *accumulated_alpha;
    out[0] += transmittance * rgb[0] * alpha;
    out[1] += transmittance * rgb[1] * alpha;
    out[2] += transmittance * rgb[2] * alpha;
    *accumulated_alpha += transmittance * alpha;
    out[3] = *accumulated_alpha;
    true
}

fn dvr_rgba_f32(rgba: [f64; 4]) -> [f32; 4] {
    [
        rgba[0].clamp(0.0, 1.0) as f32,
        rgba[1].clamp(0.0, 1.0) as f32,
        rgba[2].clamp(0.0, 1.0) as f32,
        rgba[3].clamp(0.0, 1.0) as f32,
    ]
}

impl DvrRgbaFrameParts {
    fn new(pixel_count: usize) -> Self {
        Self {
            premultiplied_rgba: vec![[0.0; 4]; pixel_count],
        }
    }

    fn set(&mut self, index: usize, rgba: [f32; 4]) {
        self.premultiplied_rgba[index] = rgba;
    }

    fn into_frame(
        self,
        width: u64,
        height: u64,
        coverage: PixelCoverage,
    ) -> Result<DvrRgbaFrame, RenderError> {
        DvrRgbaFrame::try_new(width, height, self.premultiplied_rgba, coverage)
    }
}

impl IsoSurfaceFramePartsU16 {
    fn new(pixel_count: usize) -> Self {
        Self {
            source_values: vec![0; pixel_count],
            display_scalars: vec![0; pixel_count],
            material_scalars: vec![0; pixel_count],
            hit_depth: vec![0.0; pixel_count],
            normals: vec![IsoSurfaceNormal::ZERO; pixel_count],
            diffuse_lighting: vec![u16::MAX; pixel_count],
            specular_lighting: vec![0; pixel_count],
        }
    }

    fn set(&mut self, index: usize, sample: IsoRaySurfaceSampleU16) {
        self.source_values[index] = sample.source_value;
        self.display_scalars[index] = sample.display_scalar;
        self.material_scalars[index] = sample.material_scalar;
        self.hit_depth[index] = sample.hit_depth;
        self.normals[index] = sample.normal;
        self.diffuse_lighting[index] = sample.diffuse_lighting;
        self.specular_lighting[index] = sample.specular_lighting;
    }

    fn into_frame(
        self,
        width: u64,
        height: u64,
        coverage: PixelCoverage,
    ) -> Result<IsoSurfaceFrameU16, RenderError> {
        IsoSurfaceFrameU16::try_new(
            width,
            height,
            self.source_values,
            self.display_scalars,
            self.material_scalars,
            self.hit_depth,
            self.normals,
            self.diffuse_lighting,
            self.specular_lighting,
            coverage,
        )
    }
}

impl IsoSurfaceFramePartsF32 {
    fn new(pixel_count: usize) -> Self {
        Self {
            source_values: vec![0.0; pixel_count],
            display_scalars: vec![0.0; pixel_count],
            material_scalars: vec![0.0; pixel_count],
            hit_depth: vec![0.0; pixel_count],
            normals: vec![IsoSurfaceNormal::ZERO; pixel_count],
            diffuse_lighting: vec![u16::MAX; pixel_count],
            specular_lighting: vec![0; pixel_count],
        }
    }

    fn set(&mut self, index: usize, sample: IsoRaySurfaceSampleF32) {
        self.source_values[index] = sample.source_value;
        self.display_scalars[index] = sample.display_scalar;
        self.material_scalars[index] = sample.material_scalar;
        self.hit_depth[index] = sample.hit_depth;
        self.normals[index] = sample.normal;
        self.diffuse_lighting[index] = sample.diffuse_lighting;
        self.specular_lighting[index] = sample.specular_lighting;
    }

    fn into_frame(
        self,
        width: u64,
        height: u64,
        coverage: PixelCoverage,
    ) -> Result<IsoSurfaceFrameF32, RenderError> {
        IsoSurfaceFrameF32::try_new(
            width,
            height,
            self.source_values,
            self.display_scalars,
            self.material_scalars,
            self.hit_depth,
            self.normals,
            self.diffuse_lighting,
            self.specular_lighting,
            coverage,
        )
    }
}

fn lighting_to_u16(lighting: f64) -> u16 {
    round_to_u16(lighting.clamp(0.0, 1.0) * f64::from(u16::MAX))
}

fn dvr_step_scale(ray: GridRay, grid_to_world: GridToWorld) -> f64 {
    grid_to_world
        .transform_vector(ray.direction)
        .length()
        .max(EPSILON)
}

fn dvr_step_factor(delta_t: f64, step_scale: f64) -> f64 {
    (delta_t.max(0.0) * step_scale).max(EPSILON)
}

fn project_dvr_channels_along_grid_ray(
    channels: &[DvrVolumeChannel<'_>],
    shape: Shape3D,
    grid_to_world: GridToWorld,
    ray: GridRay,
    quality: CameraRenderQuality,
) -> DvrChannelSample {
    if ray.direction.length_squared() <= EPSILON {
        return DvrChannelSample {
            value: 0,
            covered: false,
            dvr_rgba: [0.0; 4],
        };
    }

    let Some(hit) = intersect_grid_box(ray, shape) else {
        return DvrChannelSample {
            value: 0,
            covered: false,
            dvr_rgba: [0.0; 4],
        };
    };
    if quality.intensity_sampling == IntensitySamplingPolicy::SmoothLinear {
        return project_smooth_dvr_channels_along_grid_ray(channels, grid_to_world, ray, hit);
    }

    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, shape.z);

    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut current_t = hit.enter;
    let step_scale = dvr_step_scale(ray, grid_to_world);
    let mut contributions = Vec::with_capacity(channels.len());
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
        let step_factor = dvr_step_factor(next_t.min(hit.exit) - current_t, step_scale);
        contributions.clear();
        for channel in channels {
            let Some(value) = dvr_channel_voxel_value(*channel, z.index, y.index, x.index) else {
                continue;
            };
            if let Some(contribution) =
                dvr_step_contribution(value, channel.parameters(), step_factor)
            {
                contributions.push(contribution);
            }
        }
        covered |= accumulate_dvr_step(
            &mut accumulated_rgba,
            &mut accumulated_alpha,
            &contributions,
        );
        if accumulated_alpha >= 0.995 {
            break;
        }

        if !next_t.is_finite() || next_t > hit.exit + EPSILON {
            break;
        }

        if x.next_t <= next_t + EPSILON {
            x.advance();
        }
        if y.next_t <= next_t + EPSILON {
            y.advance();
        }
        if z.next_t <= next_t + EPSILON {
            z.advance();
        }
        current_t = next_t;
    }

    DvrChannelSample {
        value: round_to_u16(accumulated_alpha * f64::from(u16::MAX)),
        covered,
        dvr_rgba: dvr_rgba_f32(accumulated_rgba),
    }
}

fn project_smooth_dvr_channels_along_grid_ray(
    channels: &[DvrVolumeChannel<'_>],
    grid_to_world: GridToWorld,
    ray: GridRay,
    hit: RayBoxHit,
) -> DvrChannelSample {
    let Some(step_t) = smooth_ray_step_t(ray) else {
        return DvrChannelSample {
            value: 0,
            covered: false,
            dvr_rgba: [0.0; 4],
        };
    };
    let mut t = hit.enter;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let step_factor = dvr_step_factor(step_t, dvr_step_scale(ray, grid_to_world));
    let mut contributions = Vec::with_capacity(channels.len());

    while t <= hit.exit + EPSILON {
        let point = ray.origin + ray.direction * t;
        contributions.clear();
        for channel in channels {
            let Some(value) = dvr_channel_smooth_value(*channel, point) else {
                continue;
            };
            if let Some(contribution) =
                dvr_step_contribution(value, channel.parameters(), step_factor)
            {
                contributions.push(contribution);
            }
        }
        covered |= accumulate_dvr_step(
            &mut accumulated_rgba,
            &mut accumulated_alpha,
            &contributions,
        );
        if accumulated_alpha >= 0.995 {
            break;
        }
        t += step_t;
    }

    DvrChannelSample {
        value: round_to_u16(accumulated_alpha * f64::from(u16::MAX)),
        covered,
        dvr_rgba: dvr_rgba_f32(accumulated_rgba),
    }
}

fn dvr_channel_voxel_value(channel: DvrVolumeChannel<'_>, z: i64, y: i64, x: i64) -> Option<f64> {
    if z < 0 || y < 0 || x < 0 {
        return None;
    }
    let (z, y, x) = (z as u64, y as u64, x as u64);
    match channel {
        DvrVolumeChannel::U16 { volume, .. } => volume.render_voxel(z, y, x).map(f64::from),
        DvrVolumeChannel::F32 { volume, .. } => volume.render_voxel(z, y, x).map(f64::from),
    }
}

fn dvr_channel_smooth_value(channel: DvrVolumeChannel<'_>, point: DVec3) -> Option<f64> {
    match channel {
        DvrVolumeChannel::U16 { volume, .. } => sample_trilinear_u16(volume, point),
        DvrVolumeChannel::F32 { volume, .. } => sample_trilinear_f32(volume, point).map(f64::from),
    }
}

fn project_along_grid_ray_u8(
    volume: &DenseVolumeU8,
    ray: GridRay,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> RenderedSampleU16 {
    if ray.direction.length_squared() <= EPSILON {
        return RenderedSampleU16 {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    }

    let Some(hit) = intersect_grid_box(ray, volume.shape) else {
        return RenderedSampleU16 {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    };
    if quality.intensity_sampling == IntensitySamplingPolicy::SmoothLinear {
        return project_smooth_along_grid_ray_u8(volume, ray, hit, mode, quality);
    }

    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, volume.shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, volume.shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, volume.shape.z);

    let mut max_value = 0u8;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut current_t = hit.enter;
    let step_scale = dvr_step_scale(ray, volume.grid_to_world);
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
        let step_factor = dvr_step_factor(next_t.min(hit.exit) - current_t, step_scale);

        if let Some(value) = volume.render_voxel(z.index as u64, y.index as u64, x.index as u64) {
            match mode {
                CameraRenderMode::Mip => {
                    max_value = max_value.max(value);
                    covered = true;
                }
                CameraRenderMode::Isosurface { parameters } => {
                    let display_scalar = parameters.map_u8(value);
                    if display_scalar >= parameters.level_f64() {
                        let point = DVec3::new(x.index as f64, y.index as f64, z.index as f64);
                        let hit = IsoSurfaceHit {
                            source_value: f64::from(value),
                            display_scalar,
                            material_display_scalar: display_scalar,
                            hit_t: point_ray_t(ray, point),
                            grid_position: point,
                        };
                        return RenderedSampleU16 {
                            value: iso_display_u16(hit),
                            covered: true,
                            iso_surface: Some(iso_surface_sample_u8(
                                hit,
                                gradient_display_u8(volume, point, parameters),
                                volume.grid_to_world,
                                quality,
                            )),
                            dvr_rgba: None,
                        };
                    }
                }
                CameraRenderMode::Dvr { parameters } => {
                    covered |= accumulate_dvr_sample(
                        &mut accumulated_rgba,
                        &mut accumulated_alpha,
                        f64::from(value),
                        parameters,
                        step_factor,
                    );
                    if accumulated_alpha >= 0.995 {
                        break;
                    }
                }
            }
        }

        if !next_t.is_finite() || next_t > hit.exit + EPSILON {
            break;
        }

        if x.next_t <= next_t + EPSILON {
            x.advance();
        }
        if y.next_t <= next_t + EPSILON {
            y.advance();
        }
        if z.next_t <= next_t + EPSILON {
            z.advance();
        }
        current_t = next_t;
    }

    let value = match mode {
        CameraRenderMode::Mip => u16::from(max_value),
        CameraRenderMode::Isosurface { .. } => 0,
        CameraRenderMode::Dvr { .. } => round_to_u16(accumulated_alpha * f64::from(u16::MAX)),
    };
    RenderedSampleU16 {
        value,
        covered,
        iso_surface: None,
        dvr_rgba: matches!(mode, CameraRenderMode::Dvr { .. })
            .then_some(dvr_rgba_f32(accumulated_rgba)),
    }
}

fn project_along_grid_ray(
    volume: &DenseVolumeU16,
    ray: GridRay,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> RenderedSampleU16 {
    if ray.direction.length_squared() <= EPSILON {
        return RenderedSampleU16 {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    }

    let Some(hit) = intersect_grid_box(ray, volume.shape) else {
        return RenderedSampleU16 {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    };
    if quality.intensity_sampling == IntensitySamplingPolicy::SmoothLinear {
        return project_smooth_along_grid_ray(volume, ray, hit, mode, quality);
    }

    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, volume.shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, volume.shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, volume.shape.z);

    let mut max_value = 0u16;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut current_t = hit.enter;
    let step_scale = dvr_step_scale(ray, volume.grid_to_world);
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
        let step_factor = dvr_step_factor(next_t.min(hit.exit) - current_t, step_scale);

        if let Some(value) = volume.render_voxel(z.index as u64, y.index as u64, x.index as u64) {
            match mode {
                CameraRenderMode::Mip => {
                    max_value = max_value.max(value);
                    covered = true;
                }
                CameraRenderMode::Isosurface { parameters } => {
                    let display_scalar = parameters.map_u16(value);
                    if display_scalar >= parameters.level_f64() {
                        let point = DVec3::new(x.index as f64, y.index as f64, z.index as f64);
                        let hit = IsoSurfaceHit {
                            source_value: f64::from(value),
                            display_scalar,
                            material_display_scalar: display_scalar,
                            hit_t: point_ray_t(ray, point),
                            grid_position: point,
                        };
                        return RenderedSampleU16 {
                            value: iso_display_u16(hit),
                            covered: true,
                            iso_surface: Some(iso_surface_sample_u16(
                                hit,
                                gradient_display_u16(volume, point, parameters),
                                volume.grid_to_world,
                                quality,
                            )),
                            dvr_rgba: None,
                        };
                    }
                }
                CameraRenderMode::Dvr { parameters } => {
                    covered |= accumulate_dvr_sample(
                        &mut accumulated_rgba,
                        &mut accumulated_alpha,
                        f64::from(value),
                        parameters,
                        step_factor,
                    );
                    if accumulated_alpha >= 0.995 {
                        break;
                    }
                }
            }
        }

        if !next_t.is_finite() || next_t > hit.exit + EPSILON {
            break;
        }

        if x.next_t <= next_t + EPSILON {
            x.advance();
        }
        if y.next_t <= next_t + EPSILON {
            y.advance();
        }
        if z.next_t <= next_t + EPSILON {
            z.advance();
        }
        current_t = next_t;
    }

    let value = match mode {
        CameraRenderMode::Mip => max_value,
        CameraRenderMode::Isosurface { .. } => 0,
        CameraRenderMode::Dvr { .. } => round_to_u16(accumulated_alpha * f64::from(u16::MAX)),
    };
    RenderedSampleU16 {
        value,
        covered,
        iso_surface: None,
        dvr_rgba: matches!(mode, CameraRenderMode::Dvr { .. })
            .then_some(dvr_rgba_f32(accumulated_rgba)),
    }
}

fn project_along_grid_ray_f32(
    volume: &DenseVolumeF32,
    ray: GridRay,
    mode: CameraRenderModeF32,
    quality: CameraRenderQuality,
) -> RenderedSampleF32 {
    if ray.direction.length_squared() <= EPSILON {
        return RenderedSampleF32 {
            value: 0.0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    }

    let Some(hit) = intersect_grid_box(ray, volume.shape) else {
        return RenderedSampleF32 {
            value: 0.0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    };
    if quality.intensity_sampling == IntensitySamplingPolicy::SmoothLinear {
        return project_smooth_along_grid_ray_f32(volume, ray, hit, mode, quality);
    }

    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, volume.shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, volume.shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, volume.shape.z);

    let mut max_value: Option<f32> = None;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut current_t = hit.enter;
    let step_scale = dvr_step_scale(ray, volume.grid_to_world);
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
        let step_factor = dvr_step_factor(next_t.min(hit.exit) - current_t, step_scale);

        if let Some(value) = volume.render_voxel(z.index as u64, y.index as u64, x.index as u64) {
            match mode {
                CameraRenderModeF32::Mip => {
                    max_value = Some(max_value.map(|current| current.max(value)).unwrap_or(value));
                    covered = true;
                }
                CameraRenderModeF32::Isosurface { parameters } => {
                    let display_scalar = parameters.map_f32(value);
                    if display_scalar >= parameters.level_f64() {
                        let point = DVec3::new(x.index as f64, y.index as f64, z.index as f64);
                        let hit = IsoSurfaceHit {
                            source_value: f64::from(value),
                            display_scalar,
                            material_display_scalar: display_scalar,
                            hit_t: point_ray_t(ray, point),
                            grid_position: point,
                        };
                        return RenderedSampleF32 {
                            value: iso_display_f32(hit),
                            covered: true,
                            iso_surface: Some(iso_surface_sample_f32(
                                hit,
                                gradient_display_f32(volume, point, parameters),
                                volume.grid_to_world,
                                quality,
                            )),
                            dvr_rgba: None,
                        };
                    }
                }
                CameraRenderModeF32::Dvr { parameters } => {
                    covered |= accumulate_dvr_sample(
                        &mut accumulated_rgba,
                        &mut accumulated_alpha,
                        f64::from(value),
                        parameters,
                        step_factor,
                    );
                    if accumulated_alpha >= 0.995 {
                        break;
                    }
                }
            }
        }

        if !next_t.is_finite() || next_t > hit.exit + EPSILON {
            break;
        }

        if x.next_t <= next_t + EPSILON {
            x.advance();
        }
        if y.next_t <= next_t + EPSILON {
            y.advance();
        }
        if z.next_t <= next_t + EPSILON {
            z.advance();
        }
        current_t = next_t;
    }

    let value = match mode {
        CameraRenderModeF32::Mip => max_value.unwrap_or(0.0),
        CameraRenderModeF32::Isosurface { .. } => 0.0,
        CameraRenderModeF32::Dvr { .. } => accumulated_alpha as f32,
    };
    RenderedSampleF32 {
        value,
        covered,
        iso_surface: None,
        dvr_rgba: matches!(mode, CameraRenderModeF32::Dvr { .. })
            .then_some(dvr_rgba_f32(accumulated_rgba)),
    }
}

mod geometry;
use geometry::*;
