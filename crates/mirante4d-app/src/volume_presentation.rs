//! Native-resolution 3D navigation and hidden exact-work observations.

use std::collections::{BTreeMap, VecDeque};

use mirante4d_application::RenderGestureId;
use mirante4d_dataset::DatasetCatalog;
use mirante4d_domain::{
    CameraView, IsoShadingPolicy, LogicalLayerKey, Projection, RenderMode, RenderState,
    SamplingPolicy, ScaleLevel, Shape3D, TimeIndex, WorldPoint3,
};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    CameraFrame, FrameIdentity, MAX_EXACT_SHADER_SAMPLE_COUNT, PresentationViewport, RenderExtent,
    ShaderAdmissionError, ValidatedShaderAffine,
};
use mirante4d_render_wgpu::{GpuFrameTiming, GpuTimingTicket, VolumeColorSchedule};

// Work is expressed in quarter-voxel-MIP-step quanta. This preserves the
// existing 2.5-billion-unit navigation envelope while allowing the model to
// represent the renderer's shared traversal and measured mode/sampling costs
// without fractional arithmetic. The fixed-LOD 64^3 matrix justifies the
// schedule weights below. They preserve the measured 1080p decision
// boundaries: 8-layer voxel MIP, 4-layer fused voxel DVR, and 2-layer voxel
// ISO remain inside 12 ms, while 2-layer smooth MIP/DVR and 1-layer smooth
// ISO remain outside. Relative one-layer p95 versus voxel MIP was 4.46x for
// smooth MIP, 2.16x/6.84x for voxel/smooth DVR, and 4.94x/8.89x for
// voxel/smooth ISO on the qualified Vulkan adapter.
const INITIAL_INTERACTIVE_WORK_UNITS: u64 = 512_000_000;
const NATIVE_NAVIGATION_WORK_UNITS: u64 = 2_500_000_000;
const MIN_WORK_UNITS: u64 = 4_000_000;
// Playback is outside this refactor. These are the exact pre-cutover neutral
// work-model constants retained for playback presentation and strip sizing.
const LEGACY_PLAYBACK_INITIAL_INTERACTIVE_WORK_UNITS: u64 = 128_000_000;
const LEGACY_PLAYBACK_MIN_WORK_UNITS: u64 = 1_000_000;
const RAY_SETUP_WORK_UNITS_PER_PIXEL: u64 = 4;
const TRAVERSAL_WORK_UNITS_PER_STEP: u64 = 4;
const MIP_SMOOTH_WORK_UNITS_PER_STEP: u64 = 14;
const DVR_FUSED_WORK_UNITS_PER_LAYER_STEP: u64 = 7;
const DVR_GENERAL_WORK_UNITS_PER_LAYER_STEP: u64 = 24;
const ISO_VOXEL_WORK_UNITS_PER_STEP: u64 = 12;
const ISO_SMOOTH_WORK_UNITS_PER_STEP: u64 = 32;
const GRADIENT_TAP_WORK_UNITS: u64 = 4;
const INTERACTION_GPU_BUDGET_NS: u64 = 12_000_000;
const REFINEMENT_GPU_BUDGET_NS: u64 = 3_000_000;
const DIRECT_CERTIFICATION_NS: u64 = 8_000_000;
const ADAPTIVE_SAFETY_NUMERATOR: u64 = 3;
const ADAPTIVE_SAFETY_DENOMINATOR: u64 = 4;
const MAX_FAMILY_CALIBRATIONS: usize = 32;
const MAX_PROFILE_OBSERVATIONS: usize = 32;
const MAX_PENDING_TIMINGS: usize = 64;
const MAX_STRIP_ACCUMULATORS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VolumeLayerWorkClass {
    mode: RenderMode,
    sampling: SamplingPolicy,
    iso_shading: Option<IsoShadingPolicy>,
    /// One of 32 bounded display-threshold bands. ISO first-hit work changes
    /// radically with threshold, so unrelated regimes must not share hidden
    /// strip timing authority.
    iso_display_bucket: Option<u8>,
}

#[derive(Debug, Clone, Eq)]
struct VolumeWorkFamily {
    projection: Projection,
    model: VolumeWorkModel,
    kernel: VolumeKernelWorkClass,
    layers: Box<[VolumeLayerWorkClass]>,
}

impl PartialEq for VolumeWorkFamily {
    fn eq(&self, other: &Self) -> bool {
        self.projection == other.projection
            && self.model == other.model
            && self.layers == other.layers
            // The pre-cutover playback calibration family was projection +
            // ordered render-state classes only. Static schedule classes need
            // the new kernel distinction; playback deliberately does not.
            && (self.model == VolumeWorkModel::LegacyPlayback || self.kernel == other.kernel)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeWorkModel {
    StaticScheduleAligned,
    LegacyPlayback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeKernelWorkClass {
    HomogeneousMip,
    FusedExactDvr,
    GeneralDvr,
    HomogeneousIso,
    AuthoredMixed,
}

impl VolumeKernelWorkClass {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::HomogeneousMip => "homogeneous MIP",
            Self::FusedExactDvr => "compatible fused DVR",
            Self::GeneralDvr => "general affine DVR",
            Self::HomogeneousIso => "homogeneous ISO",
            Self::AuthoredMixed => "authored Mixed",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PixelRect {
    x_start: u32,
    y_start: u32,
    x_end: u32,
    y_end: u32,
}

impl PixelRect {
    const fn full(extent: RenderExtent) -> Self {
        Self {
            x_start: 0,
            y_start: 0,
            x_end: extent.width_pixels(),
            y_end: extent.height_pixels(),
        }
    }

    const fn width(self) -> u32 {
        self.x_end.saturating_sub(self.x_start)
    }

    const fn height(self) -> u32 {
        self.y_end.saturating_sub(self.y_start)
    }

    fn pixels(self) -> u64 {
        u64::from(self.width()).saturating_mul(u64::from(self.height()))
    }

    fn pixels_in_rows(self, y_start: u32, rows: u32) -> u64 {
        let y_end = y_start.saturating_add(rows);
        let overlap_start = self.y_start.max(y_start);
        let overlap_end = self.y_end.min(y_end);
        u64::from(self.width()).saturating_mul(u64::from(overlap_end.saturating_sub(overlap_start)))
    }

    fn union(self, other: Self) -> Self {
        if self.width() == 0 || self.height() == 0 {
            return other;
        }
        if other.width() == 0 || other.height() == 0 {
            return self;
        }
        Self {
            x_start: self.x_start.min(other.x_start),
            y_start: self.y_start.min(other.y_start),
            x_end: self.x_end.max(other.x_end),
            y_end: self.y_end.max(other.y_end),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShaderGridGeometry {
    scale: ScaleLevel,
    shape_xyz: [u64; 3],
    control_bits: [u32; 24],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct VolumeLayerWorkload {
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    render_state: RenderState,
    legacy_maximum_steps: u64,
    legacy_work_units_per_pixel: u64,
    legacy_initial_prior_work_units_per_pixel: u64,
    projected_rect: PixelRect,
    traversal_step_bound: u64,
    sample_taps_per_step: u64,
    gradient_taps_per_ray: u64,
    fixed_work_units_per_output_pixel: u64,
    scheduled_rect: PixelRect,
    scheduled_step_bound: u64,
    scheduled_work_units_per_step: u64,
    terminal_work_units_per_projected_pixel: u64,
    world_corners: [[f64; 3]; 8],
    inverse_rows: [[f32; 4]; 3],
    shader_geometry: ShaderGridGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SharedVolumeWorkload {
    fixed_work_units_per_output_pixel: u64,
    scheduled_rect: PixelRect,
    scheduled_step_bound: u64,
    scheduled_work_units_per_step: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct VolumeWorkloadProfile {
    extent: RenderExtent,
    presentation_viewport: PresentationViewport,
    camera: CameraView,
    timepoint: TimeIndex,
    layers: Box<[VolumeLayerWorkload]>,
    shared: SharedVolumeWorkload,
    model: VolumeWorkModel,
    family: VolumeWorkFamily,
}

impl PartialEq for VolumeWorkloadProfile {
    fn eq(&self, other: &Self) -> bool {
        if self.model != other.model
            || self.extent != other.extent
            || self.presentation_viewport != other.presentation_viewport
            || self.camera != other.camera
            || self.timepoint != other.timepoint
            || self.family != other.family
        {
            return false;
        }
        if self.model == VolumeWorkModel::LegacyPlayback {
            // Preserve the exact pre-cutover profile identity. The static
            // estimator's projected rectangles, renderer schedule, affine
            // controls, and kernel classification must not split playback's
            // direct-certification or hidden-strip observations.
            return self.legacy_work_units_per_pixel() == other.legacy_work_units_per_pixel()
                && self.legacy_initial_prior_work_units_per_pixel()
                    == other.legacy_initial_prior_work_units_per_pixel()
                && self.layers.len() == other.layers.len()
                && self
                    .layers
                    .iter()
                    .zip(other.layers.iter())
                    .all(|(left, right)| {
                        left.layer == right.layer
                            && left.scale == right.scale
                            && left.render_state == right.render_state
                            && left.legacy_maximum_steps == right.legacy_maximum_steps
                    });
        }
        self.layers == other.layers && self.shared == other.shared
    }
}

impl VolumeWorkloadProfile {
    pub(crate) fn from_view(
        catalog: &DatasetCatalog,
        view: &ViewState,
        camera: CameraView,
        presentation_viewport: PresentationViewport,
        layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
        extent: RenderExtent,
    ) -> anyhow::Result<Self> {
        Self::from_view_with_model(
            catalog,
            view,
            camera,
            presentation_viewport,
            layer_scales,
            extent,
            VolumeWorkModel::StaticScheduleAligned,
        )
    }

    pub(crate) fn from_product_view(
        catalog: &DatasetCatalog,
        view: &ViewState,
        camera: CameraView,
        presentation_viewport: PresentationViewport,
        layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
        extent: RenderExtent,
        playback_active: bool,
    ) -> anyhow::Result<Self> {
        if !playback_active {
            return Self::from_view(
                catalog,
                view,
                camera,
                presentation_viewport,
                layer_scales,
                extent,
            );
        }
        Self::from_view_with_model(
            catalog,
            view,
            camera,
            presentation_viewport,
            layer_scales,
            extent,
            VolumeWorkModel::LegacyPlayback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_view_with_model(
        catalog: &DatasetCatalog,
        view: &ViewState,
        camera: CameraView,
        presentation_viewport: PresentationViewport,
        layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
        extent: RenderExtent,
        model: VolumeWorkModel,
    ) -> anyhow::Result<Self> {
        let mut layers = Vec::new();
        let mut family_layers = Vec::new();
        let camera_frame = CameraFrame::new(camera, presentation_viewport)?;
        for layer in view.layers().iter().filter(|layer| layer.visible()) {
            let key = layer.layer_key();
            let scale = layer_scales.get(&key).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "3D workload has no selected scale for visible layer {}",
                    key.ordinal()
                )
            })?;
            let dataset_scale = catalog
                .layer(key)
                .and_then(|catalog_layer| catalog_layer.scale(scale))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "3D workload references absent layer {} scale s{}",
                        key.ordinal(),
                        scale.get()
                    )
                })?;
            let render_state = *layer.render_state();
            let shape = dataset_scale.shape();
            let legacy_maximum_steps = shape.x().max(shape.y()).max(shape.z());
            let sample_taps_per_step = match render_state.sampling_policy() {
                SamplingPolicy::VoxelExact => 1_u64,
                SamplingPolicy::SmoothLinear => 8_u64,
            };
            let gradient_taps_per_ray = render_state
                .iso_parameters()
                .filter(|parameters| {
                    parameters.shading_policy()
                        == mirante4d_domain::IsoShadingPolicy::GradientLighting
                })
                .map_or(0_u64, |_| 6_u64.saturating_mul(sample_taps_per_step));
            let legacy_initial_mode_prior = if render_state.mode() == RenderMode::Mip {
                1_u64
            } else {
                2_u64
            };
            let legacy_work_units_per_pixel = legacy_maximum_steps
                .saturating_mul(sample_taps_per_step)
                .saturating_add(gradient_taps_per_ray)
                .max(1);
            let legacy_initial_prior_work_units_per_pixel = legacy_maximum_steps
                .saturating_mul(sample_taps_per_step)
                .saturating_mul(legacy_initial_mode_prior)
                .saturating_add(gradient_taps_per_ray)
                .max(1);
            let validated_affine =
                ValidatedShaderAffine::new(dataset_scale.grid_to_world(), dataset_scale.shape())?;
            let inverse_rows = validated_affine.world_to_grid_rows();
            // Color traversal intersects the f32 world-to-grid rows uploaded
            // to WGSL, not the source f64 affine directly. Reconstruct its
            // implied volume corners so projection and general-DVR interval
            // accounting describe the geometry the shader actually sees.
            let world_corners = shader_volume_world_corners(validated_affine)?;
            let projected_rect = projected_pixel_rect(camera_frame, &world_corners, extent);
            let traversal_step_bound = conservative_grid_traversal_bound(
                camera_frame,
                presentation_viewport,
                extent,
                projected_rect,
                dataset_scale.shape(),
                inverse_rows,
            );
            if traversal_step_bound > MAX_EXACT_SHADER_SAMPLE_COUNT {
                return Err(ShaderAdmissionError::ShaderSampleCountExceeded {
                    layer: key,
                    bound: traversal_step_bound,
                    maximum: MAX_EXACT_SHADER_SAMPLE_COUNT,
                }
                .into());
            }
            family_layers.push(VolumeLayerWorkClass {
                mode: render_state.mode(),
                sampling: render_state.sampling_policy(),
                iso_shading: render_state
                    .iso_parameters()
                    .map(|parameters| parameters.shading_policy()),
                iso_display_bucket: render_state.iso_parameters().map(|parameters| {
                    let bucket = (parameters.display_level().clamp(0.0, 1.0) * 32.0).floor();
                    (bucket as u8).min(31)
                }),
            });
            layers.push(VolumeLayerWorkload {
                layer: key,
                scale,
                render_state,
                legacy_maximum_steps,
                legacy_work_units_per_pixel,
                legacy_initial_prior_work_units_per_pixel,
                projected_rect,
                traversal_step_bound,
                sample_taps_per_step,
                gradient_taps_per_ray,
                fixed_work_units_per_output_pixel: 0,
                scheduled_rect: projected_rect,
                scheduled_step_bound: traversal_step_bound,
                scheduled_work_units_per_step: 0,
                terminal_work_units_per_projected_pixel: gradient_taps_per_ray
                    .saturating_mul(GRADIENT_TAP_WORK_UNITS),
                world_corners,
                inverse_rows,
                shader_geometry: shader_grid_geometry(scale, validated_affine),
            });
        }
        if layers.is_empty() {
            anyhow::bail!("3D workload requires at least one visible layer");
        }
        let kernel = classify_kernel(&layers);
        let shared = configure_schedule(
            kernel,
            camera_frame,
            presentation_viewport,
            extent,
            &mut layers,
        );
        if shared.scheduled_step_bound > MAX_EXACT_SHADER_SAMPLE_COUNT {
            let layer = layers
                .first()
                .expect("a configured volume workload has one layer")
                .layer;
            return Err(ShaderAdmissionError::ShaderSampleCountExceeded {
                layer,
                bound: shared.scheduled_step_bound,
                maximum: MAX_EXACT_SHADER_SAMPLE_COUNT,
            }
            .into());
        }
        Ok(Self {
            extent,
            presentation_viewport,
            camera,
            timepoint: view.timepoint(),
            layers: layers.into_boxed_slice(),
            shared,
            model,
            family: VolumeWorkFamily {
                projection: camera.projection(),
                model,
                kernel,
                layers: family_layers.into_boxed_slice(),
            },
        })
    }

    pub(crate) const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub(crate) const fn camera(&self) -> CameraView {
        self.camera
    }

    pub(crate) fn full_work_units(&self) -> u64 {
        match self.model {
            VolumeWorkModel::StaticScheduleAligned => {
                self.schedule_work_units_for_row_range(0, self.extent.height_pixels())
            }
            VolumeWorkModel::LegacyPlayback => {
                extent_pixels(self.extent).saturating_mul(self.legacy_work_units_per_pixel())
            }
        }
    }

    fn work_units_for_rows(&self, rows: u32) -> u64 {
        if self.model == VolumeWorkModel::LegacyPlayback {
            return u64::from(self.extent.width_pixels())
                .saturating_mul(u64::from(rows.min(self.extent.height_pixels())))
                .saturating_mul(self.legacy_work_units_per_pixel());
        }
        let rows = rows.min(self.extent.height_pixels());
        let maximum_row_work = (0..self.extent.height_pixels())
            .map(|y| self.schedule_work_units_for_row_range(y, 1))
            .max()
            .unwrap_or(0);
        maximum_row_work.saturating_mul(u64::from(rows))
    }

    fn work_units_for_row_range(&self, y_start: u32, rows: u32) -> u64 {
        if self.model == VolumeWorkModel::LegacyPlayback {
            return u64::from(self.extent.width_pixels())
                .saturating_mul(u64::from(
                    rows.min(self.extent.height_pixels().saturating_sub(y_start)),
                ))
                .saturating_mul(self.legacy_work_units_per_pixel());
        }
        self.schedule_work_units_for_row_range(y_start, rows)
    }

    fn schedule_work_units_for_row_range(&self, y_start: u32, rows: u32) -> u64 {
        let rows = rows.min(self.extent.height_pixels().saturating_sub(y_start));
        let output_pixels = u64::from(self.extent.width_pixels()).saturating_mul(u64::from(rows));
        let mut work = output_pixels.saturating_mul(self.shared.fixed_work_units_per_output_pixel);
        work = work.saturating_add(schedule_region_work(
            self.shared.scheduled_rect,
            y_start,
            rows,
            self.shared.scheduled_step_bound,
            self.shared.scheduled_work_units_per_step,
        ));
        for layer in &self.layers {
            work = work.saturating_add(
                output_pixels.saturating_mul(layer.fixed_work_units_per_output_pixel),
            );
            work = work.saturating_add(schedule_region_work(
                layer.scheduled_rect,
                y_start,
                rows,
                layer.scheduled_step_bound,
                layer.scheduled_work_units_per_step,
            ));
            work = work.saturating_add(
                layer
                    .projected_rect
                    .pixels_in_rows(y_start, rows)
                    .saturating_mul(layer.terminal_work_units_per_projected_pixel),
            );
        }
        work.max(1)
    }

    fn initial_safe_work_units(&self) -> u64 {
        match self.model {
            VolumeWorkModel::StaticScheduleAligned => INITIAL_INTERACTIVE_WORK_UNITS,
            VolumeWorkModel::LegacyPlayback => LEGACY_PLAYBACK_INITIAL_INTERACTIVE_WORK_UNITS
                .saturating_mul(self.legacy_work_units_per_pixel())
                .checked_div(self.legacy_initial_prior_work_units_per_pixel().max(1))
                .unwrap_or(0)
                .max(LEGACY_PLAYBACK_MIN_WORK_UNITS),
        }
    }

    fn native_navigation_is_safe(&self) -> bool {
        self.full_work_units() <= NATIVE_NAVIGATION_WORK_UNITS
    }

    fn kernel(&self) -> VolumeKernelWorkClass {
        self.family.kernel
    }

    fn shared_work_units(&self) -> u64 {
        if self.model == VolumeWorkModel::LegacyPlayback {
            return 0;
        }
        let output_pixels = extent_pixels(self.extent);
        output_pixels
            .saturating_mul(self.shared.fixed_work_units_per_output_pixel)
            .saturating_add(schedule_region_work(
                self.shared.scheduled_rect,
                0,
                self.extent.height_pixels(),
                self.shared.scheduled_step_bound,
                self.shared.scheduled_work_units_per_step,
            ))
    }

    fn layer_work_facts(&self) -> Box<[VolumeLayerWorkFacts]> {
        let output_pixels = extent_pixels(self.extent);
        self.layers
            .iter()
            .map(|layer| {
                if self.model == VolumeWorkModel::LegacyPlayback {
                    return VolumeLayerWorkFacts {
                        layer: layer.layer,
                        scale: layer.scale,
                        mode: layer.render_state.mode(),
                        sampling: layer.render_state.sampling_policy(),
                        projected_pixels: output_pixels,
                        traversal_step_bound: layer.legacy_maximum_steps,
                        scheduled_pixels: output_pixels,
                        scheduled_step_bound: layer.legacy_maximum_steps,
                        sample_taps_per_step: layer.sample_taps_per_step,
                        gradient_taps_per_ray: layer.gradient_taps_per_ray,
                        ray_setup_work_units: 0,
                        scheduled_work_units: output_pixels
                            .saturating_mul(layer.legacy_work_units_per_pixel),
                        terminal_work_units: 0,
                    };
                }
                let ray_setup_work_units =
                    output_pixels.saturating_mul(layer.fixed_work_units_per_output_pixel);
                let scheduled_work_units = schedule_region_work(
                    layer.scheduled_rect,
                    0,
                    self.extent.height_pixels(),
                    layer.scheduled_step_bound,
                    layer.scheduled_work_units_per_step,
                );
                let terminal_work_units = layer
                    .projected_rect
                    .pixels()
                    .saturating_mul(layer.terminal_work_units_per_projected_pixel);
                VolumeLayerWorkFacts {
                    layer: layer.layer,
                    scale: layer.scale,
                    mode: layer.render_state.mode(),
                    sampling: layer.render_state.sampling_policy(),
                    projected_pixels: layer.projected_rect.pixels(),
                    traversal_step_bound: layer.traversal_step_bound,
                    scheduled_pixels: layer.scheduled_rect.pixels(),
                    scheduled_step_bound: layer.scheduled_step_bound,
                    sample_taps_per_step: layer.sample_taps_per_step,
                    gradient_taps_per_ray: layer.gradient_taps_per_ray,
                    ray_setup_work_units,
                    scheduled_work_units,
                    terminal_work_units,
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    fn legacy_work_units_per_pixel(&self) -> u64 {
        self.layers.iter().fold(0_u64, |total, layer| {
            total.saturating_add(layer.legacy_work_units_per_pixel)
        })
    }

    fn legacy_initial_prior_work_units_per_pixel(&self) -> u64 {
        self.layers.iter().fold(0_u64, |total, layer| {
            total.saturating_add(layer.legacy_initial_prior_work_units_per_pixel)
        })
    }

    const fn minimum_work_units(&self) -> u64 {
        match self.model {
            VolumeWorkModel::StaticScheduleAligned => MIN_WORK_UNITS,
            VolumeWorkModel::LegacyPlayback => LEGACY_PLAYBACK_MIN_WORK_UNITS,
        }
    }

    fn scale_configuration(&self) -> VolumeScaleConfiguration {
        VolumeScaleConfiguration(
            self.layers
                .iter()
                .map(|layer| (layer.layer, layer.scale))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    fn is_no_finer_than(&self, target: &Self) -> bool {
        self.layers.len() == target.layers.len()
            && self.layers.iter().all(|candidate| {
                target
                    .layers
                    .iter()
                    .find(|target| target.layer == candidate.layer)
                    .is_some_and(|target| candidate.scale >= target.scale)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VolumeLayerWorkFacts {
    pub(crate) layer: LogicalLayerKey,
    pub(crate) scale: ScaleLevel,
    pub(crate) mode: RenderMode,
    pub(crate) sampling: SamplingPolicy,
    pub(crate) projected_pixels: u64,
    pub(crate) traversal_step_bound: u64,
    pub(crate) scheduled_pixels: u64,
    pub(crate) scheduled_step_bound: u64,
    pub(crate) sample_taps_per_step: u64,
    pub(crate) gradient_taps_per_ray: u64,
    pub(crate) ray_setup_work_units: u64,
    pub(crate) scheduled_work_units: u64,
    pub(crate) terminal_work_units: u64,
}

impl VolumeLayerWorkFacts {
    pub(crate) fn total_work_units(self) -> u64 {
        self.ray_setup_work_units
            .saturating_add(self.scheduled_work_units)
            .saturating_add(self.terminal_work_units)
    }
}

fn classify_kernel(layers: &[VolumeLayerWorkload]) -> VolumeKernelWorkClass {
    let all_mip = layers
        .iter()
        .all(|layer| layer.render_state.mode() == RenderMode::Mip);
    let all_dvr = layers
        .iter()
        .all(|layer| layer.render_state.mode() == RenderMode::Dvr);
    let all_iso = layers
        .iter()
        .all(|layer| layer.render_state.mode() == RenderMode::Isosurface);
    if all_mip {
        VolumeKernelWorkClass::HomogeneousMip
    } else if all_dvr && fused_exact_dvr_compatible(layers) {
        VolumeKernelWorkClass::FusedExactDvr
    } else if all_dvr {
        VolumeKernelWorkClass::GeneralDvr
    } else if all_iso {
        VolumeKernelWorkClass::HomogeneousIso
    } else {
        VolumeKernelWorkClass::AuthoredMixed
    }
}

fn fused_exact_dvr_compatible(layers: &[VolumeLayerWorkload]) -> bool {
    let Some(first) = layers.first() else {
        return false;
    };
    first.render_state.sampling_policy() == SamplingPolicy::VoxelExact
        && layers.iter().all(|layer| {
            layer.render_state.sampling_policy() == SamplingPolicy::VoxelExact
                && layer.shader_geometry == first.shader_geometry
        })
}

fn configure_schedule(
    kernel: VolumeKernelWorkClass,
    camera: CameraFrame,
    presentation: PresentationViewport,
    extent: RenderExtent,
    layers: &mut [VolumeLayerWorkload],
) -> SharedVolumeWorkload {
    let mut shared = SharedVolumeWorkload {
        fixed_work_units_per_output_pixel: 0,
        scheduled_rect: PixelRect::default(),
        scheduled_step_bound: 0,
        scheduled_work_units_per_step: 0,
    };
    match kernel {
        VolumeKernelWorkClass::HomogeneousMip
        | VolumeKernelWorkClass::HomogeneousIso
        | VolumeKernelWorkClass::AuthoredMixed => {
            for layer in layers {
                layer.fixed_work_units_per_output_pixel = RAY_SETUP_WORK_UNITS_PER_PIXEL;
                layer.scheduled_work_units_per_step = TRAVERSAL_WORK_UNITS_PER_STEP
                    .saturating_add(independent_layer_work_units_per_step(*layer));
            }
        }
        VolumeKernelWorkClass::FusedExactDvr => {
            let first = layers
                .first()
                .expect("a classified volume kernel has at least one layer");
            shared = SharedVolumeWorkload {
                fixed_work_units_per_output_pixel: RAY_SETUP_WORK_UNITS_PER_PIXEL,
                scheduled_rect: first.projected_rect,
                scheduled_step_bound: first.traversal_step_bound,
                scheduled_work_units_per_step: TRAVERSAL_WORK_UNITS_PER_STEP,
            };
            for layer in layers {
                layer.scheduled_rect = shared.scheduled_rect;
                layer.scheduled_step_bound = shared.scheduled_step_bound;
                layer.scheduled_work_units_per_step = DVR_FUSED_WORK_UNITS_PER_LAYER_STEP;
            }
        }
        VolumeKernelWorkClass::GeneralDvr => {
            let union_rect = layers
                .iter()
                .map(|layer| layer.projected_rect)
                .reduce(PixelRect::union)
                .unwrap_or_default();
            let traversal =
                general_dvr_traversal_bound(camera, presentation, extent, union_rect, layers);
            shared = SharedVolumeWorkload {
                fixed_work_units_per_output_pixel: 0,
                scheduled_rect: union_rect,
                scheduled_step_bound: traversal,
                scheduled_work_units_per_step: TRAVERSAL_WORK_UNITS_PER_STEP,
            };
            for layer in layers {
                // The general shader derives every layer ray across the full
                // output, then revisits every layer in each common-world
                // segment. Charge both mechanics even where a particular
                // layer contributes no scalar sample.
                layer.fixed_work_units_per_output_pixel = RAY_SETUP_WORK_UNITS_PER_PIXEL;
                layer.scheduled_rect = union_rect;
                layer.scheduled_step_bound = traversal;
                layer.scheduled_work_units_per_step = DVR_GENERAL_WORK_UNITS_PER_LAYER_STEP;
            }
        }
    }
    shared
}

fn independent_layer_work_units_per_step(layer: VolumeLayerWorkload) -> u64 {
    match (
        layer.render_state.mode(),
        layer.render_state.sampling_policy(),
    ) {
        (RenderMode::Mip, SamplingPolicy::VoxelExact) => 0,
        (RenderMode::Mip, SamplingPolicy::SmoothLinear) => MIP_SMOOTH_WORK_UNITS_PER_STEP,
        (RenderMode::Dvr, SamplingPolicy::VoxelExact) => DVR_FUSED_WORK_UNITS_PER_LAYER_STEP,
        (RenderMode::Dvr, SamplingPolicy::SmoothLinear) => DVR_GENERAL_WORK_UNITS_PER_LAYER_STEP,
        (RenderMode::Isosurface, SamplingPolicy::VoxelExact) => ISO_VOXEL_WORK_UNITS_PER_STEP,
        (RenderMode::Isosurface, SamplingPolicy::SmoothLinear) => ISO_SMOOTH_WORK_UNITS_PER_STEP,
    }
}

fn schedule_region_work(
    rect: PixelRect,
    y_start: u32,
    rows: u32,
    steps: u64,
    work_units_per_step: u64,
) -> u64 {
    rect.pixels_in_rows(y_start, rows)
        .saturating_mul(steps)
        .saturating_mul(work_units_per_step)
}

fn shader_grid_geometry(
    scale: ScaleLevel,
    validated_affine: ValidatedShaderAffine,
) -> ShaderGridGeometry {
    let inverse = validated_affine.world_to_grid_rows();
    let mut control_bits = [0_u32; 24];
    for (target, value) in control_bits[..12]
        .iter_mut()
        .zip(inverse.into_iter().flatten())
    {
        *target = canonical_f32(value).to_bits();
    }
    for (target, value) in control_bits[12..]
        .iter_mut()
        .zip(validated_affine.grid_to_world_rows().into_iter().flatten())
    {
        *target = canonical_f32(value).to_bits();
    }
    ShaderGridGeometry {
        scale,
        shape_xyz: [
            validated_affine.grid_shape().x(),
            validated_affine.grid_shape().y(),
            validated_affine.grid_shape().z(),
        ],
        control_bits,
    }
}

fn canonical_f32(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

fn shader_volume_world_corners(affine: ValidatedShaderAffine) -> anyhow::Result<[[f64; 3]; 8]> {
    let shape = affine.grid_shape();
    let center = affine.quantized_inverse_center();
    let radius = affine.quantized_inverse_radius();
    let bounds = [
        [-0.5, shape.x() as f64 - 0.5],
        [-0.5, shape.y() as f64 - 0.5],
        [-0.5, shape.z() as f64 - 0.5],
    ];
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for bits in 0_u8..8 {
        let grid = [
            bounds[0][usize::from(bits & 1 != 0)],
            bounds[1][usize::from(bits & 2 != 0)],
            bounds[2][usize::from(bits & 4 != 0)],
        ];
        for world_axis in 0..3 {
            let value = center[world_axis][0] * grid[0]
                + center[world_axis][1] * grid[1]
                + center[world_axis][2] * grid[2]
                + center[world_axis][3];
            let error = radius[world_axis][0] * grid[0].abs()
                + radius[world_axis][1] * grid[1].abs()
                + radius[world_axis][2] * grid[2].abs()
                + radius[world_axis][3];
            minimum[world_axis] = minimum[world_axis].min(value - error);
            maximum[world_axis] = maximum[world_axis].max(value + error);
        }
    }
    if !minimum
        .into_iter()
        .chain(maximum)
        .all(|value| value.is_finite())
    {
        anyhow::bail!("validated renderer volume bounds are not finite");
    }
    Ok(std::array::from_fn(|bits| {
        std::array::from_fn(|axis| {
            if bits & (1 << axis) == 0 {
                minimum[axis]
            } else {
                maximum[axis]
            }
        })
    }))
}

fn projected_pixel_rect(
    camera: CameraFrame,
    world_corners: &[[f64; 3]; 8],
    extent: RenderExtent,
) -> PixelRect {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for corner in world_corners {
        let Ok(world) = WorldPoint3::new(corner[0], corner[1], corner[2]) else {
            return PixelRect::full(extent);
        };
        let Ok(Some(projected)) = camera.project_world_point(world) else {
            // If a corner reaches or crosses the eye plane, the perspective
            // silhouette can be unbounded. Full output is the only safe
            // analytical rectangle.
            return PixelRect::full(extent);
        };
        let x = (projected.screen_x_points() / camera.presentation().width_points() + 0.5)
            * f64::from(extent.width_pixels());
        let y = (0.5 - projected.screen_y_points() / camera.presentation().height_points())
            * f64::from(extent.height_pixels());
        if !x.is_finite() || !y.is_finite() {
            return PixelRect::full(extent);
        }
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let (x_start, x_end) = conservative_pixel_axis(min_x, max_x, extent.width_pixels());
    let (y_start, y_end) = conservative_pixel_axis(min_y, max_y, extent.height_pixels());
    PixelRect {
        x_start,
        y_start,
        x_end,
        y_end,
    }
}

fn conservative_pixel_axis(minimum: f64, maximum: f64, limit: u32) -> (u32, u32) {
    if maximum < 0.0 || minimum > f64::from(limit) {
        return (0, 0);
    }
    let start = (minimum.floor() - 1.0).clamp(0.0, f64::from(limit)) as u32;
    let end = (maximum.ceil() + 1.0).clamp(0.0, f64::from(limit)) as u32;
    (start.min(end), end)
}

fn conservative_grid_traversal_bound(
    camera: CameraFrame,
    presentation: PresentationViewport,
    extent: RenderExtent,
    rect: PixelRect,
    shape: Shape3D,
    inverse_rows: [[f32; 4]; 3],
) -> u64 {
    if rect.width() == 0 || rect.height() == 0 {
        return 0;
    }
    let world_directions = camera_direction_corners(camera, presentation, extent, rect);
    let grid_directions = world_directions.map(|direction| {
        inverse_rows.map(|row| {
            f64::from(row[0]) * direction[0]
                + f64::from(row[1]) * direction[1]
                + f64::from(row[2]) * direction[2]
        })
    });
    let maximum_steps = shape.x().max(shape.y()).max(shape.z());
    let maximum_speed = (0..3)
        .map(|axis| component_abs_max(&grid_directions, axis))
        .fold(0.0, f64::max);
    if !maximum_speed.is_finite() || maximum_speed <= 0.0 {
        return maximum_steps;
    }
    let shape_xyz = [shape.x(), shape.y(), shape.z()];
    let mut bound = maximum_steps;
    for (axis, dimension) in shape_xyz.into_iter().enumerate() {
        let minimum_component = component_abs_min(&grid_directions, axis);
        if minimum_component > 0.0 {
            let candidate = dimension as f64 * maximum_speed / minimum_component;
            bound = bound.min(saturating_ceil_u64(candidate).saturating_add(1));
        }
    }
    bound.max(1).min(maximum_steps)
}

fn general_dvr_traversal_bound(
    camera: CameraFrame,
    presentation: PresentationViewport,
    extent: RenderExtent,
    rect: PixelRect,
    layers: &[VolumeLayerWorkload],
) -> u64 {
    if rect.width() == 0 || rect.height() == 0 {
        return 0;
    }
    if layers
        .windows(2)
        .all(|pair| shader_world_bounds_match(&pair[0].world_corners, &pair[1].world_corners))
    {
        return layers
            .iter()
            .map(|layer| layer.traversal_step_bound)
            .max()
            .unwrap_or(1);
    }

    let directions = camera_direction_corners(camera, presentation, extent, rect);
    let mut world_min = [f64::INFINITY; 3];
    let mut world_max = [f64::NEG_INFINITY; 3];
    for corner in layers.iter().flat_map(|layer| layer.world_corners) {
        for axis in 0..3 {
            world_min[axis] = world_min[axis].min(corner[axis]);
            world_max[axis] = world_max[axis].max(corner[axis]);
        }
    }
    let world_size = std::array::from_fn::<_, 3, _>(|axis| world_max[axis] - world_min[axis]);
    let mut parameter_span = world_size
        .iter()
        .map(|size| size * size)
        .sum::<f64>()
        .sqrt();
    for (axis, world_axis_size) in world_size.into_iter().enumerate() {
        let minimum_component = component_abs_min(&directions, axis);
        if minimum_component > 0.0 {
            parameter_span = parameter_span.min(world_axis_size / minimum_component);
        }
    }
    let maximum_grid_speed = layers
        .iter()
        .flat_map(|layer| {
            (0..3).map(move |axis| {
                directions
                    .iter()
                    .map(|direction| {
                        let row = layer.inverse_rows[axis];
                        (f64::from(row[0]) * direction[0]
                            + f64::from(row[1]) * direction[1]
                            + f64::from(row[2]) * direction[2])
                            .abs()
                    })
                    .fold(0.0, f64::max)
            })
        })
        .fold(0.0, f64::max);
    saturating_ceil_u64(parameter_span * maximum_grid_speed)
        .saturating_add(1)
        .max(1)
}

fn shader_world_bounds_match(left: &[[f64; 3]; 8], right: &[[f64; 3]; 8]) -> bool {
    left.iter()
        .flatten()
        .zip(right.iter().flatten())
        .all(|(left, right)| {
            let tolerance = 64.0 * f64::EPSILON * (1.0 + left.abs().max(right.abs()));
            (left - right).abs() <= tolerance
        })
}

fn camera_direction_corners(
    camera: CameraFrame,
    presentation: PresentationViewport,
    extent: RenderExtent,
    rect: PixelRect,
) -> [[f64; 3]; 4] {
    let x_edges = [f64::from(rect.x_start), f64::from(rect.x_end)];
    let y_edges = [f64::from(rect.y_start), f64::from(rect.y_end)];
    let axes = camera.axes();
    let mut directions = [[0.0; 3]; 4];
    let mut index = 0;
    for y in y_edges {
        for x in x_edges {
            let screen_x =
                (x / f64::from(extent.width_pixels()) - 0.5) * presentation.width_points();
            let screen_y =
                (0.5 - y / f64::from(extent.height_pixels())) * presentation.height_points();
            directions[index] = match camera.view().projection() {
                Projection::Orthographic => axes.forward(),
                Projection::Perspective => {
                    let focal = camera.view().perspective_focal_length_screen_points();
                    std::array::from_fn(|axis| {
                        axes.forward()[axis]
                            + axes.right()[axis] * screen_x / focal
                            + axes.up()[axis] * screen_y / focal
                    })
                }
            };
            index += 1;
        }
    }
    directions
}

fn component_abs_max(vectors: &[[f64; 3]; 4], axis: usize) -> f64 {
    vectors
        .iter()
        .map(|vector| vector[axis].abs())
        .fold(0.0, f64::max)
}

fn component_abs_min(vectors: &[[f64; 3]; 4], axis: usize) -> f64 {
    let minimum = vectors
        .iter()
        .map(|vector| vector[axis])
        .fold(f64::INFINITY, f64::min);
    let maximum = vectors
        .iter()
        .map(|vector| vector[axis])
        .fold(f64::NEG_INFINITY, f64::max);
    if minimum <= 0.0 && maximum >= 0.0 {
        0.0
    } else {
        minimum.abs().min(maximum.abs())
    }
}

fn saturating_ceil_u64(value: f64) -> u64 {
    if !value.is_finite() || value >= u64::MAX as f64 {
        u64::MAX
    } else if value <= 0.0 {
        0
    } else {
        value.ceil() as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeScaleConfiguration(Box<[(LogicalLayerKey, ScaleLevel)]>);

#[derive(Debug, Clone)]
pub(crate) struct VolumePreviewCandidate {
    profile: VolumeWorkloadProfile,
    available: bool,
    cold_bootstrap: bool,
    full_volume: bool,
    resource_count: usize,
    payload_bytes: u64,
}

impl VolumePreviewCandidate {
    pub(crate) fn navigation(
        profile: VolumeWorkloadProfile,
        available: bool,
        cold_bootstrap: bool,
        resource_count: usize,
        payload_bytes: u64,
    ) -> Self {
        Self {
            profile,
            available,
            cold_bootstrap,
            full_volume: true,
            resource_count,
            payload_bytes,
        }
    }

    pub(crate) fn target(
        profile: VolumeWorkloadProfile,
        available: bool,
        resource_count: usize,
        payload_bytes: u64,
    ) -> Self {
        Self {
            profile,
            available,
            cold_bootstrap: false,
            full_volume: false,
            resource_count,
            payload_bytes,
        }
    }

    pub(crate) fn profile(&self) -> &VolumeWorkloadProfile {
        &self.profile
    }

    pub(crate) const fn available(&self) -> bool {
        self.available
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumePreviewCandidateKind {
    Navigation,
    ExactTarget,
}

impl VolumePreviewCandidateKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Navigation => "navigation",
            Self::ExactTarget => "exact-target",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumePreviewCandidateDisposition {
    NotResident,
    FinerThanTarget,
    InteractionUnsafe,
    Eligible,
    Selected,
    SelectedColdBootstrap,
    SelectedTerminalEmergency,
}

impl VolumePreviewCandidateDisposition {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NotResident => "not resident",
            Self::FinerThanTarget => "finer than target",
            Self::InteractionUnsafe => "interaction-unsafe",
            Self::Eligible => "eligible",
            Self::Selected => "selected",
            Self::SelectedColdBootstrap => "selected cold bootstrap",
            Self::SelectedTerminalEmergency => "selected terminal emergency",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumePreviewCandidateFacts {
    pub(crate) kind: VolumePreviewCandidateKind,
    pub(crate) layer_scales: Box<[(LogicalLayerKey, ScaleLevel)]>,
    pub(crate) kernel: VolumeKernelWorkClass,
    pub(crate) shared_work_units: u64,
    pub(crate) layer_work: Box<[VolumeLayerWorkFacts]>,
    pub(crate) resource_count: usize,
    pub(crate) payload_bytes: u64,
    pub(crate) schedule_work_units: u64,
    pub(crate) complete_and_resident: bool,
    pub(crate) target_quality_eligible: bool,
    pub(crate) interaction_safe: bool,
    pub(crate) full_volume: bool,
    pub(crate) disposition: VolumePreviewCandidateDisposition,
}

#[derive(Debug, Clone)]
struct ProfileObservation {
    profile: VolumeWorkloadProfile,
    certified: bool,
    last_use: u64,
}

#[derive(Debug, Clone)]
struct FamilyCalibration {
    family: VolumeWorkFamily,
    safe_work_units: u64,
    last_use: u64,
}

#[derive(Debug, Clone)]
struct ActiveRefinementSchedule {
    frame: FrameIdentity,
    profile: VolumeWorkloadProfile,
    strip_height_pixels: u32,
}

#[derive(Debug, Clone)]
struct PresentedPreview {
    frame: FrameIdentity,
}

#[derive(Debug, Clone)]
struct GesturePreviewPolicy {
    gesture: RenderGestureId,
    scales: VolumeScaleConfiguration,
    navigation_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct VolumePreviewChoice {
    candidate_index: usize,
    profile: VolumeWorkloadProfile,
}

impl VolumePreviewChoice {
    pub(crate) const fn candidate_index(&self) -> usize {
        self.candidate_index
    }

    pub(crate) fn into_profile(self) -> VolumeWorkloadProfile {
        self.profile
    }
}

#[derive(Debug, Clone)]
enum PendingTimingKind {
    Direct,
    Preview,
    AtomicStrip {
        frame: FrameIdentity,
        completed_strip: u32,
        total_strips: u32,
        strip_work_units: u64,
    },
}

#[derive(Debug, Clone)]
struct PendingTiming {
    execution_id: u64,
    profile: VolumeWorkloadProfile,
    kind: PendingTimingKind,
}

#[derive(Debug, Clone)]
struct StripAccumulator {
    frame: FrameIdentity,
    profile: VolumeWorkloadProfile,
    observed: Box<[bool]>,
    total_ns: u64,
    last_use: u64,
}

#[derive(Debug, Default)]
pub(crate) struct VolumePresentationController {
    family_calibrations: Vec<FamilyCalibration>,
    profile_observations: Vec<ProfileObservation>,
    pending_timings: VecDeque<PendingTiming>,
    strip_accumulators: Vec<StripAccumulator>,
    active_refinement: Option<ActiveRefinementSchedule>,
    presented_preview: Option<PresentedPreview>,
    gesture_preview_policy: Option<GesturePreviewPolicy>,
    latest_candidate_facts: Vec<VolumePreviewCandidateFacts>,
    clock: u64,
    #[cfg(test)]
    test_work_limits: Option<(u64, u64)>,
}

impl VolumePresentationController {
    pub(crate) fn direct_is_safe(&mut self, profile: &VolumeWorkloadProfile) -> bool {
        let now = self.tick();
        if let Some(observation) = self
            .profile_observations
            .iter_mut()
            .find(|observation| observation.profile == *profile)
        {
            observation.last_use = now;
            if observation.certified {
                return true;
            }
        }
        // Visible presentation policy is deliberately static. Runtime timing
        // may certify this exact camera/profile for a future direct frame and
        // may size hidden exact strips, but it never changes the visible
        // preview body or output resolution while the user is interacting.
        self.navigation_is_safe(profile)
    }

    pub(crate) fn select_preview_profile(
        &mut self,
        target_profile: &VolumeWorkloadProfile,
        candidates: &[VolumePreviewCandidate],
        gesture: Option<RenderGestureId>,
    ) -> Option<VolumePreviewChoice> {
        // Candidates are ordered from the guaranteed terminal navigation
        // body to the selected finer body. Choose the finest statically safe
        // native-resolution body. If none satisfies the static envelope, the
        // terminal body remains the unconditional navigation floor.
        #[cfg(test)]
        let test_navigation_limit = self.test_work_limits.map(|(interactive, _)| interactive);
        #[cfg(not(test))]
        let test_navigation_limit: Option<u64> = None;
        let profile_is_safe = |profile: &VolumeWorkloadProfile| {
            test_navigation_limit.map_or_else(
                || profile.native_navigation_is_safe(),
                |limit| profile.full_work_units() <= limit,
            )
        };
        let candidate_is_eligible = |candidate: &VolumePreviewCandidate| {
            candidate.available
                && candidate.profile.extent() == target_profile.extent()
                && candidate.profile.is_no_finer_than(target_profile)
                && profile_is_safe(&candidate.profile)
        };
        let statically_selected = || {
            candidates
                .iter()
                .enumerate()
                .rev()
                .find(|(_, candidate)| candidate_is_eligible(candidate))
                .map(|(index, _)| index)
                .or_else(|| {
                    candidates.iter().position(|candidate| {
                        candidate.full_volume && (candidate.available || candidate.cold_bootstrap)
                    })
                })
        };
        let candidate_index = (|| {
            if let Some(gesture) = gesture {
                let existing = self
                    .gesture_preview_policy
                    .as_ref()
                    .filter(|policy| policy.gesture == gesture)
                    .cloned();
                if let Some(policy) = existing {
                    if let Some(index) =
                        candidates
                            .iter()
                            .enumerate()
                            .rev()
                            .find_map(|(index, candidate)| {
                                (candidate_is_eligible(candidate)
                                    && (!policy.navigation_only || candidate.full_volume)
                                    && candidate.profile.scale_configuration() == policy.scales)
                                    .then_some(index)
                            })
                    {
                        Some(index)
                    } else {
                        // A camera-local exact body can cease to cover sustained
                        // movement. Make one monotonic cut to the finest complete
                        // safe full-volume rung and keep that configuration for
                        // the rest of the gesture.
                        let index = candidates
                            .iter()
                            .enumerate()
                            .rev()
                            .find(|(_, candidate)| {
                                candidate.full_volume && candidate_is_eligible(candidate)
                            })
                            .map(|(index, _)| index)
                            .or_else(statically_selected)?;
                        self.gesture_preview_policy = Some(GesturePreviewPolicy {
                            gesture,
                            scales: candidates[index].profile.scale_configuration(),
                            navigation_only: true,
                        });
                        Some(index)
                    }
                } else {
                    let index = statically_selected()?;
                    self.gesture_preview_policy = Some(GesturePreviewPolicy {
                        gesture,
                        scales: candidates[index].profile.scale_configuration(),
                        navigation_only: candidates[index].full_volume,
                    });
                    Some(index)
                }
            } else {
                self.gesture_preview_policy = None;
                statically_selected()
            }
        })();

        self.latest_candidate_facts = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let target_quality_eligible = candidate.profile.extent() == target_profile.extent()
                    && candidate.profile.is_no_finer_than(target_profile);
                let interaction_safe = profile_is_safe(&candidate.profile);
                let disposition = if candidate_index == Some(index) {
                    if candidate_is_eligible(candidate) {
                        VolumePreviewCandidateDisposition::Selected
                    } else if candidate.cold_bootstrap {
                        VolumePreviewCandidateDisposition::SelectedColdBootstrap
                    } else {
                        VolumePreviewCandidateDisposition::SelectedTerminalEmergency
                    }
                } else if !candidate.available {
                    VolumePreviewCandidateDisposition::NotResident
                } else if !target_quality_eligible {
                    VolumePreviewCandidateDisposition::FinerThanTarget
                } else if !interaction_safe {
                    VolumePreviewCandidateDisposition::InteractionUnsafe
                } else {
                    VolumePreviewCandidateDisposition::Eligible
                };
                VolumePreviewCandidateFacts {
                    kind: if candidate.full_volume {
                        VolumePreviewCandidateKind::Navigation
                    } else {
                        VolumePreviewCandidateKind::ExactTarget
                    },
                    layer_scales: candidate.profile.scale_configuration().0,
                    kernel: candidate.profile.kernel(),
                    shared_work_units: candidate.profile.shared_work_units(),
                    layer_work: candidate.profile.layer_work_facts(),
                    resource_count: candidate.resource_count,
                    payload_bytes: candidate.payload_bytes,
                    schedule_work_units: candidate.profile.full_work_units(),
                    complete_and_resident: candidate.available,
                    target_quality_eligible,
                    interaction_safe,
                    full_volume: candidate.full_volume,
                    disposition,
                }
            })
            .collect();

        let candidate_index = candidate_index?;
        Some(VolumePreviewChoice {
            candidate_index,
            profile: candidates[candidate_index].profile.clone(),
        })
    }

    pub(crate) fn latest_candidate_facts(&self) -> &[VolumePreviewCandidateFacts] {
        &self.latest_candidate_facts
    }

    pub(crate) fn preview_needs_replacement(
        &self,
        frame: FrameIdentity,
        proposed: &VolumeWorkloadProfile,
        output_extent: RenderExtent,
    ) -> bool {
        let _ = (proposed, output_extent);
        // A frame identity is the stable presentation transaction. Once its
        // complete native-resolution preview is visible, later resource
        // arrival or timing evidence cannot replace it with another preview.
        // The only next visible state for this frame is completed exact.
        self.presented_preview
            .as_ref()
            .is_none_or(|current| current.frame != frame)
    }

    pub(crate) fn note_presented_preview(
        &mut self,
        frame: FrameIdentity,
        profile: VolumeWorkloadProfile,
        output_extent: RenderExtent,
    ) {
        debug_assert_eq!(
            profile.extent(),
            output_extent,
            "a presented 3D preview must be native resolution"
        );
        self.presented_preview = Some(PresentedPreview { frame });
    }

    pub(crate) fn note_presented_exact(&mut self, frame: FrameIdentity) {
        if self
            .presented_preview
            .as_ref()
            .is_some_and(|preview| preview.frame == frame)
        {
            self.presented_preview = None;
        }
        if self
            .active_refinement
            .as_ref()
            .is_some_and(|refinement| refinement.frame == frame)
        {
            self.active_refinement = None;
        }
    }

    pub(crate) fn refinement_strip_height(
        &mut self,
        frame: FrameIdentity,
        profile: &VolumeWorkloadProfile,
    ) -> u32 {
        if let Some(active) = self
            .active_refinement
            .as_ref()
            .filter(|active| active.frame == frame && active.profile == *profile)
        {
            return active.strip_height_pixels;
        }
        let work_limit = self.refinement_work_limit(profile);
        let row_work = profile.work_units_for_rows(1).max(1);
        let rows = (work_limit / row_work).max(1);
        let strip_height_pixels = u32::try_from(rows)
            .unwrap_or(u32::MAX)
            .min(profile.extent.height_pixels())
            .max(1);
        self.active_refinement = Some(ActiveRefinementSchedule {
            frame,
            profile: profile.clone(),
            strip_height_pixels,
        });
        strip_height_pixels
    }

    #[cfg(test)]
    fn refinement_work_units(&mut self, profile: &VolumeWorkloadProfile) -> u64 {
        self.refinement_work_limit(profile)
    }

    pub(crate) fn track_timing(
        &mut self,
        ticket: GpuTimingTicket,
        profile: VolumeWorkloadProfile,
        schedule: VolumeColorSchedule,
        frame: FrameIdentity,
        refinement: Option<mirante4d_render_wgpu::VolumeRefinementProgress>,
    ) {
        let kind = match schedule {
            VolumeColorSchedule::Direct => PendingTimingKind::Direct,
            VolumeColorSchedule::InteractivePreview => PendingTimingKind::Preview,
            VolumeColorSchedule::AtomicRefinement {
                strip_height_pixels,
            } => {
                let Some(refinement) = refinement else {
                    return;
                };
                let completed_strip = refinement.completed_strips();
                if completed_strip == 0 {
                    return;
                }
                let y = completed_strip
                    .saturating_sub(1)
                    .saturating_mul(strip_height_pixels);
                let rows = profile
                    .extent
                    .height_pixels()
                    .saturating_sub(y)
                    .min(strip_height_pixels);
                PendingTimingKind::AtomicStrip {
                    frame,
                    completed_strip,
                    total_strips: refinement.total_strips(),
                    strip_work_units: profile.work_units_for_row_range(y, rows),
                }
            }
        };
        if self.pending_timings.len() == MAX_PENDING_TIMINGS {
            self.pending_timings.pop_front();
        }
        self.pending_timings.push_back(PendingTiming {
            execution_id: ticket.execution_id(),
            profile,
            kind,
        });
    }

    pub(crate) fn observe_timing(&mut self, timing: GpuFrameTiming) -> bool {
        let execution_id = timing.ticket().execution_id();
        let Some(index) = self
            .pending_timings
            .iter()
            .position(|pending| pending.execution_id == execution_id)
        else {
            return false;
        };
        let pending = self
            .pending_timings
            .remove(index)
            .expect("a located bounded pending timing remains present");
        let Some(render_ns) = timing.render_pass_ns() else {
            return false;
        };
        match pending.kind {
            PendingTimingKind::Direct => {
                let profile_changed =
                    self.observe_complete_profile(pending.profile.clone(), render_ns);
                self.observe_family_work(
                    &pending.profile,
                    pending.profile.full_work_units(),
                    render_ns,
                ) || profile_changed
            }
            PendingTimingKind::Preview => self.observe_family_work(
                &pending.profile,
                pending.profile.full_work_units(),
                render_ns,
            ),
            PendingTimingKind::AtomicStrip {
                frame,
                completed_strip,
                total_strips,
                strip_work_units,
            } => {
                let family_changed =
                    self.observe_family_work(&pending.profile, strip_work_units, render_ns);
                let profile_changed = self.observe_atomic_strip(
                    frame,
                    pending.profile,
                    completed_strip,
                    total_strips,
                    render_ns,
                );
                family_changed || profile_changed
            }
        }
    }

    pub(crate) fn discard_timing(&mut self, ticket: GpuTimingTicket) {
        if let Some(index) = self
            .pending_timings
            .iter()
            .position(|pending| pending.execution_id == ticket.execution_id())
        {
            self.pending_timings.remove(index);
        }
    }

    pub(crate) fn retire_dataset(&mut self) {
        self.family_calibrations.clear();
        self.profile_observations.clear();
        self.pending_timings.clear();
        self.strip_accumulators.clear();
        self.active_refinement = None;
        self.presented_preview = None;
        self.gesture_preview_policy = None;
        self.latest_candidate_facts.clear();
        #[cfg(test)]
        {
            self.test_work_limits = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_work_limits_for_test(
        &mut self,
        interactive_work_units: u64,
        refinement_work_units: u64,
    ) {
        self.test_work_limits = Some((interactive_work_units.max(1), refinement_work_units.max(1)));
        self.family_calibrations.clear();
        self.profile_observations.clear();
        self.pending_timings.clear();
        self.strip_accumulators.clear();
        self.active_refinement = None;
        self.presented_preview = None;
        self.gesture_preview_policy = None;
        self.latest_candidate_facts.clear();
    }

    fn observe_complete_profile(&mut self, profile: VolumeWorkloadProfile, render_ns: u64) -> bool {
        let now = self.tick();
        if let Some(observation) = self
            .profile_observations
            .iter_mut()
            .find(|observation| observation.profile == profile)
        {
            let previous = observation.certified;
            observation.certified = render_ns <= DIRECT_CERTIFICATION_NS;
            observation.last_use = now;
            return observation.certified != previous;
        }
        if self.profile_observations.len() == MAX_PROFILE_OBSERVATIONS {
            let oldest = self
                .profile_observations
                .iter()
                .enumerate()
                .min_by_key(|(_, observation)| observation.last_use)
                .map(|(index, _)| index)
                .expect("a full observation cache has one oldest entry");
            self.profile_observations.swap_remove(oldest);
        }
        self.profile_observations.push(ProfileObservation {
            profile,
            certified: render_ns <= DIRECT_CERTIFICATION_NS,
            last_use: now,
        });
        true
    }

    fn observe_atomic_strip(
        &mut self,
        frame: FrameIdentity,
        profile: VolumeWorkloadProfile,
        completed_strip: u32,
        total_strips: u32,
        render_ns: u64,
    ) -> bool {
        if total_strips == 0 || completed_strip == 0 || completed_strip > total_strips {
            return false;
        }
        let now = self.tick();
        let accumulator_index = self
            .strip_accumulators
            .iter()
            .position(|accumulator| {
                accumulator.frame == frame
                    && accumulator.profile == profile
                    && accumulator.observed.len() == total_strips as usize
            })
            .unwrap_or_else(|| {
                if self.strip_accumulators.len() == MAX_STRIP_ACCUMULATORS {
                    let oldest = self
                        .strip_accumulators
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, accumulator)| accumulator.last_use)
                        .map(|(index, _)| index)
                        .expect("a full accumulator cache has one oldest entry");
                    self.strip_accumulators.swap_remove(oldest);
                }
                self.strip_accumulators.push(StripAccumulator {
                    frame,
                    profile: profile.clone(),
                    observed: vec![false; total_strips as usize].into_boxed_slice(),
                    total_ns: 0,
                    last_use: now,
                });
                self.strip_accumulators.len() - 1
            });
        let accumulator = &mut self.strip_accumulators[accumulator_index];
        accumulator.last_use = now;
        let observed = &mut accumulator.observed[completed_strip as usize - 1];
        if !*observed {
            *observed = true;
            accumulator.total_ns = accumulator.total_ns.saturating_add(render_ns);
        }
        if accumulator.observed.iter().all(|observed| *observed) {
            let complete = self.strip_accumulators.swap_remove(accumulator_index);
            return self.observe_complete_profile(complete.profile, complete.total_ns);
        }
        false
    }

    fn safe_work_units(&mut self, profile: &VolumeWorkloadProfile) -> u64 {
        #[cfg(test)]
        if let Some((interactive, _)) = self.test_work_limits {
            return interactive;
        }
        let index = self.family_calibration_index(profile);
        self.family_calibrations[index].safe_work_units
    }

    fn navigation_is_safe(&self, profile: &VolumeWorkloadProfile) -> bool {
        #[cfg(test)]
        if let Some((interactive, _)) = self.test_work_limits {
            return profile.full_work_units() <= interactive;
        }
        profile.native_navigation_is_safe()
    }

    fn refinement_work_limit(&mut self, profile: &VolumeWorkloadProfile) -> u64 {
        #[cfg(test)]
        if let Some((_, refinement)) = self.test_work_limits {
            return refinement;
        }
        self.safe_work_units(profile)
            .saturating_mul(REFINEMENT_GPU_BUDGET_NS)
            .checked_div(INTERACTION_GPU_BUDGET_NS)
            .unwrap_or(0)
            .max(
                profile
                    .minimum_work_units()
                    .saturating_mul(REFINEMENT_GPU_BUDGET_NS)
                    .checked_div(INTERACTION_GPU_BUDGET_NS)
                    .unwrap_or(1),
            )
    }

    fn family_calibration_index(&mut self, profile: &VolumeWorkloadProfile) -> usize {
        let now = self.tick();
        if let Some(index) = self
            .family_calibrations
            .iter()
            .position(|calibration| calibration.family == profile.family)
        {
            self.family_calibrations[index].last_use = now;
            return index;
        }
        if self.family_calibrations.len() == MAX_FAMILY_CALIBRATIONS {
            let oldest = self
                .family_calibrations
                .iter()
                .enumerate()
                .min_by_key(|(_, calibration)| calibration.last_use)
                .map(|(index, _)| index)
                .expect("a full family calibration cache has one oldest entry");
            self.family_calibrations.swap_remove(oldest);
        }
        self.family_calibrations.push(FamilyCalibration {
            family: profile.family.clone(),
            safe_work_units: profile.initial_safe_work_units(),
            last_use: now,
        });
        self.family_calibrations.len() - 1
    }

    fn observe_family_work(
        &mut self,
        profile: &VolumeWorkloadProfile,
        observed_work: u64,
        render_ns: u64,
    ) -> bool {
        #[cfg(test)]
        if self.test_work_limits.is_some() {
            return false;
        }
        let index = self.family_calibration_index(profile);
        let calibration = &mut self.family_calibrations[index];
        if render_ns > INTERACTION_GPU_BUDGET_NS {
            let safe = scaled_safe_work(
                observed_work,
                render_ns,
                INTERACTION_GPU_BUDGET_NS,
                profile.minimum_work_units(),
            );
            let previous = calibration.safe_work_units;
            calibration.safe_work_units = calibration.safe_work_units.min(safe);
            return calibration.safe_work_units != previous;
        }
        // Fast observations do not grow a visible preview envelope. They are
        // retained only as evidence for conservative hidden-strip sizing.
        false
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }
}

fn extent_pixels(extent: RenderExtent) -> u64 {
    u64::from(extent.width_pixels()).saturating_mul(u64::from(extent.height_pixels()))
}

fn scaled_safe_work(
    observed_work: u64,
    render_ns: u64,
    budget_ns: u64,
    minimum_work_units: u64,
) -> u64 {
    let proportional = observed_work
        .saturating_mul(budget_ns)
        .checked_div(render_ns.max(1))
        .unwrap_or(0);
    proportional
        .saturating_mul(ADAPTIVE_SAFETY_NUMERATOR)
        .checked_div(ADAPTIVE_SAFETY_DENOMINATOR)
        .unwrap_or(0)
        .max(minimum_work_units)
}

#[cfg(test)]
mod tests {
    use mirante4d_application::{
        CurrentnessGeneration, RenderGestureKind, RenderIntentBase, RenderIntentMailbox,
        RenderIntentSample, RenderIntentTarget, SourceSessionGeneration,
    };
    use mirante4d_dataset::{DatasetScale, ResourceValidity};
    use mirante4d_domain::{GridToWorld, Shape3D};

    use super::*;

    fn profile(extent: RenderExtent, work_per_pixel: u64) -> VolumeWorkloadProfile {
        profile_with(
            extent,
            work_per_pixel,
            work_per_pixel,
            1,
            RenderMode::Mip,
            ScaleLevel::BASE,
        )
    }

    fn profile_with(
        extent: RenderExtent,
        work_per_pixel: u64,
        maximum_steps: u64,
        initial_mode_prior: u64,
        mode: RenderMode,
        scale: ScaleLevel,
    ) -> VolumeWorkloadProfile {
        let camera = CameraView::new(
            Projection::Orthographic,
            mirante4d_domain::WorldPoint3::origin(),
            mirante4d_domain::UnitQuaternion::identity(),
            1.0,
            1.0,
            1.0,
        )
        .unwrap();
        VolumeWorkloadProfile {
            extent,
            presentation_viewport: PresentationViewport::new(
                f64::from(extent.width_pixels()),
                f64::from(extent.height_pixels()),
            )
            .unwrap(),
            camera,
            timepoint: TimeIndex::new(0),
            layers: vec![VolumeLayerWorkload {
                layer: LogicalLayerKey::new(0),
                scale,
                render_state: RenderState::mip(SamplingPolicy::VoxelExact),
                legacy_maximum_steps: maximum_steps,
                legacy_work_units_per_pixel: work_per_pixel,
                legacy_initial_prior_work_units_per_pixel: work_per_pixel
                    .saturating_mul(initial_mode_prior),
                projected_rect: PixelRect::full(extent),
                traversal_step_bound: maximum_steps,
                sample_taps_per_step: 1,
                gradient_taps_per_ray: 0,
                fixed_work_units_per_output_pixel: 0,
                scheduled_rect: PixelRect::full(extent),
                scheduled_step_bound: 1,
                scheduled_work_units_per_step: work_per_pixel,
                terminal_work_units_per_projected_pixel: 0,
                world_corners: [[0.0; 3]; 8],
                inverse_rows: [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ],
                shader_geometry: ShaderGridGeometry {
                    scale,
                    shape_xyz: [maximum_steps; 3],
                    control_bits: [0; 24],
                },
            }]
            .into_boxed_slice(),
            shared: SharedVolumeWorkload {
                fixed_work_units_per_output_pixel: 0,
                scheduled_rect: PixelRect::default(),
                scheduled_step_bound: 0,
                scheduled_work_units_per_step: 0,
            },
            model: VolumeWorkModel::StaticScheduleAligned,
            family: VolumeWorkFamily {
                projection: camera.projection(),
                model: VolumeWorkModel::StaticScheduleAligned,
                kernel: match mode {
                    RenderMode::Mip => VolumeKernelWorkClass::HomogeneousMip,
                    RenderMode::Dvr => VolumeKernelWorkClass::GeneralDvr,
                    RenderMode::Isosurface => VolumeKernelWorkClass::HomogeneousIso,
                },
                layers: vec![VolumeLayerWorkClass {
                    mode,
                    sampling: SamplingPolicy::VoxelExact,
                    iso_shading: None,
                    iso_display_bucket: None,
                }]
                .into_boxed_slice(),
            },
        }
    }

    fn navigation_candidate(
        profile: VolumeWorkloadProfile,
        available: bool,
    ) -> VolumePreviewCandidate {
        VolumePreviewCandidate::navigation(profile, available, false, 1, 1)
    }

    fn cold_navigation_candidate(profile: VolumeWorkloadProfile) -> VolumePreviewCandidate {
        VolumePreviewCandidate::navigation(profile, false, true, 1, 1)
    }

    fn target_candidate(profile: VolumeWorkloadProfile) -> VolumePreviewCandidate {
        VolumePreviewCandidate::target(profile, true, 1, 1)
    }

    fn analytical_camera(
        projection: Projection,
        target: WorldPoint3,
        world_per_point: f64,
        focal_points: f64,
        distance: f64,
        extent: RenderExtent,
    ) -> (CameraFrame, PresentationViewport) {
        let presentation = PresentationViewport::new(
            f64::from(extent.width_pixels()),
            f64::from(extent.height_pixels()),
        )
        .unwrap();
        let view = CameraView::new(
            projection,
            target,
            mirante4d_domain::UnitQuaternion::identity(),
            world_per_point,
            focal_points,
            distance,
        )
        .unwrap();
        (CameraFrame::new(view, presentation).unwrap(), presentation)
    }

    fn analytical_layer(
        layer: u32,
        scale: ScaleLevel,
        dataset_scale: DatasetScale,
        camera: CameraFrame,
        presentation: PresentationViewport,
        extent: RenderExtent,
    ) -> VolumeLayerWorkload {
        let affine =
            ValidatedShaderAffine::new(dataset_scale.grid_to_world(), dataset_scale.shape())
                .unwrap();
        let inverse_rows = affine.world_to_grid_rows();
        let world_corners = shader_volume_world_corners(affine).unwrap();
        let projected_rect = projected_pixel_rect(camera, &world_corners, extent);
        let traversal_step_bound = conservative_grid_traversal_bound(
            camera,
            presentation,
            extent,
            projected_rect,
            dataset_scale.shape(),
            inverse_rows,
        );
        VolumeLayerWorkload {
            layer: LogicalLayerKey::new(layer),
            scale,
            render_state: RenderState::mip(SamplingPolicy::VoxelExact),
            legacy_maximum_steps: dataset_scale
                .shape()
                .x()
                .max(dataset_scale.shape().y())
                .max(dataset_scale.shape().z()),
            legacy_work_units_per_pixel: traversal_step_bound.max(1),
            legacy_initial_prior_work_units_per_pixel: traversal_step_bound.max(1),
            projected_rect,
            traversal_step_bound,
            sample_taps_per_step: 1,
            gradient_taps_per_ray: 0,
            fixed_work_units_per_output_pixel: 0,
            scheduled_rect: projected_rect,
            scheduled_step_bound: traversal_step_bound,
            scheduled_work_units_per_step: 0,
            terminal_work_units_per_projected_pixel: 0,
            world_corners,
            inverse_rows,
            shader_geometry: shader_grid_geometry(scale, affine),
        }
    }

    fn analytical_world_corners(dataset_scale: DatasetScale) -> [[f64; 3]; 8] {
        shader_volume_world_corners(
            ValidatedShaderAffine::new(dataset_scale.grid_to_world(), dataset_scale.shape())
                .unwrap(),
        )
        .unwrap()
    }

    fn measured_matrix_profile(count: usize, render_state: RenderState) -> VolumeWorkloadProfile {
        let extent = RenderExtent::new(1_920, 1_080).unwrap();
        let (camera, presentation_viewport) = analytical_camera(
            Projection::Orthographic,
            WorldPoint3::origin(),
            1.0,
            1_080.0,
            128.0,
            extent,
        );
        let projected_rect = PixelRect {
            x_start: 420,
            y_start: 0,
            x_end: 1_500,
            y_end: 1_080,
        };
        let shader_geometry = ShaderGridGeometry {
            scale: ScaleLevel::BASE,
            shape_xyz: [64; 3],
            control_bits: [0; 24],
        };
        let mut layers = (0..count)
            .map(|index| VolumeLayerWorkload {
                layer: LogicalLayerKey::new(index as u32),
                scale: ScaleLevel::BASE,
                render_state,
                legacy_maximum_steps: 64,
                legacy_work_units_per_pixel: 64_u64.saturating_mul(
                    match render_state.sampling_policy() {
                        SamplingPolicy::VoxelExact => 1,
                        SamplingPolicy::SmoothLinear => 8,
                    },
                ),
                legacy_initial_prior_work_units_per_pixel: 64_u64
                    .saturating_mul(match render_state.sampling_policy() {
                        SamplingPolicy::VoxelExact => 1,
                        SamplingPolicy::SmoothLinear => 8,
                    })
                    .saturating_mul(if render_state.mode() == RenderMode::Mip {
                        1
                    } else {
                        2
                    }),
                projected_rect,
                traversal_step_bound: 64,
                sample_taps_per_step: match render_state.sampling_policy() {
                    SamplingPolicy::VoxelExact => 1,
                    SamplingPolicy::SmoothLinear => 8,
                },
                gradient_taps_per_ray: 0,
                fixed_work_units_per_output_pixel: 0,
                scheduled_rect: projected_rect,
                scheduled_step_bound: 64,
                scheduled_work_units_per_step: 0,
                terminal_work_units_per_projected_pixel: 0,
                world_corners: [[0.0; 3]; 8],
                inverse_rows: [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ],
                shader_geometry,
            })
            .collect::<Vec<_>>();
        let kernel = classify_kernel(&layers);
        let shared = configure_schedule(kernel, camera, presentation_viewport, extent, &mut layers);
        let family_layer = VolumeLayerWorkClass {
            mode: render_state.mode(),
            sampling: render_state.sampling_policy(),
            iso_shading: render_state
                .iso_parameters()
                .map(|parameters| parameters.shading_policy()),
            iso_display_bucket: render_state.iso_parameters().map(|_| 16),
        };
        VolumeWorkloadProfile {
            extent,
            presentation_viewport,
            camera: camera.view(),
            timepoint: TimeIndex::new(0),
            layers: layers.into_boxed_slice(),
            shared,
            model: VolumeWorkModel::StaticScheduleAligned,
            family: VolumeWorkFamily {
                projection: camera.view().projection(),
                model: VolumeWorkModel::StaticScheduleAligned,
                kernel,
                layers: vec![family_layer; count].into_boxed_slice(),
            },
        }
    }

    #[test]
    fn measured_1080p_matrix_boundaries_drive_static_navigation_policy() {
        let dvr = |sampling| {
            RenderState::dvr(
                sampling,
                mirante4d_domain::DvrOpacityTransfer::new(
                    mirante4d_domain::DisplayWindow::new(0.0, 1.0).unwrap(),
                    mirante4d_domain::TransferCurve::linear(),
                ),
                0.02,
            )
            .unwrap()
        };
        let iso = |sampling| RenderState::iso(sampling, IsoShadingPolicy::Flat, 0.5).unwrap();

        assert!(
            measured_matrix_profile(8, RenderState::mip(SamplingPolicy::VoxelExact))
                .native_navigation_is_safe()
        );
        assert!(
            measured_matrix_profile(4, dvr(SamplingPolicy::VoxelExact)).native_navigation_is_safe()
        );
        assert!(
            !measured_matrix_profile(8, dvr(SamplingPolicy::VoxelExact))
                .native_navigation_is_safe()
        );
        assert!(
            measured_matrix_profile(2, iso(SamplingPolicy::VoxelExact)).native_navigation_is_safe()
        );
        assert!(
            !measured_matrix_profile(4, iso(SamplingPolicy::VoxelExact))
                .native_navigation_is_safe()
        );

        assert!(
            measured_matrix_profile(1, RenderState::mip(SamplingPolicy::SmoothLinear))
                .native_navigation_is_safe()
        );
        assert!(
            !measured_matrix_profile(2, RenderState::mip(SamplingPolicy::SmoothLinear))
                .native_navigation_is_safe()
        );
        assert!(
            measured_matrix_profile(1, dvr(SamplingPolicy::SmoothLinear))
                .native_navigation_is_safe()
        );
        assert!(
            !measured_matrix_profile(2, dvr(SamplingPolicy::SmoothLinear))
                .native_navigation_is_safe()
        );
        assert!(
            !measured_matrix_profile(1, iso(SamplingPolicy::SmoothLinear))
                .native_navigation_is_safe()
        );
    }

    #[test]
    fn playback_retains_the_pre_cutover_neutral_work_boundary() {
        let extent = RenderExtent::new(1_920, 1_080).unwrap();
        let static_profile = profile_with(extent, 2_048, 512, 1, RenderMode::Mip, ScaleLevel::BASE);
        let mut playback_profile = static_profile.clone();
        playback_profile.model = VolumeWorkModel::LegacyPlayback;
        playback_profile.family.model = VolumeWorkModel::LegacyPlayback;
        playback_profile.layers[0].legacy_work_units_per_pixel = 512;
        playback_profile.layers[0].legacy_initial_prior_work_units_per_pixel = 512;

        assert!(
            !static_profile.native_navigation_is_safe(),
            "the schedule-aligned static model deliberately rejects this synthetic boundary"
        );
        assert_eq!(
            playback_profile.full_work_units(),
            1_920_u64 * 1_080 * 512,
            "playback must retain the exact pre-cutover neutral sample-count formula"
        );
        assert!(
            playback_profile.native_navigation_is_safe(),
            "the static estimator must not force a previously safe playback body to a coarser rung"
        );
        assert_eq!(
            playback_profile.initial_safe_work_units(),
            LEGACY_PLAYBACK_INITIAL_INTERACTIVE_WORK_UNITS
        );
    }

    #[test]
    fn playback_profile_identity_ignores_static_schedule_classification() {
        let extent = RenderExtent::new(320, 200).unwrap();
        let mut playback = profile_with(extent, 512, 512, 1, RenderMode::Mip, ScaleLevel::BASE);
        playback.model = VolumeWorkModel::LegacyPlayback;
        playback.family.model = VolumeWorkModel::LegacyPlayback;
        let mut differently_classified = playback.clone();
        differently_classified.family.kernel = VolumeKernelWorkClass::GeneralDvr;
        differently_classified
            .shared
            .fixed_work_units_per_output_pixel = u64::MAX;
        differently_classified.layers[0].projected_rect = PixelRect::default();
        differently_classified.layers[0].scheduled_step_bound = u64::MAX;
        differently_classified.layers[0].scheduled_work_units_per_step = u64::MAX;

        assert_eq!(
            playback, differently_classified,
            "static schedule facts must not split playback's pre-cutover observation identity"
        );

        playback.model = VolumeWorkModel::StaticScheduleAligned;
        playback.family.model = VolumeWorkModel::StaticScheduleAligned;
        differently_classified.model = VolumeWorkModel::StaticScheduleAligned;
        differently_classified.family.model = VolumeWorkModel::StaticScheduleAligned;
        assert_ne!(
            playback, differently_classified,
            "static observations still require exact schedule identity"
        );
    }

    #[test]
    fn projected_coverage_is_clipped_and_off_center_without_charging_the_full_viewport() {
        let extent = RenderExtent::new(100, 100).unwrap();
        let (camera, _) = analytical_camera(
            Projection::Orthographic,
            WorldPoint3::new(9.5, 9.5, 9.5).unwrap(),
            1.0,
            100.0,
            100.0,
            extent,
        );
        let shape = Shape3D::new(20, 20, 20).unwrap();
        let centered = DatasetScale::new(
            ScaleLevel::BASE,
            shape,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        );
        let centered_rect =
            projected_pixel_rect(camera, &analytical_world_corners(centered), extent);
        assert!(centered_rect.width() >= 20 && centered_rect.width() <= 24);
        assert!(centered_rect.height() >= 20 && centered_rect.height() <= 24);

        let clipped = DatasetScale::new(
            ScaleLevel::BASE,
            shape,
            GridToWorld::from_row_major([
                1.0, 0.0, 0.0, 50.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            ResourceValidity::AllValid,
        );
        let clipped_rect = projected_pixel_rect(camera, &analytical_world_corners(clipped), extent);
        assert_eq!(clipped_rect.x_end, extent.width_pixels());
        assert!(clipped_rect.width() > 0 && clipped_rect.width() < centered_rect.width());

        let offscreen = DatasetScale::new(
            ScaleLevel::BASE,
            shape,
            GridToWorld::from_row_major([
                1.0, 0.0, 0.0, 1_000.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            ResourceValidity::AllValid,
        );
        let offscreen_rect =
            projected_pixel_rect(camera, &analytical_world_corners(offscreen), extent);
        assert_eq!(offscreen_rect.pixels(), 0);
    }

    #[test]
    fn perspective_eye_plane_falls_back_to_full_conservative_coverage() {
        let extent = RenderExtent::new(160, 90).unwrap();
        let (camera, _) = analytical_camera(
            Projection::Perspective,
            WorldPoint3::origin(),
            1.0,
            90.0,
            10.0,
            extent,
        );
        let scale = DatasetScale::new(
            ScaleLevel::BASE,
            Shape3D::new(20, 20, 20).unwrap(),
            GridToWorld::from_row_major([
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 10.0, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            ResourceValidity::AllValid,
        );
        assert_eq!(
            projected_pixel_rect(camera, &analytical_world_corners(scale), extent),
            PixelRect::full(extent)
        );
    }

    #[test]
    fn transform_aware_traversal_uses_camera_depth_not_the_longest_shape_axis() {
        let extent = RenderExtent::new(200, 120).unwrap();
        let shape = Shape3D::new(10, 10, 1_000).unwrap();
        let (camera, presentation) = analytical_camera(
            Projection::Orthographic,
            WorldPoint3::new(499.5, 4.5, 4.5).unwrap(),
            1.0,
            100.0,
            2_000.0,
            extent,
        );
        let inverse = ValidatedShaderAffine::new(GridToWorld::identity(), shape)
            .unwrap()
            .world_to_grid_rows();
        let bound = conservative_grid_traversal_bound(
            camera,
            presentation,
            extent,
            PixelRect::full(extent),
            shape,
            inverse,
        );
        assert!((10..=11).contains(&bound));

        let sheared = GridToWorld::from_row_major([
            1.0, 0.0, 0.5, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.25, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let sheared_bound = conservative_grid_traversal_bound(
            camera,
            presentation,
            extent,
            PixelRect::full(extent),
            shape,
            ValidatedShaderAffine::new(sheared, shape)
                .unwrap()
                .world_to_grid_rows(),
        );
        assert!(sheared_bound > 0 && sheared_bound <= shape.x());
    }

    #[test]
    fn mixed_scale_general_dvr_reuses_the_common_physical_interval_bound() {
        let extent = RenderExtent::new(128, 128).unwrap();
        let (camera, presentation) = analytical_camera(
            Projection::Orthographic,
            WorldPoint3::new(31.5, 31.5, 31.5).unwrap(),
            1.0,
            100.0,
            128.0,
            extent,
        );
        let fine = DatasetScale::new(
            ScaleLevel::BASE,
            Shape3D::new(64, 64, 64).unwrap(),
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        );
        let coarse = DatasetScale::new(
            ScaleLevel::new(1),
            Shape3D::new(32, 32, 32).unwrap(),
            GridToWorld::from_row_major([
                2.0, 0.0, 0.0, 0.5, 0.0, 2.0, 0.0, 0.5, 0.0, 0.0, 2.0, 0.5, 0.0, 0.0, 0.0, 1.0,
            ])
            .unwrap(),
            ResourceValidity::AllValid,
        );
        let layers = [
            analytical_layer(0, ScaleLevel::BASE, fine, camera, presentation, extent),
            analytical_layer(1, ScaleLevel::new(1), coarse, camera, presentation, extent),
        ];
        assert!(shader_world_bounds_match(
            &layers[0].world_corners,
            &layers[1].world_corners
        ));
        let union = layers[0].projected_rect.union(layers[1].projected_rect);
        assert_eq!(
            general_dvr_traversal_bound(camera, presentation, extent, union, &layers),
            layers
                .iter()
                .map(|layer| layer.traversal_step_bound)
                .max()
                .unwrap()
        );
    }

    #[test]
    fn analytical_overflow_saturates_instead_of_wrapping_safe() {
        assert_eq!(saturating_ceil_u64(f64::INFINITY), u64::MAX);
        assert_eq!(saturating_ceil_u64(u64::MAX as f64), u64::MAX);
        assert_eq!(
            schedule_region_work(
                PixelRect::full(RenderExtent::new(2, 2).unwrap()),
                0,
                2,
                u64::MAX,
                2
            ),
            u64::MAX
        );
    }

    #[test]
    fn cold_terminal_candidate_bootstraps_without_claiming_gpu_residency() {
        let mut controller = VolumePresentationController::default();
        let extent = RenderExtent::new(640, 480).unwrap();
        let target = profile_with(extent, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(2));
        let terminal = profile_with(extent, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(6));
        let choice = controller
            .select_preview_profile(&target, &[cold_navigation_candidate(terminal)], None)
            .expect("an uploadable cold terminal rung must start renderer residency");

        assert_eq!(choice.candidate_index, 0);
        let facts = controller.latest_candidate_facts();
        assert_eq!(facts.len(), 1);
        assert!(!facts[0].complete_and_resident);
        assert_eq!(
            facts[0].disposition,
            VolumePreviewCandidateDisposition::SelectedColdBootstrap
        );
    }

    #[test]
    fn visible_direct_policy_is_not_demoted_by_runtime_timing() {
        let mut controller = VolumePresentationController::default();
        let profile = profile(RenderExtent::new(1_000, 1_000).unwrap(), 100);
        assert!(controller.direct_is_safe(&profile));
        controller.observe_complete_profile(profile.clone(), INTERACTION_GPU_BUDGET_NS + 1);
        controller.observe_family_work(
            &profile,
            profile.full_work_units(),
            INTERACTION_GPU_BUDGET_NS * 2,
        );
        assert!(controller.direct_is_safe(&profile));
    }

    #[test]
    fn iso_threshold_bands_have_independent_hidden_strip_calibration() {
        let extent = RenderExtent::new(640, 480).unwrap();
        let mut low_threshold = profile_with(
            extent,
            64,
            64,
            2,
            RenderMode::Isosurface,
            ScaleLevel::new(3),
        );
        low_threshold.family.layers[0].iso_shading = Some(IsoShadingPolicy::Flat);
        low_threshold.family.layers[0].iso_display_bucket = Some(3);
        let mut high_threshold = low_threshold.clone();
        high_threshold.family.layers[0].iso_display_bucket = Some(27);
        let mut controller = VolumePresentationController::default();

        let low_index = controller.family_calibration_index(&low_threshold);
        let high_index = controller.family_calibration_index(&high_threshold);

        assert_ne!(low_index, high_index);
        assert_eq!(controller.family_calibrations.len(), 2);
    }

    #[test]
    fn exact_strip_height_never_exceeds_refinement_work_envelope() {
        let mut controller = VolumePresentationController::default();
        let profile = profile(RenderExtent::new(1_920, 1_080).unwrap(), 4_000);
        let limit = controller.refinement_work_units(&profile);
        let rows = controller.refinement_strip_height(FrameIdentity::new(1), &profile);
        assert!(rows >= 1);
        assert!(profile.work_units_for_rows(rows) <= limit || rows == 1);
    }

    #[test]
    fn complete_strip_observation_can_certify_the_matching_full_profile() {
        let mut controller = VolumePresentationController::default();
        let profile = profile(RenderExtent::new(100, 100).unwrap(), 300_000);
        assert!(!controller.direct_is_safe(&profile));
        controller.observe_atomic_strip(
            FrameIdentity::new(7),
            profile.clone(),
            1,
            2,
            DIRECT_CERTIFICATION_NS / 4,
        );
        assert!(!controller.direct_is_safe(&profile));
        controller.observe_atomic_strip(
            FrameIdentity::new(7),
            profile.clone(),
            2,
            2,
            DIRECT_CERTIFICATION_NS / 4,
        );
        assert!(controller.direct_is_safe(&profile));
    }

    #[test]
    fn direct_certification_does_not_cross_camera_geometry() {
        let mut controller = VolumePresentationController::default();
        let first = profile(RenderExtent::new(100, 100).unwrap(), 300_000);
        let mut moved = first.clone();
        moved.camera = CameraView::new(
            mirante4d_domain::Projection::Orthographic,
            mirante4d_domain::WorldPoint3::new(1.0, 0.0, 0.0).unwrap(),
            mirante4d_domain::UnitQuaternion::identity(),
            1.0,
            1.0,
            1.0,
        )
        .unwrap();
        assert!(!controller.direct_is_safe(&first));
        controller.observe_complete_profile(first.clone(), DIRECT_CERTIFICATION_NS / 2);
        assert!(controller.direct_is_safe(&first));
        assert!(!controller.direct_is_safe(&moved));
    }

    #[test]
    fn schedule_classes_are_independent_and_slow_families_shrink_hidden_strip_work() {
        let extent = RenderExtent::new(400, 250).unwrap();
        let mip = profile_with(extent, 1_000, 1_000, 1, RenderMode::Mip, ScaleLevel::BASE);
        let dvr = profile_with(extent, 1_000, 1_000, 2, RenderMode::Dvr, ScaleLevel::BASE);
        let mut controller = VolumePresentationController::default();

        assert_eq!(
            controller.safe_work_units(&mip),
            INITIAL_INTERACTIVE_WORK_UNITS
        );
        assert_eq!(
            controller.safe_work_units(&dvr),
            INITIAL_INTERACTIVE_WORK_UNITS
        );
        assert!(!controller.observe_family_work(&dvr, 64_000_000, 3_000_000));
        assert_eq!(
            controller.safe_work_units(&dvr),
            INITIAL_INTERACTIVE_WORK_UNITS
        );
        assert_eq!(
            controller.safe_work_units(&mip),
            INITIAL_INTERACTIVE_WORK_UNITS
        );

        assert!(controller.observe_family_work(&dvr, 128_000_000, 24_000_000));
        assert_eq!(controller.safe_work_units(&dvr), 48_000_000);
        assert_eq!(
            controller.safe_work_units(&mip),
            INITIAL_INTERACTIVE_WORK_UNITS
        );
    }

    #[test]
    fn preview_selection_is_native_resolution_and_uses_static_data_detail() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let floor = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(3));
        let target = profile_with(output, 3_000, 3_000, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(80_000_000, 10_000_000);
        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor.clone(), true),
                    target_candidate(target.clone()),
                ],
                None,
            )
            .unwrap();
        assert_eq!(choice.candidate_index(), 0);
        assert_eq!(choice.into_profile().extent(), output);

        let target = profile_with(output, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(2));
        controller.set_work_limits_for_test(400_000_000, 10_000_000);
        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor, true),
                    target_candidate(target.clone()),
                ],
                None,
            )
            .unwrap();
        assert_eq!(choice.candidate_index(), 1);
        assert_eq!(choice.into_profile().extent(), output);
    }

    #[test]
    fn preview_selects_the_finest_complete_safe_intermediate_rung() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let terminal = profile_with(output, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(6));
        let s5 = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(5));
        let s4 = profile_with(output, 300, 300, 1, RenderMode::Mip, ScaleLevel::new(4));
        let s3 = profile_with(output, 500, 500, 1, RenderMode::Mip, ScaleLevel::new(3));
        let target = profile_with(output, 3_000, 3_000, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(350_000_000, 10_000_000);

        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal, true),
                    navigation_candidate(s5, true),
                    navigation_candidate(s4, true),
                    navigation_candidate(s3, true),
                    target_candidate(target.clone()),
                ],
                None,
            )
            .expect("the resident ladder has a safe candidate");

        assert_eq!(choice.candidate_index(), 2);
        assert_eq!(choice.into_profile().layers[0].scale, ScaleLevel::new(4));
        let facts = controller.latest_candidate_facts();
        assert_eq!(
            facts[2].disposition,
            VolumePreviewCandidateDisposition::Selected
        );
        assert_eq!(
            facts[3].disposition,
            VolumePreviewCandidateDisposition::InteractionUnsafe
        );
        assert_eq!(
            facts[4].disposition,
            VolumePreviewCandidateDisposition::InteractionUnsafe
        );
    }

    #[test]
    fn preview_never_selects_a_resident_rung_finer_than_the_current_target() {
        let output = RenderExtent::new(100, 100).unwrap();
        let terminal = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(6));
        let too_fine = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(2));
        let target = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(3));
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(u64::MAX, 10_000_000);

        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal, true),
                    navigation_candidate(too_fine, true),
                ],
                None,
            )
            .expect("the terminal body remains eligible");

        assert_eq!(choice.candidate_index(), 0);
        assert_eq!(
            controller.latest_candidate_facts()[1].disposition,
            VolumePreviewCandidateDisposition::FinerThanTarget
        );
    }

    #[test]
    fn losing_a_camera_local_target_makes_one_way_cut_to_finest_resident_rung() {
        let output = RenderExtent::new(100, 100).unwrap();
        let terminal = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(6));
        let intermediate = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(4));
        let target = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut mailbox = RenderIntentMailbox::new();
        let base = RenderIntentBase::new(
            SourceSessionGeneration::new(1),
            CurrentnessGeneration::initial(),
        );
        mailbox
            .sample(
                base,
                RenderIntentSample::camera(RenderGestureKind::Drag, target.camera),
                1,
                true,
            )
            .unwrap();
        let gesture = mailbox.snapshot().active_gesture;
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(u64::MAX, 10_000_000);

        let first = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal.clone(), true),
                    navigation_candidate(intermediate.clone(), true),
                    target_candidate(target.clone()),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(first.into_profile().layers[0].scale, ScaleLevel::new(2));

        let lost = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal.clone(), true),
                    navigation_candidate(intermediate.clone(), true),
                    VolumePreviewCandidate::target(target.clone(), false, 1, 1),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(
            lost.into_profile().layers[0].scale,
            ScaleLevel::new(4),
            "loss of the camera-local target must choose the finest safe full-volume rung"
        );

        let after_arrival = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal, true),
                    navigation_candidate(intermediate, true),
                    target_candidate(target.clone()),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(
            after_arrival.into_profile().layers[0].scale,
            ScaleLevel::new(4),
            "one gesture must not upgrade again after its one-way full-volume cut"
        );
    }

    #[test]
    fn one_frame_never_replaces_its_presented_preview() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let current = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(3));
        let proposed = profile_with(output, 400, 400, 1, RenderMode::Mip, ScaleLevel::new(2));
        let frame = FrameIdentity::new(9);
        let mut controller = VolumePresentationController::default();
        controller.note_presented_preview(frame, current, output);
        assert!(!controller.preview_needs_replacement(frame, &proposed, output));
        assert!(controller.preview_needs_replacement(FrameIdentity::new(10), &proposed, output));
    }

    #[test]
    fn one_gesture_freezes_floor_even_if_a_finer_body_arrives() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let floor = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(3));
        let target = profile_with(output, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut mailbox = RenderIntentMailbox::new();
        let base = RenderIntentBase::new(
            SourceSessionGeneration::new(1),
            CurrentnessGeneration::initial(),
        );
        mailbox
            .sample(
                base,
                RenderIntentSample::camera(RenderGestureKind::Drag, target.camera),
                1,
                true,
            )
            .unwrap();
        let gesture = mailbox.snapshot().active_gesture;
        let mut controller = VolumePresentationController::default();

        let first = controller
            .select_preview_profile(
                &target,
                &[navigation_candidate(floor.clone(), true)],
                gesture,
            )
            .unwrap();
        assert_eq!(first.candidate_index(), 0);
        let after_arrival = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor.clone(), true),
                    target_candidate(target.clone()),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(
            after_arrival.into_profile().layers[0].scale,
            floor.layers[0].scale,
            "resource arrival must not upgrade the visible body during one gesture"
        );

        mailbox.finish(base, RenderIntentTarget::ThreeD).unwrap();
        mailbox
            .sample(
                base,
                RenderIntentSample::camera(RenderGestureKind::Drag, target.camera),
                2,
                true,
            )
            .unwrap();
        let next_gesture = mailbox.snapshot().active_gesture;
        let next = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor, true),
                    target_candidate(target.clone()),
                ],
                next_gesture,
            )
            .unwrap();
        assert_eq!(next.into_profile().layers[0].scale, target.layers[0].scale);
    }

    #[test]
    fn learned_strip_size_changes_only_for_the_next_exact_candidate() {
        let profile = profile(RenderExtent::new(1_920, 1_080).unwrap(), 4_000);
        let mut controller = VolumePresentationController::default();
        let first_frame = FrameIdentity::new(3);
        let first = controller.refinement_strip_height(first_frame, &profile);
        assert!(first > 1);
        assert!(controller.observe_family_work(&profile, 32_000_000, 24_000_000));
        assert_eq!(
            controller.refinement_strip_height(first_frame, &profile),
            first
        );
        assert!(
            controller.refinement_strip_height(FrameIdentity::new(4), &profile) < first,
            "new timing must resize future work without restarting the active candidate"
        );
    }

    #[test]
    fn calibration_cache_is_bounded_and_dataset_retirement_clears_adaptation() {
        let mut controller = VolumePresentationController::default();
        for layer_count in 1..=(MAX_FAMILY_CALIBRATIONS + 5) {
            let mut profile = profile(RenderExtent::new(64, 64).unwrap(), 10);
            profile.family.layers = vec![
                VolumeLayerWorkClass {
                    mode: RenderMode::Mip,
                    sampling: SamplingPolicy::VoxelExact,
                    iso_shading: None,
                    iso_display_bucket: None,
                };
                layer_count
            ]
            .into_boxed_slice();
            let _ = controller.safe_work_units(&profile);
        }
        assert_eq!(
            controller.family_calibrations.len(),
            MAX_FAMILY_CALIBRATIONS
        );
        controller.retire_dataset();
        assert!(controller.family_calibrations.is_empty());
        assert!(controller.presented_preview.is_none());
        assert!(controller.active_refinement.is_none());
    }
}
