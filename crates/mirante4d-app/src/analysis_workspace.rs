use std::cmp::Ordering;

use eframe::egui;
use mirante4d_analysis::{AnalysisCell, AnalysisPlot, AnalysisTable, export_table_csv};
use mirante4d_application::{
    AnalysisPlotDescriptor, AnalysisPlotId, AnalysisPlotPointSelection, AnalysisTableDescriptor,
    AnalysisTableId, ApplicationCommand,
};

use crate::{
    current_runtime::{analysis::CurrentAnalysisRuntime, ui::CurrentUiRuntime},
    ui_kit::{self, StatusTone},
};

const ANALYSIS_TABLE_PREVIEW_HEIGHT: f32 = 220.0;
const ANALYSIS_PLOT_PREVIEW_HEIGHT: f32 = 150.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisTableSort {
    pub(crate) column_key: String,
    pub(crate) ascending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AnalysisPlotViewRange {
    pub(crate) plot_index: usize,
    pub(crate) min_x: f64,
    pub(crate) max_x: f64,
    pub(crate) min_y: f64,
    pub(crate) max_y: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisWorkspaceViewInput<'a> {
    pub(crate) table_descriptors: &'a [AnalysisTableDescriptor],
    pub(crate) plot_descriptors: &'a [AnalysisPlotDescriptor],
    pub(crate) selected_table: Option<AnalysisTableId>,
    pub(crate) selected_plot: Option<AnalysisPlotId>,
    pub(crate) selected_plot_point: Option<AnalysisPlotPointSelection>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisTableExportInput<'a> {
    pub(crate) table_descriptors: &'a [AnalysisTableDescriptor],
    pub(crate) selected_table: Option<AnalysisTableId>,
}

pub(crate) fn show_analysis_workspace_window(
    ctx: &egui::Context,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    input: AnalysisWorkspaceViewInput<'_>,
) -> Vec<ApplicationCommand> {
    if !ui_runtime.analysis_workspace_open {
        return Vec::new();
    }
    let mut open = ui_runtime.analysis_workspace_open;
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
                    commands.extend(show_analysis_workspace(ui, analysis, ui_runtime, input));
                });
        });
    ui_runtime.analysis_workspace_open = open;
    commands
}

pub(crate) fn show_analysis_workspace(
    ui: &mut egui::Ui,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    input: AnalysisWorkspaceViewInput<'_>,
) -> Vec<ApplicationCommand> {
    let mut commands = Vec::new();
    ui_kit::property_row(ui, "tables", analysis.analysis_tables.len().to_string());
    ui_kit::property_row(ui, "plots", analysis.analysis_plots.len().to_string());
    ui_kit::property_row(
        ui,
        "operations",
        analysis.analysis_operations.len().to_string(),
    );
    show_analysis_result_browser(
        ui,
        analysis,
        input,
        &mut ui_runtime.analysis_plot_view,
        &mut commands,
    );
    if let Some(table_index) = selected_analysis_table_index(analysis, input) {
        let table = &analysis.analysis_tables[table_index];
        ui_kit::property_row(ui, "selected table", &table.name);
        ui_kit::property_row(ui, "state", format!("{:?}", table.state));
        ui_kit::property_row(ui, "rows", table.rows.len().to_string());
        ui_kit::property_row(ui, "scope", &table.provenance.scope);
        show_analysis_table_preview(
            ui,
            table,
            &mut ui_runtime.analysis_filter,
            &mut ui_runtime.analysis_sort,
        );
    }
    if let Some(plot_index) = selected_analysis_plot_index(analysis, input) {
        let plot = &analysis.analysis_plots[plot_index];
        let plot_id = input.plot_descriptors[plot_index].id();
        let point_count = plot
            .series
            .iter()
            .map(|series| series.points.len())
            .sum::<usize>();
        ui_kit::property_row(ui, "plot", &plot.name);
        ui_kit::property_row(ui, "plot points", point_count.to_string());
        show_analysis_plot_preview(
            ui,
            plot,
            plot_index,
            plot_id,
            input.selected_plot_point,
            &mut ui_runtime.analysis_plot_view,
            &mut commands,
        );
    }
    if let Some(csv) = &analysis.last_analysis_export_csv {
        ui_kit::property_row(ui, "csv", format!("{} byte(s)", csv.len()));
    }
    commands
}

fn show_analysis_result_browser(
    ui: &mut egui::Ui,
    analysis: &CurrentAnalysisRuntime,
    input: AnalysisWorkspaceViewInput<'_>,
    plot_view: &mut Option<AnalysisPlotViewRange>,
    commands: &mut Vec<ApplicationCommand>,
) {
    if analysis.analysis_tables.is_empty() && analysis.analysis_plots.is_empty() {
        ui_kit::status_badge(ui, StatusTone::Ready, "no analysis results");
        return;
    }

    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            ui.strong("Tables");
            egui::ScrollArea::vertical()
                .id_salt("analysis-table-browser")
                .max_height(92.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (index, (table, descriptor)) in analysis
                        .analysis_tables
                        .iter()
                        .zip(input.table_descriptors)
                        .enumerate()
                    {
                        let selected = input.selected_table == Some(descriptor.id());
                        if ui
                            .selectable_label(selected, analysis_table_browser_label(index, table))
                            .clicked()
                        {
                            commands.push(ApplicationCommand::SelectAnalysisTable(Some(
                                descriptor.id(),
                            )));
                        }
                    }
                });
        });
        ui.vertical(|ui| {
            ui.strong("Plots");
            egui::ScrollArea::vertical()
                .id_salt("analysis-plot-browser")
                .max_height(92.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (index, (plot, descriptor)) in analysis
                        .analysis_plots
                        .iter()
                        .zip(input.plot_descriptors)
                        .enumerate()
                    {
                        let selected = input.selected_plot == Some(descriptor.id());
                        if ui
                            .selectable_label(selected, analysis_plot_browser_label(index, plot))
                            .clicked()
                        {
                            commands.push(ApplicationCommand::SelectAnalysisPlot(Some(
                                descriptor.id(),
                            )));
                            *plot_view = None;
                        }
                    }
                });
        });
    });
}

fn selected_analysis_table_index(
    analysis: &CurrentAnalysisRuntime,
    input: AnalysisWorkspaceViewInput<'_>,
) -> Option<usize> {
    let selected = input.selected_table?;
    input
        .table_descriptors
        .iter()
        .position(|descriptor| descriptor.id() == selected)
        .filter(|index| *index < analysis.analysis_tables.len())
}

fn selected_analysis_plot_index(
    analysis: &CurrentAnalysisRuntime,
    input: AnalysisWorkspaceViewInput<'_>,
) -> Option<usize> {
    let selected = input.selected_plot?;
    input
        .plot_descriptors
        .iter()
        .position(|descriptor| descriptor.id() == selected)
        .filter(|index| *index < analysis.analysis_plots.len())
}

fn analysis_table_browser_label(index: usize, table: &AnalysisTable) -> String {
    format!(
        "{:02} {} ({} rows)",
        index + 1,
        table.name,
        table.rows.len()
    )
}

fn analysis_plot_browser_label(index: usize, plot: &AnalysisPlot) -> String {
    let point_count = plot
        .series
        .iter()
        .map(|series| series.points.len())
        .sum::<usize>();
    format!("{:02} {} ({} pts)", index + 1, plot.name, point_count)
}

fn show_analysis_table_preview(
    ui: &mut egui::Ui,
    table: &AnalysisTable,
    filter: &mut String,
    sort: &mut Option<AnalysisTableSort>,
) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("filter");
        ui.add(
            egui::TextEdit::singleline(filter)
                .desired_width(180.0)
                .hint_text("rows"),
        );
        if ui_kit::toolbar_button(ui, "Clear", !filter.is_empty()).clicked() {
            filter.clear();
        }
    });

    ui.horizontal_wrapped(|ui| {
        for column in &table.columns {
            let label = analysis_column_header_label(&column.label, sort.as_ref(), &column.key);
            if ui_kit::toolbar_button(ui, label, true).clicked() {
                toggle_analysis_sort(sort, &column.key);
            }
        }
    });

    let preview = analysis_table_preview_rows(table, filter, sort.as_ref());
    ui_kit::property_row(
        ui,
        "showing",
        format!(
            "{} matched ({} total)",
            preview.matched_rows, preview.total_rows
        ),
    );

    egui::Grid::new(("analysis-table-preview-header", &table.id))
        .min_col_width(72.0)
        .show(ui, |ui| {
            for column in &table.columns {
                ui.strong(&column.label);
            }
            ui.end_row();
        });

    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    egui::ScrollArea::both()
        .id_salt(("analysis-table-preview-scroll", &table.id))
        .max_height(ANALYSIS_TABLE_PREVIEW_HEIGHT)
        .auto_shrink([false, false])
        .show_rows(
            ui,
            row_height,
            preview.shown_indices.len(),
            |ui, row_range| {
                egui::Grid::new(("analysis-table-preview-grid", &table.id))
                    .striped(true)
                    .min_col_width(72.0)
                    .show(ui, |ui| {
                        for visible_row_index in row_range {
                            let row_index = preview.shown_indices[visible_row_index];
                            let row = &table.rows[row_index];
                            for column in &table.columns {
                                let cell_text = row
                                    .cells
                                    .get(&column.key)
                                    .map(analysis_cell_text)
                                    .unwrap_or_default();
                                ui.label(cell_text);
                            }
                            ui.end_row();
                        }
                    });
            },
        );
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
        ui_kit::status_badge(ui, StatusTone::Warning, "plot has no finite points");
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
    let border = egui::Stroke::new(1.0, tokens.colors.border);
    painter.line_segment([rect.left_top(), rect.right_top()], border);
    painter.line_segment([rect.right_top(), rect.right_bottom()], border);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()], border);
    painter.line_segment([rect.left_bottom(), rect.left_top()], border);

    let plot_rect = rect.shrink2(egui::vec2(10.0, 8.0));
    let zero_y = if bounds.min_y <= 0.0 && bounds.max_y >= 0.0 {
        Some(plot_screen_position(bounds.min_x, 0.0, bounds, plot_rect).y)
    } else {
        None
    };
    if let Some(y) = zero_y {
        painter.line_segment(
            [
                egui::pos2(plot_rect.left(), y),
                egui::pos2(plot_rect.right(), y),
            ],
            egui::Stroke::new(1.0, tokens.colors.border.linear_multiply(0.65)),
        );
    }

    let palette = [
        tokens.colors.accent,
        tokens.colors.status_ready,
        tokens.colors.status_warning,
        tokens.colors.status_error,
    ];
    for (series_index, series) in plot.series.iter().enumerate() {
        let color = palette[series_index % palette.len()];
        let stroke = egui::Stroke::new(1.5, color);
        let mut previous = None;
        for point in &series.points {
            if !point.x.is_finite() || !point.y.is_finite() {
                previous = None;
                continue;
            }
            let current = plot_screen_position(point.x, point.y, bounds, plot_rect);
            if let Some(previous) = previous {
                painter.line_segment([previous, current], stroke);
            }
            previous = Some(current);
        }
    }

    ui_kit::property_row(
        ui,
        "x",
        format!("{:.3}..{:.3} {}", bounds.min_x, bounds.max_x, plot.x_label),
    );
    ui_kit::property_row(
        ui,
        "y",
        format!("{:.3}..{:.3} {}", bounds.min_y, bounds.max_y, plot.y_label),
    );
    let nearest_hover = response
        .hovered()
        .then(|| ui.input(|input| input.pointer.hover_pos()))
        .flatten()
        .and_then(|pointer| nearest_analysis_plot_point(plot, bounds, plot_rect, pointer));
    if response.clicked()
        && let Some(nearest) = nearest_hover.as_ref()
        && let Ok(series_index) = u16::try_from(nearest.series_index)
        && let Ok(point_index) = u64::try_from(nearest.point_index)
    {
        commands.push(ApplicationCommand::SelectAnalysisPlotPoint(Some(
            AnalysisPlotPointSelection::new(plot_id, series_index, point_index),
        )));
    }
    if let Some(nearest) = nearest_hover {
        ui_kit::property_row(
            ui,
            "nearest",
            format!(
                "{} #{}: {} {:.3}, {} {:.3}",
                nearest.series_name,
                nearest.point_index,
                plot.x_label,
                nearest.x,
                plot.y_label,
                nearest.y
            ),
        );
    }
    if let Some(selection) = selected_point
        && selection.plot_id() == plot_id
        && let Some(series) = plot.series.get(usize::from(selection.series_index()))
        && let Ok(point_index) = usize::try_from(selection.point_index())
        && let Some(point) = series.points.get(point_index)
    {
        ui_kit::property_row(
            ui,
            "selected",
            format!(
                "{} #{}: {} {:.3}, {} {:.3}",
                series.name, point_index, plot.x_label, point.x, plot.y_label, point.y
            ),
        );
    }
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
        if ui_kit::toolbar_button(ui, "Down", true).clicked() {
            pan_analysis_plot_view(view_range, plot_index, full_bounds, 0.0, -0.25);
        }
        if ui_kit::toolbar_button(ui, "Up", true).clicked() {
            pan_analysis_plot_view(view_range, plot_index, full_bounds, 0.0, 0.25);
        }
        if ui_kit::toolbar_button(ui, "Reset", view_range.is_some()).clicked() {
            *view_range = None;
        }
    });
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
    full_bounds: AnalysisPlotBounds,
) -> AnalysisPlotViewRange {
    let (min_x, max_x) =
        clamp_analysis_plot_axis(view.min_x, view.max_x, full_bounds.min_x, full_bounds.max_x);
    let (min_y, max_y) =
        clamp_analysis_plot_axis(view.min_y, view.max_y, full_bounds.min_y, full_bounds.max_y);
    AnalysisPlotViewRange {
        plot_index: view.plot_index,
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

fn clamp_analysis_plot_axis(min: f64, max: f64, full_min: f64, full_max: f64) -> (f64, f64) {
    if !min.is_finite()
        || !max.is_finite()
        || !full_min.is_finite()
        || !full_max.is_finite()
        || min >= max
        || full_min >= full_max
    {
        return (full_min, full_max);
    }
    let full_span = full_max - full_min;
    let span = (max - min).min(full_span);
    let mut clamped_min = min;
    let mut clamped_max = min + span;
    if clamped_min < full_min {
        clamped_max += full_min - clamped_min;
        clamped_min = full_min;
    }
    if clamped_max > full_max {
        clamped_min -= clamped_max - full_max;
        clamped_max = full_max;
    }
    if clamped_min < full_min {
        clamped_min = full_min;
    }
    if clamped_min >= clamped_max {
        (full_min, full_max)
    } else {
        (clamped_min, clamped_max)
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisTablePreviewRows {
    pub(crate) total_rows: usize,
    pub(crate) matched_rows: usize,
    pub(crate) shown_indices: Vec<usize>,
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
    pub(crate) series_index: usize,
    pub(crate) point_index: usize,
    pub(crate) series_name: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) distance_sq: f32,
}

pub(crate) fn analysis_table_preview_rows(
    table: &AnalysisTable,
    filter: &str,
    sort: Option<&AnalysisTableSort>,
) -> AnalysisTablePreviewRows {
    let normalized_filter = filter.trim().to_ascii_lowercase();
    let mut matched_indices = table
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            if normalized_filter.is_empty()
                || row.cells.values().any(|cell| {
                    analysis_cell_text(cell)
                        .to_ascii_lowercase()
                        .contains(&normalized_filter)
                })
            {
                Some(index)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if let Some(sort) = sort {
        matched_indices.sort_by(|left, right| {
            let order = compare_analysis_table_rows(table, sort, *left, *right);
            if order == Ordering::Equal {
                left.cmp(right)
            } else {
                order
            }
        });
    }

    let matched_rows = matched_indices.len();
    AnalysisTablePreviewRows {
        total_rows: table.rows.len(),
        matched_rows,
        shown_indices: matched_indices,
    }
}

fn compare_analysis_table_rows(
    table: &AnalysisTable,
    sort: &AnalysisTableSort,
    left_index: usize,
    right_index: usize,
) -> Ordering {
    let left = table
        .rows
        .get(left_index)
        .and_then(|row| row.cells.get(&sort.column_key));
    let right = table
        .rows
        .get(right_index)
        .and_then(|row| row.cells.get(&sort.column_key));
    let order = match (left, right) {
        (Some(left), Some(right)) => compare_analysis_cells(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    if sort.ascending {
        order
    } else {
        order.reverse()
    }
}

fn compare_analysis_cells(left: &AnalysisCell, right: &AnalysisCell) -> Ordering {
    match (
        analysis_cell_numeric_value(left),
        analysis_cell_numeric_value(right),
    ) {
        (Some(left), Some(right)) => compare_analysis_numbers(left, right),
        _ => analysis_cell_text(left)
            .to_ascii_lowercase()
            .cmp(&analysis_cell_text(right).to_ascii_lowercase()),
    }
}

fn compare_analysis_numbers(left: f64, right: f64) -> Ordering {
    match (left.is_finite(), right.is_finite()) {
        (true, true) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => Ordering::Equal,
    }
}

fn analysis_cell_numeric_value(cell: &AnalysisCell) -> Option<f64> {
    match cell {
        AnalysisCell::Integer(value) => Some(*value as f64),
        AnalysisCell::Float(value) => Some(*value),
        AnalysisCell::Text(_) => None,
    }
}

fn analysis_cell_text(cell: &AnalysisCell) -> String {
    match cell {
        AnalysisCell::Text(value) => value.clone(),
        AnalysisCell::Integer(value) => value.to_string(),
        AnalysisCell::Float(value) if value.is_finite() => format!("{value:.6}"),
        AnalysisCell::Float(value) => value.to_string(),
    }
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
        Some(sort) if sort.column_key == key && sort.ascending => format!("{label} asc"),
        Some(sort) if sort.column_key == key => format!("{label} desc"),
        _ => label.to_owned(),
    }
}

pub(crate) fn analysis_plot_bounds(plot: &AnalysisPlot) -> Option<AnalysisPlotBounds> {
    let mut bounds: Option<AnalysisPlotBounds> = None;
    for point in plot.series.iter().flat_map(|series| &series.points) {
        if !point.x.is_finite() || !point.y.is_finite() {
            continue;
        }
        bounds = Some(match bounds {
            Some(bounds) => AnalysisPlotBounds {
                min_x: bounds.min_x.min(point.x),
                max_x: bounds.max_x.max(point.x),
                min_y: bounds.min_y.min(point.y),
                max_y: bounds.max_y.max(point.y),
            },
            None => AnalysisPlotBounds {
                min_x: point.x,
                max_x: point.x,
                min_y: point.y,
                max_y: point.y,
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
    plot.series
        .iter()
        .enumerate()
        .flat_map(|(series_index, series)| {
            series
                .points
                .iter()
                .enumerate()
                .filter_map(move |(point_index, point)| {
                    if !point.x.is_finite() || !point.y.is_finite() {
                        return None;
                    }
                    let screen = plot_screen_position(point.x, point.y, bounds, rect);
                    Some(AnalysisPlotNearestPoint {
                        series_index,
                        point_index,
                        series_name: series.name.clone(),
                        x: point.x,
                        y: point.y,
                        distance_sq: screen.distance_sq(position),
                    })
                })
        })
        .min_by(|left, right| {
            compare_analysis_numbers(left.distance_sq as f64, right.distance_sq as f64)
        })
}

fn expand_degenerate_plot_bounds(mut bounds: AnalysisPlotBounds) -> AnalysisPlotBounds {
    if bounds.min_x == bounds.max_x {
        let padding = degenerate_plot_padding(bounds.min_x);
        bounds.min_x -= padding;
        bounds.max_x += padding;
    }
    if bounds.min_y == bounds.max_y {
        let padding = degenerate_plot_padding(bounds.min_y);
        bounds.min_y -= padding;
        bounds.max_y += padding;
    }
    bounds
}

fn degenerate_plot_padding(value: f64) -> f64 {
    (value.abs() * 0.05).max(1.0)
}

pub(crate) fn export_selected_analysis_table(
    analysis: &mut CurrentAnalysisRuntime,
    input: AnalysisTableExportInput<'_>,
) -> anyhow::Result<usize> {
    let selected = input
        .selected_table
        .ok_or_else(|| anyhow::anyhow!("no analysis table is selected for export"))?;
    let table_index = input
        .table_descriptors
        .iter()
        .position(|descriptor| descriptor.id() == selected)
        .ok_or_else(|| anyhow::anyhow!("selected analysis table is not in the payload catalog"))?;
    let table = analysis
        .analysis_tables
        .get(table_index)
        .ok_or_else(|| anyhow::anyhow!("selected analysis table payload is unavailable"))?;
    let csv = export_table_csv(table)?;
    let byte_count = csv.len();
    analysis.last_analysis_export_csv = Some(csv);
    Ok(byte_count)
}
