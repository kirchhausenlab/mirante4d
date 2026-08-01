use mirante4d_render_wgpu::WgpuRenderRuntimeError;

use crate::{FrameFailureKind, ResidentRenderFailureStatus};

pub(crate) fn render_failure_status(error: &anyhow::Error) -> ResidentRenderFailureStatus {
    let kind = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WgpuRenderRuntimeError>())
        .map(frame_failure_kind_for_renderer_error)
        .unwrap_or(FrameFailureKind::InvalidModeParameter);
    ResidentRenderFailureStatus::new(kind, format!("{error:#}"))
}

pub(crate) fn frame_failure_kind_for_renderer_error(
    error: &WgpuRenderRuntimeError,
) -> FrameFailureKind {
    use WgpuRenderRuntimeError as Error;
    match error {
        Error::RequirementCapacityExceeded { .. }
        | Error::LeaseCapacityExceeded { .. }
        | Error::ResidencyEvictionEventCapacityExceeded { .. }
        | Error::ControlCapacityExceeded
        | Error::ResidentMetadataCapacityExceeded { .. }
        | Error::CapacityExceeded { .. }
        | Error::PayloadPlacementUnavailable { .. } => FrameFailureKind::BudgetExceeded,
        Error::SoftwareAdapter
        | Error::UnsupportedBackend
        | Error::AdapterLimitsInsufficient
        | Error::DeviceLimitsInsufficient
        | Error::PipelineCompilerSpawnFailed
        | Error::HiddenRefinementWorkerSpawnFailed
        | Error::HiddenRefinementIdentityExhausted
        | Error::PipelineCompilationFailed { .. }
        | Error::DeviceLost
        | Error::DeviceOutOfMemory
        | Error::BackendInternal
        | Error::ExtentExceeded
        | Error::PresentationCapacityExceeded { .. }
        | Error::PresentationNotRegistered
        | Error::PrivatePresentationIdExhausted
        | Error::TextureRevisionExhausted
        | Error::RendererDeviceGenerationExhausted
        | Error::CoordinateLimitExceeded => FrameFailureKind::BackendLimit,
        Error::UnsupportedView | Error::InvalidResourceGridCatalog => {
            FrameFailureKind::InvalidTransform
        }
        Error::BackendValidation
        | Error::UnknownValidationCapture
        | Error::StaleValidationCapture
        | Error::ValidationCaptureFailed
        | Error::UnknownGpuTiming
        | Error::GpuTimingFailed
        | Error::HiddenRefinementFailed
        | Error::PickCapacityExceeded
        | Error::PickTicketExhausted
        | Error::PickBackpressure
        | Error::UnknownVolumePick
        | Error::VolumePickFailed => FrameFailureKind::AllocationFailed,
        Error::PipelineNotReady { .. }
        | Error::PayloadRecoveryDeferred
        | Error::PickFrameUnavailable => FrameFailureKind::IncompleteResidency,
        Error::InvalidConfiguration
        | Error::FrameContractMismatch
        | Error::StaleFrame { .. }
        | Error::RequirementSetChanged
        | Error::InvalidVolumeColorSchedule { .. }
        | Error::InvalidCoordinatedPublicationGroup
        | Error::DuplicateCoordinatedTarget { .. }
        | Error::CoordinatedTargetNotConfigured { .. }
        | Error::CoordinatedTargetViewMismatch { .. }
        | Error::CoordinatedTargetExtentMismatch { .. }
        | Error::DuplicateLease
        | Error::UnexpectedLease
        | Error::PayloadContractMismatch
        | Error::PickQueryMismatch
        | Error::FrameProgressContract => FrameFailureKind::InvalidModeParameter,
    }
}

#[cfg(test)]
mod renderer_error_tests {
    use mirante4d_render_api::GpuLedgerCategory;
    use mirante4d_render_wgpu::{PipelineCapability, PipelineCompilationFailureCause};

    use super::*;

    #[test]
    fn renderer_capacity_and_adapter_failures_keep_typed_product_status() {
        let capacity = WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes: 2,
            available_bytes: 1,
        };
        assert_eq!(
            frame_failure_kind_for_renderer_error(&capacity),
            FrameFailureKind::BudgetExceeded
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(
                &WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded {
                    actual: 2,
                    maximum: 1,
                },
            ),
            FrameFailureKind::BudgetExceeded
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(&WgpuRenderRuntimeError::UnsupportedBackend),
            FrameFailureKind::BackendLimit
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(
                &WgpuRenderRuntimeError::PipelineCompilerSpawnFailed
            ),
            FrameFailureKind::BackendLimit
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(
                &WgpuRenderRuntimeError::PipelineCompilationFailed {
                    capability: PipelineCapability::InitialRender,
                    cause: PipelineCompilationFailureCause::Validation,
                },
            ),
            FrameFailureKind::BackendLimit
        );
        for error in [
            WgpuRenderRuntimeError::DeviceLost,
            WgpuRenderRuntimeError::DeviceOutOfMemory,
            WgpuRenderRuntimeError::BackendInternal,
        ] {
            assert_eq!(
                frame_failure_kind_for_renderer_error(&error),
                FrameFailureKind::BackendLimit
            );
        }
        assert_eq!(
            frame_failure_kind_for_renderer_error(&WgpuRenderRuntimeError::PickFrameUnavailable,),
            FrameFailureKind::IncompleteResidency
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(&WgpuRenderRuntimeError::PipelineNotReady {
                capability: PipelineCapability::Pick,
            }),
            FrameFailureKind::IncompleteResidency
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(&WgpuRenderRuntimeError::PickBackpressure),
            FrameFailureKind::AllocationFailed
        );
        assert_eq!(
            frame_failure_kind_for_renderer_error(&WgpuRenderRuntimeError::PickQueryMismatch),
            FrameFailureKind::InvalidModeParameter
        );
    }
}
