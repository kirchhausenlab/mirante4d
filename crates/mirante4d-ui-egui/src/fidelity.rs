//! Frame-fidelity presentation for the egui shell.

use eframe::egui;
use mirante4d_application::{
    DisplayedFrameFreshness, FrameCompleteness, FrameFailureKind, FrameFidelityStatus,
    IsoShadingPolicy, LodDecisionReason, RenderBackend, SamplingPolicy,
};

use crate::property_row;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedPanelFidelityStatus {
    pub ideal_scale_level: Option<u32>,
    pub selected_scale_level: Option<u32>,
    pub displayed_scale_level: Option<u32>,
    pub finest_fallback_scale_level: Option<u32>,
    pub fallback_scale_level: Option<u32>,
    pub target_available_requirements: u64,
    pub target_total_requirements: u64,
    pub mixed: bool,
    pub exact: bool,
    pub provisional: bool,
    pub refining: bool,
    pub display_current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Linked2dFidelityStatus {
    /// XY, XZ, and YZ, in that order.
    pub panels: [LinkedPanelFidelityStatus; 3],
}

pub(crate) fn show_frame_fidelity_property_rows(ui: &mut egui::Ui, fidelity: &FrameFidelityStatus) {
    property_row(ui, "3D scale", frame_fidelity_scale_label(fidelity));
    property_row(ui, "3D quality", frame_3d_presentation_label(fidelity));
    if fidelity.three_d_preview_candidate_count != 0 {
        property_row(ui, "3D navigation", frame_3d_navigation_label(fidelity));
    }
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

fn frame_3d_navigation_label(fidelity: &FrameFidelityStatus) -> String {
    let selection = if fidelity.three_d_preview {
        if fidelity.three_d_preview_is_finest_safe {
            "finest complete interaction-safe"
        } else if fidelity.three_d_preview_uses_emergency_floor {
            "complete emergency floor"
        } else {
            "complete preview"
        }
    } else {
        "exact display"
    };
    format!(
        "{selection} · {} / {} resident · {} interaction-safe",
        fidelity.three_d_preview_resident_candidate_count,
        fidelity.three_d_preview_candidate_count,
        fidelity.three_d_preview_safe_candidate_count,
    )
}

pub(crate) fn show_linked_2d_fidelity_property_rows(
    ui: &mut egui::Ui,
    fidelity: Linked2dFidelityStatus,
) {
    let [xy, xz, yz] = fidelity.panels;
    if xy == xz && xz == yz {
        property_row(ui, "linked 2D", linked_panel_fidelity_label(xy));
        return;
    }
    for (name, panel) in ["XY LOD", "XZ LOD", "YZ LOD"]
        .into_iter()
        .zip(fidelity.panels)
    {
        property_row(ui, name, linked_panel_fidelity_label(panel));
    }
}

pub fn linked_panel_fidelity_label(fidelity: LinkedPanelFidelityStatus) -> String {
    let scale =
        |level: Option<u32>| level.map_or_else(|| "none".to_owned(), |level| format!("s{level}"));
    let shown = if fidelity.mixed {
        format!(
            "mixed {}–{}",
            scale(fidelity.selected_scale_level),
            scale(fidelity.fallback_scale_level)
        )
    } else if fidelity.displayed_scale_level.is_none()
        && fidelity.target_total_requirements != 0
        && fidelity.target_available_requirements == 0
        && fidelity.finest_fallback_scale_level.is_some()
        && fidelity.fallback_scale_level.is_some()
    {
        format!(
            "fallback {}–{}",
            scale(fidelity.finest_fallback_scale_level),
            scale(fidelity.fallback_scale_level)
        )
    } else {
        scale(fidelity.displayed_scale_level)
    };
    let mut states = Vec::with_capacity(4);
    if fidelity.provisional {
        states.push("provisional");
    }
    if fidelity.exact {
        states.push("exact");
    } else if fidelity.refining {
        states.push("refining");
    } else if !fidelity.provisional {
        states.push("not exact");
    }
    if fidelity.exact
        && fidelity.selected_scale_level.is_some()
        && fidelity.selected_scale_level != fidelity.ideal_scale_level
    {
        states.push("adaptive");
    }
    let target_progress = (fidelity.target_total_requirements != 0
        && fidelity.target_available_requirements < fidelity.target_total_requirements)
        .then(|| {
            format!(
                "target {}/{}",
                fidelity.target_available_requirements, fidelity.target_total_requirements
            )
        });
    states.push(if fidelity.display_current {
        "display current"
    } else {
        "display stale"
    });
    let mut label = format!(
        "shown {} / selected {} / ideal {} · {}",
        shown,
        scale(fidelity.selected_scale_level),
        scale(fidelity.ideal_scale_level),
        states.join(", ")
    );
    if let Some(progress) = target_progress {
        label.push_str(" · ");
        label.push_str(&progress);
    }
    label
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
        frame_3d_presentation_label(fidelity),
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
        Some(displayed)
            if fidelity.adaptive_capacity_limited && displayed == fidelity.target_scale_level =>
        {
            format!(
                "shown s{} · adaptive / ideal s{}",
                displayed, fidelity.ideal_scale_level
            )
        }
        Some(displayed) if fidelity.adaptive_capacity_limited => {
            format!(
                "shown s{} / selected s{} · adaptive / ideal s{}",
                displayed, fidelity.target_scale_level, fidelity.ideal_scale_level
            )
        }
        Some(displayed) if fidelity.refinement_pending => {
            format!(
                "shown s{} · refining toward s{}",
                displayed, fidelity.target_scale_level
            )
        }
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
        LodDecisionReason::AdaptiveCapacity => "adaptive aggregate capacity",
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

fn frame_3d_presentation_label(fidelity: &FrameFidelityStatus) -> String {
    let internal = fidelity.three_d_render_viewport;
    let output = fidelity.viewport;
    let mut label = if fidelity.three_d_preview {
        debug_assert_eq!(
            internal, output,
            "native-resolution 3D previews must match the physical output"
        );
        format!(
            "native preview {}x{}",
            output.width_pixels(),
            output.height_pixels()
        )
    } else if fidelity.backend == RenderBackend::Loading {
        format!(
            "pending output {}x{}",
            output.width_pixels(),
            output.height_pixels()
        )
    } else if fidelity.backend == RenderBackend::Empty {
        "no visible data".to_owned()
    } else {
        format!(
            "direct {}x{}",
            internal.width_pixels(),
            internal.height_pixels()
        )
    };
    if fidelity.three_d_refinement_strips_total != 0 {
        label.push_str(&format!(
            " · exact rows {}/{}",
            fidelity.three_d_refinement_strips_completed, fidelity.three_d_refinement_strips_total
        ));
    }
    label
}

#[cfg(test)]
mod tests {
    use mirante4d_application::{PresentationViewport, RenderExtent};

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
            (
                LodDecisionReason::AdaptiveCapacity,
                "adaptive aggregate capacity",
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

    #[test]
    fn linked_panel_label_keeps_shown_selected_ideal_and_state_separate() {
        assert_eq!(
            linked_panel_fidelity_label(LinkedPanelFidelityStatus {
                ideal_scale_level: Some(0),
                selected_scale_level: Some(1),
                displayed_scale_level: Some(3),
                finest_fallback_scale_level: Some(3),
                fallback_scale_level: Some(3),
                target_available_requirements: 0,
                target_total_requirements: 0,
                mixed: false,
                exact: false,
                provisional: true,
                refining: true,
                display_current: true,
            }),
            "shown s3 / selected s1 / ideal s0 · provisional, refining, display current"
        );
        assert_eq!(
            linked_panel_fidelity_label(LinkedPanelFidelityStatus {
                ideal_scale_level: Some(0),
                selected_scale_level: Some(1),
                displayed_scale_level: Some(1),
                finest_fallback_scale_level: Some(2),
                fallback_scale_level: Some(3),
                target_available_requirements: 4,
                target_total_requirements: 4,
                mixed: false,
                exact: true,
                provisional: false,
                refining: false,
                display_current: true,
            }),
            "shown s1 / selected s1 / ideal s0 · exact, adaptive, display current"
        );
        assert_eq!(
            linked_panel_fidelity_label(LinkedPanelFidelityStatus {
                ideal_scale_level: Some(0),
                selected_scale_level: Some(0),
                displayed_scale_level: None,
                finest_fallback_scale_level: Some(1),
                fallback_scale_level: Some(3),
                target_available_requirements: 7,
                target_total_requirements: 20,
                mixed: true,
                exact: false,
                provisional: true,
                refining: true,
                display_current: true,
            }),
            "shown mixed s0–s3 / selected s0 / ideal s0 · provisional, refining, display current · target 7/20"
        );
        assert_eq!(
            linked_panel_fidelity_label(LinkedPanelFidelityStatus {
                ideal_scale_level: Some(0),
                selected_scale_level: Some(0),
                displayed_scale_level: None,
                finest_fallback_scale_level: Some(1),
                fallback_scale_level: Some(3),
                target_available_requirements: 0,
                target_total_requirements: 20,
                mixed: false,
                exact: false,
                provisional: true,
                refining: true,
                display_current: true,
            }),
            "shown fallback s1–s3 / selected s0 / ideal s0 · provisional, refining, display current · target 0/20"
        );
    }

    #[test]
    fn scale_label_distinguishes_displayed_selected_ideal_and_pending() {
        let mut fidelity = FrameFidelityStatus::new_with_presentation(
            RenderExtent::new(640, 480).unwrap(),
            PresentationViewport::new(640.0, 480.0).unwrap(),
        );
        fidelity.displayed_scale_level = Some(3);
        fidelity.target_scale_level = 3;
        fidelity.ideal_scale_level = 2;
        fidelity.adaptive_capacity_limited = true;
        assert_eq!(
            frame_fidelity_scale_label(&fidelity),
            "shown s3 · adaptive / ideal s2"
        );

        fidelity.displayed_scale_level = Some(4);
        assert_eq!(
            frame_fidelity_scale_label(&fidelity),
            "shown s4 / selected s3 · adaptive / ideal s2"
        );

        fidelity.adaptive_capacity_limited = false;
        fidelity.refinement_pending = true;
        fidelity.target_scale_level = 2;
        assert_eq!(
            frame_fidelity_scale_label(&fidelity),
            "shown s4 · refining toward s2"
        );
    }

    #[test]
    fn volume_presentation_label_retains_native_preview_during_hidden_rows() {
        let mut fidelity = FrameFidelityStatus::new_with_presentation(
            RenderExtent::new(900, 500).unwrap(),
            PresentationViewport::new(900.0, 500.0).unwrap(),
        );
        fidelity.backend = RenderBackend::GpuCameraMip;
        assert_eq!(frame_3d_presentation_label(&fidelity), "direct 900x500");

        fidelity.three_d_preview = true;
        fidelity.three_d_render_viewport = RenderExtent::new(900, 500).unwrap();
        fidelity.three_d_refinement_strips_completed = 4;
        fidelity.three_d_refinement_strips_total = 12;
        assert_eq!(
            frame_3d_presentation_label(&fidelity),
            "native preview 900x500 · exact rows 4/12"
        );

        fidelity.backend = RenderBackend::Loading;
        assert_eq!(
            frame_3d_presentation_label(&fidelity),
            "native preview 900x500 · exact rows 4/12",
            "backend loading must not replace the retained visible preview"
        );
    }

    #[test]
    fn navigation_label_names_finest_safe_and_emergency_selection_truthfully() {
        let mut fidelity = FrameFidelityStatus::new_with_presentation(
            RenderExtent::new(900, 500).unwrap(),
            PresentationViewport::new(900.0, 500.0).unwrap(),
        );
        fidelity.three_d_preview = true;
        fidelity.three_d_preview_candidate_count = 5;
        fidelity.three_d_preview_resident_candidate_count = 4;
        fidelity.three_d_preview_safe_candidate_count = 3;
        fidelity.three_d_preview_is_finest_safe = true;
        assert_eq!(
            frame_3d_navigation_label(&fidelity),
            "finest complete interaction-safe · 4 / 5 resident · 3 interaction-safe"
        );

        fidelity.three_d_preview_is_finest_safe = false;
        fidelity.three_d_preview_uses_emergency_floor = true;
        assert_eq!(
            frame_3d_navigation_label(&fidelity),
            "complete emergency floor · 4 / 5 resident · 3 interaction-safe"
        );
    }
}
