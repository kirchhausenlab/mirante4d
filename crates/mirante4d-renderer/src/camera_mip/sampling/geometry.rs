use super::*;

pub(super) fn project_smooth_along_grid_ray_u8(
    volume: &DenseVolumeU8,
    ray: GridRay,
    hit: RayBoxHit,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> RenderedSampleU16 {
    let Some(step_t) = smooth_ray_step_t(ray) else {
        return RenderedSampleU16 {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    };
    let mut t = hit.enter;
    let mut max_value: f64 = 0.0;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut previous_iso_sample: Option<(f64, f64, f64)> = None;
    let step_factor = dvr_step_factor(step_t, dvr_step_scale(ray, volume.grid_to_world));

    while t <= hit.exit + EPSILON {
        let point = ray.origin + ray.direction * t;
        let Some(value) = sample_trilinear_u8(volume, point) else {
            t += step_t;
            continue;
        };
        match mode {
            CameraRenderMode::Mip => {
                max_value = max_value.max(value);
                covered = true;
            }
            CameraRenderMode::Isosurface { parameters } => {
                let display_scalar = parameters.transfer.map_source_value_f64(value);
                if display_scalar >= parameters.level_f64() {
                    let hit = refined_iso_hit(
                        previous_iso_sample,
                        t,
                        value,
                        display_scalar,
                        ray,
                        parameters,
                    );
                    return RenderedSampleU16 {
                        value: iso_display_u16(hit),
                        covered: true,
                        iso_surface: Some(iso_surface_sample_u8(
                            hit,
                            gradient_display_u8(volume, hit.grid_position, parameters),
                            volume.grid_to_world,
                            quality,
                        )),
                        dvr_rgba: None,
                    };
                }
                previous_iso_sample = Some((t, value, display_scalar));
            }
            CameraRenderMode::Dvr { parameters } => {
                covered |= accumulate_dvr_sample(
                    &mut accumulated_rgba,
                    &mut accumulated_alpha,
                    value,
                    parameters,
                    step_factor,
                );
                if accumulated_alpha >= 0.995 {
                    break;
                }
            }
        }
        t += step_t;
    }

    let value = match mode {
        CameraRenderMode::Mip => round_to_u16(max_value),
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

pub(super) fn project_smooth_along_grid_ray(
    volume: &DenseVolumeU16,
    ray: GridRay,
    hit: RayBoxHit,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> RenderedSampleU16 {
    let Some(step_t) = smooth_ray_step_t(ray) else {
        return RenderedSampleU16 {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    };
    let mut t = hit.enter;
    let mut max_value: f64 = 0.0;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut previous_iso_sample: Option<(f64, f64, f64)> = None;
    let step_factor = dvr_step_factor(step_t, dvr_step_scale(ray, volume.grid_to_world));

    while t <= hit.exit + EPSILON {
        let point = ray.origin + ray.direction * t;
        let Some(value) = sample_trilinear_u16(volume, point) else {
            t += step_t;
            continue;
        };
        match mode {
            CameraRenderMode::Mip => {
                max_value = max_value.max(value);
                covered = true;
            }
            CameraRenderMode::Isosurface { parameters } => {
                let display_scalar = parameters.transfer.map_source_value_f64(value);
                if display_scalar >= parameters.level_f64() {
                    let hit = refined_iso_hit(
                        previous_iso_sample,
                        t,
                        value,
                        display_scalar,
                        ray,
                        parameters,
                    );
                    return RenderedSampleU16 {
                        value: iso_display_u16(hit),
                        covered: true,
                        iso_surface: Some(iso_surface_sample_u16(
                            hit,
                            gradient_display_u16(volume, hit.grid_position, parameters),
                            volume.grid_to_world,
                            quality,
                        )),
                        dvr_rgba: None,
                    };
                }
                previous_iso_sample = Some((t, value, display_scalar));
            }
            CameraRenderMode::Dvr { parameters } => {
                covered |= accumulate_dvr_sample(
                    &mut accumulated_rgba,
                    &mut accumulated_alpha,
                    value,
                    parameters,
                    step_factor,
                );
                if accumulated_alpha >= 0.995 {
                    break;
                }
            }
        }
        t += step_t;
    }

    let value = match mode {
        CameraRenderMode::Mip => round_to_u16(max_value),
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

pub(super) fn project_smooth_along_grid_ray_f32(
    volume: &DenseVolumeF32,
    ray: GridRay,
    hit: RayBoxHit,
    mode: CameraRenderModeF32,
    quality: CameraRenderQuality,
) -> RenderedSampleF32 {
    let Some(step_t) = smooth_ray_step_t(ray) else {
        return RenderedSampleF32 {
            value: 0.0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
        };
    };
    let mut t = hit.enter;
    let mut max_value: Option<f32> = None;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut previous_iso_sample: Option<(f64, f64, f64)> = None;
    let step_factor = dvr_step_factor(step_t, dvr_step_scale(ray, volume.grid_to_world));

    while t <= hit.exit + EPSILON {
        let point = ray.origin + ray.direction * t;
        let Some(value) = sample_trilinear_f32(volume, point) else {
            t += step_t;
            continue;
        };
        match mode {
            CameraRenderModeF32::Mip => {
                max_value = Some(max_value.map(|current| current.max(value)).unwrap_or(value));
                covered = true;
            }
            CameraRenderModeF32::Isosurface { parameters } => {
                let display_scalar = parameters.map_f32(value);
                if display_scalar >= parameters.level_f64() {
                    let hit = refined_iso_hit(
                        previous_iso_sample,
                        t,
                        f64::from(value),
                        display_scalar,
                        ray,
                        parameters,
                    );
                    return RenderedSampleF32 {
                        value: iso_display_f32(hit),
                        covered: true,
                        iso_surface: Some(iso_surface_sample_f32(
                            hit,
                            gradient_display_f32(volume, hit.grid_position, parameters),
                            volume.grid_to_world,
                            quality,
                        )),
                        dvr_rgba: None,
                    };
                }
                previous_iso_sample = Some((t, f64::from(value), display_scalar));
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
        t += step_t;
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

pub(super) fn smooth_ray_step_t(ray: GridRay) -> Option<f64> {
    let length = ray.direction.length();
    (length > EPSILON).then_some(SMOOTH_RAY_STEP_VOXELS / length)
}

pub(super) fn pick_along_grid_ray(
    volume: &DenseVolumeU16,
    ray: GridRay,
    mode: CameraRenderMode,
) -> CameraVolumePick {
    let policy = match mode {
        CameraRenderMode::Mip => PickPolicy::MipArgmax,
        CameraRenderMode::Isosurface { .. } => PickPolicy::FirstThresholdHit,
        CameraRenderMode::Dvr { .. } => PickPolicy::ProbeRay,
    };
    if ray.direction.length_squared() <= EPSILON {
        return empty_pick(policy);
    }

    let Some(hit) = intersect_grid_box(ray, volume.shape) else {
        return empty_pick(policy);
    };
    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, volume.shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, volume.shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, volume.shape.z);

    let mut selected: Option<PickSample> = None;
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        if let Some(intensity) = volume.render_voxel(z.index as u64, y.index as u64, x.index as u64)
        {
            let sample = PickSample {
                intensity,
                z: z.index as u64,
                y: y.index as u64,
                x: x.index as u64,
            };
            match mode {
                CameraRenderMode::Mip => {
                    if selected
                        .map(|current| sample.intensity > current.intensity)
                        .unwrap_or(true)
                    {
                        selected = Some(sample);
                    }
                }
                CameraRenderMode::Isosurface { parameters } => {
                    if parameters.map_u16(sample.intensity) >= parameters.level_f64() {
                        selected = Some(sample);
                        break;
                    }
                }
                CameraRenderMode::Dvr { .. } => {
                    if sample.intensity > 0 {
                        selected = Some(sample);
                        break;
                    }
                }
            }
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
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
    }

    selected
        .map(|sample| pick_from_sample(volume, sample, policy))
        .unwrap_or_else(|| empty_pick(policy))
}

pub(super) fn pick_along_grid_ray_u8(
    volume: &DenseVolumeU8,
    ray: GridRay,
    mode: CameraRenderMode,
) -> CameraVolumePickU8 {
    let policy = match mode {
        CameraRenderMode::Mip => PickPolicy::MipArgmax,
        CameraRenderMode::Isosurface { .. } => PickPolicy::FirstThresholdHit,
        CameraRenderMode::Dvr { .. } => PickPolicy::ProbeRay,
    };
    if ray.direction.length_squared() <= EPSILON {
        return empty_pick_u8(policy);
    }

    let Some(hit) = intersect_grid_box(ray, volume.shape) else {
        return empty_pick_u8(policy);
    };
    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, volume.shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, volume.shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, volume.shape.z);

    let mut selected: Option<PickSampleU8> = None;
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        if let Some(intensity) = volume.render_voxel(z.index as u64, y.index as u64, x.index as u64)
        {
            let sample = PickSampleU8 {
                intensity,
                z: z.index as u64,
                y: y.index as u64,
                x: x.index as u64,
            };
            match mode {
                CameraRenderMode::Mip => {
                    if selected
                        .map(|current| sample.intensity > current.intensity)
                        .unwrap_or(true)
                    {
                        selected = Some(sample);
                    }
                }
                CameraRenderMode::Isosurface { parameters } => {
                    if parameters.map_u16(u16::from(sample.intensity)) >= parameters.level_f64() {
                        selected = Some(sample);
                        break;
                    }
                }
                CameraRenderMode::Dvr { .. } => {
                    if sample.intensity > 0 {
                        selected = Some(sample);
                        break;
                    }
                }
            }
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
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
    }

    selected
        .map(|sample| pick_from_sample_u8(volume, sample, policy))
        .unwrap_or_else(|| empty_pick_u8(policy))
}

pub(super) fn pick_along_grid_ray_f32(
    volume: &DenseVolumeF32,
    ray: GridRay,
    mode: CameraRenderModeF32,
) -> CameraVolumePickF32 {
    let policy = match mode {
        CameraRenderModeF32::Mip => PickPolicy::MipArgmax,
        CameraRenderModeF32::Isosurface { .. } => PickPolicy::FirstThresholdHit,
        CameraRenderModeF32::Dvr { .. } => PickPolicy::ProbeRay,
    };
    if ray.direction.length_squared() <= EPSILON {
        return empty_pick_f32(policy);
    }

    let Some(hit) = intersect_grid_box(ray, volume.shape) else {
        return empty_pick_f32(policy);
    };
    let entry = ray.origin + ray.direction * hit.enter;
    let mut x = AxisTraversal::new(entry.x, ray.direction.x, hit.enter, volume.shape.x);
    let mut y = AxisTraversal::new(entry.y, ray.direction.y, hit.enter, volume.shape.y);
    let mut z = AxisTraversal::new(entry.z, ray.direction.z, hit.enter, volume.shape.z);

    let mut selected: Option<PickSampleF32> = None;
    loop {
        if !x.is_inside() || !y.is_inside() || !z.is_inside() {
            break;
        }

        if let Some(intensity) = volume.render_voxel(z.index as u64, y.index as u64, x.index as u64)
        {
            let sample = PickSampleF32 {
                intensity,
                z: z.index as u64,
                y: y.index as u64,
                x: x.index as u64,
            };
            match mode {
                CameraRenderModeF32::Mip => {
                    if selected
                        .map(|current| sample.intensity > current.intensity)
                        .unwrap_or(true)
                    {
                        selected = Some(sample);
                    }
                }
                CameraRenderModeF32::Isosurface { parameters } => {
                    if parameters.map_f32(sample.intensity) >= parameters.level_f64() {
                        selected = Some(sample);
                        break;
                    }
                }
                CameraRenderModeF32::Dvr { .. } => {
                    if sample.intensity != 0.0 {
                        selected = Some(sample);
                        break;
                    }
                }
            }
        }

        let next_t = x.next_t.min(y.next_t.min(z.next_t));
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
    }

    selected
        .map(|sample| pick_from_sample_f32(volume, sample, policy))
        .unwrap_or_else(|| empty_pick_f32(policy))
}

pub(super) fn pick_from_sample(
    volume: &DenseVolumeU16,
    sample: PickSample,
    policy: PickPolicy,
) -> CameraVolumePick {
    let grid_position = GridPosition {
        z: sample.z as f64,
        y: sample.y as f64,
        x: sample.x as f64,
    };
    let world_position = volume.grid_to_world.transform_point(DVec3::new(
        sample.x as f64 + 0.5,
        sample.y as f64 + 0.5,
        sample.z as f64 + 0.5,
    ));
    CameraVolumePick {
        intensity: sample.intensity,
        world_position: Some(world_position),
        grid_position: Some(grid_position),
        policy,
        completeness: PickCompleteness::Exact,
    }
}

pub(super) fn pick_from_sample_u8(
    volume: &DenseVolumeU8,
    sample: PickSampleU8,
    policy: PickPolicy,
) -> CameraVolumePickU8 {
    let grid_position = GridPosition {
        z: sample.z as f64,
        y: sample.y as f64,
        x: sample.x as f64,
    };
    let world_position = volume.grid_to_world.transform_point(DVec3::new(
        sample.x as f64 + 0.5,
        sample.y as f64 + 0.5,
        sample.z as f64 + 0.5,
    ));
    CameraVolumePickU8 {
        intensity: sample.intensity,
        world_position: Some(world_position),
        grid_position: Some(grid_position),
        policy,
        completeness: PickCompleteness::Exact,
    }
}

pub(super) fn pick_from_sample_f32(
    volume: &DenseVolumeF32,
    sample: PickSampleF32,
    policy: PickPolicy,
) -> CameraVolumePickF32 {
    let grid_position = GridPosition {
        z: sample.z as f64,
        y: sample.y as f64,
        x: sample.x as f64,
    };
    let world_position = volume.grid_to_world.transform_point(DVec3::new(
        sample.x as f64 + 0.5,
        sample.y as f64 + 0.5,
        sample.z as f64 + 0.5,
    ));
    CameraVolumePickF32 {
        intensity: sample.intensity,
        world_position: Some(world_position),
        grid_position: Some(grid_position),
        policy,
        completeness: PickCompleteness::Exact,
    }
}

pub(super) fn empty_pick(policy: PickPolicy) -> CameraVolumePick {
    CameraVolumePick {
        intensity: 0,
        world_position: None,
        grid_position: None,
        policy,
        completeness: PickCompleteness::Exact,
    }
}

pub(super) fn empty_pick_u8(policy: PickPolicy) -> CameraVolumePickU8 {
    CameraVolumePickU8 {
        intensity: 0,
        world_position: None,
        grid_position: None,
        policy,
        completeness: PickCompleteness::Exact,
    }
}

pub(super) fn empty_pick_f32(policy: PickPolicy) -> CameraVolumePickF32 {
    CameraVolumePickF32 {
        intensity: 0.0,
        world_position: None,
        grid_position: None,
        policy,
        completeness: PickCompleteness::Exact,
    }
}

pub(super) fn sample_trilinear_u8(volume: &DenseVolumeU8, point: DVec3) -> Option<f64> {
    let x = interpolation_axis(point.x, volume.shape.x)?;
    let y = interpolation_axis(point.y, volume.shape.y)?;
    let z = interpolation_axis(point.z, volume.shape.z)?;
    let c000 = f64::from(volume.render_voxel(z.lower, y.lower, x.lower)?);
    let c100 = f64::from(volume.render_voxel(z.lower, y.lower, x.upper)?);
    let c010 = f64::from(volume.render_voxel(z.lower, y.upper, x.lower)?);
    let c110 = f64::from(volume.render_voxel(z.lower, y.upper, x.upper)?);
    let c001 = f64::from(volume.render_voxel(z.upper, y.lower, x.lower)?);
    let c101 = f64::from(volume.render_voxel(z.upper, y.lower, x.upper)?);
    let c011 = f64::from(volume.render_voxel(z.upper, y.upper, x.lower)?);
    let c111 = f64::from(volume.render_voxel(z.upper, y.upper, x.upper)?);
    Some(trilinear_value(
        c000, c100, c010, c110, c001, c101, c011, c111, x.fraction, y.fraction, z.fraction,
    ))
}

pub(super) fn sample_trilinear_u16(volume: &DenseVolumeU16, point: DVec3) -> Option<f64> {
    let x = interpolation_axis(point.x, volume.shape.x)?;
    let y = interpolation_axis(point.y, volume.shape.y)?;
    let z = interpolation_axis(point.z, volume.shape.z)?;
    let c000 = f64::from(volume.render_voxel(z.lower, y.lower, x.lower)?);
    let c100 = f64::from(volume.render_voxel(z.lower, y.lower, x.upper)?);
    let c010 = f64::from(volume.render_voxel(z.lower, y.upper, x.lower)?);
    let c110 = f64::from(volume.render_voxel(z.lower, y.upper, x.upper)?);
    let c001 = f64::from(volume.render_voxel(z.upper, y.lower, x.lower)?);
    let c101 = f64::from(volume.render_voxel(z.upper, y.lower, x.upper)?);
    let c011 = f64::from(volume.render_voxel(z.upper, y.upper, x.lower)?);
    let c111 = f64::from(volume.render_voxel(z.upper, y.upper, x.upper)?);
    Some(trilinear_value(
        c000, c100, c010, c110, c001, c101, c011, c111, x.fraction, y.fraction, z.fraction,
    ))
}

pub(super) fn sample_trilinear_f32(volume: &DenseVolumeF32, point: DVec3) -> Option<f32> {
    let x = interpolation_axis(point.x, volume.shape.x)?;
    let y = interpolation_axis(point.y, volume.shape.y)?;
    let z = interpolation_axis(point.z, volume.shape.z)?;
    let c000 = f64::from(volume.render_voxel(z.lower, y.lower, x.lower)?);
    let c100 = f64::from(volume.render_voxel(z.lower, y.lower, x.upper)?);
    let c010 = f64::from(volume.render_voxel(z.lower, y.upper, x.lower)?);
    let c110 = f64::from(volume.render_voxel(z.lower, y.upper, x.upper)?);
    let c001 = f64::from(volume.render_voxel(z.upper, y.lower, x.lower)?);
    let c101 = f64::from(volume.render_voxel(z.upper, y.lower, x.upper)?);
    let c011 = f64::from(volume.render_voxel(z.upper, y.upper, x.lower)?);
    let c111 = f64::from(volume.render_voxel(z.upper, y.upper, x.upper)?);
    Some(trilinear_value(
        c000, c100, c010, c110, c001, c101, c011, c111, x.fraction, y.fraction, z.fraction,
    ) as f32)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct InterpolationAxis {
    lower: u64,
    upper: u64,
    fraction: f64,
}

pub(super) fn interpolation_axis(coordinate: f64, limit: u64) -> Option<InterpolationAxis> {
    if !coordinate.is_finite() || limit == 0 {
        return None;
    }
    let maximum = limit as f64 - 1.0;
    if coordinate < -0.5 - EPSILON || coordinate > maximum + 0.5 + EPSILON {
        return None;
    }
    let clamped = coordinate.clamp(0.0, maximum);
    let lower = clamped.floor() as u64;
    let upper = (lower + 1).min(limit - 1);
    Some(InterpolationAxis {
        lower,
        upper,
        fraction: if lower == upper {
            0.0
        } else {
            clamped - lower as f64
        },
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trilinear_value(
    c000: f64,
    c100: f64,
    c010: f64,
    c110: f64,
    c001: f64,
    c101: f64,
    c011: f64,
    c111: f64,
    fx: f64,
    fy: f64,
    fz: f64,
) -> f64 {
    let c00 = lerp(c000, c100, fx);
    let c10 = lerp(c010, c110, fx);
    let c01 = lerp(c001, c101, fx);
    let c11 = lerp(c011, c111, fx);
    let c0 = lerp(c00, c10, fy);
    let c1 = lerp(c01, c11, fy);
    lerp(c0, c1, fz)
}

pub(super) fn lerp(a: f64, b: f64, fraction: f64) -> f64 {
    a + (b - a) * fraction
}

pub(super) fn refined_iso_hit(
    previous_sample: Option<(f64, f64, f64)>,
    current_t: f64,
    current_source_value: f64,
    current_display_scalar: f64,
    ray: GridRay,
    parameters: IsoSurfaceParameters,
) -> IsoSurfaceHit {
    let mut hit_t = current_t;
    let mut source_value = current_source_value;
    let mut display_scalar = current_display_scalar;
    let material_display_scalar = current_display_scalar;
    if let Some((previous_t, previous_source_value, previous_display_scalar)) = previous_sample
        && previous_display_scalar < parameters.level_f64()
        && (current_display_scalar - previous_display_scalar).abs() > EPSILON
    {
        let fraction = ((parameters.level_f64() - previous_display_scalar)
            / (current_display_scalar - previous_display_scalar))
            .clamp(0.0, 1.0);
        hit_t = lerp(previous_t, current_t, fraction);
        source_value = lerp(previous_source_value, current_source_value, fraction);
        display_scalar = parameters.level_f64();
    }
    IsoSurfaceHit {
        source_value,
        display_scalar,
        material_display_scalar,
        hit_t,
        grid_position: ray.origin + ray.direction * hit_t,
    }
}

pub(super) fn gradient_display_u16(
    volume: &DenseVolumeU16,
    point: DVec3,
    parameters: IsoSurfaceParameters,
) -> Option<DVec3> {
    let dx = central_difference_display_u16(volume, point, DVec3::X, parameters)?;
    let dy = central_difference_display_u16(volume, point, DVec3::Y, parameters)?;
    let dz = central_difference_display_u16(volume, point, DVec3::Z, parameters)?;
    Some(DVec3::new(dx, dy, dz))
}

pub(super) fn gradient_display_u8(
    volume: &DenseVolumeU8,
    point: DVec3,
    parameters: IsoSurfaceParameters,
) -> Option<DVec3> {
    let dx = central_difference_display_u8(volume, point, DVec3::X, parameters)?;
    let dy = central_difference_display_u8(volume, point, DVec3::Y, parameters)?;
    let dz = central_difference_display_u8(volume, point, DVec3::Z, parameters)?;
    Some(DVec3::new(dx, dy, dz))
}

pub(super) fn gradient_display_f32(
    volume: &DenseVolumeF32,
    point: DVec3,
    parameters: IsoSurfaceParameters,
) -> Option<DVec3> {
    let dx = central_difference_display_f32(volume, point, DVec3::X, parameters)?;
    let dy = central_difference_display_f32(volume, point, DVec3::Y, parameters)?;
    let dz = central_difference_display_f32(volume, point, DVec3::Z, parameters)?;
    Some(DVec3::new(dx, dy, dz))
}

pub(super) fn central_difference_display_u8(
    volume: &DenseVolumeU8,
    point: DVec3,
    axis: DVec3,
    parameters: IsoSurfaceParameters,
) -> Option<f64> {
    let center = parameters
        .transfer
        .map_source_value_f64(sample_trilinear_u8(volume, point)?);
    match (
        sample_trilinear_u8(volume, point - axis)
            .map(|value| parameters.transfer.map_source_value_f64(value)),
        sample_trilinear_u8(volume, point + axis)
            .map(|value| parameters.transfer.map_source_value_f64(value)),
    ) {
        (Some(minus), Some(plus)) => Some((plus - minus) * 0.5),
        (None, Some(plus)) => Some(plus - center),
        (Some(minus), None) => Some(center - minus),
        (None, None) => Some(0.0),
    }
}

pub(super) fn central_difference_display_u16(
    volume: &DenseVolumeU16,
    point: DVec3,
    axis: DVec3,
    parameters: IsoSurfaceParameters,
) -> Option<f64> {
    let center = parameters
        .transfer
        .map_source_value_f64(sample_trilinear_u16(volume, point)?);
    match (
        sample_trilinear_u16(volume, point - axis)
            .map(|value| parameters.transfer.map_source_value_f64(value)),
        sample_trilinear_u16(volume, point + axis)
            .map(|value| parameters.transfer.map_source_value_f64(value)),
    ) {
        (Some(minus), Some(plus)) => Some((plus - minus) * 0.5),
        (None, Some(plus)) => Some(plus - center),
        (Some(minus), None) => Some(center - minus),
        (None, None) => Some(0.0),
    }
}

pub(super) fn central_difference_display_f32(
    volume: &DenseVolumeF32,
    point: DVec3,
    axis: DVec3,
    parameters: IsoSurfaceParameters,
) -> Option<f64> {
    let center = parameters.map_f32(sample_trilinear_f32(volume, point)?);
    match (
        sample_trilinear_f32(volume, point - axis).map(|value| parameters.map_f32(value)),
        sample_trilinear_f32(volume, point + axis).map(|value| parameters.map_f32(value)),
    ) {
        (Some(minus), Some(plus)) => Some((plus - minus) * 0.5),
        (None, Some(plus)) => Some(plus - center),
        (Some(minus), None) => Some(center - minus),
        (None, None) => Some(0.0),
    }
}

pub(super) fn iso_display_u16(hit: IsoSurfaceHit) -> u16 {
    debug_assert!(hit.source_value.is_finite());
    round_to_u16(hit.display_scalar * f64::from(u16::MAX))
}

pub(super) fn iso_display_f32(hit: IsoSurfaceHit) -> f32 {
    debug_assert!(hit.source_value.is_finite());
    hit.display_scalar.clamp(0.0, 1.0) as f32
}

pub(super) fn iso_surface_sample_u16(
    hit: IsoSurfaceHit,
    gradient: Option<DVec3>,
    grid_to_world: GridToWorld,
    quality: CameraRenderQuality,
) -> IsoRaySurfaceSampleU16 {
    debug_assert!(hit.source_value.is_finite());
    let shading = iso_surface_shading(gradient, grid_to_world, quality);
    IsoRaySurfaceSampleU16 {
        source_value: round_to_u16(hit.source_value),
        display_scalar: iso_display_u16(hit),
        material_scalar: round_to_u16(hit.material_display_scalar * f64::from(u16::MAX)),
        hit_depth: hit.hit_t as f32,
        normal: shading.normal,
        diffuse_lighting: lighting_to_u16(shading.diffuse),
        specular_lighting: lighting_to_u16(shading.specular),
    }
}

pub(super) fn iso_surface_sample_u8(
    hit: IsoSurfaceHit,
    gradient: Option<DVec3>,
    grid_to_world: GridToWorld,
    quality: CameraRenderQuality,
) -> IsoRaySurfaceSampleU16 {
    debug_assert!(hit.source_value.is_finite());
    let shading = iso_surface_shading(gradient, grid_to_world, quality);
    IsoRaySurfaceSampleU16 {
        source_value: round_to_u16(hit.source_value),
        display_scalar: iso_display_u16(hit),
        material_scalar: round_to_u16(hit.material_display_scalar * f64::from(u16::MAX)),
        hit_depth: hit.hit_t as f32,
        normal: shading.normal,
        diffuse_lighting: lighting_to_u16(shading.diffuse),
        specular_lighting: lighting_to_u16(shading.specular),
    }
}

pub(super) fn iso_surface_sample_f32(
    hit: IsoSurfaceHit,
    gradient: Option<DVec3>,
    grid_to_world: GridToWorld,
    quality: CameraRenderQuality,
) -> IsoRaySurfaceSampleF32 {
    debug_assert!(hit.source_value.is_finite());
    let shading = iso_surface_shading(gradient, grid_to_world, quality);
    IsoRaySurfaceSampleF32 {
        source_value: hit.source_value as f32,
        display_scalar: iso_display_f32(hit),
        material_scalar: hit.material_display_scalar.clamp(0.0, 1.0) as f32,
        hit_depth: hit.hit_t as f32,
        normal: shading.normal,
        diffuse_lighting: lighting_to_u16(shading.diffuse),
        specular_lighting: lighting_to_u16(shading.specular),
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct IsoSurfaceShading {
    normal: IsoSurfaceNormal,
    diffuse: f64,
    specular: f64,
}

pub(super) fn iso_surface_shading(
    gradient: Option<DVec3>,
    grid_to_world: GridToWorld,
    quality: CameraRenderQuality,
) -> IsoSurfaceShading {
    if quality.iso_shading != IsoShadingMode::GradientLighting {
        return flat_iso_surface_shading();
    }
    let Some(gradient) = gradient.map(|gradient| world_space_gradient(gradient, grid_to_world))
    else {
        return flat_iso_surface_shading();
    };
    let normal = gradient.normalize_or_zero();
    if normal.length_squared() <= EPSILON {
        return flat_iso_surface_shading();
    }
    IsoSurfaceShading {
        normal: IsoSurfaceNormal::from_unit_components(normal.x, normal.y, normal.z),
        diffuse: 1.0,
        specular: 0.0,
    }
}

pub(super) fn flat_iso_surface_shading() -> IsoSurfaceShading {
    IsoSurfaceShading {
        normal: IsoSurfaceNormal::ZERO,
        diffuse: 1.0,
        specular: 0.0,
    }
}

pub(super) fn point_ray_t(ray: GridRay, point: DVec3) -> f64 {
    let denom = ray.direction.length_squared();
    if denom <= EPSILON {
        0.0
    } else {
        (point - ray.origin).dot(ray.direction) / denom
    }
}

pub(super) fn world_space_gradient(grid_gradient: DVec3, grid_to_world: GridToWorld) -> DVec3 {
    grid_to_world
        .to_dmat4()
        .inverse()
        .transpose()
        .transform_vector3(grid_gradient)
}

pub(super) fn round_to_u16(value: f64) -> u16 {
    value.clamp(0.0, f64::from(u16::MAX)).round() as u16
}

pub(super) fn initial_voxel_index(coordinate: f64, direction: f64, limit: i64) -> i64 {
    let adjusted = if direction < -EPSILON {
        coordinate + 0.5 - EPSILON
    } else {
        coordinate + 0.5
    };
    (adjusted.floor() as i64).clamp(0, limit - 1)
}

pub(super) fn intersect_grid_box(ray: GridRay, shape: Shape3D) -> Option<RayBoxHit> {
    let mut enter = f64::NEG_INFINITY;
    let mut exit = f64::INFINITY;

    slab(
        ray.origin.x,
        ray.direction.x,
        -0.5,
        shape.x as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;
    slab(
        ray.origin.y,
        ray.direction.y,
        -0.5,
        shape.y as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;
    slab(
        ray.origin.z,
        ray.direction.z,
        -0.5,
        shape.z as f64 - 0.5,
        &mut enter,
        &mut exit,
    )?;

    if exit < enter || exit < 0.0 {
        return None;
    }

    Some(RayBoxHit {
        enter: enter.max(0.0),
        exit,
    })
}

pub(super) fn slab(
    origin: f64,
    direction: f64,
    minimum: f64,
    maximum: f64,
    enter: &mut f64,
    exit: &mut f64,
) -> Option<()> {
    if direction.abs() <= EPSILON {
        if origin < minimum || origin > maximum {
            return None;
        }
        return Some(());
    }

    let near = (minimum - origin) / direction;
    let far = (maximum - origin) / direction;
    let axis_enter = near.min(far);
    let axis_exit = near.max(far);
    *enter = enter.max(axis_enter);
    *exit = exit.min(axis_exit);
    Some(())
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;
    use glam::DMat4;

    use super::*;

    #[test]
    fn world_space_gradient_uses_inverse_transpose_transform() {
        let grid_to_world = GridToWorld::from_dmat4(DMat4::from_cols_array(&[
            2.0, 0.0, 0.0, 0.0, //
            0.25, 3.0, 0.0, 0.0, //
            0.0, 0.5, 4.0, 0.0, //
            7.0, 11.0, 13.0, 1.0,
        ]));
        let grid_gradient = DVec3::new(1.0, 2.0, 3.0);

        let actual = world_space_gradient(grid_gradient, grid_to_world).normalize();
        let expected = grid_to_world
            .to_dmat4()
            .inverse()
            .transpose()
            .transform_vector3(grid_gradient)
            .normalize();

        assert_abs_diff_eq!(actual.x, expected.x, epsilon = 1e-12);
        assert_abs_diff_eq!(actual.y, expected.y, epsilon = 1e-12);
        assert_abs_diff_eq!(actual.z, expected.z, epsilon = 1e-12);
    }

    #[test]
    fn iso_surface_shading_keeps_canonical_normal_direction() {
        let shading = iso_surface_shading(
            Some(DVec3::new(0.0, 0.0, -1.0)),
            GridToWorld::identity(),
            CameraRenderQuality::smooth_linear(),
        );
        let normal = shading.normal.components_f32();

        assert_abs_diff_eq!(normal[0], 0.0, epsilon = 1e-6);
        assert_abs_diff_eq!(normal[1], 0.0, epsilon = 1e-6);
        assert!(normal[2] < -0.999);
        assert_abs_diff_eq!(shading.diffuse, 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(shading.specular, 0.0, epsilon = 1e-12);
    }
}
