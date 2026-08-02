//! Developer-local trace for the real-window interaction continuity check.
//!
//! This is deliberately not product automation or a qualification protocol.
//! It observes input after winit/egui receipt and main-loop progress, retains a
//! bounded in-memory timeline, and writes one ignored local CSV at exit.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui;
use mirante4d_application::PresentationSlot;
use rustix::time::{ClockId, clock_gettime};

const TRACE_DIR_ENV: &str = "MIRANTE4D_REAL_INTERACTION_TRACE_DIR";
const TRACE_CAPACITY: usize = 131_072;
const INPUT_RECEIPT_WRITE_INTERVAL: u64 = 30;
const UI_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(250);

thread_local! {
    static TRACE: RefCell<Option<RealInteractionTrace>> =
        RefCell::new(RealInteractionTrace::from_env());
}

#[derive(Debug, Clone, Copy)]
struct TraceEvent {
    monotonic_ns: u64,
    realtime_ns: u64,
    kind: &'static str,
    x: f32,
    y: f32,
    value: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TargetRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

struct RealInteractionTrace {
    directory: PathBuf,
    events: Vec<TraceEvent>,
    live_writer: BufWriter<File>,
    flushed_event_count: usize,
    dropped_events: u64,
    ready_written: bool,
    presentation_targets: [Option<TargetRect>; 4],
    presentation_targets_written: bool,
    presentation_target_change_recorded: bool,
    gesture_armed_written: bool,
    primary_pointer_down: bool,
    last_ui_status_value: Option<u64>,
    last_linked_lod_value: Option<u64>,
    last_linked_runtime_status_value: Option<u64>,
    final_ui_end_value: Option<u64>,
    last_boundary_counters: BTreeMap<&'static str, u64>,
    input_scroll_receipts: u64,
    shifted_drag_receipts: u64,
    ui_turn_receipts: u64,
    last_ui_heartbeat_at: Option<Instant>,
}

impl RealInteractionTrace {
    fn from_env() -> Option<Self> {
        let directory = env::var_os(TRACE_DIR_ENV).map(PathBuf::from)?;
        if !directory.is_absolute() || !directory.is_dir() {
            eprintln!(
                "real_interaction_trace disabled: {TRACE_DIR_ENV} must name an existing absolute directory"
            );
            return None;
        }
        let trace_path = directory.join("app-trace.csv");
        let mut live_writer = match File::create(&trace_path).map(BufWriter::new) {
            Ok(writer) => writer,
            Err(error) => {
                eprintln!(
                    "real_interaction_trace disabled: could not create {}: {error}",
                    trace_path.display()
                );
                return None;
            }
        };
        if writeln!(live_writer, "monotonic_ns,realtime_ns,kind,x,y,value")
            .and_then(|_| live_writer.flush())
            .is_err()
        {
            eprintln!(
                "real_interaction_trace disabled: could not initialize {}",
                trace_path.display()
            );
            return None;
        }
        Some(Self {
            directory,
            events: Vec::with_capacity(TRACE_CAPACITY),
            live_writer,
            flushed_event_count: 0,
            dropped_events: 0,
            ready_written: false,
            presentation_targets: [None; 4],
            presentation_targets_written: false,
            presentation_target_change_recorded: false,
            gesture_armed_written: false,
            primary_pointer_down: false,
            last_ui_status_value: None,
            last_linked_lod_value: None,
            last_linked_runtime_status_value: None,
            final_ui_end_value: None,
            last_boundary_counters: BTreeMap::new(),
            input_scroll_receipts: 0,
            shifted_drag_receipts: 0,
            ui_turn_receipts: 0,
            last_ui_heartbeat_at: None,
        })
    }

    fn push(&mut self, kind: &'static str, x: f32, y: f32, value: u64) {
        let (monotonic_ns, realtime_ns) = clock_pair_ns();
        self.push_at(monotonic_ns, realtime_ns, kind, x, y, value);
    }

    fn push_at(
        &mut self,
        monotonic_ns: u64,
        realtime_ns: u64,
        kind: &'static str,
        x: f32,
        y: f32,
        value: u64,
    ) {
        if self.events.len() >= TRACE_CAPACITY {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return;
        }
        self.events.push(TraceEvent {
            monotonic_ns,
            realtime_ns,
            kind,
            x,
            y,
            value,
        });
    }

    fn record_raw_input(&mut self, raw_input: &egui::RawInput) {
        let (monotonic_ns, realtime_ns) = clock_pair_ns();
        self.push_at(monotonic_ns, realtime_ns, "ui_begin", 0.0, 0.0, 0);
        let mut write_live_receipts = false;
        for event in &raw_input.events {
            let (kind, position, event_flags, shift) = match event {
                egui::Event::PointerMoved(position) => {
                    ("input_move", *position, 0_u64, raw_input.modifiers.shift)
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    if *button == egui::PointerButton::Primary {
                        self.primary_pointer_down = *pressed;
                    }
                    (
                        if *pressed {
                            "input_button_down"
                        } else {
                            "input_button_up"
                        },
                        *pos,
                        0_u64,
                        modifiers.shift,
                    )
                }
                egui::Event::MouseWheel {
                    delta, modifiers, ..
                } => (
                    "input_scroll",
                    egui::pos2(delta.x, delta.y),
                    u64::from(delta.y.to_bits()),
                    modifiers.shift,
                ),
                _ => continue,
            };
            let pointer_flags = u64::from(self.primary_pointer_down) | (u64::from(shift) << 1);
            self.push(
                kind,
                position.x,
                position.y,
                pointer_flags | (event_flags << 2),
            );
            if kind == "input_scroll" {
                self.input_scroll_receipts = self.input_scroll_receipts.saturating_add(1);
                write_live_receipts = true;
            } else if kind == "input_move" && self.primary_pointer_down && shift {
                self.shifted_drag_receipts = self.shifted_drag_receipts.saturating_add(1);
                // A 2 Hz live handshake is sufficient for the external
                // watchdog without adding a filesystem write to every 60 Hz
                // motion sample.
                write_live_receipts |= self.shifted_drag_receipts == 1
                    || self
                        .shifted_drag_receipts
                        .is_multiple_of(INPUT_RECEIPT_WRITE_INTERVAL);
            }
            if self.primary_pointer_down && shift {
                self.write_gesture_armed_once();
            }
        }
        if write_live_receipts {
            self.write_live_input_receipts();
            if let Err(error) = self.flush_live_events() {
                eprintln!("real_interaction_trace could not checkpoint input events: {error}");
            }
        }
    }

    fn write_ready_once(&mut self) {
        if self.ready_written {
            return;
        }
        let path = self.directory.join("ready");
        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                let (monotonic_ns, realtime_ns) = clock_pair_ns();
                writeln!(file, "monotonic_ns={monotonic_ns}")?;
                writeln!(file, "realtime_ns={realtime_ns}")?;
                file.flush()
            });
        match result {
            Ok(()) => self.ready_written = true,
            Err(_error) if path.exists() => self.ready_written = true,
            Err(error) => {
                eprintln!("real_interaction_trace could not write readiness: {error}");
            }
        }
    }

    fn write_presentation_target_once(
        &mut self,
        slot: PresentationSlot,
        rect: egui::Rect,
        pixels_per_point: f32,
    ) {
        if !rect.is_finite() || !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return;
        }
        let x = (rect.min.x * pixels_per_point).floor() as i32;
        let y = (rect.min.y * pixels_per_point).floor() as i32;
        let right = (rect.max.x * pixels_per_point).ceil() as i32;
        let bottom = (rect.max.y * pixels_per_point).ceil() as i32;
        let width = right.saturating_sub(x);
        let height = bottom.saturating_sub(y);
        if width < 32 || height < 32 {
            return;
        }
        let target = TargetRect {
            x,
            y,
            width,
            height,
        };
        let target_index = presentation_slot_index(slot);
        if self.presentation_targets_written {
            if self.presentation_targets[target_index] != Some(target)
                && !self.presentation_target_change_recorded
            {
                self.presentation_target_change_recorded = true;
                self.push(
                    "presentation_target_changed",
                    target.x as f32,
                    target.y as f32,
                    u64::try_from(target_index).unwrap_or(u64::MAX),
                );
            }
            return;
        }
        self.presentation_targets[target_index] = Some(target);
        let [Some(three_d), Some(xy), Some(xz), Some(yz)] = self.presentation_targets else {
            return;
        };
        let path = self.directory.join("interaction-target.txt");
        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                for (name, target) in [("three_d", three_d), ("xy", xy), ("xz", xz), ("yz", yz)] {
                    writeln!(file, "{name}_x={}", target.x)?;
                    writeln!(file, "{name}_y={}", target.y)?;
                    writeln!(file, "{name}_width={}", target.width)?;
                    writeln!(file, "{name}_height={}", target.height)?;
                }
                file.flush()
            });
        match result {
            Ok(()) => self.presentation_targets_written = true,
            Err(_error) if path.exists() => self.presentation_targets_written = true,
            Err(error) => {
                eprintln!("real_interaction_trace could not write interaction target: {error}");
            }
        }
    }

    fn write_gesture_armed_once(&mut self) {
        if self.gesture_armed_written {
            return;
        }
        let path = self.directory.join("gesture-armed");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let (monotonic_ns, realtime_ns) = clock_pair_ns();
                if writeln!(file, "monotonic_ns={monotonic_ns}")
                    .and_then(|_| writeln!(file, "realtime_ns={realtime_ns}"))
                    .and_then(|_| file.flush())
                    .is_ok()
                {
                    self.gesture_armed_written = true;
                }
            }
            Err(_error) if path.exists() => self.gesture_armed_written = true,
            Err(error) => {
                eprintln!("real_interaction_trace could not write gesture readiness: {error}");
            }
        }
    }

    fn write_live_lod_status(&self, value: u64) {
        let path = self.directory.join("lod-status.txt");
        if let Err(error) = replace_live_text(&path, &format!("{value}\n")) {
            eprintln!("real_interaction_trace could not write live LOD status: {error}");
        }
    }

    fn write_live_input_receipts(&self) {
        let path = self.directory.join("input-receipts.txt");
        let contents = format!(
            "scroll={}\nshift_drag={}\nui_turn={}\n",
            self.input_scroll_receipts, self.shifted_drag_receipts, self.ui_turn_receipts
        );
        if let Err(error) = replace_live_text(&path, &contents) {
            eprintln!("real_interaction_trace could not write live input receipts: {error}");
        }
    }

    fn flush_live_events(&mut self) -> std::io::Result<()> {
        for event in &self.events[self.flushed_event_count..] {
            write_trace_event(&mut self.live_writer, *event)?;
        }
        self.flushed_event_count = self.events.len();
        self.live_writer.flush()
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.flush_live_events()?;
        if let Some(value) = self.final_ui_end_value {
            let (monotonic_ns, realtime_ns) = clock_pair_ns();
            writeln!(
                self.live_writer,
                "{monotonic_ns},{realtime_ns},ui_end,0.000,0.000,{value}"
            )?;
        }
        if self.dropped_events > 0 {
            let (monotonic_ns, realtime_ns) = clock_pair_ns();
            writeln!(
                self.live_writer,
                "{monotonic_ns},{realtime_ns},dropped_events,0.000,0.000,{}",
                self.dropped_events
            )?;
        }
        self.live_writer.flush()
    }
}

pub(crate) fn record_raw_input(raw_input: &egui::RawInput) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.record_raw_input(raw_input);
    })
}

pub(crate) fn record_camera_sample(camera: mirante4d_domain::CameraView, currentness: u64) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.push(
            "camera_sample",
            camera.orthographic_world_per_screen_point() as f32,
            camera.perspective_focal_length_screen_points() as f32,
            currentness,
        );
    });
}

/// Records the renderer cutoff that backed a semantic presentation update.
/// `y` is the cutoff's color-submission count, plus 1024 when this target was
/// actually published by the report. This remains developer-local causal
/// evidence; it is not a product acceptance oracle.
pub(crate) fn record_coordinated_execution(
    target: mirante4d_render_api::PresentationTarget,
    frame: mirante4d_render_api::FrameIdentity,
    cross_section_scale: Option<f64>,
    presented: bool,
    color_submissions: u32,
) {
    let kind = match target {
        mirante4d_render_api::PresentationTarget::ThreeD => "internal_target_publication_3d",
        mirante4d_render_api::PresentationTarget::Xy => "internal_target_publication_xy",
        mirante4d_render_api::PresentationTarget::Xz => "internal_target_publication_xz",
        mirante4d_render_api::PresentationTarget::Yz => "internal_target_publication_yz",
    };
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.push(
            kind,
            cross_section_scale.unwrap_or(f64::NAN) as f32,
            color_submissions as f32 + if presented { 1024.0 } else { 0.0 },
            frame.get(),
        );
    });
}

pub(crate) fn record_ui_update_duration(duration_ns: u64) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.push("ui_update_duration", 0.0, 0.0, duration_ns);
    });
}

pub(crate) fn record_renderer_cpu_timing(timing: mirante4d_render_wgpu::CpuFrameTiming) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.push(
            "renderer_cpu_timing",
            timing.planning_ns() as f32,
            timing.queue_submit_ns() as f32,
            0,
        );
    });
}

pub(crate) fn record_gpu_timing(timing: mirante4d_render_wgpu::GpuFrameTiming) {
    let kind = match timing.target() {
        mirante4d_render_api::PresentationTarget::ThreeD => "gpu_timing_3d",
        mirante4d_render_api::PresentationTarget::Xy => "gpu_timing_xy",
        mirante4d_render_api::PresentationTarget::Xz => "gpu_timing_xz",
        mirante4d_render_api::PresentationTarget::Yz => "gpu_timing_yz",
    };
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.push(
            kind,
            timing.batch_gpu_envelope_ns().unwrap_or(u64::MAX) as f32,
            timing.render_pass_ns().unwrap_or(u64::MAX) as f32,
            timing.generation().get(),
        );
    });
}

pub(crate) fn record_egui_texture_paint_queued(
    target: mirante4d_render_api::PresentationTarget,
    texture_revision: u64,
) {
    let kind = match target {
        mirante4d_render_api::PresentationTarget::ThreeD => "egui_texture_paint_queued_3d",
        mirante4d_render_api::PresentationTarget::Xy => "egui_texture_paint_queued_xy",
        mirante4d_render_api::PresentationTarget::Xz => "egui_texture_paint_queued_xz",
        mirante4d_render_api::PresentationTarget::Yz => "egui_texture_paint_queued_yz",
    };
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.push(kind, 0.0, 0.0, texture_revision);
    });
}

pub(crate) fn record_boundary_counter(kind: &'static str, value: u64) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        if trace.last_boundary_counters.get(kind) == Some(&value) {
            return;
        }
        trace.last_boundary_counters.insert(kind, value);
        trace.push(kind, 0.0, 0.0, value);
    });
}

pub(crate) fn enabled() -> bool {
    TRACE.with(|slot| slot.borrow().is_some())
}

/// Records the three linked panels without reusing the independent 3D LOD
/// projection. Each 16-bit panel word contains ideal, installed, displayed,
/// exact-current, provisional, and display-current facts.
pub(crate) type LinkedLodTracePanel = (Option<u32>, Option<u32>, Option<u32>, bool, bool, bool);

pub(crate) fn record_linked_lod_status(panels: [LinkedLodTracePanel; 3]) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        let encode_scale = |scale: Option<u32>| {
            u64::from(
                scale
                    .and_then(|scale| u8::try_from(scale).ok())
                    .filter(|scale| *scale < 15)
                    .unwrap_or(15),
            )
        };
        let value = panels
            .into_iter()
            .enumerate()
            .fold(0_u64, |value, (index, panel)| {
                let (ideal, installed, displayed, exact, provisional, display_current) = panel;
                let panel_word = encode_scale(ideal)
                    | (encode_scale(installed) << 4)
                    | (encode_scale(displayed) << 8)
                    | (u64::from(exact) << 12)
                    | (u64::from(provisional) << 13)
                    | (u64::from(display_current) << 14);
                value | (panel_word << (index * 16))
            });
        if trace.last_linked_lod_value == Some(value) {
            return;
        }
        trace.last_linked_lod_value = Some(value);
        trace.push("linked_lod_status", 0.0, 0.0, value);
        let path = trace.directory.join("linked-lod-status.txt");
        if let Err(error) = replace_live_text(&path, &format!("{value}\n")) {
            eprintln!("real_interaction_trace could not write linked LOD status: {error}");
        }
    });
}

/// Writes compact live worker/residency facts for diagnosing a finite linked
/// workflow failure. This is ignored developer-local trace state, not product
/// currentness or an acceptance oracle.
pub(crate) fn record_linked_runtime_status(value: u64) -> bool {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return false;
        };
        if trace.last_linked_runtime_status_value == Some(value) {
            return false;
        }
        trace.last_linked_runtime_status_value = Some(value);
        trace.push("linked_runtime_status", 0.0, 0.0, value);
        let path = trace.directory.join("linked-runtime-status.txt");
        if let Err(error) = replace_live_text(&path, &format!("{value}\n")) {
            eprintln!("real_interaction_trace could not write linked runtime status: {error}");
        }
        true
    })
}

pub(crate) fn record_linked_runtime_detail(detail: &str) {
    TRACE.with(|slot| {
        let slot = slot.borrow();
        let Some(trace) = slot.as_ref() else {
            return;
        };
        let path = trace.directory.join("linked-runtime-detail.txt");
        if let Err(error) = File::create(path).and_then(|mut file| {
            file.write_all(detail.as_bytes())?;
            file.flush()
        }) {
            eprintln!("real_interaction_trace could not write linked runtime detail: {error}");
        }
    });
}

pub(crate) fn record_presentation_target(
    target_slot: PresentationSlot,
    rect: egui::Rect,
    pixels_per_point: f32,
) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        trace.write_presentation_target_once(target_slot, rect, pixels_per_point);
    });
}

const fn presentation_slot_index(slot: PresentationSlot) -> usize {
    match slot {
        PresentationSlot::ThreeD => 0,
        PresentationSlot::Xy => 1,
        PresentationSlot::Xz => 2,
        PresentationSlot::Yz => 3,
    }
}

pub(crate) fn record_ui_end(
    ready: bool,
    displayed_scale: Option<u32>,
    selected_scale: Option<u32>,
    ideal_scale: Option<u32>,
    state_flags: u64,
) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else {
            return;
        };
        let displayed = displayed_scale.map_or(u64::from(u8::MAX), u64::from);
        let selected = selected_scale.map_or(u64::from(u8::MAX), u64::from);
        let ideal = ideal_scale.map_or(u64::from(u8::MAX), u64::from);
        let value = u64::from(ready)
            | (displayed << 8)
            | (selected << 16)
            | (ideal << 24)
            | (state_flags << 32);
        let lod_policy_flags = state_flags & ((1 << 6) | (1 << 7) | (1 << 8));
        let status_key =
            (displayed << 8) | (selected << 16) | (ideal << 24) | (lod_policy_flags << 32);
        if trace.last_ui_status_value != Some(status_key) {
            trace.push("ui_status", 0.0, 0.0, value);
            trace.write_live_lod_status(value);
            trace.last_ui_status_value = Some(status_key);
        }
        trace.final_ui_end_value = Some(value);
        trace.ui_turn_receipts = trace.ui_turn_receipts.saturating_add(1);
        let now = Instant::now();
        if trace
            .last_ui_heartbeat_at
            .is_none_or(|last| now.duration_since(last) >= UI_HEARTBEAT_INTERVAL)
        {
            trace.write_live_input_receipts();
            if let Err(error) = trace.flush_live_events() {
                eprintln!("real_interaction_trace could not checkpoint UI events: {error}");
            }
            trace.last_ui_heartbeat_at = Some(now);
        }
        if ready {
            trace.write_ready_once();
        }
    });
}

pub(crate) fn finish() {
    TRACE.with(|slot| {
        let Some(trace) = slot.borrow_mut().take() else {
            return;
        };
        if let Err(error) = trace.finish() {
            eprintln!("real_interaction_trace could not write app timeline: {error}");
        }
    });
}

fn clock_pair_ns() -> (u64, u64) {
    (clock_ns(ClockId::Monotonic), clock_ns(ClockId::Realtime))
}

fn replace_live_text(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
}

fn write_trace_event(writer: &mut impl Write, event: TraceEvent) -> std::io::Result<()> {
    writeln!(
        writer,
        "{},{},{},{:.3},{:.3},{}",
        event.monotonic_ns, event.realtime_ns, event.kind, event.x, event.y, event.value
    )
}

fn clock_ns(clock: ClockId) -> u64 {
    let time = clock_gettime(clock);
    u64::try_from(time.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(time.tv_nsec).unwrap_or(0))
}
