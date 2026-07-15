use mirante4d_render_api::RenderExtent;
use mirante4d_render_wgpu::WgpuRenderRuntimeError;

use crate::{FrameFailureKind, RenderCoordinationState, ResidentRenderFailureStatus};

pub(crate) fn set_render_viewport(
    render: &mut RenderCoordinationState,
    viewport: RenderExtent,
) -> bool {
    render.set_render_viewport(viewport)
}

pub(crate) fn render_failure_status(error: &anyhow::Error) -> ResidentRenderFailureStatus {
    let kind = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WgpuRenderRuntimeError>())
        .map(frame_failure_kind_for_successor_error)
        .unwrap_or(FrameFailureKind::InvalidModeParameter);
    ResidentRenderFailureStatus::new(kind, format!("{error:#}"))
}

pub(crate) fn frame_failure_kind_for_successor_error(
    error: &WgpuRenderRuntimeError,
) -> FrameFailureKind {
    use WgpuRenderRuntimeError as Error;
    match error {
        Error::RequirementCapacityExceeded { .. }
        | Error::LeaseCapacityExceeded { .. }
        | Error::ControlCapacityExceeded
        | Error::CapacityExceeded { .. } => FrameFailureKind::BudgetExceeded,
        Error::DeviceUnavailable
        | Error::SoftwareAdapter
        | Error::UnsupportedBackend
        | Error::AdapterLimitsInsufficient
        | Error::DeviceLimitsInsufficient
        | Error::DeviceCreationFailed
        | Error::ExtentExceeded
        | Error::PresentationCapacityExceeded { .. }
        | Error::PresentationNotRegistered { .. }
        | Error::PresentationTokenExhausted
        | Error::CoordinateLimitExceeded
        | Error::RaySampleLimitExceeded => FrameFailureKind::BackendLimit,
        Error::UnsupportedView => FrameFailureKind::InvalidTransform,
        Error::BackendValidation
        | Error::UnknownValidationCapture
        | Error::StaleValidationCapture
        | Error::ValidationCaptureFailed => FrameFailureKind::AllocationFailed,
        Error::InvalidConfiguration
        | Error::FrameContractMismatch
        | Error::StaleFrame { .. }
        | Error::RequirementSetChanged
        | Error::MixedScaleRequirements
        | Error::OverlappingResources
        | Error::DuplicateLease
        | Error::UnexpectedLease
        | Error::PayloadContractMismatch
        | Error::UnsupportedSampling
        | Error::UnsupportedIsoShading
        | Error::FrameProgressContract => FrameFailureKind::InvalidModeParameter,
    }
}

#[cfg(test)]
mod successor_error_tests {
    use mirante4d_render_api::GpuLedgerCategory;

    use super::*;

    #[test]
    fn successor_capacity_and_adapter_failures_keep_typed_product_status() {
        let capacity = WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes: 2,
            available_bytes: 1,
        };
        assert_eq!(
            frame_failure_kind_for_successor_error(&capacity),
            FrameFailureKind::BudgetExceeded
        );
        assert_eq!(
            frame_failure_kind_for_successor_error(&WgpuRenderRuntimeError::UnsupportedBackend),
            FrameFailureKind::BackendLimit
        );
    }
}
