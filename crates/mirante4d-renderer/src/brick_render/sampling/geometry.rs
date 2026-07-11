use super::*;

pub(super) fn project_smooth_volume_along_grid_ray(
    resident: &impl IntegerResidentSet,
    ray: GridRay,
    hit: RayBoxHit,
    mode: CameraRenderMode,
    quality: CameraRenderQuality,
) -> BrickRaySample {
    let Some(step_t) = smooth_ray_step_t(ray) else {
        return BrickRaySample {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 0,
        };
    };
    let mut t = hit.enter;
    let mut max_value: f64 = 0.0;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut missing_voxel_samples = 0u64;
    let mut previous_iso_sample: Option<(f64, f64, f64)> = None;
    let step_factor = dvr_step_factor(step_t, dvr_step_scale(ray, resident.grid_to_world()));

    while t <= hit.exit + EPSILON {
        let point = ray.origin + ray.direction * t;
        let sample = sample_trilinear_u16(resident, point);
        missing_voxel_samples += sample.missing_voxel_samples;
        if !sample.covered {
            t += step_t;
            continue;
        }
        match mode {
            CameraRenderMode::Mip => {
                max_value = max_value.max(sample.value);
                covered = true;
            }
            CameraRenderMode::Isosurface { parameters } => {
                let display_scalar = parameters.transfer.map_source_value_f64(sample.value);
                if display_scalar >= parameters.level_f64() {
                    let hit = refined_iso_hit(
                        previous_iso_sample,
                        t,
                        sample.value,
                        display_scalar,
                        ray,
                        parameters,
                    );
                    return BrickRaySample {
                        value: iso_display_u16(hit),
                        covered: true,
                        iso_surface: Some(iso_surface_sample_u16(
                            hit,
                            gradient_display_u16(resident, hit.grid_position, parameters),
                            resident.grid_to_world(),
                            quality,
                        )),
                        dvr_rgba: None,
                        missing_voxel_samples,
                    };
                }
                previous_iso_sample = Some((t, sample.value, display_scalar));
            }
            CameraRenderMode::Dvr { parameters } => {
                covered |= accumulate_dvr_sample(
                    &mut accumulated_rgba,
                    &mut accumulated_alpha,
                    sample.value,
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
    BrickRaySample {
        value,
        covered,
        iso_surface: None,
        dvr_rgba: matches!(mode, CameraRenderMode::Dvr { .. })
            .then_some(dvr_rgba_f32(accumulated_rgba)),
        missing_voxel_samples,
    }
}

pub(super) fn project_smooth_f32_volume_along_grid_ray(
    resident: &ResidentBrickSetF32,
    ray: GridRay,
    hit: RayBoxHit,
    mode: CameraRenderModeF32,
    quality: CameraRenderQuality,
) -> BrickRaySampleF32 {
    let Some(step_t) = smooth_ray_step_t(ray) else {
        return BrickRaySampleF32 {
            value: 0.0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 0,
        };
    };
    let mut t = hit.enter;
    let mut max_value: Option<f32> = None;
    let mut accumulated_rgba = [0.0; 4];
    let mut accumulated_alpha = 0.0;
    let mut covered = false;
    let mut missing_voxel_samples = 0u64;
    let mut previous_iso_sample: Option<(f64, f64, f64)> = None;
    let step_factor = dvr_step_factor(step_t, dvr_step_scale(ray, resident.grid_to_world));

    while t <= hit.exit + EPSILON {
        let point = ray.origin + ray.direction * t;
        let sample = sample_trilinear_f32(resident, point);
        missing_voxel_samples += sample.missing_voxel_samples;
        if !sample.covered {
            t += step_t;
            continue;
        }
        match mode {
            CameraRenderModeF32::Mip => {
                max_value = Some(
                    max_value
                        .map(|current| current.max(sample.value))
                        .unwrap_or(sample.value),
                );
                covered = true;
            }
            CameraRenderModeF32::Isosurface { parameters } => {
                let display_scalar = parameters.map_f32(sample.value);
                if display_scalar >= parameters.level_f64() {
                    let hit = refined_iso_hit(
                        previous_iso_sample,
                        t,
                        f64::from(sample.value),
                        display_scalar,
                        ray,
                        parameters,
                    );
                    return BrickRaySampleF32 {
                        value: iso_display_f32(hit),
                        covered: true,
                        iso_surface: Some(iso_surface_sample_f32(
                            hit,
                            gradient_display_f32(resident, hit.grid_position, parameters),
                            resident.grid_to_world,
                            quality,
                        )),
                        dvr_rgba: None,
                        missing_voxel_samples,
                    };
                }
                previous_iso_sample = Some((t, f64::from(sample.value), display_scalar));
            }
            CameraRenderModeF32::Dvr { parameters } => {
                covered |= accumulate_dvr_sample(
                    &mut accumulated_rgba,
                    &mut accumulated_alpha,
                    f64::from(sample.value),
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
    BrickRaySampleF32 {
        value,
        covered,
        iso_surface: None,
        dvr_rgba: matches!(mode, CameraRenderModeF32::Dvr { .. })
            .then_some(dvr_rgba_f32(accumulated_rgba)),
        missing_voxel_samples,
    }
}

pub(super) fn smooth_ray_step_t(ray: GridRay) -> Option<f64> {
    let length = ray.direction.length();
    (length > EPSILON).then_some(SMOOTH_RAY_STEP_VOXELS / length)
}

pub(super) fn sample_trilinear_u16(
    resident: &impl IntegerResidentSet,
    point: DVec3,
) -> BrickLinearSample {
    sample_trilinear_u16_checked(resident, point).unwrap_or(BrickLinearSample {
        value: 0.0,
        covered: false,
        missing_voxel_samples: 0,
    })
}

pub(super) fn sample_trilinear_u16_checked(
    resident: &impl IntegerResidentSet,
    point: DVec3,
) -> Option<BrickLinearSample> {
    let volume_shape = resident.volume_shape();
    let x = interpolation_axis(point.x, volume_shape.x)?;
    let y = interpolation_axis(point.y, volume_shape.y)?;
    let z = interpolation_axis(point.z, volume_shape.z)?;
    let c000 = resident_sample_u16(resident, z.lower, y.lower, x.lower);
    let c100 = resident_sample_u16(resident, z.lower, y.lower, x.upper);
    let c010 = resident_sample_u16(resident, z.lower, y.upper, x.lower);
    let c110 = resident_sample_u16(resident, z.lower, y.upper, x.upper);
    let c001 = resident_sample_u16(resident, z.upper, y.lower, x.lower);
    let c101 = resident_sample_u16(resident, z.upper, y.lower, x.upper);
    let c011 = resident_sample_u16(resident, z.upper, y.upper, x.lower);
    let c111 = resident_sample_u16(resident, z.upper, y.upper, x.upper);
    let covered = c000.covered
        && c100.covered
        && c010.covered
        && c110.covered
        && c001.covered
        && c101.covered
        && c011.covered
        && c111.covered;
    Some(BrickLinearSample {
        value: trilinear_value(
            f64::from(c000.value),
            f64::from(c100.value),
            f64::from(c010.value),
            f64::from(c110.value),
            f64::from(c001.value),
            f64::from(c101.value),
            f64::from(c011.value),
            f64::from(c111.value),
            x.fraction,
            y.fraction,
            z.fraction,
        ),
        covered,
        missing_voxel_samples: c000.missing_voxel_samples
            + c100.missing_voxel_samples
            + c010.missing_voxel_samples
            + c110.missing_voxel_samples
            + c001.missing_voxel_samples
            + c101.missing_voxel_samples
            + c011.missing_voxel_samples
            + c111.missing_voxel_samples,
    })
}

pub(super) fn sample_trilinear_f32(
    resident: &ResidentBrickSetF32,
    point: DVec3,
) -> BrickRaySampleF32 {
    sample_trilinear_f32_checked(resident, point).unwrap_or(BrickRaySampleF32 {
        value: 0.0,
        covered: false,
        iso_surface: None,
        dvr_rgba: None,
        missing_voxel_samples: 0,
    })
}

pub(super) fn sample_trilinear_f32_checked(
    resident: &ResidentBrickSetF32,
    point: DVec3,
) -> Option<BrickRaySampleF32> {
    let x = interpolation_axis(point.x, resident.volume_shape.x)?;
    let y = interpolation_axis(point.y, resident.volume_shape.y)?;
    let z = interpolation_axis(point.z, resident.volume_shape.z)?;
    let c000 = resident_sample_f32(resident, z.lower, y.lower, x.lower);
    let c100 = resident_sample_f32(resident, z.lower, y.lower, x.upper);
    let c010 = resident_sample_f32(resident, z.lower, y.upper, x.lower);
    let c110 = resident_sample_f32(resident, z.lower, y.upper, x.upper);
    let c001 = resident_sample_f32(resident, z.upper, y.lower, x.lower);
    let c101 = resident_sample_f32(resident, z.upper, y.lower, x.upper);
    let c011 = resident_sample_f32(resident, z.upper, y.upper, x.lower);
    let c111 = resident_sample_f32(resident, z.upper, y.upper, x.upper);
    let covered = c000.covered
        && c100.covered
        && c010.covered
        && c110.covered
        && c001.covered
        && c101.covered
        && c011.covered
        && c111.covered;
    Some(BrickRaySampleF32 {
        value: trilinear_value(
            f64::from(c000.value),
            f64::from(c100.value),
            f64::from(c010.value),
            f64::from(c110.value),
            f64::from(c001.value),
            f64::from(c101.value),
            f64::from(c011.value),
            f64::from(c111.value),
            x.fraction,
            y.fraction,
            z.fraction,
        ) as f32,
        covered,
        iso_surface: None,
        dvr_rgba: None,
        missing_voxel_samples: c000.missing_voxel_samples
            + c100.missing_voxel_samples
            + c010.missing_voxel_samples
            + c110.missing_voxel_samples
            + c001.missing_voxel_samples
            + c101.missing_voxel_samples
            + c011.missing_voxel_samples
            + c111.missing_voxel_samples,
    })
}

pub(super) fn resident_sample_u16(
    resident: &impl IntegerResidentSet,
    z: u64,
    y: u64,
    x: u64,
) -> BrickRaySample {
    match resident.sample_u16(z, y, x) {
        ResidentVoxel::Visible(value) => BrickRaySample {
            value,
            covered: true,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 0,
        },
        ResidentVoxel::RenderInvalid => BrickRaySample {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 0,
        },
        ResidentVoxel::Missing => BrickRaySample {
            value: 0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 1,
        },
    }
}

pub(super) fn resident_sample_f32(
    resident: &ResidentBrickSetF32,
    z: u64,
    y: u64,
    x: u64,
) -> BrickRaySampleF32 {
    match resident.sample(z, y, x) {
        ResidentVoxel::Visible(value) => BrickRaySampleF32 {
            value,
            covered: true,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 0,
        },
        ResidentVoxel::RenderInvalid => BrickRaySampleF32 {
            value: 0.0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 0,
        },
        ResidentVoxel::Missing => BrickRaySampleF32 {
            value: 0.0,
            covered: false,
            iso_surface: None,
            dvr_rgba: None,
            missing_voxel_samples: 1,
        },
    }
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
    parameters: crate::IsoSurfaceParameters,
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
    resident: &impl IntegerResidentSet,
    point: DVec3,
    parameters: crate::IsoSurfaceParameters,
) -> Option<DVec3> {
    let dx = central_difference_display_u16(resident, point, DVec3::X, parameters)?;
    let dy = central_difference_display_u16(resident, point, DVec3::Y, parameters)?;
    let dz = central_difference_display_u16(resident, point, DVec3::Z, parameters)?;
    Some(DVec3::new(dx, dy, dz))
}

pub(super) fn gradient_display_f32(
    resident: &ResidentBrickSetF32,
    point: DVec3,
    parameters: crate::IsoSurfaceParameters,
) -> Option<DVec3> {
    let dx = central_difference_display_f32(resident, point, DVec3::X, parameters)?;
    let dy = central_difference_display_f32(resident, point, DVec3::Y, parameters)?;
    let dz = central_difference_display_f32(resident, point, DVec3::Z, parameters)?;
    Some(DVec3::new(dx, dy, dz))
}

pub(super) fn central_difference_display_u16(
    resident: &impl IntegerResidentSet,
    point: DVec3,
    axis: DVec3,
    parameters: crate::IsoSurfaceParameters,
) -> Option<f64> {
    let center = sample_trilinear_u16_checked(resident, point)?;
    if center.missing_voxel_samples != 0 {
        return None;
    }
    let minus = sample_trilinear_u16_checked(resident, point - axis)
        .filter(|sample| sample.missing_voxel_samples == 0);
    let plus = sample_trilinear_u16_checked(resident, point + axis)
        .filter(|sample| sample.missing_voxel_samples == 0);
    let center_value = parameters.transfer.map_source_value_f64(center.value);
    match (minus, plus) {
        (Some(minus), Some(plus)) => Some(
            (parameters.transfer.map_source_value_f64(plus.value)
                - parameters.transfer.map_source_value_f64(minus.value))
                * 0.5,
        ),
        (None, Some(plus)) => {
            Some(parameters.transfer.map_source_value_f64(plus.value) - center_value)
        }
        (Some(minus), None) => {
            Some(center_value - parameters.transfer.map_source_value_f64(minus.value))
        }
        (None, None) => Some(0.0),
    }
}

pub(super) fn central_difference_display_f32(
    resident: &ResidentBrickSetF32,
    point: DVec3,
    axis: DVec3,
    parameters: crate::IsoSurfaceParameters,
) -> Option<f64> {
    let center = sample_trilinear_f32_checked(resident, point)?;
    if center.missing_voxel_samples != 0 {
        return None;
    }
    let minus = sample_trilinear_f32_checked(resident, point - axis)
        .filter(|sample| sample.missing_voxel_samples == 0);
    let plus = sample_trilinear_f32_checked(resident, point + axis)
        .filter(|sample| sample.missing_voxel_samples == 0);
    let center_value = parameters.map_f32(center.value);
    match (minus, plus) {
        (Some(minus), Some(plus)) => {
            Some((parameters.map_f32(plus.value) - parameters.map_f32(minus.value)) * 0.5)
        }
        (None, Some(plus)) => Some(parameters.map_f32(plus.value) - center_value),
        (Some(minus), None) => Some(center_value - parameters.map_f32(minus.value)),
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

impl AxisTraversal {
    pub(super) fn new(entry_coordinate: f64, direction: f64, entry_t: f64, limit: u64) -> Self {
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

    pub(super) fn is_inside(self) -> bool {
        self.index >= 0 && self.index < self.limit
    }

    pub(super) fn advance(&mut self) {
        if self.step == 0 {
            return;
        }
        self.index += self.step;
        self.next_t += self.delta_t;
    }
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
