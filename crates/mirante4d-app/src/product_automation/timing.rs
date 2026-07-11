use std::collections::BTreeMap;

use mirante4d_domain::RenderMode;
use serde_json::{Map, Value, json};

use crate::display_refresh::DisplayRefreshTiming;
use crate::viewer_layout::PanelId;
use crate::{DisplayedFrameFreshness, MiranteWorkbenchApp};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProductAutomationAppUpdatePhases {
    pub(crate) setup_ms: f64,
    pub(crate) task_drain_ms: f64,
    pub(crate) playback_ms: f64,
    pub(crate) ui_build_ms: f64,
    pub(crate) histogram_ui_ms: f64,
    pub(crate) command_apply_ms: f64,
    pub(crate) display_refresh_trigger_ms: f64,
    pub(crate) import_action_ms: f64,
    pub(crate) brick_result_drain_ms: f64,
    pub(crate) background_repaint_request_ms: f64,
    pub(crate) automation_step_ms: f64,
    pub(crate) total_update_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProductAutomationAppUpdateSample {
    pub(crate) sample_index: usize,
    pub(crate) command_index: usize,
    pub(crate) event_epoch_ms: u128,
    pub(crate) timing: ProductAutomationAppUpdatePhases,
    pub(crate) background_work_active: bool,
    pub(crate) active_timepoint: u64,
    pub(crate) render_mode: RenderMode,
    pub(crate) display_freshness: DisplayedFrameFreshness,
    pub(crate) target_scale_level: u32,
    pub(crate) displayed_scale_level: Option<u32>,
    pub(crate) visible_bricks: usize,
    pub(crate) resident_bricks: usize,
}

impl ProductAutomationAppUpdateSample {
    pub(crate) fn json(&self) -> Value {
        json!({
            "sample_index": self.sample_index,
            "command_index": self.command_index,
            "event_epoch_ms": self.event_epoch_ms,
            "background_work_active": self.background_work_active,
            "active_timepoint": self.active_timepoint,
            "render_mode": format!("{:?}", self.render_mode),
            "display_freshness": format!("{:?}", self.display_freshness),
            "target_scale_level": self.target_scale_level,
            "displayed_scale_level": self.displayed_scale_level,
            "visible_bricks": self.visible_bricks,
            "resident_bricks": self.resident_bricks,
            "timing": app_update_timing_json(self.timing),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProductAutomationDisplayRefreshSample {
    pub(crate) command_index: usize,
    pub(crate) command: &'static str,
    pub(crate) event_epoch_ms: u128,
    pub(crate) timing: DisplayRefreshTiming,
}

impl ProductAutomationDisplayRefreshSample {
    pub(crate) fn json(&self) -> Value {
        json!({
            "command_index": self.command_index,
            "command": self.command,
            "event_epoch_ms": self.event_epoch_ms,
            "timing": display_refresh_timing_json(self.timing),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProductAutomationInputToPresentSample {
    pub(crate) command_index: usize,
    pub(crate) command: &'static str,
    pub(crate) event_epoch_ms: u128,
    pub(crate) latency_ms: f64,
    pub(crate) display_refresh_timing: DisplayRefreshTiming,
}

impl ProductAutomationInputToPresentSample {
    pub(crate) fn json(&self) -> Value {
        json!({
            "kind": "input_to_present_proxy_timing",
            "taxonomy_version": 1,
            "measurement_scope": input_to_present_measurement_scope(),
            "command_index": self.command_index,
            "command": self.command,
            "event_epoch_ms": self.event_epoch_ms,
            "latency_ms": self.latency_ms,
            "presentation_proxy": "app_display_refresh_complete",
            "display_refresh_path": self.display_refresh_timing.path.label(),
            "display_refresh_timing": display_refresh_timing_json(self.display_refresh_timing),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProductAutomationCrossSectionLatencySample {
    pub(crate) command_index: usize,
    pub(crate) command: &'static str,
    pub(crate) operation: &'static str,
    pub(crate) panel_id: PanelId,
    pub(crate) event_epoch_ms: u128,
    pub(crate) latency_ms: f64,
    pub(crate) target_generation: u64,
    pub(crate) displayed_generation: u64,
    pub(crate) active_timepoint: u64,
    pub(crate) target_scale_level: Option<u32>,
    pub(crate) render_scale_level: Option<u32>,
    pub(crate) missing_occupied_chunks: usize,
}

impl ProductAutomationCrossSectionLatencySample {
    pub(crate) fn json(&self) -> Value {
        json!({
            "kind": "cross_section_command_to_current_partial_latency",
            "taxonomy_version": 1,
            "measurement_scope": cross_section_latency_measurement_scope(),
            "presentation_proxy": "panel_displayed_generation_with_gpu_display_frame",
            "command_index": self.command_index,
            "command": self.command,
            "operation": self.operation,
            "panel": self.panel_id.label(),
            "event_epoch_ms": self.event_epoch_ms,
            "latency_ms": self.latency_ms,
            "target_generation": self.target_generation,
            "displayed_generation": self.displayed_generation,
            "active_timepoint": self.active_timepoint,
            "target_scale_level": self.target_scale_level,
            "render_scale_level": self.render_scale_level,
            "missing_occupied_chunks": self.missing_occupied_chunks,
        })
    }
}

pub(crate) fn details_with_display_refresh_timing(
    app: &MiranteWorkbenchApp,
    previous_display_refresh_timing: Option<DisplayRefreshTiming>,
    details: Value,
) -> Value {
    let mut details = match details {
        Value::Object(details) => details,
        other => {
            let mut object = Map::new();
            object.insert("value".to_owned(), other);
            object
        }
    };
    details.insert(
        "display_refresh_timing".to_owned(),
        latest_new_display_refresh_timing(app, previous_display_refresh_timing)
            .map(display_refresh_timing_json)
            .unwrap_or(Value::Null),
    );
    Value::Object(details)
}

pub(crate) fn new_display_refresh_timing_from_details(
    details: &Value,
    app: &MiranteWorkbenchApp,
    previous_display_refresh_timing: Option<DisplayRefreshTiming>,
) -> Option<DisplayRefreshTiming> {
    details
        .get("display_refresh_timing")
        .filter(|value| value.is_object())?;
    latest_new_display_refresh_timing(app, previous_display_refresh_timing)
}

fn latest_new_display_refresh_timing(
    app: &MiranteWorkbenchApp,
    previous_display_refresh_timing: Option<DisplayRefreshTiming>,
) -> Option<DisplayRefreshTiming> {
    let timing = app.render_runtime.last_display_refresh_timing?;
    (Some(timing) != previous_display_refresh_timing).then_some(timing)
}

pub(crate) fn presentation_timing_json() -> Value {
    json!({
        "kind": "presentation_timing",
        "taxonomy_version": 1,
        "status": "app_proxy_available_os_compositor_timestamp_unavailable",
        "available_measurements": {
            "input_to_present_proxy": {
                "status": "available_when_scripted_commands_refresh_display",
                "measurement_scope": input_to_present_measurement_scope(),
                "presentation_proxy": "app_display_refresh_complete",
                "sample_summary_field": "input_to_present_timing_summary",
            },
            "display_refresh_wall_clock": {
                "status": "available_when_display_refresh_runs",
                "measurement_scope": "app_display_refresh_cpu_wall_clock_with_optional_gpu_timestamp_query",
                "sample_summary_field": "display_refresh_timing_summary",
            },
            "gpu_compute_timestamp": {
                "status": "available_when_MIRANTE4D_GPU_TIMESTAMPS_is_enabled_and_supported",
                "measurement_scope": "renderer_compute_pass_elapsed_time_from_wgpu_timestamp_queries",
                "sample_field": "gpu_compute_ms",
            },
            "gpu_upload_wall_clock": {
                "status": "available_when_renderer_returns_gpu_display_timings",
                "measurement_scope": "renderer_upload_cpu_wall_clock_subset_of_display_render",
                "sample_field": "gpu_upload_ms",
            },
        },
        "os_compositor_present_timestamp": {
            "status": "unsupported_by_current_eframe_wgpu_integration",
            "reason": "eframe presents the WGPU surface after App::update and does not expose a present-complete timestamp callback to the app",
            "winit_pre_present_notify": {
                "available": true,
                "is_timestamp": false,
                "scope": "pre-present scheduling hint; Wayland frame-callback throttling only",
            },
        },
    })
}

pub(crate) fn app_update_timing_json(timing: ProductAutomationAppUpdatePhases) -> Value {
    json!({
        "kind": "app_update_timing",
        "taxonomy_version": 1,
        "measurement_scope": "cpu_wall_clock_duration_inside_eframe_app_update",
        "phase_measurement_scopes": app_update_phase_measurement_scopes_json(),
        "dominant_non_total_phase": dominant_app_update_phase(timing),
        "phases_ms": {
            "setup": timing.setup_ms,
            "task_drain": timing.task_drain_ms,
            "playback": timing.playback_ms,
            "ui_build": timing.ui_build_ms,
            "histogram_ui": timing.histogram_ui_ms,
            "command_apply": timing.command_apply_ms,
            "display_refresh_trigger": timing.display_refresh_trigger_ms,
            "import_action": timing.import_action_ms,
            "brick_result_drain": timing.brick_result_drain_ms,
            "background_repaint_request": timing.background_repaint_request_ms,
            "automation_step": timing.automation_step_ms,
            "total_update": timing.total_update_ms,
        },
    })
}

pub(crate) fn app_update_timing_summary_json(
    samples: &[ProductAutomationAppUpdateSample],
) -> Value {
    let mut setup = Vec::with_capacity(samples.len());
    let mut task_drain = Vec::with_capacity(samples.len());
    let mut playback = Vec::with_capacity(samples.len());
    let mut ui_build = Vec::with_capacity(samples.len());
    let mut histogram_ui = Vec::with_capacity(samples.len());
    let mut command_apply = Vec::with_capacity(samples.len());
    let mut display_refresh_trigger = Vec::with_capacity(samples.len());
    let mut import_action = Vec::with_capacity(samples.len());
    let mut brick_result_drain = Vec::with_capacity(samples.len());
    let mut background_repaint_request = Vec::with_capacity(samples.len());
    let mut automation_step = Vec::with_capacity(samples.len());
    let mut total_update = Vec::with_capacity(samples.len());
    let mut background_work_active_samples = 0_u64;
    let mut dominant_counts = Map::new();

    for sample in samples {
        let timing = sample.timing;
        setup.push(timing.setup_ms);
        task_drain.push(timing.task_drain_ms);
        playback.push(timing.playback_ms);
        ui_build.push(timing.ui_build_ms);
        histogram_ui.push(timing.histogram_ui_ms);
        command_apply.push(timing.command_apply_ms);
        display_refresh_trigger.push(timing.display_refresh_trigger_ms);
        import_action.push(timing.import_action_ms);
        brick_result_drain.push(timing.brick_result_drain_ms);
        background_repaint_request.push(timing.background_repaint_request_ms);
        automation_step.push(timing.automation_step_ms);
        total_update.push(timing.total_update_ms);
        if sample.background_work_active {
            background_work_active_samples += 1;
        }
        let dominant = dominant_app_update_phase(timing);
        let current = dominant_counts
            .get(dominant)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        dominant_counts.insert(dominant.to_owned(), json!(current + 1));
    }

    let phase_summaries = [
        ("setup", timing_values_summary_json(setup)),
        ("task_drain", timing_values_summary_json(task_drain)),
        ("playback", timing_values_summary_json(playback)),
        ("ui_build", timing_values_summary_json(ui_build)),
        ("histogram_ui", timing_values_summary_json(histogram_ui)),
        ("command_apply", timing_values_summary_json(command_apply)),
        (
            "display_refresh_trigger",
            timing_values_summary_json(display_refresh_trigger),
        ),
        ("import_action", timing_values_summary_json(import_action)),
        (
            "brick_result_drain",
            timing_values_summary_json(brick_result_drain),
        ),
        (
            "background_repaint_request",
            timing_values_summary_json(background_repaint_request),
        ),
        (
            "automation_step",
            timing_values_summary_json(automation_step),
        ),
        ("total_update", timing_values_summary_json(total_update)),
    ];
    let dominant_by_p95 = phase_summaries
        .iter()
        .filter(|(phase, _)| *phase != "total_update")
        .filter_map(|(phase, summary)| {
            summary
                .get("p95")
                .and_then(Value::as_f64)
                .map(|p95| (*phase, p95))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(phase, _)| phase);
    let phases_ms = phase_summaries
        .into_iter()
        .map(|(phase, summary)| (phase.to_owned(), summary))
        .collect::<Map<_, _>>();

    json!({
        "kind": "app_update_timing_summary",
        "taxonomy_version": 1,
        "measurement_scope": "cpu_wall_clock_duration_inside_eframe_app_update",
        "phase_measurement_scopes": app_update_phase_measurement_scopes_json(),
        "sample_count": samples.len(),
        "background_work_active_samples": background_work_active_samples,
        "dominant_non_total_phase_counts": dominant_counts,
        "dominant_non_total_phase_by_p95": dominant_by_p95,
        "phases_ms": phases_ms,
    })
}

fn dominant_app_update_phase(timing: ProductAutomationAppUpdatePhases) -> &'static str {
    let mut dominant = ("setup", timing.setup_ms);
    for phase in [
        ("task_drain", timing.task_drain_ms),
        ("playback", timing.playback_ms),
        ("ui_build", timing.ui_build_ms),
        ("histogram_ui", timing.histogram_ui_ms),
        ("command_apply", timing.command_apply_ms),
        ("display_refresh_trigger", timing.display_refresh_trigger_ms),
        ("import_action", timing.import_action_ms),
        ("brick_result_drain", timing.brick_result_drain_ms),
        (
            "background_repaint_request",
            timing.background_repaint_request_ms,
        ),
        ("automation_step", timing.automation_step_ms),
    ] {
        if phase.1 > dominant.1 {
            dominant = phase;
        }
    }
    dominant.0
}

pub(crate) fn display_refresh_timing_json(timing: DisplayRefreshTiming) -> Value {
    json!({
        "kind": "display_refresh_timing",
        "taxonomy_version": 1,
        "measurement_scope": "app_display_refresh_cpu_wall_clock_with_optional_gpu_timestamp_query",
        "phase_measurement_scopes": display_refresh_phase_measurement_scopes_json(),
        "gpu_upload_timing_status": if timing.gpu_upload_ms.is_some() { "measured" } else { "not_reported_for_sample" },
        "gpu_compute_timing_status": if timing.gpu_compute_ms.is_some() { "measured" } else { "not_reported_for_sample" },
        "path": timing.path.label(),
        "dominant_non_total_phase": dominant_display_refresh_phase(timing),
        "phases_ms": {
            "render": timing.render_ms,
            "gpu_upload": timing.gpu_upload_ms,
            "gpu_compute": timing.gpu_compute_ms,
            "egui_texture_registration": timing.egui_texture_ms,
            "visible_brick_request": timing.visible_brick_request_ms,
            "cpu_texture_update": timing.cpu_texture_update_ms,
            "total_refresh": timing.total_ms,
        },
    })
}

pub(crate) fn display_refresh_timing_summary_json(
    samples: &[ProductAutomationDisplayRefreshSample],
) -> Value {
    let mut render = Vec::with_capacity(samples.len());
    let mut gpu_upload = Vec::new();
    let mut gpu_compute = Vec::new();
    let mut egui_texture_registration = Vec::with_capacity(samples.len());
    let mut visible_brick_request = Vec::with_capacity(samples.len());
    let mut cpu_texture_update = Vec::with_capacity(samples.len());
    let mut total_refresh = Vec::with_capacity(samples.len());
    let mut gpu_path_samples = 0_u64;
    let mut cpu_path_samples = 0_u64;
    let mut dominant_counts = Map::new();

    for sample in samples {
        let timing = sample.timing;
        render.push(timing.render_ms);
        if let Some(value) = timing.gpu_upload_ms {
            gpu_upload.push(value);
        }
        if let Some(value) = timing.gpu_compute_ms {
            gpu_compute.push(value);
        }
        egui_texture_registration.push(timing.egui_texture_ms);
        visible_brick_request.push(timing.visible_brick_request_ms);
        cpu_texture_update.push(timing.cpu_texture_update_ms);
        total_refresh.push(timing.total_ms);
        match timing.path {
            crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay => gpu_path_samples += 1,
            crate::display_refresh::DisplayRefreshPath::CpuTexture => cpu_path_samples += 1,
        }
        let dominant = dominant_display_refresh_phase(timing);
        let current = dominant_counts
            .get(dominant)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        dominant_counts.insert(dominant.to_owned(), json!(current + 1));
    }

    let gpu_upload_timing_status = if gpu_upload.is_empty() {
        "not_reported_in_samples"
    } else {
        "measured_in_some_samples"
    };
    let gpu_compute_timing_status = if gpu_compute.is_empty() {
        "not_reported_in_samples"
    } else {
        "measured_in_some_samples"
    };
    let phase_summaries = [
        ("render", timing_values_summary_json(render)),
        ("gpu_upload", timing_values_summary_json(gpu_upload)),
        ("gpu_compute", timing_values_summary_json(gpu_compute)),
        (
            "egui_texture_registration",
            timing_values_summary_json(egui_texture_registration),
        ),
        (
            "visible_brick_request",
            timing_values_summary_json(visible_brick_request),
        ),
        (
            "cpu_texture_update",
            timing_values_summary_json(cpu_texture_update),
        ),
        ("total_refresh", timing_values_summary_json(total_refresh)),
    ];
    let dominant_by_p95 = phase_summaries
        .iter()
        .filter(|(phase, _)| *phase != "total_refresh")
        .filter_map(|(phase, summary)| {
            summary
                .get("p95")
                .and_then(Value::as_f64)
                .map(|p95| (*phase, p95))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(phase, _)| phase);
    let phases_ms = phase_summaries
        .into_iter()
        .map(|(phase, summary)| (phase.to_owned(), summary))
        .collect::<Map<_, _>>();

    json!({
        "kind": "display_refresh_timing_summary",
        "taxonomy_version": 1,
        "measurement_scope": "app_display_refresh_cpu_wall_clock_with_optional_gpu_timestamp_query",
        "phase_measurement_scopes": display_refresh_phase_measurement_scopes_json(),
        "gpu_upload_timing_status": gpu_upload_timing_status,
        "gpu_compute_timing_status": gpu_compute_timing_status,
        "sample_count": samples.len(),
        "path_counts": {
            "gpu display": gpu_path_samples,
            "cpu texture": cpu_path_samples,
        },
        "dominant_non_total_phase_counts": dominant_counts,
        "dominant_non_total_phase_by_p95": dominant_by_p95,
        "phases_ms": phases_ms,
    })
}

pub(crate) fn input_to_present_timing_summary_json(
    samples: &[ProductAutomationInputToPresentSample],
) -> Value {
    let mut latency = Vec::with_capacity(samples.len());
    let mut gpu_path_samples = 0_u64;
    let mut cpu_path_samples = 0_u64;
    let mut command_counts = Map::new();

    for sample in samples {
        latency.push(sample.latency_ms);
        match sample.display_refresh_timing.path {
            crate::display_refresh::DisplayRefreshPath::GpuResidentDisplay => gpu_path_samples += 1,
            crate::display_refresh::DisplayRefreshPath::CpuTexture => cpu_path_samples += 1,
        }
        let current = command_counts
            .get(sample.command)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        command_counts.insert(sample.command.to_owned(), json!(current + 1));
    }

    json!({
        "kind": "input_to_present_proxy_timing_summary",
        "taxonomy_version": 1,
        "measurement_scope": input_to_present_measurement_scope(),
        "sample_count": samples.len(),
        "presentation_proxy": "app_display_refresh_complete",
        "latency_ms": timing_values_summary_json(latency),
        "path_counts": {
            "gpu display": gpu_path_samples,
            "cpu texture": cpu_path_samples,
        },
        "command_counts": command_counts,
    })
}

pub(crate) fn cross_section_latency_summary_json(
    samples: &[ProductAutomationCrossSectionLatencySample],
    pending_count: usize,
) -> Value {
    let mut all_latency = Vec::with_capacity(samples.len());
    let mut warm_latency = Vec::new();
    let mut operation_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut panel_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut operation_counts = Map::new();
    let mut panel_counts = Map::new();

    for sample in samples {
        all_latency.push(sample.latency_ms);
        if latency_gate_kind(sample.operation) == "warm_interaction" {
            warm_latency.push(sample.latency_ms);
        }
        operation_values
            .entry(sample.operation.to_owned())
            .or_default()
            .push(sample.latency_ms);
        panel_values
            .entry(sample.panel_id.label().to_owned())
            .or_default()
            .push(sample.latency_ms);
        increment_count(&mut operation_counts, sample.operation);
        increment_count(&mut panel_counts, sample.panel_id.label());
    }

    let mut operation_gate_statuses = Vec::new();
    let by_operation = operation_values
        .into_iter()
        .map(|(operation, values)| {
            let summary = timing_values_summary_json(values);
            let p95 = summary.get("p95").and_then(Value::as_f64);
            let threshold_ms = latency_gate_threshold_ms(&operation);
            let gate_kind = latency_gate_kind(&operation);
            let gate_status = latency_gate_status(p95, threshold_ms);
            operation_gate_statuses.push(gate_status);
            let value = json!({
                "latency_ms": summary,
                "latency_gate": {
                    "kind": gate_kind,
                    "threshold_ms": threshold_ms,
                    "status": gate_status,
                },
                "warm_interaction_gate": {
                    "threshold_ms": threshold_ms,
                    "status": gate_status,
                    "compatibility_note": "deprecated_alias_for_latency_gate",
                },
            });
            (operation, value)
        })
        .collect::<Map<_, _>>();
    let by_panel = panel_values
        .into_iter()
        .map(|(panel, values)| (panel, timing_values_summary_json(values)))
        .collect::<Map<_, _>>();
    let all_summary = timing_values_summary_json(all_latency);
    let warm_summary = timing_values_summary_json(warm_latency);
    let warm_p95 = warm_summary.get("p95").and_then(Value::as_f64);

    json!({
        "kind": "cross_section_latency_summary",
        "taxonomy_version": 1,
        "measurement_scope": cross_section_latency_measurement_scope(),
        "presentation_proxy": "panel_displayed_generation_with_gpu_display_frame",
        "sample_count": samples.len(),
        "pending_sample_count": pending_count,
        "latency_ms": all_summary,
        "operation_counts": operation_counts,
        "panel_counts": panel_counts,
        "by_operation": by_operation,
        "by_panel": by_panel,
        "operation_gate": {
            "status": aggregate_gate_status(&operation_gate_statuses),
            "policy": {
                "warm_interaction_threshold_ms": warm_interaction_threshold_ms(),
                "cold_timepoint_current_partial_threshold_ms": cold_current_partial_threshold_ms(),
            },
        },
        "warm_interaction_gate": {
            "threshold_ms": warm_interaction_threshold_ms(),
            "status": latency_gate_status(warm_p95, warm_interaction_threshold_ms()),
            "latency_ms": warm_summary,
        },
    })
}

fn input_to_present_measurement_scope() -> &'static str {
    "automation_command_start_to_app_display_refresh_complete"
}

fn cross_section_latency_measurement_scope() -> &'static str {
    "automation_cross_section_command_start_to_panel_displayed_generation"
}

fn latency_gate_threshold_ms(operation: &str) -> f64 {
    if operation == "timepoint_change" {
        cold_current_partial_threshold_ms()
    } else {
        warm_interaction_threshold_ms()
    }
}

fn latency_gate_kind(operation: &str) -> &'static str {
    if operation == "timepoint_change" {
        "cold_current_partial"
    } else {
        "warm_interaction"
    }
}

fn warm_interaction_threshold_ms() -> f64 {
    250.0
}

fn cold_current_partial_threshold_ms() -> f64 {
    2000.0
}

fn latency_gate_status(p95: Option<f64>, threshold_ms: f64) -> &'static str {
    match p95 {
        Some(value) if value <= threshold_ms => "passed",
        Some(_) => "failed",
        None => "insufficient_samples",
    }
}

fn aggregate_gate_status(statuses: &[&'static str]) -> &'static str {
    if statuses.contains(&"failed") {
        "failed"
    } else if statuses.contains(&"passed") {
        "passed"
    } else {
        "insufficient_samples"
    }
}

fn increment_count(counts: &mut Map<String, Value>, key: &'static str) {
    let current = counts.get(key).and_then(Value::as_u64).unwrap_or(0);
    counts.insert(key.to_owned(), json!(current + 1));
}

fn app_update_phase_measurement_scopes_json() -> Value {
    json!({
        "setup": "cpu_wall_clock",
        "task_drain": "cpu_wall_clock",
        "playback": "cpu_wall_clock",
        "ui_build": "cpu_wall_clock",
        "histogram_ui": "cpu_wall_clock_subset_of_ui_build",
        "command_apply": "cpu_wall_clock",
        "display_refresh_trigger": "cpu_wall_clock",
        "import_action": "cpu_wall_clock",
        "brick_result_drain": "cpu_wall_clock",
        "background_repaint_request": "cpu_wall_clock",
        "automation_step": "cpu_wall_clock",
        "total_update": "cpu_wall_clock",
    })
}

fn display_refresh_phase_measurement_scopes_json() -> Value {
    json!({
        "render": "cpu_wall_clock",
        "gpu_upload": "renderer_upload_cpu_wall_clock_subset_of_render",
        "gpu_compute": "wgpu_timestamp_query_elapsed_when_enabled",
        "egui_texture_registration": "cpu_wall_clock",
        "visible_brick_request": "cpu_wall_clock",
        "cpu_texture_update": "cpu_wall_clock",
        "total_refresh": "cpu_wall_clock",
    })
}

pub(crate) fn timing_values_summary_json(mut values: Vec<f64>) -> Value {
    values.retain(|value| value.is_finite());
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return json!({
            "sample_count": 0,
            "p50": Value::Null,
            "p95": Value::Null,
            "p99": Value::Null,
            "max": Value::Null,
        });
    }
    json!({
        "sample_count": values.len(),
        "p50": nearest_rank_percentile(&values, 0.50),
        "p95": nearest_rank_percentile(&values, 0.95),
        "p99": nearest_rank_percentile(&values, 0.99),
        "max": values[values.len() - 1],
    })
}

fn nearest_rank_percentile(sorted_values: &[f64], quantile: f64) -> f64 {
    debug_assert!(!sorted_values.is_empty());
    let rank = (quantile.clamp(0.0, 1.0) * sorted_values.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted_values.len() - 1);
    sorted_values[index]
}

fn dominant_display_refresh_phase(timing: DisplayRefreshTiming) -> &'static str {
    let mut dominant = ("render", timing.render_ms);
    if let Some(gpu_upload_ms) = timing.gpu_upload_ms
        && gpu_upload_ms > dominant.1
    {
        dominant = ("gpu_upload", gpu_upload_ms);
    }
    for phase in [
        ("egui_texture_registration", timing.egui_texture_ms),
        ("visible_brick_request", timing.visible_brick_request_ms),
        ("cpu_texture_update", timing.cpu_texture_update_ms),
    ] {
        if phase.1 > dominant.1 {
            dominant = phase;
        }
    }
    if let Some(gpu_compute_ms) = timing.gpu_compute_ms
        && gpu_compute_ms > dominant.1
    {
        dominant = ("gpu_compute", gpu_compute_ms);
    }
    dominant.0
}
