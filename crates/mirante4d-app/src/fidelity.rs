use crate::{
    ChannelFidelityStatus, ChannelFidelityWarning, FrameCompleteness, FrameFailureKind,
    FrameFidelityStatus, LodDecisionReason, RenderBackend, application_view,
    current_runtime::render::CurrentRenderRuntime, display_graph::DisplayGraph,
    state::DisplayedFrameFreshness, ui_kit,
};
use eframe::egui;
use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::{IsoShadingPolicy, RenderMode, SamplingPolicy};

pub(crate) fn visible_channel_fidelity_is_mixed(channels: &[ChannelFidelityStatus]) -> bool {
    let mut visible = channels.iter().filter(|channel| channel.visible);
    let Some(first) = visible.next() else {
        return false;
    };
    visible.any(|channel| {
        channel.displayed_scale_level != first.displayed_scale_level
            || channel.target_scale_level != first.target_scale_level
            || channel.completeness != first.completeness
            || channel.reason != first.reason
    })
}

pub(crate) fn show_frame_fidelity_property_rows(ui: &mut egui::Ui, fidelity: &FrameFidelityStatus) {
    ui_kit::property_row(ui, "scale", frame_fidelity_scale_label(fidelity));
    ui_kit::property_row(ui, "state", frame_completeness_label(fidelity.completeness));
    ui_kit::property_row(ui, "reason", frame_reason_label(fidelity.reason));
    if let Some(kind) = fidelity.last_failure_kind {
        ui_kit::property_row(ui, "failure", frame_failure_kind_label(kind));
    }
    ui_kit::property_row(ui, "backend", render_backend_label(fidelity.backend));
    ui_kit::property_row(ui, "viewport", frame_viewport_label(fidelity));
    if let Some(display_label) = frame_display_freshness_label(fidelity) {
        ui_kit::property_row(ui, "display", display_label);
    }
    ui_kit::property_row(ui, "render", frame_render_time_label(fidelity));
}

pub(crate) fn frame_fidelity_label(fidelity: &FrameFidelityStatus) -> String {
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

pub(crate) fn frame_display_freshness_label(
    fidelity: &FrameFidelityStatus,
) -> Option<&'static str> {
    match fidelity.display_freshness {
        DisplayedFrameFreshness::Unknown => None,
        DisplayedFrameFreshness::Current => Some("display current"),
        DisplayedFrameFreshness::Stale => Some("display stale"),
    }
}

pub(crate) fn frame_render_time_label(fidelity: &FrameFidelityStatus) -> String {
    fidelity
        .frame_time_ms
        .filter(|ms| *ms > 0.0)
        .map(|ms| format!("render {ms:.1} ms"))
        .unwrap_or_else(|| "render pending".to_owned())
}

pub(crate) fn composite_fidelity_label(
    snapshot: &ApplicationSnapshot,
    render: &CurrentRenderRuntime,
) -> String {
    let mut label = frame_fidelity_label(&render.frame_fidelity);
    label.push_str(" | ");
    let display_graph = DisplayGraph::from_snapshot(snapshot);
    if display_graph.is_mixed_mode() {
        label.push_str("mixed render modes");
    } else {
        let view = application_view(snapshot);
        let sampling = view
            .layer(view.active_layer())
            .expect("application view contains its active layer")
            .render_state()
            .sampling_policy();
        label.push_str(render_sampling_policy_label(sampling));
    }
    if visible_channel_fidelity_is_mixed(&render.channel_fidelity) {
        label.push_str(" | mixed channel fidelity");
    }
    label
}

pub(crate) fn channel_fidelity_label(channel: &ChannelFidelityStatus) -> String {
    if !channel.visible {
        return "hidden, no current-frame work".to_owned();
    }
    let mode = match channel.render_mode {
        RenderMode::Mip => "MIP",
        RenderMode::Isosurface => "ISO",
        RenderMode::Dvr => "DVR",
    };
    let scale = match channel.displayed_scale_level {
        Some(displayed) if displayed == channel.target_scale_level => {
            format!("shown s{displayed}")
        }
        Some(displayed) => {
            format!(
                "shown s{} / target s{}",
                displayed, channel.target_scale_level
            )
        }
        None => format!("shown none / target s{}", channel.target_scale_level),
    };
    let warning = match channel.warning {
        Some(ChannelFidelityWarning::MixedFidelity) => ", mixed fidelity",
        Some(ChannelFidelityWarning::Incomplete) => ", incomplete",
        Some(ChannelFidelityWarning::Hidden) | None => "",
    };
    format!(
        "{} | {} {}, {} resident / {} visible{}",
        mode,
        scale,
        frame_completeness_label(channel.completeness),
        channel.resident_bricks,
        channel.visible_bricks,
        warning
    )
}

pub(crate) fn frame_fidelity_scale_label(fidelity: &FrameFidelityStatus) -> String {
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

pub(crate) fn frame_completeness_label(completeness: FrameCompleteness) -> &'static str {
    match completeness {
        FrameCompleteness::Exact => "exact",
        FrameCompleteness::Complete => "complete",
        FrameCompleteness::Loading => "loading",
        FrameCompleteness::Incomplete => "incomplete",
        FrameCompleteness::BudgetLimited => "budget-limited",
    }
}

pub(crate) fn frame_reason_label(reason: LodDecisionReason) -> &'static str {
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

pub(crate) fn frame_failure_kind_label(kind: FrameFailureKind) -> &'static str {
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

pub(crate) fn render_backend_label(backend: RenderBackend) -> &'static str {
    match backend {
        RenderBackend::Loading => "loading",
        RenderBackend::Empty => "empty",
        RenderBackend::GpuCameraMip => "GPU MIP",
        RenderBackend::GpuCameraIso => "GPU ISO",
        RenderBackend::GpuCameraDvr => "GPU DVR",
    }
}

pub(crate) fn frame_viewport_label(fidelity: &FrameFidelityStatus) -> String {
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

pub(crate) fn render_sampling_policy_label(policy: SamplingPolicy) -> &'static str {
    match policy {
        SamplingPolicy::SmoothLinear => "Smooth linear",
        SamplingPolicy::VoxelExact => "Voxel exact",
    }
}

pub(crate) fn iso_shading_policy_label(policy: IsoShadingPolicy) -> &'static str {
    match policy {
        IsoShadingPolicy::GradientLighting => "Gradient lighting",
        IsoShadingPolicy::Flat => "Flat threshold hit",
    }
}
