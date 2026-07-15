//! Frame-fidelity presentation for the egui shell.

use eframe::egui;
use mirante4d_application::{
    DisplayedFrameFreshness, FrameCompleteness, FrameFailureKind, FrameFidelityStatus,
    IsoShadingPolicy, LodDecisionReason, RenderBackend, SamplingPolicy,
};

use crate::property_row;

pub(crate) fn show_frame_fidelity_property_rows(ui: &mut egui::Ui, fidelity: &FrameFidelityStatus) {
    property_row(ui, "scale", frame_fidelity_scale_label(fidelity));
    property_row(ui, "state", frame_completeness_label(fidelity.completeness));
    property_row(ui, "reason", frame_reason_label(fidelity.reason));
    if let Some(kind) = fidelity.last_failure_kind {
        property_row(ui, "failure", frame_failure_kind_label(kind));
    }
    property_row(ui, "backend", render_backend_label(fidelity.backend));
    property_row(ui, "viewport", frame_viewport_label(fidelity));
    if let Some(display_label) = frame_display_freshness_label(fidelity) {
        property_row(ui, "display", display_label);
    }
    property_row(ui, "render", frame_render_time_label(fidelity));
}

pub fn frame_fidelity_label(fidelity: &FrameFidelityStatus) -> String {
    let reason = frame_reason_label(fidelity.reason);
    let reason_suffix = if fidelity.displayed_scale_level == Some(0)
        && fidelity.target_scale_level == 0
        && fidelity.completeness == FrameCompleteness::Exact
        && fidelity.reason == LodDecisionReason::ExactS0
    {
        String::new()
    } else {
        format!(" ({reason})")
    };
    let mut parts = vec![
        format!(
            "{} {}{}",
            frame_fidelity_scale_label(fidelity),
            frame_completeness_label(fidelity.completeness),
            reason_suffix
        ),
        render_backend_label(fidelity.backend).to_owned(),
        frame_viewport_label(fidelity),
    ];
    if let Some(display_label) = frame_display_freshness_label(fidelity) {
        parts.push(display_label.to_owned());
    }
    parts.push(frame_render_time_label(fidelity));
    parts.join(" | ")
}

pub fn render_sampling_policy_label(policy: SamplingPolicy) -> &'static str {
    match policy {
        SamplingPolicy::SmoothLinear => "Smooth linear",
        SamplingPolicy::VoxelExact => "Voxel exact",
    }
}

pub fn iso_shading_policy_label(policy: IsoShadingPolicy) -> &'static str {
    match policy {
        IsoShadingPolicy::GradientLighting => "Gradient lighting",
        IsoShadingPolicy::Flat => "Flat threshold hit",
    }
}

fn frame_display_freshness_label(fidelity: &FrameFidelityStatus) -> Option<&'static str> {
    match fidelity.display_freshness {
        DisplayedFrameFreshness::Unknown => None,
        DisplayedFrameFreshness::Current => Some("display current"),
        DisplayedFrameFreshness::Stale => Some("display stale"),
    }
}

fn frame_render_time_label(fidelity: &FrameFidelityStatus) -> String {
    fidelity
        .frame_time_ms
        .filter(|ms| *ms > 0.0)
        .map(|ms| format!("render {ms:.1} ms"))
        .unwrap_or_else(|| "render pending".to_owned())
}

fn frame_fidelity_scale_label(fidelity: &FrameFidelityStatus) -> String {
    match fidelity.displayed_scale_level {
        Some(displayed) if displayed == fidelity.target_scale_level => {
            format!("shown s{displayed}")
        }
        Some(displayed) => {
            format!(
                "shown s{} / target s{}",
                displayed, fidelity.target_scale_level
            )
        }
        None => format!("shown none / target s{}", fidelity.target_scale_level),
    }
}

fn frame_completeness_label(completeness: FrameCompleteness) -> &'static str {
    match completeness {
        FrameCompleteness::Exact => "exact",
        FrameCompleteness::Complete => "complete",
        FrameCompleteness::Loading => "loading",
        FrameCompleteness::Incomplete => "incomplete",
        FrameCompleteness::BudgetLimited => "budget-limited",
    }
}

fn frame_reason_label(reason: LodDecisionReason) -> &'static str {
    match reason {
        LodDecisionReason::ExactS0 => "exact s0",
        LodDecisionReason::ScreenEquivalentCoarserScale => "screen-equivalent LOD",
        LodDecisionReason::PlaybackDownshift => "playback LOD",
        LodDecisionReason::LoadingTargetScale => "loading target LOD",
        LodDecisionReason::NoVisibleData => "outside selected data",
        LodDecisionReason::FrameBudgetLimited => "frame budget",
        LodDecisionReason::GpuBudgetLimited => "GPU budget",
        LodDecisionReason::CpuBudgetLimited => "CPU budget",
        LodDecisionReason::BackendLimit => "backend limit",
        LodDecisionReason::AllocationFailed => "allocation failed",
        LodDecisionReason::IncompleteResidency => "incomplete residency",
        LodDecisionReason::InvalidModeParameter => "invalid mode parameter",
        LodDecisionReason::UnsupportedDtype => "unsupported dtype",
        LodDecisionReason::InvalidTransform => "invalid transform",
    }
}

fn frame_failure_kind_label(kind: FrameFailureKind) -> &'static str {
    match kind {
        FrameFailureKind::BudgetExceeded => "budget exceeded",
        FrameFailureKind::BackendLimit => "backend limit",
        FrameFailureKind::AllocationFailed => "allocation failed",
        FrameFailureKind::IncompleteResidency => "incomplete residency",
        FrameFailureKind::InvalidModeParameter => "invalid mode parameter",
        FrameFailureKind::UnsupportedDtype => "unsupported dtype",
        FrameFailureKind::InvalidTransform => "invalid transform",
    }
}

fn render_backend_label(backend: RenderBackend) -> &'static str {
    match backend {
        RenderBackend::Loading => "loading",
        RenderBackend::Empty => "empty",
        RenderBackend::GpuCameraMip => "GPU MIP",
        RenderBackend::GpuCameraIso => "GPU ISO",
        RenderBackend::GpuCameraDvr => "GPU DVR",
    }
}

fn frame_viewport_label(fidelity: &FrameFidelityStatus) -> String {
    let render = format!(
        "{}x{} px",
        fidelity.viewport.width_pixels(),
        fidelity.viewport.height_pixels()
    );
    let presentation = format!(
        "{:.0}x{:.0} pt",
        fidelity.presentation_viewport.width_points(),
        fidelity.presentation_viewport.height_points()
    );
    if (fidelity.presentation_viewport.width_points() - f64::from(fidelity.viewport.width_pixels()))
        .abs()
        < 0.5
        && (fidelity.presentation_viewport.height_points()
            - f64::from(fidelity.viewport.height_pixels()))
        .abs()
            < 0.5
    {
        render
    } else {
        format!("{render}; {presentation}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_fidelity_labels_cover_status_reason_and_failure_vocabularies() {
        for (value, expected) in [
            (FrameCompleteness::Exact, "exact"),
            (FrameCompleteness::Complete, "complete"),
            (FrameCompleteness::Loading, "loading"),
            (FrameCompleteness::Incomplete, "incomplete"),
            (FrameCompleteness::BudgetLimited, "budget-limited"),
        ] {
            assert_eq!(frame_completeness_label(value), expected);
        }
        for (value, expected) in [
            (LodDecisionReason::ExactS0, "exact s0"),
            (
                LodDecisionReason::ScreenEquivalentCoarserScale,
                "screen-equivalent LOD",
            ),
            (LodDecisionReason::PlaybackDownshift, "playback LOD"),
            (LodDecisionReason::LoadingTargetScale, "loading target LOD"),
            (LodDecisionReason::NoVisibleData, "outside selected data"),
            (LodDecisionReason::FrameBudgetLimited, "frame budget"),
            (LodDecisionReason::GpuBudgetLimited, "GPU budget"),
            (LodDecisionReason::CpuBudgetLimited, "CPU budget"),
            (LodDecisionReason::BackendLimit, "backend limit"),
            (LodDecisionReason::AllocationFailed, "allocation failed"),
            (
                LodDecisionReason::IncompleteResidency,
                "incomplete residency",
            ),
            (
                LodDecisionReason::InvalidModeParameter,
                "invalid mode parameter",
            ),
            (LodDecisionReason::UnsupportedDtype, "unsupported dtype"),
            (LodDecisionReason::InvalidTransform, "invalid transform"),
        ] {
            assert_eq!(frame_reason_label(value), expected);
        }
        for (value, expected) in [
            (FrameFailureKind::BudgetExceeded, "budget exceeded"),
            (FrameFailureKind::BackendLimit, "backend limit"),
            (FrameFailureKind::AllocationFailed, "allocation failed"),
            (
                FrameFailureKind::IncompleteResidency,
                "incomplete residency",
            ),
            (
                FrameFailureKind::InvalidModeParameter,
                "invalid mode parameter",
            ),
            (FrameFailureKind::UnsupportedDtype, "unsupported dtype"),
            (FrameFailureKind::InvalidTransform, "invalid transform"),
        ] {
            assert_eq!(frame_failure_kind_label(value), expected);
        }
        assert_eq!(render_backend_label(RenderBackend::Empty), "empty");
    }
}
