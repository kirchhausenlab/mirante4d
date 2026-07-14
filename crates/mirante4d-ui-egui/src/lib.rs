//! Egui presentation components for Mirante4D.

#![forbid(unsafe_code)]

use std::{fmt::Display, hash::Hash};

use eframe::egui::{self, Color32, RichText};
use mirante4d_application::{ApplicationEvent, OperationOutcome};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiColors {
    pub app_background: Color32,
    pub panel_background: Color32,
    pub panel_background_alt: Color32,
    pub viewport_background: Color32,
    pub border: Color32,
    pub text_primary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub status_ready: Color32,
    pub status_warning: Color32,
    pub status_error: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiSpacing {
    pub panel_margin: f32,
    pub section_gap: f32,
    pub row_gap: f32,
    pub property_label_width: f32,
    pub control_height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UiTokens {
    pub colors: UiColors,
    pub spacing: UiSpacing,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkbenchLayoutSpec {
    pub top_bar_height: f32,
    pub bottom_strip_height: f32,
    pub left_panel_width: f32,
    pub right_panel_width: f32,
    pub min_left_panel_width: f32,
    pub max_left_panel_width: f32,
    pub min_right_panel_width: f32,
    pub max_right_panel_width: f32,
    pub min_viewport_width: f32,
    pub min_viewport_height: f32,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkbenchLayoutReport {
    pub window_size_points: egui::Vec2,
    pub pixels_per_point: f32,
    pub top_bar_height: f32,
    pub bottom_strip_height: f32,
    pub left_panel_width: f32,
    pub right_panel_width: f32,
    pub viewport_size_points: egui::Vec2,
    pub viewport_size_pixels: egui::Vec2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Ready,
    Warning,
    Error,
}

impl Default for UiColors {
    fn default() -> Self {
        Self {
            app_background: Color32::from_rgb(16, 18, 21),
            panel_background: Color32::from_rgb(24, 26, 29),
            panel_background_alt: Color32::from_rgb(28, 31, 35),
            viewport_background: Color32::from_rgb(8, 10, 13),
            border: Color32::from_rgb(58, 64, 72),
            text_primary: Color32::from_rgb(232, 236, 241),
            text_muted: Color32::from_rgb(156, 164, 174),
            accent: Color32::from_rgb(92, 177, 188),
            status_ready: Color32::from_rgb(93, 179, 128),
            status_warning: Color32::from_rgb(222, 170, 76),
            status_error: Color32::from_rgb(224, 102, 102),
        }
    }
}

impl Default for UiSpacing {
    fn default() -> Self {
        Self {
            panel_margin: 8.0,
            section_gap: 10.0,
            row_gap: 4.0,
            property_label_width: 96.0,
            control_height: 24.0,
        }
    }
}

impl Default for WorkbenchLayoutSpec {
    fn default() -> Self {
        Self {
            top_bar_height: 34.0,
            bottom_strip_height: 0.0,
            left_panel_width: 260.0,
            right_panel_width: 340.0,
            min_left_panel_width: 220.0,
            max_left_panel_width: 360.0,
            min_right_panel_width: 300.0,
            max_right_panel_width: 420.0,
            min_viewport_width: 320.0,
            min_viewport_height: 240.0,
        }
    }
}

impl WorkbenchLayoutSpec {
    #[cfg(test)]
    pub fn report(
        self,
        window_size_points: egui::Vec2,
        pixels_per_point: f32,
    ) -> Option<WorkbenchLayoutReport> {
        if window_size_points.x <= 0.0
            || window_size_points.y <= 0.0
            || pixels_per_point <= 0.0
            || !window_size_points.x.is_finite()
            || !window_size_points.y.is_finite()
            || !pixels_per_point.is_finite()
        {
            return None;
        }

        let viewport_width = window_size_points.x - self.left_panel_width - self.right_panel_width;
        let viewport_height = window_size_points.y - self.top_bar_height - self.bottom_strip_height;
        if viewport_width < self.min_viewport_width || viewport_height < self.min_viewport_height {
            return None;
        }

        let viewport_size_points = egui::vec2(viewport_width, viewport_height);
        Some(WorkbenchLayoutReport {
            window_size_points,
            pixels_per_point,
            top_bar_height: self.top_bar_height,
            bottom_strip_height: self.bottom_strip_height,
            left_panel_width: self.left_panel_width,
            right_panel_width: self.right_panel_width,
            viewport_size_points,
            viewport_size_pixels: viewport_size_points * pixels_per_point,
        })
    }

    pub fn left_width_range(self) -> std::ops::RangeInclusive<f32> {
        self.min_left_panel_width..=self.max_left_panel_width
    }

    pub fn right_width_range(self) -> std::ops::RangeInclusive<f32> {
        self.min_right_panel_width..=self.max_right_panel_width
    }
}

pub fn configure_visuals(ctx: &egui::Context) {
    let tokens = UiTokens::default();
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = tokens.colors.panel_background;
    visuals.window_fill = tokens.colors.panel_background_alt;
    visuals.extreme_bg_color = tokens.colors.app_background;
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(38, 41, 46);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(48, 52, 58);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(60, 65, 73);
    visuals.widgets.active.bg_fill = Color32::from_rgb(68, 75, 84);
    visuals.selection.bg_fill = tokens.colors.accent.linear_multiply(0.55);
    visuals.selection.stroke.color = tokens.colors.accent;
    ctx.set_visuals(visuals);
}

pub fn application_problem_message(event: Option<&ApplicationEvent>) -> Option<String> {
    match event? {
        ApplicationEvent::OperationCompleted {
            token,
            outcome: OperationOutcome::Failed(code),
        } => Some(format!(
            "{:?} failed ({code:?}); correct the input, permissions, or resource limit and retry",
            token.kind()
        )),
        ApplicationEvent::ResourcePolicyRejected { reason, .. } => Some(format!(
            "settings save failed ({reason:?}); correct the settings file or permissions and retry"
        )),
        _ => None,
    }
}

pub fn section<R>(
    ui: &mut egui::Ui,
    title: impl Into<String>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let tokens = UiTokens::default();
    ui.add_space(tokens.spacing.row_gap);
    ui.label(
        RichText::new(title.into())
            .strong()
            .color(tokens.colors.text_primary),
    );
    ui.separator();
    let output = add_contents(ui);
    ui.add_space(tokens.spacing.section_gap);
    output
}

pub fn panel_scroll<R>(
    ui: &mut egui::Ui,
    id_salt: impl Hash,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .auto_shrink([false, false])
        .show(ui, add_contents)
        .inner
}

pub fn property_row(ui: &mut egui::Ui, label: impl Display, value: impl Display) {
    let tokens = UiTokens::default();
    ui.horizontal_top(|ui| {
        ui.set_min_height(tokens.spacing.control_height);
        ui.add_sized(
            [
                tokens.spacing.property_label_width,
                tokens.spacing.control_height,
            ],
            egui::Label::new(RichText::new(label.to_string()).color(tokens.colors.text_muted)),
        );
        let value_text = value.to_string();
        let value_width = ui.available_width().max(48.0);
        ui.add_sized(
            [value_width, tokens.spacing.control_height],
            egui::Label::new(RichText::new(value_text).color(tokens.colors.text_primary)).wrap(),
        );
    });
}

pub fn muted_label(ui: &mut egui::Ui, text: impl Display) {
    let tokens = UiTokens::default();
    ui.label(RichText::new(text.to_string()).color(tokens.colors.text_muted));
}

pub fn elided_label(ui: &mut egui::Ui, text: impl Display, max_chars: usize) {
    let full = text.to_string();
    let elided = elide_middle(&full, max_chars);
    ui.label(elided).on_hover_text(full);
}

pub fn elide_middle(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars || max_chars < 8 {
        return text.to_owned();
    }
    let keep = max_chars - 3;
    let head = keep / 2;
    let tail = keep - head;
    let prefix = text.chars().take(head).collect::<String>();
    let suffix = text
        .chars()
        .rev()
        .take(tail)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{prefix}...{suffix}")
}

pub fn status_badge(ui: &mut egui::Ui, tone: StatusTone, label: impl Display) {
    let tokens = UiTokens::default();
    let color = match tone {
        StatusTone::Ready => tokens.colors.status_ready,
        StatusTone::Warning => tokens.colors.status_warning,
        StatusTone::Error => tokens.colors.status_error,
    };
    let text = RichText::new(label.to_string())
        .strong()
        .color(Color32::BLACK);
    egui::Frame::new()
        .fill(color)
        .corner_radius(egui::CornerRadius::same(3))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(text);
        });
}

pub fn toolbar_button(
    ui: &mut egui::Ui,
    label: impl Into<String>,
    enabled: bool,
) -> egui::Response {
    let tokens = UiTokens::default();
    ui.add_enabled(
        enabled,
        egui::Button::new(label.into()).min_size(egui::vec2(0.0, tokens.spacing.control_height)),
    )
}

pub fn layer_row(
    ui: &mut egui::Ui,
    selected: bool,
    visible: bool,
    name: &str,
    detail: &str,
) -> egui::Response {
    let tokens = UiTokens::default();
    ui.horizontal(|ui| {
        let visibility_color = if visible {
            tokens.colors.status_ready
        } else {
            tokens.colors.text_muted
        };
        egui::Frame::new()
            .fill(visibility_color)
            .corner_radius(egui::CornerRadius::same(2))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |_| {});
        ui.selectable_label(selected, name)
            .on_hover_text(detail.to_owned())
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workbench_layout_reserves_viewport_at_common_sizes() {
        let spec = WorkbenchLayoutSpec::default();

        let laptop = spec.report(egui::vec2(1440.0, 900.0), 1.5).unwrap();
        assert!(laptop.viewport_size_points.x >= spec.min_viewport_width);
        assert!(laptop.viewport_size_points.y >= spec.min_viewport_height);
        assert_eq!(laptop.viewport_size_pixels, egui::vec2(1260.0, 1299.0));

        let desktop = spec.report(egui::vec2(1920.0, 1080.0), 2.0).unwrap();
        assert_eq!(desktop.viewport_size_points, egui::vec2(1320.0, 1046.0));
        assert_eq!(desktop.viewport_size_pixels, egui::vec2(2640.0, 2092.0));
    }

    #[test]
    fn workbench_layout_rejects_windows_that_cannot_hold_viewport() {
        let spec = WorkbenchLayoutSpec::default();

        assert!(spec.report(egui::vec2(900.0, 640.0), 1.0).is_none());
        assert!(spec.report(egui::vec2(1440.0, 900.0), 0.0).is_none());
    }

    #[test]
    fn design_tokens_have_stable_snapshot() {
        insta::assert_snapshot!(
            "mirante4d_ui_tokens",
            format!(
                "panel={:?}\nviewport={:?}\naccent={:?}\nleft={} right={} top={} bottom={}",
                UiTokens::default().colors.panel_background,
                UiTokens::default().colors.viewport_background,
                UiTokens::default().colors.accent,
                WorkbenchLayoutSpec::default().left_panel_width,
                WorkbenchLayoutSpec::default().right_panel_width,
                WorkbenchLayoutSpec::default().top_bar_height,
                WorkbenchLayoutSpec::default().bottom_strip_height,
            )
        );
    }

    #[test]
    fn shared_components_are_accessible_in_narrow_layout() {
        use egui_kittest::{Harness, kittest::Queryable};

        let harness = Harness::builder()
            .with_size(egui::vec2(320.0, 220.0))
            .with_pixels_per_point(2.0)
            .build_ui(|ui| {
                configure_visuals(ui.ctx());
                section(ui, "Dataset", |ui| {
                    property_row(ui, "name", "very-long-scientific-dataset-name");
                    property_row(ui, "shape", "t12 z64 y512 x512");
                    status_badge(ui, StatusTone::Ready, "ready");
                });
            });

        harness.get_by_label("Dataset");
        harness.get_by_label("name");
        harness.get_by_label("shape");
        harness.get_by_label("ready");
    }
}
