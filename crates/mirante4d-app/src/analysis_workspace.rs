use std::cmp::Ordering;

use anyhow::{Context, Result, ensure};
use eframe::egui;
use mirante4d_analysis_core::{AnalysisPlot, AnalysisTable, IntensityStatistics};
use mirante4d_application::{
    AnalysisPlotDescriptor, AnalysisPlotId, AnalysisPlotPointSelection, AnalysisTableDescriptor,
    AnalysisTableId, ApplicationCommand,
};
use mirante4d_ui_egui::{AnalysisPlotViewRange, AnalysisTableSort, EguiUiState};

use crate::{
    current_runtime::analysis::AnalysisProductRuntime,
    ui_kit::{self, StatusTone},
};

const ANALYSIS_TABLE_PREVIEW_HEIGHT: f32 = 220.0;
const ANALYSIS_PLOT_PREVIEW_HEIGHT: f32 = 150.0;
const TABLE_COLUMNS: [(&str, &str); 9] = [
    ("timepoint", "time"),
    ("geometric", "samples"),
    ("valid", "valid"),
    ("nonzero", "nonzero"),
    ("minimum", "min"),
    ("maximum", "max"),
    ("sum", "sum"),
    ("mean", "mean"),
    ("variance", "variance"),
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisWorkspaceViewInput<'a> {
    pub(crate) table_descriptors: &'a [AnalysisTableDescriptor],
    pub(crate) plot_descriptors: &'a [AnalysisPlotDescriptor],
    pub(crate) selected_table: Option<AnalysisTableId>,
    pub(crate) selected_plot: Option<AnalysisPlotId>,
    pub(crate) selected_plot_point: Option<AnalysisPlotPointSelection>,
}

#[derive(Debug)]
pub(crate) struct AnalysisWorkspaceView<'a> {
    status_text: &'a str,
    progress_blocks: Option<(u64, u64)>,
    tables: Vec<AnalysisTableView<'a>>,
    plots: Vec<AnalysisPlotView<'a>>,
    selected_table: Option<AnalysisTableId>,
    selected_plot: Option<AnalysisPlotId>,
    selected_plot_point: Option<AnalysisPlotPointSelection>,
    last_export_csv_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct AnalysisTableView<'a> {
    id: AnalysisTableId,
    table: Option<&'a AnalysisTable>,
}

#[derive(Debug, Clone, Copy)]
struct AnalysisPlotView<'a> {
    id: AnalysisPlotId,
    plot: Option<&'a AnalysisPlot>,
}

impl<'a> AnalysisWorkspaceView<'a> {
    pub(crate) fn new(
        analysis: &'a AnalysisProductRuntime,
        input: AnalysisWorkspaceViewInput<'_>,
    ) -> Self {
        Self {
            status_text: analysis.status_text(),
            progress_blocks: analysis
                .progress()
                .map(|progress| (progress.completed_blocks(), progress.total_blocks())),
            tables: input
                .table_descriptors
                .iter()
                .map(|descriptor| AnalysisTableView {
                    id: descriptor.id(),
                    table: analysis.table(descriptor.id()),
                })
                .collect(),
            plots: input
                .plot_descriptors
                .iter()
                .map(|descriptor| AnalysisPlotView {
                    id: descriptor.id(),
                    plot: analysis.plot(descriptor.id()),
                })
                .collect(),
            selected_table: input.selected_table,
            selected_plot: input.selected_plot,
            selected_plot_point: input.selected_plot_point,
            last_export_csv_bytes: analysis.last_export_csv().map(str::len),
        }
    }

    fn table(&self, id: AnalysisTableId) -> Option<&'a AnalysisTable> {
        self.tables
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.table)
    }

    fn plot(&self, id: AnalysisPlotId) -> Option<&'a AnalysisPlot> {
        self.plots
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.plot)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisTableExportInput<'a> {
    pub(crate) table_descriptors: &'a [AnalysisTableDescriptor],
    pub(crate) selected_table: Option<AnalysisTableId>,
}

pub(crate) fn show_analysis_workspace_window(
    ctx: &egui::Context,
    view: &AnalysisWorkspaceView<'_>,
    egui_ui: &mut EguiUiState,
) -> Vec<ApplicationCommand> {
    if !egui_ui.analysis_workspace_open {
        return Vec::new();
    }
    let mut open = egui_ui.analysis_workspace_open;
    let mut commands = Vec::new();
    egui::Window::new("Analysis Workspace")
        .open(&mut open)
        .resizable(true)
        .default_size(egui::vec2(760.0, 560.0))
        .min_size(egui::vec2(420.0, 320.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("analysis-workspace-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    commands.extend(show_analysis_workspace(ui, view, egui_ui));
                });
        });
    egui_ui.analysis_workspace_open = open;
    commands
}

pub(crate) fn show_analysis_workspace(
    ui: &mut egui::Ui,
    view: &AnalysisWorkspaceView<'_>,
    egui_ui: &mut EguiUiState,
) -> Vec<ApplicationCommand> {
    let mut commands = Vec::new();
    let tone = if view.status_text.contains("failed") {
        StatusTone::Error
    } else if view.status_text.contains("cancelled") {
        StatusTone::Warning
    } else {
        StatusTone::Ready
    };
    ui_kit::status_badge(ui, tone, view.status_text);
    ui_kit::property_row(ui, "saved tables", view.tables.len().to_string());
    ui_kit::property_row(ui, "saved plots", view.plots.len().to_string());
    if let Some((completed_blocks, total_blocks)) = view.progress_blocks {
        ui_kit::property_row(
            ui,
            "progress",
            format!("{completed_blocks} / {total_blocks} blocks"),
        );
    }

    show_analysis_result_browser(ui, view, &mut egui_ui.analysis_plot_view, &mut commands);
    if let Some(table_id) = view.selected_table {
        if let Some(table) = view.table(table_id) {
            ui.add_space(8.0);
            ui_kit::property_row(ui, "selected table", table.name());
            ui_kit::property_row(ui, "rows", table.rows().len().to_string());
            ui_kit::property_row(
                ui,
                "time range",
                format!(
                    "{}..{}",
                    table.provenance().time_start(),
                    table.provenance().time_end_exclusive()
                ),
            );
            show_analysis_table_preview(
                ui,
                table,
                &mut egui_ui.analysis_filter,
                &mut egui_ui.analysis_sort,
            );
        } else {
            ui_kit::status_badge(ui, StatusTone::Warning, "selected table is not loaded");
        }
    }
    if let Some(plot_id) = view.selected_plot {
        if let Some(plot) = view.plot(plot_id) {
            let plot_index = view
                .plots
                .iter()
                .position(|entry| entry.id == plot_id)
                .unwrap_or_default();
            ui.add_space(8.0);
            ui_kit::property_row(ui, "selected plot", plot.name());
            ui_kit::property_row(ui, "points", plot.points().len().to_string());
            show_analysis_plot_preview(
                ui,
                plot,
                plot_index,
                plot_id,
                view.selected_plot_point,
                &mut egui_ui.analysis_plot_view,
                &mut commands,
            );
        } else {
            ui_kit::status_badge(ui, StatusTone::Warning, "selected plot is not loaded");
        }
    }
    if let Some(bytes) = view.last_export_csv_bytes {
        ui_kit::property_row(ui, "last CSV", format!("{bytes} bytes"));
    }
    commands
}

fn show_analysis_result_browser(
    ui: &mut egui::Ui,
    view: &AnalysisWorkspaceView<'_>,
    plot_view: &mut Option<AnalysisPlotViewRange>,
    commands: &mut Vec<ApplicationCommand>,
) {
    if view.tables.is_empty() && view.plots.is_empty() {
        ui_kit::status_badge(ui, StatusTone::Ready, "no saved analysis results");
        return;
    }
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            ui.strong("Tables");
            egui::ScrollArea::vertical()
                .id_salt("analysis-table-browser")
                .max_height(92.0)
                .show(ui, |ui| {
                    for (index, entry) in view.tables.iter().enumerate() {
                        let label = entry.table.map_or_else(
                            || format!("{:02} saved table (loading)", index + 1),
                            |table| {
                                format!(
                                    "{:02} {} ({} rows)",
                                    index + 1,
                                    table.name(),
                                    table.rows().len()
                                )
                            },
                        );
                        if ui
                            .selectable_label(view.selected_table == Some(entry.id), label)
                            .clicked()
                        {
                            commands.push(ApplicationCommand::SelectAnalysisTable(Some(entry.id)));
                        }
                    }
                });
        });
        ui.vertical(|ui| {
            ui.strong("Plots");
            egui::ScrollArea::vertical()
                .id_salt("analysis-plot-browser")
                .max_height(92.0)
                .show(ui, |ui| {
                    for (index, entry) in view.plots.iter().enumerate() {
                        let label = entry.plot.map_or_else(
                            || format!("{:02} saved plot (loading)", index + 1),
                            |plot| {
                                format!(
                                    "{:02} {} ({} points)",
                                    index + 1,
                                    plot.name(),
                                    plot.points().len()
                                )
                            },
                        );
                        if ui
                            .selectable_label(view.selected_plot == Some(entry.id), label)
                            .clicked()
                        {
                            commands.push(ApplicationCommand::SelectAnalysisPlot(Some(entry.id)));
                            *plot_view = None;
                        }
                    }
                });
        });
    });
}

fn show_analysis_table_preview(
    ui: &mut egui::Ui,
    table: &AnalysisTable,
    filter: &mut String,
    sort: &mut Option<AnalysisTableSort>,
) {
    ui.horizontal(|ui| {
        ui.label("filter");
        ui.add(
            egui::TextEdit::singleline(filter)
                .desired_width(180.0)
                .hint_text("time or value"),
        );
        if ui_kit::toolbar_button(ui, "Clear", !filter.is_empty()).clicked() {
            filter.clear();
        }
    });
    ui.horizontal_wrapped(|ui| {
        for (key, label) in TABLE_COLUMNS {
            let label = analysis_column_header_label(label, sort.as_ref(), key);
            if ui_kit::toolbar_button(ui, label, true).clicked() {
                toggle_analysis_sort(sort, key);
            }
        }
    });

    let preview = analysis_table_preview_rows(table, filter, sort.as_ref());
    ui_kit::property_row(
        ui,
        "showing",
        format!("{} of {} rows", preview.matched_rows, preview.total_rows),
    );
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    egui::ScrollArea::both()
        .id_salt(("analysis-table-preview", table.name()))
        .max_height(ANALYSIS_TABLE_PREVIEW_HEIGHT)
        .auto_shrink([false, false])
        .show_rows(ui, row_height, preview.shown_indices.len(), |ui, range| {
            egui::Grid::new(("analysis-table-grid", table.name()))
                .striped(true)
                .min_col_width(64.0)
                .show(ui, |ui| {
                    for (_, label) in TABLE_COLUMNS {
                        ui.strong(label);
                    }
                    ui.end_row();
                    for visible_index in range {
                        let row = &table.rows()[preview.shown_indices[visible_index]];
                        for (key, _) in TABLE_COLUMNS {
                            ui.label(analysis_row_value(row, key));
                        }
                        ui.end_row();
                    }
                });
        });
}

fn show_analysis_plot_preview(
    ui: &mut egui::Ui,
    plot: &AnalysisPlot,
    plot_index: usize,
    plot_id: AnalysisPlotId,
    selected_point: Option<AnalysisPlotPointSelection>,
    view_range: &mut Option<AnalysisPlotViewRange>,
    commands: &mut Vec<ApplicationCommand>,
) {
    let Some(full_bounds) = analysis_plot_bounds(plot) else {
        ui_kit::status_badge(ui, StatusTone::Warning, "plot has no valid mean values");
        return;
    };
    normalize_analysis_plot_view_for_plot(plot_index, full_bounds, view_range);
    show_analysis_plot_view_controls(ui, plot_index, full_bounds, view_range);
    let bounds = analysis_plot_visible_bounds(plot_index, full_bounds, view_range.as_ref());
    let tokens = ui_kit::UiTokens::default();
    let width = ui.available_width().max(160.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, ANALYSIS_PLOT_PREVIEW_HEIGHT),
        egui::Sense::click(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, tokens.colors.border),
        egui::StrokeKind::Inside,
    );
    let plot_rect = rect.shrink2(egui::vec2(10.0, 8.0));
    let mut previous = None;
    for point in plot.points() {
        let Some(mean) = point.mean() else {
            previous = None;
            continue;
        };
        let current = plot_screen_position(point.timepoint() as f64, mean, bounds, plot_rect);
        if let Some(previous) = previous {
            painter.line_segment(
                [previous, current],
                egui::Stroke::new(1.5, tokens.colors.accent),
            );
        }
        previous = Some(current);
    }

    let nearest = response
        .hovered()
        .then(|| ui.input(|input| input.pointer.hover_pos()))
        .flatten()
        .and_then(|position| nearest_analysis_plot_point(plot, bounds, plot_rect, position));
    if response.clicked()
        && let Some(nearest) = nearest.as_ref()
        && let Ok(point_index) = u64::try_from(nearest.point_index)
    {
        commands.push(ApplicationCommand::SelectAnalysisPlotPoint(Some(
            AnalysisPlotPointSelection::new(plot_id, 0, point_index),
        )));
    }
    if let Some(nearest) = nearest {
        ui_kit::property_row(
            ui,
            "nearest",
            format!("time {:.0}, mean {:.6}", nearest.x, nearest.y),
        );
    }
    if let Some(selection) = selected_point
        && selection.plot_id() == plot_id
        && selection.series_index() == 0
        && let Ok(index) = usize::try_from(selection.point_index())
        && let Some(point) = plot.points().get(index)
    {
        ui_kit::property_row(
            ui,
            "selected",
            format_optional_mean(point.timepoint(), point.mean()),
        );
    }
    ui_kit::property_row(
        ui,
        "axes",
        format!(
            "time {:.1}..{:.1}; mean {:.3}..{:.3}",
            bounds.min_x, bounds.max_x, bounds.min_y, bounds.max_y
        ),
    );
}

fn show_analysis_plot_view_controls(
    ui: &mut egui::Ui,
    plot_index: usize,
    full_bounds: AnalysisPlotBounds,
    view_range: &mut Option<AnalysisPlotViewRange>,
) {
    ui.horizontal_wrapped(|ui| {
        if ui_kit::toolbar_button(ui, "Zoom In", true).clicked() {
            zoom_analysis_plot_view(view_range, plot_index, full_bounds, 0.5);
        }
        if ui_kit::toolbar_button(ui, "Zoom Out", true).clicked() {
            zoom_analysis_plot_view(view_range, plot_index, full_bounds, 2.0);
        }
        if ui_kit::toolbar_button(ui, "Left", true).clicked() {
            pan_analysis_plot_view(view_range, plot_index, full_bounds, -0.25, 0.0);
        }
        if ui_kit::toolbar_button(ui, "Right", true).clicked() {
            pan_analysis_plot_view(view_range, plot_index, full_bounds, 0.25, 0.0);
        }
        if ui_kit::toolbar_button(ui, "Reset", view_range.is_some()).clicked() {
            *view_range = None;
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisTablePreviewRows {
    pub(crate) total_rows: usize,
    pub(crate) matched_rows: usize,
    pub(crate) shown_indices: Vec<usize>,
}

pub(crate) fn analysis_table_preview_rows(
    table: &AnalysisTable,
    filter: &str,
    sort: Option<&AnalysisTableSort>,
) -> AnalysisTablePreviewRows {
    let filter = filter.trim().to_ascii_lowercase();
    let mut shown_indices = table
        .rows()
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (filter.is_empty() || analysis_row_search_text(row).contains(&filter)).then_some(index)
        })
        .collect::<Vec<_>>();
    if let Some(sort) = sort {
        shown_indices.sort_by(|left, right| {
            let order = compare_analysis_rows(
                &table.rows()[*left],
                &table.rows()[*right],
                &sort.column_key,
            );
            let order = if sort.ascending {
                order
            } else {
                order.reverse()
            };
            order.then_with(|| left.cmp(right))
        });
    }
    AnalysisTablePreviewRows {
        total_rows: table.rows().len(),
        matched_rows: shown_indices.len(),
        shown_indices,
    }
}

fn analysis_row_search_text(row: &IntensityStatistics) -> String {
    TABLE_COLUMNS
        .into_iter()
        .map(|(key, _)| analysis_row_value(row, key))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn analysis_row_value(row: &IntensityStatistics, key: &str) -> String {
    match key {
        "timepoint" => row.timepoint().to_string(),
        "geometric" => row.geometric_sample_count().to_string(),
        "valid" => row.valid_sample_count().to_string(),
        "nonzero" => row.nonzero_sample_count().to_string(),
        "minimum" => format_optional_number(row.minimum()),
        "maximum" => format_optional_number(row.maximum()),
        "sum" => format_optional_number(row.sum()),
        "mean" => format_optional_number(row.mean()),
        "variance" => format_optional_number(row.population_variance()),
        _ => String::new(),
    }
}

fn compare_analysis_rows(
    left: &IntensityStatistics,
    right: &IntensityStatistics,
    key: &str,
) -> Ordering {
    match key {
        "timepoint" => left.timepoint().cmp(&right.timepoint()),
        "geometric" => left
            .geometric_sample_count()
            .cmp(&right.geometric_sample_count()),
        "valid" => left.valid_sample_count().cmp(&right.valid_sample_count()),
        "nonzero" => left
            .nonzero_sample_count()
            .cmp(&right.nonzero_sample_count()),
        "minimum" => compare_optional_numbers(left.minimum(), right.minimum()),
        "maximum" => compare_optional_numbers(left.maximum(), right.maximum()),
        "sum" => compare_optional_numbers(left.sum(), right.sum()),
        "mean" => compare_optional_numbers(left.mean(), right.mean()),
        "variance" => {
            compare_optional_numbers(left.population_variance(), right.population_variance())
        }
        _ => Ordering::Equal,
    }
}

fn compare_optional_numbers(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn format_optional_number(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:.6}"))
}

fn format_optional_mean(timepoint: u64, mean: Option<f64>) -> String {
    mean.map_or_else(
        || format!("time {timepoint}, no valid samples"),
        |mean| format!("time {timepoint}, mean {mean:.6}"),
    )
}

fn toggle_analysis_sort(sort: &mut Option<AnalysisTableSort>, column_key: &str) {
    match sort {
        Some(current) if current.column_key == column_key => {
            current.ascending = !current.ascending;
        }
        _ => {
            *sort = Some(AnalysisTableSort {
                column_key: column_key.to_owned(),
                ascending: true,
            });
        }
    }
}

fn analysis_column_header_label(
    label: &str,
    sort: Option<&AnalysisTableSort>,
    key: &str,
) -> String {
    match sort {
        Some(sort) if sort.column_key == key && sort.ascending => format!("{label} ↑"),
        Some(sort) if sort.column_key == key => format!("{label} ↓"),
        _ => label.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AnalysisPlotBounds {
    pub(crate) min_x: f64,
    pub(crate) max_x: f64,
    pub(crate) min_y: f64,
    pub(crate) max_y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AnalysisPlotNearestPoint {
    pub(crate) point_index: usize,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) distance_sq: f32,
}

pub(crate) fn analysis_plot_bounds(plot: &AnalysisPlot) -> Option<AnalysisPlotBounds> {
    let mut bounds = None::<AnalysisPlotBounds>;
    for point in plot.points() {
        let Some(y) = point.mean() else {
            continue;
        };
        let x = point.timepoint() as f64;
        bounds = Some(match bounds {
            Some(bounds) => AnalysisPlotBounds {
                min_x: bounds.min_x.min(x),
                max_x: bounds.max_x.max(x),
                min_y: bounds.min_y.min(y),
                max_y: bounds.max_y.max(y),
            },
            None => AnalysisPlotBounds {
                min_x: x,
                max_x: x,
                min_y: y,
                max_y: y,
            },
        });
    }
    bounds.map(expand_degenerate_plot_bounds)
}

pub(crate) fn nearest_analysis_plot_point(
    plot: &AnalysisPlot,
    bounds: AnalysisPlotBounds,
    rect: egui::Rect,
    position: egui::Pos2,
) -> Option<AnalysisPlotNearestPoint> {
    if !rect.contains(position) {
        return None;
    }
    plot.points()
        .iter()
        .enumerate()
        .filter_map(|(point_index, point)| {
            let y = point.mean()?;
            let x = point.timepoint() as f64;
            let screen = plot_screen_position(x, y, bounds, rect);
            Some(AnalysisPlotNearestPoint {
                point_index,
                x,
                y,
                distance_sq: screen.distance_sq(position),
            })
        })
        .min_by(|left, right| left.distance_sq.total_cmp(&right.distance_sq))
}

pub(crate) fn plot_screen_position(
    x: f64,
    y: f64,
    bounds: AnalysisPlotBounds,
    rect: egui::Rect,
) -> egui::Pos2 {
    let x_t = ((x - bounds.min_x) / (bounds.max_x - bounds.min_x)) as f32;
    let y_t = ((y - bounds.min_y) / (bounds.max_y - bounds.min_y)) as f32;
    egui::pos2(
        rect.left() + x_t.clamp(0.0, 1.0) * rect.width(),
        rect.bottom() - y_t.clamp(0.0, 1.0) * rect.height(),
    )
}

pub(crate) fn analysis_plot_visible_bounds(
    plot_index: usize,
    full_bounds: AnalysisPlotBounds,
    view_range: Option<&AnalysisPlotViewRange>,
) -> AnalysisPlotBounds {
    match view_range {
        Some(view) if view.plot_index == plot_index => AnalysisPlotBounds {
            min_x: view.min_x,
            max_x: view.max_x,
            min_y: view.min_y,
            max_y: view.max_y,
        },
        _ => full_bounds,
    }
}

pub(crate) fn normalize_analysis_plot_view_for_plot(
    plot_index: usize,
    full_bounds: AnalysisPlotBounds,
    view_range: &mut Option<AnalysisPlotViewRange>,
) {
    let Some(view) = *view_range else {
        return;
    };
    if view.plot_index != plot_index || !analysis_plot_view_is_valid(view) {
        *view_range = None;
        return;
    }
    *view_range = Some(clamp_analysis_plot_view(view, full_bounds));
}

pub(crate) fn zoom_analysis_plot_view(
    view_range: &mut Option<AnalysisPlotViewRange>,
    plot_index: usize,
    full_bounds: AnalysisPlotBounds,
    factor: f64,
) {
    if !factor.is_finite() || factor <= 0.0 {
        return;
    }
    let current = analysis_plot_visible_bounds(plot_index, full_bounds, view_range.as_ref());
    let center_x = (current.min_x + current.max_x) * 0.5;
    let center_y = (current.min_y + current.max_y) * 0.5;
    let half_x = (current.max_x - current.min_x) * factor * 0.5;
    let half_y = (current.max_y - current.min_y) * factor * 0.5;
    *view_range = Some(clamp_analysis_plot_view(
        AnalysisPlotViewRange {
            plot_index,
            min_x: center_x - half_x,
            max_x: center_x + half_x,
            min_y: center_y - half_y,
            max_y: center_y + half_y,
        },
        full_bounds,
    ));
}

pub(crate) fn pan_analysis_plot_view(
    view_range: &mut Option<AnalysisPlotViewRange>,
    plot_index: usize,
    full_bounds: AnalysisPlotBounds,
    dx_fraction: f64,
    dy_fraction: f64,
) {
    if !dx_fraction.is_finite() || !dy_fraction.is_finite() {
        return;
    }
    let current = analysis_plot_visible_bounds(plot_index, full_bounds, view_range.as_ref());
    let dx = (current.max_x - current.min_x) * dx_fraction;
    let dy = (current.max_y - current.min_y) * dy_fraction;
    *view_range = Some(clamp_analysis_plot_view(
        AnalysisPlotViewRange {
            plot_index,
            min_x: current.min_x + dx,
            max_x: current.max_x + dx,
            min_y: current.min_y + dy,
            max_y: current.max_y + dy,
        },
        full_bounds,
    ));
}

fn analysis_plot_view_is_valid(view: AnalysisPlotViewRange) -> bool {
    view.min_x.is_finite()
        && view.max_x.is_finite()
        && view.min_y.is_finite()
        && view.max_y.is_finite()
        && view.min_x < view.max_x
        && view.min_y < view.max_y
}

fn clamp_analysis_plot_view(
    view: AnalysisPlotViewRange,
    full: AnalysisPlotBounds,
) -> AnalysisPlotViewRange {
    let (min_x, max_x) = clamp_axis(view.min_x, view.max_x, full.min_x, full.max_x);
    let (min_y, max_y) = clamp_axis(view.min_y, view.max_y, full.min_y, full.max_y);
    AnalysisPlotViewRange {
        plot_index: view.plot_index,
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

fn clamp_axis(minimum: f64, maximum: f64, full_min: f64, full_max: f64) -> (f64, f64) {
    let full_span = full_max - full_min;
    let span = (maximum - minimum).min(full_span);
    if !span.is_finite() || span <= 0.0 {
        return (full_min, full_max);
    }
    let mut clamped_min = minimum.clamp(full_min, full_max - span);
    let mut clamped_max = clamped_min + span;
    if clamped_max > full_max {
        clamped_max = full_max;
        clamped_min = full_max - span;
    }
    (clamped_min, clamped_max)
}

fn expand_degenerate_plot_bounds(mut bounds: AnalysisPlotBounds) -> AnalysisPlotBounds {
    if bounds.min_x == bounds.max_x {
        let padding = (bounds.min_x.abs() * 0.05).max(1.0);
        bounds.min_x -= padding;
        bounds.max_x += padding;
    }
    if bounds.min_y == bounds.max_y {
        let padding = (bounds.min_y.abs() * 0.05).max(1.0);
        bounds.min_y -= padding;
        bounds.max_y += padding;
    }
    bounds
}

pub(crate) fn export_selected_analysis_table(
    analysis: &mut AnalysisProductRuntime,
    input: AnalysisTableExportInput<'_>,
) -> Result<usize> {
    let selected = input
        .selected_table
        .context("no analysis table is selected for export")?;
    ensure!(
        input
            .table_descriptors
            .iter()
            .any(|descriptor| descriptor.id() == selected),
        "selected analysis table is not in the durable project catalog"
    );
    analysis
        .export_selected_table_csv(Some(selected))
        .map(str::len)
}
