use glam::{DMat4, DVec3};
use mirante4d_data::DenseVolumeU16;
use mirante4d_domain::{GridToWorld, Projection};
use mirante4d_format::CurrentGridToWorldExt;
use mirante4d_render_api::CameraFrame;

use super::GpuRenderError;
use crate::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, IntensitySamplingPolicy,
    IsoShadingMode, RenderError, RenderViewport, ScalarDisplayTransfer,
};

pub(super) const GPU_PARAM_SAMPLING_POLICY_INDEX: usize = 25;
pub(super) const GPU_PARAM_ISO_SHADING_INDEX: usize = 26;
pub(super) const GPU_PARAM_ISO_LEVEL_INDEX: usize = 22;
pub(super) const GPU_PARAM_ISO_DISPLAY_LOW_INDEX: usize = 23;
pub(super) const GPU_PARAM_ISO_DISPLAY_HIGH_INDEX: usize = 24;
pub(super) const GPU_PARAM_ISO_GAMMA_INDEX: usize = 27;
pub(super) const GPU_PARAM_GRID_X_SPACING_INDEX: usize = 28;
pub(super) const GPU_PARAM_GRID_Y_SPACING_INDEX: usize = 29;
pub(super) const GPU_PARAM_GRID_Z_SPACING_INDEX: usize = 30;
pub(super) const GPU_PARAM_DVR_COLOR_R_INDEX: usize = 31;
pub(super) const GPU_PARAM_DVR_COLOR_G_INDEX: usize = 32;
pub(super) const GPU_PARAM_DVR_COLOR_B_INDEX: usize = 33;
pub(super) const GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX: usize = 34;
pub(super) const GPU_PARAM_GRID_X_AXIS_WORLD_INDEX: usize = 35;
pub(super) const GPU_PARAM_GRID_Y_AXIS_WORLD_INDEX: usize = 38;
pub(super) const GPU_PARAM_GRID_Z_AXIS_WORLD_INDEX: usize = 41;
pub(super) const GPU_PARAM_DVR_OPACITY_LOW_INDEX: usize = 44;
pub(super) const GPU_PARAM_DVR_OPACITY_HIGH_INDEX: usize = 45;
pub(super) const GPU_PARAM_DVR_OPACITY_GAMMA_INDEX: usize = 46;
pub(super) const GPU_PARAM_NORMAL_X_AXIS_WORLD_INDEX: usize = 47;
pub(super) const GPU_PARAM_NORMAL_Y_AXIS_WORLD_INDEX: usize = 50;
pub(super) const GPU_PARAM_NORMAL_Z_AXIS_WORLD_INDEX: usize = 53;
pub(super) const GPU_CAMERA_PARAM_F32_COUNT: usize = 56;
pub(super) const GPU_CAMERA_PARAM_F32_UNIFORM_COUNT: usize = 56;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GpuModeParams {
    pub mode_code: u32,
    pub iso_invert: u32,
    pub iso_display_level: f32,
    pub iso_transfer: ScalarDisplayTransfer,
    pub dvr_opacity_transfer: ScalarDisplayTransfer,
    pub density_scale: f32,
    pub dvr_color_rgb: [f32; 3],
    pub dvr_alpha_multiplier: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GpuModeParamsF32 {
    pub mode_code: u32,
    pub iso_invert: u32,
    pub iso_display_level: f32,
    pub iso_transfer: ScalarDisplayTransfer,
    pub dvr_opacity_transfer: ScalarDisplayTransfer,
    pub density_scale: f32,
    pub dvr_color_rgb: [f32; 3],
    pub dvr_alpha_multiplier: f32,
}

pub(super) fn camera_grid_params(
    volume: &DenseVolumeU16,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> Result<[f32; GPU_CAMERA_PARAM_F32_COUNT], RenderError> {
    camera_grid_params_for_transform(volume.grid_to_world, camera, viewport)
}

pub(super) fn camera_grid_params_for_transform(
    grid_to_world: GridToWorld,
    camera: CameraFrame,
    _viewport: RenderViewport,
) -> Result<[f32; GPU_CAMERA_PARAM_F32_COUNT], RenderError> {
    let world_to_grid = grid_to_world.inverse()?;
    let (forward, right, up) = camera_world_basis(camera);
    let grid_eye = world_to_grid.transform_point(crate::current_camera::eye(camera));
    let grid_forward = world_to_grid.transform_vector(forward);
    let grid_right = world_to_grid.transform_vector(right);
    let grid_up = world_to_grid.transform_vector(up);
    let mut params = [0.0; GPU_CAMERA_PARAM_F32_COUNT];
    params[0] = grid_eye.x as f32;
    params[1] = grid_eye.y as f32;
    params[2] = grid_eye.z as f32;
    params[3] = grid_forward.x as f32;
    params[4] = grid_forward.y as f32;
    params[5] = grid_forward.z as f32;
    params[6] = grid_right.x as f32;
    params[7] = grid_right.y as f32;
    params[8] = grid_right.z as f32;
    params[9] = grid_up.x as f32;
    params[10] = grid_up.y as f32;
    params[11] = grid_up.z as f32;
    params[12] = crate::current_camera::orthographic_world_per_screen_point(camera) as f32;
    params[13] = crate::current_camera::perspective_focal_length_screen_points(camera) as f32;
    params[14] = crate::current_camera::presentation_width_points(camera) as f32;
    params[16] = crate::current_camera::presentation_height_points(camera) as f32;
    params[GPU_PARAM_GRID_X_SPACING_INDEX] =
        grid_to_world.transform_vector(DVec3::X).length() as f32;
    params[GPU_PARAM_GRID_Y_SPACING_INDEX] =
        grid_to_world.transform_vector(DVec3::Y).length() as f32;
    params[GPU_PARAM_GRID_Z_SPACING_INDEX] =
        grid_to_world.transform_vector(DVec3::Z).length() as f32;
    set_gpu_vector_param(
        &mut params,
        GPU_PARAM_GRID_X_AXIS_WORLD_INDEX,
        grid_to_world.transform_vector(DVec3::X),
    );
    set_gpu_vector_param(
        &mut params,
        GPU_PARAM_GRID_Y_AXIS_WORLD_INDEX,
        grid_to_world.transform_vector(DVec3::Y),
    );
    set_gpu_vector_param(
        &mut params,
        GPU_PARAM_GRID_Z_AXIS_WORLD_INDEX,
        grid_to_world.transform_vector(DVec3::Z),
    );
    let normal_transform = normal_transform_grid_gradient_to_world(grid_to_world);
    set_gpu_vector_param(
        &mut params,
        GPU_PARAM_NORMAL_X_AXIS_WORLD_INDEX,
        normal_transform.transform_vector3(DVec3::X),
    );
    set_gpu_vector_param(
        &mut params,
        GPU_PARAM_NORMAL_Y_AXIS_WORLD_INDEX,
        normal_transform.transform_vector3(DVec3::Y),
    );
    set_gpu_vector_param(
        &mut params,
        GPU_PARAM_NORMAL_Z_AXIS_WORLD_INDEX,
        normal_transform.transform_vector3(DVec3::Z),
    );
    Ok(params)
}

pub(super) fn camera_grid_params_f32_for_transform(
    grid_to_world: GridToWorld,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> Result<[f32; GPU_CAMERA_PARAM_F32_COUNT], RenderError> {
    let base = camera_grid_params_for_transform(grid_to_world, camera, viewport)?;
    let mut params = [0.0; GPU_CAMERA_PARAM_F32_COUNT];
    params[..base.len()].copy_from_slice(&base);
    Ok(params)
}

pub(super) fn projection_code(projection: Projection) -> u32 {
    match projection {
        Projection::Perspective => 0,
        Projection::Orthographic => 1,
    }
}

pub(super) fn apply_gpu_quality_params(
    params: &mut [f32; GPU_CAMERA_PARAM_F32_COUNT],
    quality: CameraRenderQuality,
) {
    params[GPU_PARAM_SAMPLING_POLICY_INDEX] = match quality.intensity_sampling {
        IntensitySamplingPolicy::VoxelExact => 0.0,
        IntensitySamplingPolicy::SmoothLinear => 1.0,
    };
    params[GPU_PARAM_ISO_SHADING_INDEX] = match quality.iso_shading {
        IsoShadingMode::Flat => 0.0,
        IsoShadingMode::GradientLighting => 1.0,
    };
}

pub(super) fn gpu_mode_params(
    volume: &DenseVolumeU16,
    mode: CameraRenderMode,
) -> Result<GpuModeParams, GpuRenderError> {
    gpu_mode_params_for_transform(volume.grid_to_world, mode)
}

pub(super) fn gpu_mode_params_for_transform(
    _grid_to_world: GridToWorld,
    mode: CameraRenderMode,
) -> Result<GpuModeParams, GpuRenderError> {
    match mode {
        CameraRenderMode::Mip => Ok(GpuModeParams {
            mode_code: 0,
            iso_invert: 0,
            iso_display_level: 0.0,
            iso_transfer: ScalarDisplayTransfer::identity_u16(),
            dvr_opacity_transfer: ScalarDisplayTransfer::identity_u16(),
            density_scale: 0.0,
            dvr_color_rgb: [1.0, 1.0, 1.0],
            dvr_alpha_multiplier: 1.0,
        }),
        CameraRenderMode::Isosurface { parameters } => Ok(GpuModeParams {
            mode_code: 1,
            iso_invert: u32::from(parameters.transfer.invert),
            iso_display_level: parameters.display_level,
            iso_transfer: parameters.transfer,
            dvr_opacity_transfer: ScalarDisplayTransfer::identity_u16(),
            density_scale: 0.0,
            dvr_color_rgb: [1.0, 1.0, 1.0],
            dvr_alpha_multiplier: 1.0,
        }),
        CameraRenderMode::Dvr { parameters } => Ok(GpuModeParams {
            mode_code: 2,
            iso_invert: u32::from(parameters.color_transfer.invert),
            iso_display_level: 0.0,
            iso_transfer: parameters.color_transfer,
            dvr_opacity_transfer: parameters.opacity_transfer,
            density_scale: parameters.density_scale as f32,
            dvr_color_rgb: [
                parameters.color_rgba[0],
                parameters.color_rgba[1],
                parameters.color_rgba[2],
            ],
            dvr_alpha_multiplier: parameters.channel_opacity * parameters.color_rgba[3],
        }),
    }
}

pub(super) fn gpu_mode_params_f32_for_transform(
    _grid_to_world: GridToWorld,
    mode: CameraRenderModeF32,
) -> Result<GpuModeParamsF32, GpuRenderError> {
    match mode {
        CameraRenderModeF32::Mip => Ok(GpuModeParamsF32 {
            mode_code: 0,
            iso_invert: 0,
            iso_display_level: 0.0,
            iso_transfer: ScalarDisplayTransfer::identity_f32(),
            dvr_opacity_transfer: ScalarDisplayTransfer::identity_f32(),
            density_scale: 0.0,
            dvr_color_rgb: [1.0, 1.0, 1.0],
            dvr_alpha_multiplier: 1.0,
        }),
        CameraRenderModeF32::Isosurface { parameters } => Ok(GpuModeParamsF32 {
            mode_code: 1,
            iso_invert: u32::from(parameters.transfer.invert),
            iso_display_level: parameters.display_level,
            iso_transfer: parameters.transfer,
            dvr_opacity_transfer: ScalarDisplayTransfer::identity_f32(),
            density_scale: 0.0,
            dvr_color_rgb: [1.0, 1.0, 1.0],
            dvr_alpha_multiplier: 1.0,
        }),
        CameraRenderModeF32::Dvr { parameters } => Ok(GpuModeParamsF32 {
            mode_code: 2,
            iso_invert: u32::from(parameters.color_transfer.invert),
            iso_display_level: 0.0,
            iso_transfer: parameters.color_transfer,
            dvr_opacity_transfer: parameters.opacity_transfer,
            density_scale: parameters.density_scale as f32,
            dvr_color_rgb: [
                parameters.color_rgba[0],
                parameters.color_rgba[1],
                parameters.color_rgba[2],
            ],
            dvr_alpha_multiplier: parameters.channel_opacity * parameters.color_rgba[3],
        }),
    }
}

fn camera_world_basis(camera: CameraFrame) -> (DVec3, DVec3, DVec3) {
    let forward =
        (crate::current_camera::target(camera) - crate::current_camera::eye(camera)).normalize();
    let right = forward.cross(crate::current_camera::up(camera)).normalize();
    let up = right.cross(forward).normalize();
    (forward, right, up)
}

fn set_gpu_vector_param(
    params: &mut [f32; GPU_CAMERA_PARAM_F32_COUNT],
    offset: usize,
    value: DVec3,
) {
    params[offset] = value.x as f32;
    params[offset + 1] = value.y as f32;
    params[offset + 2] = value.z as f32;
}

fn normal_transform_grid_gradient_to_world(grid_to_world: GridToWorld) -> DMat4 {
    grid_to_world.to_dmat4().inverse().transpose()
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;
    use mirante4d_domain::{DisplayWindow, Projection, Shape3D, TimeIndex, TransferCurve};
    use mirante4d_format::{DatasetId, LayerId};

    use super::*;

    #[test]
    fn maps_supported_gpu_modes() {
        let volume = u16_test_volume();

        assert_eq!(
            gpu_mode_params(&volume, CameraRenderMode::Mip).unwrap(),
            GpuModeParams {
                mode_code: 0,
                iso_invert: 0,
                iso_display_level: 0.0,
                iso_transfer: ScalarDisplayTransfer::identity_u16(),
                dvr_opacity_transfer: ScalarDisplayTransfer::identity_u16(),
                density_scale: 0.0,
                dvr_color_rgb: [1.0, 1.0, 1.0],
                dvr_alpha_multiplier: 1.0,
            }
        );
        let iso_parameters = iso_u16_threshold(123);
        assert_eq!(
            gpu_mode_params(
                &volume,
                CameraRenderMode::Isosurface {
                    parameters: iso_parameters
                }
            )
            .unwrap(),
            GpuModeParams {
                mode_code: 1,
                iso_invert: 0,
                iso_display_level: iso_parameters.display_level,
                iso_transfer: iso_parameters.transfer,
                dvr_opacity_transfer: ScalarDisplayTransfer::identity_u16(),
                density_scale: 0.0,
                dvr_color_rgb: [1.0, 1.0, 1.0],
                dvr_alpha_multiplier: 1.0,
            }
        );
        let dvr_params = dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false);
        assert_eq!(
            gpu_mode_params(
                &volume,
                CameraRenderMode::Dvr {
                    parameters: dvr_params
                }
            )
            .unwrap(),
            GpuModeParams {
                mode_code: 2,
                iso_invert: 0,
                iso_display_level: 0.0,
                iso_transfer: dvr_params.color_transfer,
                dvr_opacity_transfer: dvr_params.opacity_transfer,
                density_scale: 12.0,
                dvr_color_rgb: [1.0, 1.0, 1.0],
                dvr_alpha_multiplier: 1.0,
            }
        );
    }

    #[test]
    fn camera_grid_params_store_inverse_transpose_normal_transform() {
        let grid_to_world = mirante4d_format::grid_to_world_from_dmat4(DMat4::from_cols_array(&[
            2.0, 0.0, 0.0, 0.0, //
            0.25, 3.0, 0.0, 0.0, //
            0.0, 0.5, 4.0, 0.0, //
            7.0, 11.0, 13.0, 1.0,
        ]))
        .unwrap();
        let camera = crate::current_camera::frame_from_look_at(
            Projection::Orthographic,
            DVec3::new(0.0, 0.0, 10.0),
            DVec3::ZERO,
            DVec3::Y,
            1.0,
            8.0,
            crate::current_camera::presentation(8.0, 8.0),
        );
        let params = camera_grid_params_for_transform(
            grid_to_world,
            camera,
            RenderViewport::new(8, 8).unwrap(),
        )
        .unwrap();

        let gradient = DVec3::new(1.0, 2.0, 3.0);
        let expected = normal_transform_grid_gradient_to_world(grid_to_world)
            .transform_vector3(gradient)
            .normalize();
        let actual = (param_vec3(&params, GPU_PARAM_NORMAL_X_AXIS_WORLD_INDEX) * gradient.x
            + param_vec3(&params, GPU_PARAM_NORMAL_Y_AXIS_WORLD_INDEX) * gradient.y
            + param_vec3(&params, GPU_PARAM_NORMAL_Z_AXIS_WORLD_INDEX) * gradient.z)
            .normalize();

        assert_abs_diff_eq!(actual.x, expected.x, epsilon = 1e-6);
        assert_abs_diff_eq!(actual.y, expected.y, epsilon = 1e-6);
        assert_abs_diff_eq!(actual.z, expected.z, epsilon = 1e-6);
    }

    fn u16_test_volume() -> DenseVolumeU16 {
        DenseVolumeU16::new(
            DatasetId::new("gpu-params").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex::new(0),
            Shape3D::new(2, 2, 2).unwrap(),
            GridToWorld::identity(),
            vec![0; 8],
        )
        .unwrap()
    }

    fn iso_u16_threshold(threshold: u16) -> crate::IsoSurfaceParameters {
        crate::IsoSurfaceParameters::new(
            f32::from(threshold) / f32::from(u16::MAX),
            ScalarDisplayTransfer::identity_u16(),
        )
    }

    fn dvr_parameters(
        low: f32,
        high: f32,
        density_scale: f64,
        invert: bool,
    ) -> crate::DvrRenderParameters {
        let transfer = ScalarDisplayTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            TransferCurve::linear(),
            invert,
        );
        crate::DvrRenderParameters::new(
            transfer,
            transfer,
            [1.0, 1.0, 1.0, 1.0],
            1.0,
            density_scale,
        )
    }

    fn param_vec3(params: &[f32; GPU_CAMERA_PARAM_F32_COUNT], offset: usize) -> DVec3 {
        DVec3::new(
            params[offset] as f64,
            params[offset + 1] as f64,
            params[offset + 2] as f64,
        )
    }
}
