use mirante4d_renderer::{RenderError, gpu::GpuRenderError};
use serde_json::{Value, json};

pub(crate) fn phase13_failure_policy_probe_report() -> Value {
    let cases = [
        phase13_failure_policy_gpu_case_json(
            "budget_exceeded_capacity",
            GpuRenderError::BudgetExceeded {
                resource: "brick atlas packed uint16 values",
                required_bytes: 32,
                budget_bytes: 1,
            },
        ),
        phase13_failure_policy_gpu_case_json(
            "backend_limit_buffer_too_large",
            GpuRenderError::BufferTooLarge {
                resource: "brick atlas packed uint16 values",
                required_bytes: u64::from(u32::MAX) + 1,
                limit_bytes: u64::from(u32::MAX),
            },
        ),
        phase13_failure_policy_gpu_case_json(
            "backend_limit_buffer_size_overflow",
            GpuRenderError::BufferSizeOverflow {
                resource: "output pixels",
            },
        ),
        phase13_failure_policy_gpu_case_json(
            "allocation_failed_readback",
            GpuRenderError::MapFailed("synthetic map failure".to_owned()),
        ),
        phase13_failure_policy_gpu_case_json(
            "invalid_mode_parameter",
            GpuRenderError::UnsupportedCameraMode("synthetic mode"),
        ),
        phase13_failure_policy_render_case_json(
            "budget_exceeded_resource_plan",
            RenderError::ResourcePlanCapacityExceeded {
                kind: mirante4d_renderer::ResourcePlanCapacityKind::Resources,
                maximum: 1,
            },
        ),
        phase13_failure_policy_render_case_json(
            "backend_limit_dimension_too_large",
            RenderError::DimensionTooLarge {
                axis: "x",
                value: u64::from(u32::MAX) + 1,
            },
        ),
        phase13_failure_policy_render_case_json(
            "invalid_transform_empty_volume",
            RenderError::EmptyVolume,
        ),
        phase13_failure_policy_render_case_json(
            "invalid_mode_parameter_viewport",
            RenderError::InvalidViewport {
                width: 0,
                height: 1,
            },
        ),
    ];
    let visible_count = cases
        .iter()
        .filter(|case| case["user_visible"].as_bool().unwrap_or(false))
        .count();
    let downgrade_count = cases
        .iter()
        .filter(|case| case["valid_lod_downgrade"].as_bool().unwrap_or(false))
        .count();
    let backend_limit_count = cases
        .iter()
        .filter(|case| case["error_kind"].as_str() == Some("backend_limit"))
        .count();

    json!({
        "ok": visible_count == cases.len() && backend_limit_count >= 3,
        "purpose": "phase13_failure_taxonomy_and_visible_downgrade_policy_probe",
        "policy": "renderer failures must remain typed and visible; only budget/backend/allocation failures are eligible for coarser-LOD retry",
        "summary": {
            "cases": cases.len(),
            "user_visible_cases": visible_count,
            "valid_lod_downgrade_cases": downgrade_count,
            "backend_limit_cases": backend_limit_count,
        },
        "cases": cases,
    })
}

fn phase13_failure_policy_gpu_case_json(label: &'static str, err: GpuRenderError) -> Value {
    let error_kind = phase13_gpu_error_kind(&err);
    phase13_failure_policy_case_json(label, "gpu", error_kind, err.to_string())
}

fn phase13_failure_policy_render_case_json(label: &'static str, err: RenderError) -> Value {
    let error_kind = phase13_render_error_kind(&err);
    phase13_failure_policy_case_json(label, "render", error_kind, err.to_string())
}

fn phase13_failure_policy_case_json(
    label: &'static str,
    source: &'static str,
    error_kind: &'static str,
    error: String,
) -> Value {
    let valid_lod_downgrade = phase13_failure_kind_allows_lod_downgrade(error_kind);
    json!({
        "label": label,
        "source": source,
        "error_kind": error_kind,
        "error": error,
        "user_visible": true,
        "valid_lod_downgrade": valid_lod_downgrade,
        "hidden_dense_fallback_allowed": false,
    })
}

fn phase13_failure_kind_allows_lod_downgrade(error_kind: &str) -> bool {
    matches!(
        error_kind,
        "budget_exceeded" | "backend_limit" | "allocation_failed"
    )
}

pub(crate) fn phase13_gpu_error_kind(err: &GpuRenderError) -> &'static str {
    match err {
        GpuRenderError::Render(render) => phase13_render_error_kind(render),
        GpuRenderError::CpuLedger(mirante4d_dataset::CpuLedgerError::CapacityExceeded {
            ..
        }) => "budget_exceeded",
        GpuRenderError::CpuLedger(
            mirante4d_dataset::CpuLedgerError::ZeroByteReservation
            | mirante4d_dataset::CpuLedgerError::ShuttingDown,
        ) => "allocation_failed",
        GpuRenderError::AdapterUnavailable(_)
        | GpuRenderError::CpuAdapterOnly(_)
        | GpuRenderError::RequestDevice(_)
        | GpuRenderError::BufferTooLarge { .. }
        | GpuRenderError::BufferSizeOverflow { .. }
        | GpuRenderError::RequiredLimitUnsupported { .. }
        | GpuRenderError::DeviceLimitTooLow { .. } => "backend_limit",
        GpuRenderError::UnsupportedCameraMode(_) => "invalid_mode_parameter",
        GpuRenderError::BudgetExceeded { .. } => "budget_exceeded",
        GpuRenderError::MapFailed(_)
        | GpuRenderError::PollFailed(_)
        | GpuRenderError::ReadbackChannelClosed
        | GpuRenderError::CachePoisoned => "allocation_failed",
    }
}

pub(crate) fn phase13_render_error_kind(err: &RenderError) -> &'static str {
    match err {
        RenderError::ResourcePlanCapacityExceeded { .. } => "budget_exceeded",
        RenderError::InvalidViewport { .. }
        | RenderError::InvalidReadoutPixel { .. }
        | RenderError::InvalidBrickPixelStride
        | RenderError::InvalidBrickAtlas(_)
        | RenderError::InvalidRgbaImageBuffer { .. }
        | RenderError::InvalidIntensityFrameBuffer { .. }
        | RenderError::InvalidPixelCoverageBuffer { .. }
        | RenderError::InvalidPixelCoverageValue { .. }
        | RenderError::InvalidPixelLightingBuffer { .. }
        | RenderError::InvalidIsoSurfaceFrameBuffer { .. }
        | RenderError::InvalidIsoSurfaceFrameDimensions { .. }
        | RenderError::InvalidDvrRgbaFrameBuffer { .. }
        | RenderError::InvalidDvrRgbaFrameDimensions { .. }
        | RenderError::InvalidDvrChannelSet(_)
        | RenderError::InvalidChannelComposite(_)
        | RenderError::InvalidIntensitySummaryRegion(_)
        | RenderError::ResourceContract(_)
        | RenderError::ResourceIdentityMismatch(_)
        | RenderError::InvalidResourceId { .. } => "invalid_mode_parameter",
        RenderError::DimensionTooLarge { .. } => "backend_limit",
        RenderError::EmptyVolume
        | RenderError::Shape(_)
        | RenderError::Space(_)
        | RenderError::Camera(_) => "invalid_transform",
    }
}
