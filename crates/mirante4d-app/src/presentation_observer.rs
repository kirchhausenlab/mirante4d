//! Opt-in Vulkan presentation observation for the trusted local GPU campaign.
//!
//! The ordinary product does not create this observer. When the private
//! campaign supplies an output path, the final WGPU Vulkan swapchain assigns a
//! present ID. A bounded worker waits for that ID away from the UI and WGPU
//! presentation threads, then decodes a tiny foreground marker painted by the
//! normal egui frame. X11 remains only the independent marker and window-
//! lifecycle observer. Wait-return timestamps establish visibility, maximum
//! stalls, and coarse settlement. Correlated
//! `VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT` timestamps establish exact
//! scanout cadence, but do not claim physical photon visibility.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use ash::{khr, vk};
use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::Serialize;
use serde_json::{Value, json};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xproto::{ChangeWindowAttributesAux, ConnectionExt as _, EventMask, ImageFormat, MapState},
    },
};

const OUTPUT_ENV: &str = "MIRANTE4D_PRESENTATION_OBSERVER_REPORT";
const REPORT_SCHEMA: &str = "mirante4d-vulkan-present-timing-observer-report";
const REPORT_SCHEMA_VERSION: u32 = 2;
const AUTHORITY: &str = "vulkan_ext_present_timing_first_pixel_out_marker_v2";
const MARKER_COLUMNS: usize = 10;
const MARKER_ROWS: usize = 6;
const MARKER_CELLS: usize = MARKER_COLUMNS * MARKER_ROWS;
const MARKER_X_POINTS: f32 = 4.0;
const MARKER_Y_POINTS: f32 = 4.0;
const MARKER_CELL_POINTS: f32 = 2.0;
const MAX_RECORDS: usize = 16_384;
const MAX_WAIT_TASKS: usize = 256;
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const TIMING_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MARKER_SETTLE_TIMEOUT: Duration = Duration::from_millis(100);

type HalTimingConfiguration =
    wgpu::hal::vulkan::present_wait_observer::PresentationTimingConfiguration;
type HalTimingQuery = wgpu::hal::vulkan::present_wait_observer::PresentationTimingQuery;
type HalTimingRecord = wgpu::hal::vulkan::present_wait_observer::PresentationTimingRecord;
type HalTimingDevice = wgpu::hal::vulkan::present_wait_observer::PresentationTimingDevice;
type HalPresentationReservation = wgpu::hal::vulkan::present_wait_observer::PresentationReservation;

#[derive(Debug, Clone)]
pub(crate) struct PresentationObservation {
    pub(crate) scenario: String,
    pub(crate) phase: String,
    pub(crate) command_index: usize,
    pub(crate) active_input: bool,
    pub(crate) eligible: bool,
    pub(crate) exact: bool,
    pub(crate) identity: Value,
    pub(crate) input_generation: u64,
    pub(crate) input_age_ns: u64,
    pub(crate) surface_generation: u64,
    pub(crate) work_identity: Value,
    pub(crate) environment_identity: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerSpec {
    x: i16,
    y: i16,
    cell_pixels: u16,
}

impl MarkerSpec {
    fn width(self) -> u16 {
        self.cell_pixels.saturating_mul(MARKER_COLUMNS as u16)
    }

    fn height(self) -> u16 {
        self.cell_pixels.saturating_mul(MARKER_ROWS as u16)
    }
}

#[derive(Debug, Clone)]
struct PendingFrame {
    marker_sequence: u32,
    enqueued_at_ns: u64,
    observation: PresentationObservation,
}

#[derive(Debug, Clone, Copy)]
struct PresentBinding {
    swapchain: u64,
    swapchain_generation: u64,
    marker_sequence: u32,
    marker_spec: MarkerSpec,
    window_lifecycle_generation: u64,
    submitted_at_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct PresentWaitTask {
    swapchain: vk::SwapchainKHR,
    swapchain_generation: u64,
    present_id: u64,
}

#[derive(Debug, Clone, Copy)]
struct ConfiguredSwapchain {
    generation: u64,
    configuration: HalTimingConfiguration,
}

#[derive(Debug, Clone, Copy)]
struct TimingBinding {
    swapchain: u64,
    swapchain_generation: u64,
    submitted_at_ns: u64,
}

#[derive(Debug, Clone, Copy)]
struct EarlyTimingRecord {
    raw_swapchain: u64,
    configured: ConfiguredSwapchain,
    record: HalTimingRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ScanoutTiming {
    swapchain_generation: u64,
    first_pixel_out_ns: u64,
    time_domain: i32,
    time_domain_id: u64,
}

#[derive(Debug, Clone)]
struct QualifiedPresent {
    marker_sequence: u32,
    present_id: u64,
    observed_at_ns: u64,
    wait_return_delay_ns: u64,
    phase: String,
    command_index: usize,
    active_input: bool,
    exact: bool,
    identity: Value,
    input_generation: u64,
    input_to_visible_coarse_ns: u64,
    surface_generation: u64,
    work_identity: Value,
    swapchain_generation: u64,
}

#[derive(Debug, Clone)]
enum MarkerOutcome {
    Qualified(Box<QualifiedPresent>),
    Nonqualifying,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct TimingCounterState {
    timing_properties_counter: Option<u64>,
    time_domains_counter: Option<u64>,
    refresh_duration_ns: Option<u64>,
    refresh_interval_ns: Option<u64>,
    refresh_properties_counter: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct PresentedRecord {
    marker_sequence: u32,
    present_id: u64,
    observed_at_ns: u64,
    wait_return_delay_ns: u64,
    phase: String,
    command_index: usize,
    active_input: bool,
    exact: bool,
    identity: Value,
    input_generation: u64,
    input_to_visible_coarse_ns: u64,
    surface_generation: u64,
    work_identity: Value,
    swapchain_generation: u64,
    first_pixel_out_ns: u64,
    time_domain: i32,
    time_domain_id: u64,
}

#[derive(Debug, Clone, Serialize)]
struct StateTransition {
    observed_at_ns: u64,
    phase: String,
    active_input: bool,
    command_index: usize,
    input_generation: u64,
}

#[derive(Default)]
struct ObserverState {
    scenario: Option<String>,
    environment_identity: Option<Value>,
    marker_sequence: u32,
    marker_spec: Option<MarkerSpec>,
    last_eligible_identity: Option<Value>,
    last_phase_and_input: Option<(String, bool, usize)>,
    latest_work_identity: Option<Value>,
    pending: BTreeMap<u32, PendingFrame>,
    present_bindings: BTreeMap<u64, PresentBinding>,
    rejected_present_bindings: BTreeMap<u64, PresentBinding>,
    timing_bindings: BTreeMap<u64, TimingBinding>,
    early_timing_records: BTreeMap<u64, EarlyTimingRecord>,
    timing_records: BTreeMap<u64, ScanoutTiming>,
    marker_outcomes: BTreeMap<u64, MarkerOutcome>,
    configured_swapchains: BTreeMap<u64, ConfiguredSwapchain>,
    configured_swapchain_history: BTreeMap<u64, HalTimingConfiguration>,
    timing_counters: BTreeMap<u64, TimingCounterState>,
    completed_timing_ids: BTreeSet<u64>,
    last_timing_present_id: BTreeMap<u64, u64>,
    last_qualified_marker_sequence: Option<u32>,
    last_qualified_marker_present_id: Option<u64>,
    presented: Vec<PresentedRecord>,
    transitions: Vec<StateTransition>,
    complete_events: u64,
    submitted_present_events: u64,
    rejected_present_events: u64,
    benign_rejected_present_events: u64,
    fatal_rejected_present_events: u64,
    present_timing_query_events: u64,
    present_timing_complete_events: u64,
    present_timing_incomplete_events: u64,
    present_timing_duplicate_events: u64,
    present_timing_unknown_id_events: u64,
    present_timing_rejected_result_events: u64,
    present_timing_out_of_order_events: u64,
    present_timing_zero_stage_events: u64,
    present_timing_zero_time_events: u64,
    present_timing_failure_events: u64,
    present_timing_timeout_events: u64,
    present_timing_queue_full_events: u64,
    timing_properties_change_events: u64,
    time_domain_change_events: u64,
    configured_swapchain_events: u64,
    wait_timeout_events: u64,
    wait_failure_events: u64,
    swapchain_changes: u64,
    last_swapchain: Option<u64>,
    unchanged_present_events: u64,
    superseded_before_present: u64,
    superseded_before_marker_observation: u64,
    ambiguous_completion_events: u64,
    nonqualifying_completion_events: u64,
    window_unavailable_completion_events: u64,
    configure_events: u64,
    focus_loss_events: u64,
    occlusion_events: u64,
    measured_focus_loss_events: u64,
    measured_occlusion_events: u64,
    measured_unmap_events: u64,
    map_events: u64,
    unmap_events: u64,
    window_lifecycle_generation: u64,
    map_after_unmap_observed: bool,
    surface_generation_changes: u64,
    last_surface_generation: Option<u64>,
    initial_geometry: Option<(u16, u16)>,
    final_geometry: Option<(u16, u16)>,
    initially_mapped: bool,
    currently_mapped: bool,
    unmap_started_at_ns: Option<u64>,
    map_after_unmap_at_ns: Option<u64>,
    event_error: Option<String>,
    first_ambiguous_completion: Option<String>,
    dropped_records: u64,
}

struct Shared {
    epoch: Instant,
    stop: AtomicBool,
    wait_stop: AtomicBool,
    state: Mutex<ObserverState>,
}

pub(crate) struct PresentationObserver {
    shared: Arc<Shared>,
    lifecycle_worker: Option<JoinHandle<()>>,
    wait_worker: Option<JoinHandle<()>>,
}

struct PresentWaitBridge {
    next_present_id: AtomicU64,
    next_swapchain_generation: AtomicU64,
    task_tx: SyncSender<PresentWaitTask>,
    task_rx: Mutex<Option<Receiver<PresentWaitTask>>>,
    shared: Mutex<Option<Weak<Shared>>>,
    configured_swapchains: Mutex<BTreeMap<u64, ConfiguredSwapchain>>,
    wait_finished: Condvar,
}

/// Keeps the weak WGPU hook live from before device creation until eframe exits.
pub struct PreparedPresentationObserver {
    _bridge: Arc<PresentWaitBridge>,
}

fn prepared_bridge_slot() -> &'static Mutex<Option<Weak<PresentWaitBridge>>> {
    static SLOT: OnceLock<Mutex<Option<Weak<PresentWaitBridge>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn observer_output_path() -> anyhow::Result<Option<PathBuf>> {
    let Some(output_path) = env::var_os(OUTPUT_ENV).map(PathBuf::from) else {
        return Ok(None);
    };
    if !output_path.is_absolute() || output_path.exists() {
        bail!("{OUTPUT_ENV} must name one absent absolute report path");
    }
    let parent = output_path
        .parent()
        .context("presentation observer report path has no parent")?;
    let parent_metadata = fs::symlink_metadata(parent)
        .context("presentation observer report parent is unavailable")?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        bail!("presentation observer report parent must be a real directory");
    }
    Ok(Some(output_path))
}

/// Install the opt-in final-present hook before eframe creates its WGPU device.
pub fn prepare_presentation_observer() -> anyhow::Result<Option<PreparedPresentationObserver>> {
    if observer_output_path()?.is_none() {
        return Ok(None);
    }
    let (task_tx, task_rx) = sync_channel(MAX_WAIT_TASKS);
    let bridge = Arc::new(PresentWaitBridge {
        next_present_id: AtomicU64::new(1),
        next_swapchain_generation: AtomicU64::new(1),
        task_tx,
        task_rx: Mutex::new(Some(task_rx)),
        shared: Mutex::new(None),
        configured_swapchains: Mutex::new(BTreeMap::new()),
        wait_finished: Condvar::new(),
    });
    let observer: Arc<dyn wgpu::hal::vulkan::present_wait_observer::PresentationWaitObserver> =
        bridge.clone();
    wgpu::hal::vulkan::present_wait_observer::install_presentation_wait_observer(&observer)
        .map_err(anyhow::Error::msg)?;
    let mut slot = prepared_bridge_slot()
        .lock()
        .map_err(|_| anyhow::anyhow!("prepared presentation observer lock was poisoned"))?;
    if slot.as_ref().and_then(Weak::upgrade).is_some() {
        bail!("a live prepared presentation observer already exists");
    }
    *slot = Some(Arc::downgrade(&bridge));
    Ok(Some(PreparedPresentationObserver { _bridge: bridge }))
}

impl wgpu::hal::vulkan::present_wait_observer::PresentationWaitObserver for PresentWaitBridge {
    fn presentation_timing_configured(
        &self,
        swapchain: vk::SwapchainKHR,
        configuration: HalTimingConfiguration,
    ) {
        use ash::vk::Handle as _;

        let generation = match self.next_swapchain_generation.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_add(1),
        ) {
            Ok(generation) => generation,
            Err(_) => {
                if let Some(shared) = self
                    .shared
                    .lock()
                    .ok()
                    .and_then(|shared| shared.as_ref().and_then(Weak::upgrade))
                {
                    record_error(&shared, "swapchain timing generation overflowed".to_owned());
                }
                return;
            }
        };
        let configured = ConfiguredSwapchain {
            generation,
            configuration,
        };
        let raw_swapchain = swapchain.as_raw();
        let Ok(mut configurations) = self.configured_swapchains.lock() else {
            return;
        };
        if configurations.insert(raw_swapchain, configured).is_some() {
            if let Some(shared) = self
                .shared
                .lock()
                .ok()
                .and_then(|shared| shared.as_ref().and_then(Weak::upgrade))
            {
                record_error(
                    &shared,
                    "a live raw swapchain handle was configured twice".to_owned(),
                );
            }
            return;
        }
        drop(configurations);
        if let Some(shared) = self
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.as_ref().and_then(Weak::upgrade))
            && let Ok(mut state) = shared.state.lock()
        {
            state
                .configured_swapchains
                .insert(raw_swapchain, configured);
            state
                .configured_swapchain_history
                .insert(configured.generation, configuration);
            state.configured_swapchain_events = state.configured_swapchain_events.saturating_add(1);
        }
    }

    fn reserve_present(&self, swapchain: vk::SwapchainKHR) -> Option<HalPresentationReservation> {
        use ash::vk::Handle as _;

        let raw_swapchain = swapchain.as_raw();
        let configured = self
            .configured_swapchains
            .lock()
            .ok()?
            .get(&raw_swapchain)
            .copied();
        let shared = self.shared.lock().ok()?.as_ref()?.upgrade()?;
        let mut state = shared.state.lock().ok()?;
        let Some(configured) = configured else {
            state
                .event_error
                .get_or_insert_with(|| "present used an unconfigured timing swapchain".to_owned());
            return None;
        };
        let marker_spec = state.marker_spec?;
        if state.present_bindings.len() >= MAX_WAIT_TASKS
            || state.timing_bindings.len() >= MAX_WAIT_TASKS
        {
            if state.present_bindings.len() >= MAX_WAIT_TASKS
                || state.timing_bindings.len() >= MAX_WAIT_TASKS
            {
                state.dropped_records = state.dropped_records.saturating_add(1);
                state.event_error.get_or_insert_with(|| {
                    "presentation observer binding capacity was exhausted".to_owned()
                });
            }
            return None;
        }
        let present_id = self
            .next_present_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .ok()?;
        if state
            .last_swapchain
            .is_some_and(|last| last != raw_swapchain)
        {
            state.swapchain_changes = state.swapchain_changes.saturating_add(1);
        }
        state.last_swapchain = Some(raw_swapchain);
        let marker_sequence = state.marker_sequence;
        let window_lifecycle_generation = state.window_lifecycle_generation;
        state.present_bindings.insert(
            present_id,
            PresentBinding {
                swapchain: raw_swapchain,
                swapchain_generation: configured.generation,
                marker_sequence,
                marker_spec,
                window_lifecycle_generation,
                submitted_at_ns: None,
            },
        );
        Some(HalPresentationReservation {
            present_id,
            time_domain_id: configured.configuration.time_domain_id,
        })
    }

    fn present_submitted(&self, swapchain: vk::SwapchainKHR, present_id: u64) {
        let Some(shared) = self
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.as_ref().and_then(Weak::upgrade))
        else {
            return;
        };
        let mut state = match shared.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        let Some(binding) = state.present_bindings.get_mut(&present_id) else {
            return;
        };
        let submitted_at_ns = elapsed_ns(shared.epoch);
        binding.submitted_at_ns = Some(submitted_at_ns);
        let raw_swapchain = binding.swapchain;
        let swapchain_generation = binding.swapchain_generation;
        state.timing_bindings.insert(
            present_id,
            TimingBinding {
                swapchain: raw_swapchain,
                swapchain_generation,
                submitted_at_ns,
            },
        );
        state.submitted_present_events = state.submitted_present_events.saturating_add(1);
        if let Some(early) = state.early_timing_records.remove(&present_id) {
            accept_timing_record(
                &mut state,
                early.raw_swapchain,
                early.configured,
                early.record,
            );
        }
        drop(state);
        match self.task_tx.try_send(PresentWaitTask {
            swapchain,
            swapchain_generation,
            present_id,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                if let Ok(mut state) = shared.state.lock() {
                    state.present_bindings.remove(&present_id);
                    state
                        .marker_outcomes
                        .insert(present_id, MarkerOutcome::Nonqualifying);
                    state.dropped_records = state.dropped_records.saturating_add(1);
                    state.event_error.get_or_insert_with(|| {
                        "presentation-wait worker queue was unavailable".to_owned()
                    });
                }
                self.wait_finished.notify_all();
            }
        }
    }

    fn present_rejected(&self, _swapchain: vk::SwapchainKHR, present_id: u64, result: vk::Result) {
        let Some(shared) = self
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.as_ref().and_then(Weak::upgrade))
        else {
            return;
        };
        if let Ok(mut state) = shared.state.lock() {
            let binding = state.present_bindings.remove(&present_id);
            if state.early_timing_records.remove(&present_id).is_some() {
                state.present_timing_rejected_result_events = state
                    .present_timing_rejected_result_events
                    .saturating_add(1);
            }
            state.timing_bindings.remove(&present_id);
            state.timing_records.remove(&present_id);
            state.marker_outcomes.remove(&present_id);
            state.rejected_present_events = state.rejected_present_events.saturating_add(1);
            if result.as_raw() == -1_000_208_000 {
                state.fatal_rejected_present_events =
                    state.fatal_rejected_present_events.saturating_add(1);
                state.present_timing_queue_full_events =
                    state.present_timing_queue_full_events.saturating_add(1);
                state.event_error.get_or_insert_with(|| {
                    "Vulkan presentation timing result queue was exhausted".to_owned()
                });
            } else if result == vk::Result::ERROR_OUT_OF_DATE_KHR {
                state.benign_rejected_present_events =
                    state.benign_rejected_present_events.saturating_add(1);
                if let Some(binding) = binding {
                    if state.rejected_present_bindings.len() >= MAX_RECORDS {
                        state.dropped_records = state.dropped_records.saturating_add(1);
                        state.event_error.get_or_insert_with(|| {
                            "rejected-present timing tombstone capacity was exhausted".to_owned()
                        });
                    } else {
                        state.rejected_present_bindings.insert(present_id, binding);
                    }
                }
            } else {
                state.fatal_rejected_present_events =
                    state.fatal_rejected_present_events.saturating_add(1);
                state.event_error.get_or_insert_with(|| {
                    format!("numbered Vulkan presentation was rejected: {result}")
                });
            }
            self.wait_finished.notify_all();
        }
    }

    fn before_swapchain_destroy(&self, swapchain: vk::SwapchainKHR) {
        use ash::vk::Handle as _;

        let Some(shared) = self
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.as_ref().and_then(Weak::upgrade))
        else {
            return;
        };
        let raw_swapchain = swapchain.as_raw();
        let Ok(mut state) = shared.state.lock() else {
            return;
        };
        while state
            .present_bindings
            .values()
            .any(|binding| binding.swapchain == raw_swapchain)
            || state
                .timing_bindings
                .values()
                .any(|binding| binding.swapchain == raw_swapchain)
        {
            state = match self.wait_finished.wait(state) {
                Ok(state) => state,
                Err(_) => return,
            };
        }
        state
            .rejected_present_bindings
            .retain(|_, binding| binding.swapchain != raw_swapchain);
        state
            .early_timing_records
            .retain(|_, early| early.raw_swapchain != raw_swapchain);
        state.configured_swapchains.remove(&raw_swapchain);
        drop(state);
        if let Ok(mut configurations) = self.configured_swapchains.lock() {
            configurations.remove(&raw_swapchain);
        }
    }
}

impl PresentationObserver {
    pub(crate) fn from_creation_context(
        cc: &eframe::CreationContext<'_>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(output_path) = observer_output_path()? else {
            return Ok(None);
        };
        let bridge = prepared_bridge_slot()
            .lock()
            .map_err(|_| anyhow::anyhow!("prepared presentation observer lock was poisoned"))?
            .as_ref()
            .and_then(Weak::upgrade)
            .context("presentation observer must be prepared before the WGPU device is created")?;

        let window = match cc.window_handle()?.as_raw() {
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)
                .context("Xlib window ID does not fit the X11 protocol")?,
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            _ => bail!("the trusted presentation observer requires an X11 window"),
        };
        let (lifecycle_connection, _) =
            x11rb::connect(None).context("failed to open observer X11 connection")?;
        lifecycle_connection
            .change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(
                    EventMask::STRUCTURE_NOTIFY
                        | EventMask::FOCUS_CHANGE
                        | EventMask::VISIBILITY_CHANGE,
                ),
            )?
            .check()
            .context("failed to select X11 observer window events")?;
        lifecycle_connection.flush()?;
        let geometry = lifecycle_connection
            .get_geometry(window)?
            .reply()
            .context("failed to inspect the observed X11 window")?;
        let attributes = lifecycle_connection
            .get_window_attributes(window)?
            .reply()
            .context("failed to inspect the observed X11 window attributes")?;
        let configured_swapchains = bridge
            .configured_swapchains
            .lock()
            .map_err(|_| anyhow::anyhow!("configured swapchain lock was poisoned"))?
            .clone();
        let configured_swapchain_history = configured_swapchains
            .values()
            .map(|configured| (configured.generation, configured.configuration))
            .collect();
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            stop: AtomicBool::new(false),
            wait_stop: AtomicBool::new(false),
            state: Mutex::new(ObserverState {
                initial_geometry: Some((geometry.width, geometry.height)),
                final_geometry: Some((geometry.width, geometry.height)),
                initially_mapped: attributes.map_state != MapState::UNMAPPED,
                currently_mapped: attributes.map_state != MapState::UNMAPPED,
                configured_swapchain_events: configured_swapchains.len() as u64,
                configured_swapchains,
                configured_swapchain_history,
                ..ObserverState::default()
            }),
        });
        *bridge
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("presentation-wait bridge lock was poisoned"))? =
            Some(Arc::downgrade(&shared));
        let task_rx = bridge
            .task_rx
            .lock()
            .map_err(|_| anyhow::anyhow!("presentation-wait receiver lock was poisoned"))?
            .take()
            .context("presentation-wait receiver was already attached")?;

        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .context("presentation observer requires the WGPU renderer")?;
        // SAFETY: the guard belongs to this live WGPU device. We copy only
        // Ash's dispatch wrappers, retain no WGPU-owned resource, and join the
        // wait worker before the application shell is destroyed.
        let hal_device = unsafe { render_state.device.as_hal::<wgpu::hal::api::Vulkan>() }
            .context("presentation observer requires the Vulkan WGPU backend")?;
        let raw_instance = hal_device.raw_instance().clone();
        let raw_device = hal_device.raw_device().clone();
        drop(hal_device);
        let wait_loader = khr::present_wait::Device::new(&raw_instance, &raw_device);
        // SAFETY: WGPU created this live Vulkan device with the observer's
        // required extension and feature chain before constructing the app.
        let timing_device = unsafe { HalTimingDevice::new(&raw_instance, &raw_device) }
            .map_err(anyhow::Error::msg)?;
        let (marker_connection, _) =
            x11rb::connect(None).context("failed to open marker-readback X11 connection")?;

        let wait_shared = Arc::clone(&shared);
        let wait_bridge = Arc::clone(&bridge);
        let wait_worker = std::thread::Builder::new()
            .name("mirante4d-vulkan-present-wait".to_owned())
            .spawn(move || {
                presentation_wait_worker(
                    wait_loader,
                    timing_device,
                    task_rx,
                    marker_connection,
                    window,
                    wait_shared,
                    wait_bridge,
                )
            })
            .context("failed to start the Vulkan presentation-wait worker")?;
        let lifecycle_shared = Arc::clone(&shared);
        let lifecycle_worker = match std::thread::Builder::new()
            .name("mirante4d-x11-window-observer".to_owned())
            .spawn(move || {
                lifecycle_worker(lifecycle_connection, window, output_path, lifecycle_shared)
            }) {
            Ok(worker) => worker,
            Err(error) => {
                shared.wait_stop.store(true, Ordering::Release);
                let _ = wait_worker.join();
                return Err(error).context("failed to start the X11 window observer");
            }
        };
        Ok(Some(Self {
            shared,
            lifecycle_worker: Some(lifecycle_worker),
            wait_worker: Some(wait_worker),
        }))
    }

    pub(crate) fn observe(&self, observation: PresentationObservation) {
        let now_ns = elapsed_ns(self.shared.epoch);
        let Ok(mut state) = self.shared.state.lock() else {
            return;
        };
        if state
            .scenario
            .as_deref()
            .is_some_and(|value| value != observation.scenario)
        {
            state.event_error = Some("presentation scenario identity changed".to_owned());
            return;
        }
        state
            .scenario
            .get_or_insert_with(|| observation.scenario.clone());
        let phase_and_input = (
            observation.phase.clone(),
            observation.active_input,
            observation.command_index,
        );
        if state.last_phase_and_input.as_ref() != Some(&phase_and_input) {
            if state.transitions.len() >= MAX_RECORDS {
                state.dropped_records = state.dropped_records.saturating_add(1);
            } else {
                state.transitions.push(StateTransition {
                    observed_at_ns: now_ns,
                    phase: observation.phase.clone(),
                    active_input: observation.active_input,
                    command_index: observation.command_index,
                    input_generation: observation.input_generation,
                });
            }
            state.last_phase_and_input = Some(phase_and_input);
        }
        if state.last_surface_generation != Some(observation.surface_generation) {
            if state.last_surface_generation.is_some() {
                state.surface_generation_changes =
                    state.surface_generation_changes.saturating_add(1);
            }
            state.last_surface_generation = Some(observation.surface_generation);
        }
        state.latest_work_identity = Some(observation.work_identity.clone());
        if !observation.eligible {
            return;
        }
        if state
            .environment_identity
            .as_ref()
            .is_some_and(|value| value != &observation.environment_identity)
        {
            state.event_error = Some("presentation environment identity changed".to_owned());
            return;
        }
        state
            .environment_identity
            .get_or_insert_with(|| observation.environment_identity.clone());
        if state.last_eligible_identity.as_ref() == Some(&observation.identity) {
            return;
        }
        if state.pending.len() >= MAX_RECORDS {
            state.dropped_records = state.dropped_records.saturating_add(1);
            state
                .event_error
                .get_or_insert_with(|| "presentation marker capacity was exhausted".to_owned());
            return;
        }
        let Some(marker_sequence) = state.marker_sequence.checked_add(1) else {
            state.event_error = Some("presentation marker sequence overflowed".to_owned());
            return;
        };
        state.marker_sequence = marker_sequence;
        state.last_eligible_identity = Some(observation.identity.clone());
        state.pending.insert(
            marker_sequence,
            PendingFrame {
                marker_sequence,
                enqueued_at_ns: now_ns,
                observation,
            },
        );
    }

    pub(crate) fn paint_marker(&self, ctx: &egui::Context) {
        let pixels_per_point = ctx.pixels_per_point();
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return;
        }
        let cell_pixels = (MARKER_CELL_POINTS * pixels_per_point)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let spec = MarkerSpec {
            x: (MARKER_X_POINTS * pixels_per_point).round() as i16,
            y: (MARKER_Y_POINTS * pixels_per_point).round() as i16,
            cell_pixels,
        };
        let sequence = {
            let Ok(mut state) = self.shared.state.lock() else {
                return;
            };
            state.marker_spec = Some(spec);
            state.marker_sequence
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("mirante4d-present-marker"),
        ));
        let origin = egui::pos2(MARKER_X_POINTS, MARKER_Y_POINTS);
        let size = egui::vec2(
            MARKER_COLUMNS as f32 * MARKER_CELL_POINTS,
            MARKER_ROWS as f32 * MARKER_CELL_POINTS,
        );
        painter.rect_filled(
            egui::Rect::from_min_size(origin, size),
            0.0,
            egui::Color32::BLACK,
        );
        let bits = marker_bits(sequence);
        for row in 0..MARKER_ROWS {
            for column in 0..MARKER_COLUMNS {
                let index = row * MARKER_COLUMNS + column;
                if !bits[index] {
                    continue;
                }
                let min = origin
                    + egui::vec2(
                        column as f32 * MARKER_CELL_POINTS,
                        row as f32 * MARKER_CELL_POINTS,
                    );
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        min,
                        egui::vec2(MARKER_CELL_POINTS, MARKER_CELL_POINTS),
                    ),
                    0.0,
                    egui::Color32::WHITE,
                );
            }
        }
    }

    pub(crate) fn map_after_unmap_observed(&self) -> bool {
        self.shared
            .state
            .lock()
            .is_ok_and(|state| state.map_after_unmap_observed)
    }
}

impl Drop for PresentationObserver {
    fn drop(&mut self) {
        self.shared.wait_stop.store(true, Ordering::Release);
        if let Some(worker) = self.wait_worker.take() {
            let _ = worker.join();
        }
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.lifecycle_worker.take() {
            let _ = worker.join();
        }
    }
}

fn presentation_wait_worker(
    wait_loader: khr::present_wait::Device,
    timing_device: HalTimingDevice,
    receiver: Receiver<PresentWaitTask>,
    marker_connection: x11rb::rust_connection::RustConnection,
    window: u32,
    shared: Arc<Shared>,
    bridge: Arc<PresentWaitBridge>,
) {
    let mut shutdown_started = None;
    loop {
        let task = match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(task) => Some(task),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                shared.wait_stop.store(true, Ordering::Release);
                None
            }
        };
        if let Some(task) = task {
            process_present_wait(
                &wait_loader,
                task,
                &marker_connection,
                window,
                &shared,
                &bridge,
            );
        }
        poll_presentation_timings(&timing_device, &shared, &bridge);
        expire_presentation_timings(&shared, &bridge);

        if shared.wait_stop.load(Ordering::Acquire) {
            let started = shutdown_started.get_or_insert_with(Instant::now);
            let pending = shared.state.lock().map_or(0, |state| {
                state.present_bindings.len() + state.timing_bindings.len()
            });
            if pending == 0 {
                break;
            }
            if started.elapsed() >= TIMING_DRAIN_TIMEOUT {
                if let Ok(mut state) = shared.state.lock() {
                    let missing = state.timing_bindings.len();
                    state.present_timing_timeout_events = state
                        .present_timing_timeout_events
                        .saturating_add(missing as u64);
                    state.event_error.get_or_insert_with(|| {
                        format!(
                            "{missing} presentation timing records did not drain during shutdown"
                        )
                    });
                    state.present_bindings.clear();
                    state.timing_bindings.clear();
                    state.timing_records.clear();
                    state.marker_outcomes.clear();
                }
                bridge.wait_finished.notify_all();
                break;
            }
        }
    }
}

fn process_present_wait(
    wait_loader: &khr::present_wait::Device,
    task: PresentWaitTask,
    marker_connection: &x11rb::rust_connection::RustConnection,
    window: u32,
    shared: &Shared,
    bridge: &PresentWaitBridge,
) {
    // SAFETY: the opt-in WGPU hook enabled both required device features,
    // assigned this ID to this swapchain's successful queue-present call, and
    // blocks swapchain destruction until this bounded worker releases it.
    let result = unsafe {
        wait_loader.wait_for_present(
            task.swapchain,
            task.present_id,
            u64::try_from(WAIT_TIMEOUT.as_nanos()).unwrap_or(u64::MAX),
        )
    };
    match result {
        Ok(()) => {
            let binding = match shared.state.lock() {
                Ok(mut state) => {
                    state.complete_events = state.complete_events.saturating_add(1);
                    state.present_bindings.remove(&task.present_id)
                }
                Err(_) => return,
            };
            bridge.wait_finished.notify_all();
            let Some(binding) = binding else {
                record_marker_outcome(shared, task.present_id, MarkerOutcome::Nonqualifying);
                return;
            };
            if binding.swapchain_generation != task.swapchain_generation {
                record_error(
                    shared,
                    "present-wait task crossed a swapchain generation".to_owned(),
                );
                record_marker_outcome(shared, task.present_id, MarkerOutcome::Nonqualifying);
                return;
            }
            if !window_is_mapped(marker_connection, window) {
                record_window_unavailable_completion(shared);
                record_marker_outcome(shared, task.present_id, MarkerOutcome::Nonqualifying);
                return;
            }
            let marker_deadline = Instant::now() + MARKER_SETTLE_TIMEOUT;
            let sequence = loop {
                match read_marker_sequence(marker_connection, window, binding.marker_spec) {
                    Ok(sequence) if sequence == binding.marker_sequence => break Some(sequence),
                    Ok(sequence) if sequence > binding.marker_sequence => {
                        if let Ok(mut state) = shared.state.lock() {
                            state.superseded_before_marker_observation =
                                state.superseded_before_marker_observation.saturating_add(1);
                            state.nonqualifying_completion_events =
                                state.nonqualifying_completion_events.saturating_add(1);
                        }
                        break None;
                    }
                    Ok(sequence) if Instant::now() >= marker_deadline => {
                        if controlled_lifecycle_recovery(shared, binding) {
                            record_window_unavailable_completion(shared);
                        } else {
                            record_ambiguous_completion(
                                shared,
                                format!(
                                    "present ID {} expected marker {} but marker remained at {} for {} ms after the Vulkan wait",
                                    task.present_id,
                                    binding.marker_sequence,
                                    sequence,
                                    MARKER_SETTLE_TIMEOUT.as_millis(),
                                ),
                            );
                        }
                        break None;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if !window_is_mapped(marker_connection, window) {
                            record_window_unavailable_completion(shared);
                            break None;
                        }
                        if Instant::now() >= marker_deadline {
                            if controlled_lifecycle_recovery(shared, binding) {
                                record_window_unavailable_completion(shared);
                            } else {
                                record_error(
                                    shared,
                                    format!(
                                        "presentation marker readback failed for present ID {} in swapchain generation {}: {error}",
                                        task.present_id, task.swapchain_generation,
                                    ),
                                );
                            }
                            break None;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let Some(sequence) = sequence else {
                record_marker_outcome(shared, task.present_id, MarkerOutcome::Nonqualifying);
                return;
            };
            let now_ns = elapsed_ns(shared.epoch);
            let outcome = match shared.state.lock() {
                Ok(mut state) => {
                    let Some(submitted_at_ns) = binding.submitted_at_ns else {
                        state.nonqualifying_completion_events =
                            state.nonqualifying_completion_events.saturating_add(1);
                        drop(state);
                        record_marker_outcome(
                            shared,
                            task.present_id,
                            MarkerOutcome::Nonqualifying,
                        );
                        return;
                    };
                    record_completed_marker(
                        &mut state,
                        sequence,
                        now_ns,
                        CompletionStamp {
                            present_id: task.present_id,
                            wait_return_delay_ns: now_ns.saturating_sub(submitted_at_ns),
                            swapchain_generation: task.swapchain_generation,
                        },
                    )
                    .map_or(MarkerOutcome::Nonqualifying, |qualified| {
                        MarkerOutcome::Qualified(Box::new(qualified))
                    })
                }
                Err(_) => return,
            };
            record_marker_outcome(shared, task.present_id, outcome);
        }
        Err(vk::Result::TIMEOUT) => {
            if let Ok(mut state) = shared.state.lock() {
                state.present_bindings.remove(&task.present_id);
                state.wait_timeout_events = state.wait_timeout_events.saturating_add(1);
                state.event_error.get_or_insert_with(|| {
                    format!(
                        "Vulkan presentation wait timed out after {} ms",
                        WAIT_TIMEOUT.as_millis()
                    )
                });
            }
            record_marker_outcome(shared, task.present_id, MarkerOutcome::Nonqualifying);
            bridge.wait_finished.notify_all();
        }
        Err(error) => {
            if let Ok(mut state) = shared.state.lock() {
                state.present_bindings.remove(&task.present_id);
                state.wait_failure_events = state.wait_failure_events.saturating_add(1);
                state
                    .event_error
                    .get_or_insert_with(|| format!("Vulkan presentation wait failed: {error}"));
            }
            record_marker_outcome(shared, task.present_id, MarkerOutcome::Nonqualifying);
            bridge.wait_finished.notify_all();
        }
    }
}

fn poll_presentation_timings(
    timing_device: &HalTimingDevice,
    shared: &Shared,
    bridge: &PresentWaitBridge,
) {
    use ash::vk::Handle as _;

    let pending_swapchains = match shared.state.lock() {
        Ok(state) => state
            .timing_bindings
            .values()
            .map(|binding| binding.swapchain)
            .collect::<BTreeSet<_>>(),
        Err(_) => return,
    };
    if pending_swapchains.is_empty() {
        return;
    }
    let configurations = match bridge.configured_swapchains.lock() {
        Ok(configurations) => configurations
            .iter()
            .filter(|(swapchain, _)| pending_swapchains.contains(swapchain))
            .map(|(swapchain, configured)| (*swapchain, *configured))
            .collect::<Vec<_>>(),
        Err(_) => return,
    };
    for (raw_swapchain, configured) in configurations {
        let swapchain = vk::SwapchainKHR::from_raw(raw_swapchain);
        // SAFETY: the bridge retains this live configured swapchain until the
        // worker drains it and releases `before_swapchain_destroy`.
        match unsafe { timing_device.query_past(swapchain) } {
            Ok(query) => accept_timing_query(shared, raw_swapchain, configured, query),
            Err(error) => {
                if let Ok(mut state) = shared.state.lock() {
                    state.present_timing_failure_events =
                        state.present_timing_failure_events.saturating_add(1);
                    state.event_error.get_or_insert_with(|| {
                        format!("Vulkan past-presentation timing query failed: {error}")
                    });
                }
            }
        }
        // SAFETY: same live-swapchain ownership as the timing query above.
        match unsafe { timing_device.timing_properties(swapchain) } {
            Ok(Some((refresh_duration, refresh_interval, counter))) => {
                record_refresh_properties(
                    shared,
                    configured.generation,
                    refresh_duration,
                    refresh_interval,
                    counter,
                );
            }
            Ok(None) => {}
            Err(error) => {
                if let Ok(mut state) = shared.state.lock() {
                    state.present_timing_failure_events =
                        state.present_timing_failure_events.saturating_add(1);
                    state.event_error.get_or_insert_with(|| {
                        format!("Vulkan swapchain timing-properties query failed: {error}")
                    });
                }
            }
        }
    }
    bridge.wait_finished.notify_all();
}

fn expire_presentation_timings(shared: &Shared, bridge: &PresentWaitBridge) {
    let now_ns = elapsed_ns(shared.epoch);
    let timeout_ns = u64::try_from(TIMING_DRAIN_TIMEOUT.as_nanos()).unwrap_or(u64::MAX);
    let Ok(mut state) = shared.state.lock() else {
        return;
    };
    let expired = state
        .timing_bindings
        .iter()
        .filter_map(|(present_id, binding)| {
            (now_ns.saturating_sub(binding.submitted_at_ns) >= timeout_ns).then_some(*present_id)
        })
        .collect::<Vec<_>>();
    if expired.is_empty() {
        return;
    }
    for present_id in &expired {
        state.timing_bindings.remove(present_id);
        state.timing_records.remove(present_id);
        state.marker_outcomes.remove(present_id);
    }
    state.present_timing_timeout_events = state
        .present_timing_timeout_events
        .saturating_add(expired.len() as u64);
    state.event_error.get_or_insert_with(|| {
        format!(
            "{} first-pixel-out timing records exceeded the {} ms drain deadline",
            expired.len(),
            TIMING_DRAIN_TIMEOUT.as_millis()
        )
    });
    drop(state);
    bridge.wait_finished.notify_all();
}

fn lifecycle_worker(
    connection: x11rb::rust_connection::RustConnection,
    window: u32,
    output_path: PathBuf,
    shared: Arc<Shared>,
) {
    while !shared.stop.load(Ordering::Acquire) {
        match connection.poll_for_event() {
            Ok(Some(event)) => handle_event(window, &shared, event),
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(error) => {
                record_error(&shared, format!("X11 observer event failure: {error}"));
                break;
            }
        }
    }
    while let Ok(Some(event)) = connection.poll_for_event() {
        handle_event(window, &shared, event);
    }
    if let Err(error) = write_report(&connection, window, &output_path, &shared) {
        tracing::error!(%error, "failed to write Vulkan presentation observer report");
    }
}

fn handle_event(window: u32, shared: &Shared, event: Event) {
    match event {
        Event::ConfigureNotify(event) if event.window == window => {
            if let Ok(mut state) = shared.state.lock() {
                state.window_lifecycle_generation =
                    state.window_lifecycle_generation.saturating_add(1);
                state.configure_events = state.configure_events.saturating_add(1);
                state.final_geometry = Some((event.width, event.height));
            }
        }
        Event::FocusOut(event) if event.event == window => {
            if let Ok(mut state) = shared.state.lock() {
                state.window_lifecycle_generation =
                    state.window_lifecycle_generation.saturating_add(1);
                state.focus_loss_events = state.focus_loss_events.saturating_add(1);
                if observer_measurement_phase(&state) {
                    state.measured_focus_loss_events =
                        state.measured_focus_loss_events.saturating_add(1);
                }
            }
        }
        Event::FocusIn(event) if event.event == window => {
            if let Ok(mut state) = shared.state.lock() {
                state.window_lifecycle_generation =
                    state.window_lifecycle_generation.saturating_add(1);
            }
        }
        Event::VisibilityNotify(event) if event.window == window => {
            if let Ok(mut state) = shared.state.lock() {
                state.window_lifecycle_generation =
                    state.window_lifecycle_generation.saturating_add(1);
                if u8::from(event.state) != 0 {
                    state.occlusion_events = state.occlusion_events.saturating_add(1);
                    if observer_measurement_phase(&state) {
                        state.measured_occlusion_events =
                            state.measured_occlusion_events.saturating_add(1);
                    }
                }
            }
        }
        Event::MapNotify(event) if event.window == window => {
            if let Ok(mut state) = shared.state.lock() {
                state.window_lifecycle_generation =
                    state.window_lifecycle_generation.saturating_add(1);
                state.map_events = state.map_events.saturating_add(1);
                state.currently_mapped = true;
                if state.unmap_events > 0 {
                    state.map_after_unmap_observed = true;
                    state
                        .map_after_unmap_at_ns
                        .get_or_insert_with(|| elapsed_ns(shared.epoch));
                }
            }
        }
        Event::UnmapNotify(event) if event.window == window => {
            if let Ok(mut state) = shared.state.lock() {
                state.window_lifecycle_generation =
                    state.window_lifecycle_generation.saturating_add(1);
                state.unmap_events = state.unmap_events.saturating_add(1);
                state.occlusion_events = state.occlusion_events.saturating_add(1);
                if observer_measurement_phase(&state) {
                    state.measured_unmap_events = state.measured_unmap_events.saturating_add(1);
                    state.measured_occlusion_events =
                        state.measured_occlusion_events.saturating_add(1);
                }
                state.currently_mapped = false;
                state
                    .unmap_started_at_ns
                    .get_or_insert_with(|| elapsed_ns(shared.epoch));
            }
        }
        Event::DestroyNotify(event) if event.window == window => {
            record_error(
                shared,
                "observed X11 window was destroyed before report closeout".to_owned(),
            );
        }
        _ => {}
    }
}

fn observer_measurement_phase(state: &ObserverState) -> bool {
    state
        .last_phase_and_input
        .as_ref()
        .is_some_and(|(phase, _, _)| {
            matches!(
                phase.as_str(),
                "standalone_interaction"
                    | "four_panel_interaction"
                    | "resident_settlement"
                    | "prepared_nonresident_replacement"
            )
        })
}

fn controlled_lifecycle_recovery(shared: &Shared, binding: PresentBinding) -> bool {
    shared
        .state
        .lock()
        .is_ok_and(|state| binding_crossed_controlled_lifecycle(&state, binding))
}

fn binding_crossed_controlled_lifecycle(state: &ObserverState, binding: PresentBinding) -> bool {
    state.scenario.as_deref() == Some("representative_gpu_presentation_probe")
        && (state.window_lifecycle_generation > binding.window_lifecycle_generation
            || state
                .configured_swapchain_history
                .keys()
                .next_back()
                .is_some_and(|generation| *generation > binding.swapchain_generation))
}

#[derive(Debug, Clone, Copy)]
struct CompletionStamp {
    present_id: u64,
    wait_return_delay_ns: u64,
    swapchain_generation: u64,
}

fn record_completed_marker(
    state: &mut ObserverState,
    sequence: u32,
    now_ns: u64,
    completion: CompletionStamp,
) -> Option<QualifiedPresent> {
    if state
        .last_qualified_marker_sequence
        .is_some_and(|last| sequence <= last)
    {
        state.unchanged_present_events = state.unchanged_present_events.saturating_add(1);
        return None;
    }
    let Some(oldest_pending) = state.pending.keys().next().copied() else {
        if sequence <= state.marker_sequence {
            state.unchanged_present_events = state.unchanged_present_events.saturating_add(1);
        } else {
            state.event_error = Some("presented marker has no eligible frame authority".to_owned());
        }
        return None;
    };
    if sequence != oldest_pending {
        if state.pending.contains_key(&sequence) {
            let superseded = state
                .pending
                .keys()
                .copied()
                .filter(|pending| *pending < sequence)
                .collect::<Vec<_>>();
            for pending in &superseded {
                state.pending.remove(pending);
            }
            state.superseded_before_present = state
                .superseded_before_present
                .saturating_add(superseded.len() as u64);
        } else if sequence <= state.marker_sequence {
            state.unchanged_present_events = state.unchanged_present_events.saturating_add(1);
            return None;
        } else {
            state.event_error = Some("presented marker has no eligible frame authority".to_owned());
            return None;
        }
    }
    let frame = state.pending.remove(&sequence).unwrap();
    if completion.present_id == 0
        || state
            .last_qualified_marker_present_id
            .is_some_and(|last| completion.present_id <= last)
    {
        state.nonqualifying_completion_events =
            state.nonqualifying_completion_events.saturating_add(1);
        return None;
    }
    if state
        .presented
        .len()
        .saturating_add(state.marker_outcomes.len())
        >= MAX_RECORDS
    {
        state.dropped_records = state.dropped_records.saturating_add(1);
        return None;
    }
    let observer_delay = now_ns.saturating_sub(frame.enqueued_at_ns);
    state.last_qualified_marker_sequence = Some(sequence);
    state.last_qualified_marker_present_id = Some(completion.present_id);
    Some(QualifiedPresent {
        marker_sequence: frame.marker_sequence,
        present_id: completion.present_id,
        observed_at_ns: now_ns,
        wait_return_delay_ns: completion.wait_return_delay_ns,
        phase: frame.observation.phase,
        command_index: frame.observation.command_index,
        active_input: frame.observation.active_input,
        exact: frame.observation.exact,
        identity: frame.observation.identity,
        input_generation: frame.observation.input_generation,
        input_to_visible_coarse_ns: frame
            .observation
            .input_age_ns
            .saturating_add(observer_delay),
        surface_generation: frame.observation.surface_generation,
        work_identity: frame.observation.work_identity,
        swapchain_generation: completion.swapchain_generation,
    })
}

fn record_marker_outcome(shared: &Shared, present_id: u64, outcome: MarkerOutcome) {
    let Ok(mut state) = shared.state.lock() else {
        return;
    };
    if state.marker_outcomes.insert(present_id, outcome).is_some() {
        state.nonqualifying_completion_events =
            state.nonqualifying_completion_events.saturating_add(1);
        state
            .event_error
            .get_or_insert_with(|| format!("present ID {present_id} produced two marker outcomes"));
        return;
    }
    finalize_timing_correlation(&mut state, present_id);
}

fn finalize_timing_correlation(state: &mut ObserverState, present_id: u64) {
    if !state.timing_records.contains_key(&present_id)
        || !state.marker_outcomes.contains_key(&present_id)
    {
        return;
    }
    let timing = state
        .timing_records
        .remove(&present_id)
        .expect("presence was checked");
    let outcome = state
        .marker_outcomes
        .remove(&present_id)
        .expect("presence was checked");
    let MarkerOutcome::Qualified(qualified) = outcome else {
        return;
    };
    let qualified = *qualified;
    if qualified.swapchain_generation != timing.swapchain_generation {
        state.nonqualifying_completion_events =
            state.nonqualifying_completion_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!("present ID {present_id} crossed a swapchain timing generation")
        });
        return;
    }
    if state.presented.len() >= MAX_RECORDS {
        state.dropped_records = state.dropped_records.saturating_add(1);
        return;
    }
    if state.presented.last().is_some_and(|record| {
        qualified.present_id <= record.present_id
            || qualified.marker_sequence <= record.marker_sequence
    }) {
        state.present_timing_out_of_order_events =
            state.present_timing_out_of_order_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!("qualified present ID {present_id} completed out of order")
        });
        return;
    }
    state.presented.push(PresentedRecord {
        marker_sequence: qualified.marker_sequence,
        present_id: qualified.present_id,
        observed_at_ns: qualified.observed_at_ns,
        wait_return_delay_ns: qualified.wait_return_delay_ns,
        phase: qualified.phase,
        command_index: qualified.command_index,
        active_input: qualified.active_input,
        exact: qualified.exact,
        identity: qualified.identity,
        input_generation: qualified.input_generation,
        input_to_visible_coarse_ns: qualified.input_to_visible_coarse_ns,
        surface_generation: qualified.surface_generation,
        work_identity: qualified.work_identity,
        swapchain_generation: timing.swapchain_generation,
        first_pixel_out_ns: timing.first_pixel_out_ns,
        time_domain: timing.time_domain,
        time_domain_id: timing.time_domain_id,
    });
}

fn accept_timing_query(
    shared: &Shared,
    raw_swapchain: u64,
    configured: ConfiguredSwapchain,
    query: HalTimingQuery,
) {
    let Ok(mut state) = shared.state.lock() else {
        return;
    };
    state.present_timing_query_events = state.present_timing_query_events.saturating_add(1);
    if query.incomplete {
        state.present_timing_incomplete_events =
            state.present_timing_incomplete_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            "Vulkan returned more timing records than the bounded results queue".to_owned()
        });
    }
    let counters = state
        .timing_counters
        .entry(configured.generation)
        .or_default();
    let timing_counter_changed = counters
        .timing_properties_counter
        .replace(query.timing_properties_counter)
        .is_some_and(|previous| previous != query.timing_properties_counter);
    let time_domain_counter_changed = counters
        .time_domains_counter
        .replace(query.time_domains_counter)
        .is_some_and(|previous| previous != query.time_domains_counter);
    if timing_counter_changed {
        state.timing_properties_change_events =
            state.timing_properties_change_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            "swapchain presentation timing properties changed during observation".to_owned()
        });
    }
    if time_domain_counter_changed {
        state.time_domain_change_events = state.time_domain_change_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            "swapchain presentation time domains changed during observation".to_owned()
        });
    }
    for record in query.records {
        accept_timing_record(&mut state, raw_swapchain, configured, record);
    }
}

fn accept_timing_record(
    state: &mut ObserverState,
    raw_swapchain: u64,
    configured: ConfiguredSwapchain,
    record: HalTimingRecord,
) {
    if state
        .present_bindings
        .get(&record.present_id)
        .is_some_and(|binding| binding.submitted_at_ns.is_none())
    {
        if state.early_timing_records.len() >= MAX_WAIT_TASKS {
            state.dropped_records = state.dropped_records.saturating_add(1);
            state.event_error.get_or_insert_with(|| {
                "early presentation timing capacity was exhausted".to_owned()
            });
        } else if state
            .early_timing_records
            .insert(
                record.present_id,
                EarlyTimingRecord {
                    raw_swapchain,
                    configured,
                    record,
                },
            )
            .is_some()
        {
            state.present_timing_duplicate_events =
                state.present_timing_duplicate_events.saturating_add(1);
            state.event_error.get_or_insert_with(|| {
                format!(
                    "first-pixel-out timing duplicated reserved present ID {}",
                    record.present_id
                )
            });
        }
        return;
    }
    if let Some(rejected) = state.rejected_present_bindings.remove(&record.present_id) {
        if rejected.swapchain != raw_swapchain
            || rejected.swapchain_generation != configured.generation
            || configured.configuration.time_domain != record.time_domain
            || configured.configuration.time_domain_id != record.time_domain_id
        {
            state.time_domain_change_events = state.time_domain_change_events.saturating_add(1);
            state.event_error.get_or_insert_with(|| {
                format!(
                    "rejected present ID {} returned from an unexpected swapchain generation or time domain",
                    record.present_id
                )
            });
        } else {
            state.present_timing_rejected_result_events = state
                .present_timing_rejected_result_events
                .saturating_add(1);
        }
        return;
    }
    if state.completed_timing_ids.contains(&record.present_id) {
        state.present_timing_duplicate_events =
            state.present_timing_duplicate_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "first-pixel-out timing duplicated present ID {}",
                record.present_id
            )
        });
        return;
    }
    let Some(binding) = state.timing_bindings.get(&record.present_id).copied() else {
        state.present_timing_unknown_id_events =
            state.present_timing_unknown_id_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "first-pixel-out timing reported unknown present ID {}",
                record.present_id
            )
        });
        return;
    };
    if binding.swapchain != raw_swapchain
        || binding.swapchain_generation != configured.generation
        || configured.configuration.time_domain != record.time_domain
        || configured.configuration.time_domain_id != record.time_domain_id
    {
        state.time_domain_change_events = state.time_domain_change_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "present ID {} returned from an unexpected swapchain generation or time domain",
                record.present_id
            )
        });
        state.timing_bindings.remove(&record.present_id);
        state.marker_outcomes.remove(&record.present_id);
        return;
    }
    if !record.report_complete || record.present_stage_count != 1 {
        state.present_timing_zero_stage_events =
            state.present_timing_zero_stage_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "present ID {} returned an incomplete or missing first-pixel-out stage",
                record.present_id
            )
        });
        state.timing_bindings.remove(&record.present_id);
        state.marker_outcomes.remove(&record.present_id);
        return;
    }
    if record.stage != wgpu::hal::vulkan::present_wait_observer::PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT
    {
        state.present_timing_zero_stage_events =
            state.present_timing_zero_stage_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "present ID {} returned the wrong presentation stage",
                record.present_id
            )
        });
        state.timing_bindings.remove(&record.present_id);
        state.marker_outcomes.remove(&record.present_id);
        return;
    }
    if record.time_ns == 0 {
        state.present_timing_zero_time_events =
            state.present_timing_zero_time_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "present ID {} returned a zero first-pixel-out timestamp",
                record.present_id
            )
        });
        state.timing_bindings.remove(&record.present_id);
        state.marker_outcomes.remove(&record.present_id);
        return;
    }
    if state
        .last_timing_present_id
        .get(&configured.generation)
        .is_some_and(|previous| record.present_id <= *previous)
    {
        state.present_timing_out_of_order_events =
            state.present_timing_out_of_order_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "first-pixel-out timing present ID {} arrived out of order",
                record.present_id
            )
        });
        state.timing_bindings.remove(&record.present_id);
        state.marker_outcomes.remove(&record.present_id);
        return;
    }
    state
        .last_timing_present_id
        .insert(configured.generation, record.present_id);
    if state.completed_timing_ids.len() >= MAX_RECORDS {
        state.dropped_records = state.dropped_records.saturating_add(1);
        state
            .event_error
            .get_or_insert_with(|| "presentation timing ID capacity was exhausted".to_owned());
        state.timing_bindings.remove(&record.present_id);
        state.marker_outcomes.remove(&record.present_id);
        return;
    }
    state.completed_timing_ids.insert(record.present_id);
    state.timing_bindings.remove(&record.present_id);
    if state
        .timing_records
        .insert(
            record.present_id,
            ScanoutTiming {
                swapchain_generation: configured.generation,
                first_pixel_out_ns: record.time_ns,
                time_domain: record.time_domain.as_raw(),
                time_domain_id: record.time_domain_id,
            },
        )
        .is_some()
    {
        state.present_timing_duplicate_events =
            state.present_timing_duplicate_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            format!(
                "first-pixel-out timing duplicated present ID {}",
                record.present_id
            )
        });
        return;
    }
    state.present_timing_complete_events = state.present_timing_complete_events.saturating_add(1);
    finalize_timing_correlation(state, record.present_id);
}

fn record_refresh_properties(
    shared: &Shared,
    swapchain_generation: u64,
    refresh_duration_ns: u64,
    refresh_interval_ns: u64,
    counter: u64,
) {
    let Ok(mut state) = shared.state.lock() else {
        return;
    };
    let counters = state
        .timing_counters
        .entry(swapchain_generation)
        .or_default();
    let changed = counters
        .refresh_properties_counter
        .replace(counter)
        .is_some_and(|previous| previous != counter)
        || counters
            .refresh_duration_ns
            .replace(refresh_duration_ns)
            .is_some_and(|previous| previous != refresh_duration_ns)
        || counters
            .refresh_interval_ns
            .replace(refresh_interval_ns)
            .is_some_and(|previous| previous != refresh_interval_ns);
    if changed {
        state.timing_properties_change_events =
            state.timing_properties_change_events.saturating_add(1);
        state.event_error.get_or_insert_with(|| {
            "swapchain refresh timing properties changed during observation".to_owned()
        });
    }
}

fn window_is_mapped(connection: &x11rb::rust_connection::RustConnection, window: u32) -> bool {
    connection
        .get_window_attributes(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|attributes| attributes.map_state != MapState::UNMAPPED)
}

fn read_marker_sequence(
    connection: &x11rb::rust_connection::RustConnection,
    window: u32,
    spec: MarkerSpec,
) -> anyhow::Result<u32> {
    let reply = connection
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            spec.x,
            spec.y,
            spec.width(),
            spec.height(),
            u32::MAX,
        )?
        .reply()
        .context("failed to read the mapped presentation marker")?;
    let format = connection
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == reply.depth)
        .context("presentation marker pixel format is unavailable")?;
    if format.bits_per_pixel == 0 || !format.bits_per_pixel.is_multiple_of(8) {
        bail!("presentation marker uses an unsupported pixel format");
    }
    let bytes_per_pixel = usize::from(format.bits_per_pixel / 8);
    let width = usize::from(spec.width());
    let pad_bits = usize::from(format.scanline_pad);
    if pad_bits == 0 || !pad_bits.is_multiple_of(8) {
        bail!("presentation marker uses an unsupported scanline alignment");
    }
    let stride_bits = width
        .checked_mul(usize::from(format.bits_per_pixel))
        .and_then(|bits| bits.checked_add(pad_bits.saturating_sub(1)))
        .context("presentation marker stride overflowed")?
        / pad_bits
        * pad_bits;
    let stride = stride_bits / 8;
    if reply.data.len() < stride.saturating_mul(usize::from(spec.height())) {
        bail!("presentation marker readback is truncated");
    }
    let intensity = |index: usize| -> anyhow::Result<u64> {
        let row = index / MARKER_COLUMNS;
        let column = index % MARKER_COLUMNS;
        let x = column * usize::from(spec.cell_pixels) + usize::from(spec.cell_pixels) / 2;
        let y = row * usize::from(spec.cell_pixels) + usize::from(spec.cell_pixels) / 2;
        let offset = y
            .checked_mul(stride)
            .and_then(|value| value.checked_add(x.saturating_mul(bytes_per_pixel)))
            .context("presentation marker sample offset overflowed")?;
        let pixel = reply
            .data
            .get(offset..offset + bytes_per_pixel)
            .context("presentation marker sample is out of bounds")?;
        Ok(pixel.iter().map(|byte| u64::from(*byte)).sum())
    };
    let black = (intensity(0)? + intensity(2)?) / 2;
    let white = (intensity(1)? + intensity(3)?) / 2;
    if white <= black.saturating_add(u64::from(format.bits_per_pixel)) {
        bail!("presentation marker black/white calibration is not distinguishable");
    }
    let threshold = black + (white - black) / 2;
    let mut payload = [false; MARKER_CELLS - 4];
    for (bit, slot) in payload.iter_mut().enumerate() {
        *slot = intensity(bit + 4)? > threshold;
    }
    let sequence = decode_bits(&payload[..32]) as u32;
    let checksum = decode_bits(&payload[32..48]) as u16;
    let magic = decode_bits(&payload[48..56]) as u8;
    if magic != 0xd7 || checksum != marker_checksum(sequence) {
        bail!("presentation marker checksum or magic is invalid");
    }
    Ok(sequence)
}

fn marker_bits(sequence: u32) -> [bool; MARKER_CELLS] {
    let mut bits = [false; MARKER_CELLS];
    bits[1] = true;
    bits[3] = true;
    encode_bits(&mut bits[4..36], u64::from(sequence));
    encode_bits(&mut bits[36..52], u64::from(marker_checksum(sequence)));
    encode_bits(&mut bits[52..60], 0xd7);
    bits
}

fn marker_checksum(sequence: u32) -> u16 {
    (sequence as u16) ^ ((sequence >> 16) as u16) ^ 0xa5a5
}

fn encode_bits(output: &mut [bool], value: u64) {
    for (index, bit) in output.iter_mut().enumerate() {
        *bit = value & (1_u64 << index) != 0;
    }
}

fn decode_bits(bits: &[bool]) -> u64 {
    bits.iter().enumerate().fold(0_u64, |value, (index, bit)| {
        value | (u64::from(*bit) << index)
    })
}

fn write_report(
    connection: &x11rb::rust_connection::RustConnection,
    window: u32,
    output_path: &PathBuf,
    shared: &Shared,
) -> anyhow::Result<()> {
    let geometry = connection.get_geometry(window)?.reply().ok();
    let final_mapped = connection
        .get_window_attributes(window)?
        .reply()
        .map(|attributes| attributes.map_state != MapState::UNMAPPED)
        .unwrap_or(false);
    let mut state = shared
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("presentation observer state lock was poisoned"))?;
    if let Some(geometry) = geometry {
        state.final_geometry = Some((geometry.width, geometry.height));
    }
    let scenario = state.scenario.clone().unwrap_or_default();
    let is_probe = scenario == "representative_gpu_presentation_probe";
    let controlled_unmapped_gap_not_counted = state
        .unmap_started_at_ns
        .zip(state.map_after_unmap_at_ns)
        .is_some_and(|(unmapped_at, remapped_at)| {
            remapped_at > unmapped_at
                && state.presented.iter().all(|record| {
                    record.observed_at_ns < unmapped_at || record.observed_at_ns > remapped_at
                })
        });
    let boundary_proof = json!({
        "controlled_unmapped_gap_not_counted": is_probe
            && controlled_unmapped_gap_not_counted,
        "unchanged_repeat_not_counted": state.unchanged_present_events > 0,
        "queued_distinct_not_collapsed": is_probe && has_strict_present_pair(&state.presented),
        "resize_detected": is_probe && state.configure_events > 0,
        "occlusion_detected": is_probe && state.occlusion_events > 0,
        "focus_loss_detected": is_probe && state.focus_loss_events > 0,
        "surface_recreation_detected": is_probe
            && state.unmap_events > 0
            && state.map_after_unmap_observed
            && state.configure_events > 0
            && state.swapchain_changes > 0
            && state.map_after_unmap_at_ns.is_some_and(|remapped_at| {
                state.presented.iter().any(|record| record.observed_at_ns > remapped_at)
            }),
        "target_extent_matched": state.final_geometry == Some((1920, 1080)),
        "mapped_window_confirmed": final_mapped,
        "marker_decode_after_present_only": true,
        "internal_publication_alone_cannot_advance": true,
        "first_pixel_out_timing_observed": state.present_timing_complete_events > 0,
        "first_pixel_out_correlated_to_marker": !state.presented.is_empty()
            && state.presented.iter().all(|record| record.first_pixel_out_ns > 0),
        "timing_queue_drained": state.timing_bindings.is_empty()
            && state.timing_records.is_empty()
            && state.marker_outcomes.is_empty(),
        "stable_time_domain": state.time_domain_change_events == 0,
        "stable_timing_properties": state.timing_properties_change_events == 0,
    });
    let metrics = if is_probe {
        Value::Null
    } else {
        product_metrics(&state).unwrap_or(Value::Null)
    };
    let proof_passed = boundary_proof.as_object().is_some_and(|proof| {
        let required: &[&str] = if is_probe {
            &[
                "controlled_unmapped_gap_not_counted",
                "unchanged_repeat_not_counted",
                "queued_distinct_not_collapsed",
                "resize_detected",
                "occlusion_detected",
                "focus_loss_detected",
                "surface_recreation_detected",
                "target_extent_matched",
                "mapped_window_confirmed",
                "first_pixel_out_timing_observed",
                "first_pixel_out_correlated_to_marker",
                "timing_queue_drained",
                "stable_time_domain",
                "stable_timing_properties",
            ]
        } else {
            &[
                "target_extent_matched",
                "mapped_window_confirmed",
                "first_pixel_out_timing_observed",
                "first_pixel_out_correlated_to_marker",
                "timing_queue_drained",
                "stable_time_domain",
                "stable_timing_properties",
            ]
        };
        required
            .iter()
            .all(|field| proof.get(*field).and_then(Value::as_bool) == Some(true))
    });
    let invalid_environment = if is_probe {
        false
    } else {
        state.measured_focus_loss_events > 0
            || state.measured_occlusion_events > 0
            || state.measured_unmap_events > 0
    };
    let timing_clean = state.present_timing_incomplete_events == 0
        && state.present_timing_duplicate_events == 0
        && state.present_timing_unknown_id_events == 0
        && state.present_timing_out_of_order_events == 0
        && state.present_timing_zero_stage_events == 0
        && state.present_timing_zero_time_events == 0
        && state.present_timing_failure_events == 0
        && state.present_timing_timeout_events == 0
        && state.present_timing_queue_full_events == 0
        && state.fatal_rejected_present_events == 0
        && state.timing_properties_change_events == 0
        && state.time_domain_change_events == 0
        && state.timing_bindings.is_empty()
        && state.early_timing_records.is_empty()
        && state.timing_records.is_empty()
        && state.marker_outcomes.is_empty()
        && state.present_timing_complete_events == state.submitted_present_events
        && state.configured_swapchain_events > 0;
    let exact_cadence_status =
        if timing_clean && !state.presented.is_empty() && (is_probe || !metrics.is_null()) {
            "pass"
        } else if invalid_environment {
            "invalid"
        } else {
            "unevaluated"
        };
    let status = if state.event_error.is_none()
        && state.dropped_records == 0
        && state.wait_timeout_events == 0
        && state.wait_failure_events == 0
        && state.present_bindings.is_empty()
        && timing_clean
        && state.complete_events > 0
        && !state.presented.is_empty()
        && proof_passed
        && !invalid_environment
        && (is_probe || !metrics.is_null())
    {
        "pass"
    } else if invalid_environment {
        "invalid"
    } else {
        "unevaluated"
    };
    let clocks = json!({
        "coarse_visibility": "std_instant_monotonic_vulkan_present_wait_completion",
        "exact_scanout_cadence": "vulkan_ext_present_timing_image_first_pixel_out",
        "cross_clock_subtraction_permitted": false,
    });
    let environment = json!({
        "window_id_published": false,
        "window_width_pixels": state.final_geometry.map(|value| value.0),
        "window_height_pixels": state.final_geometry.map(|value| value.1),
        "initial_window_width_pixels": state.initial_geometry.map(|value| value.0),
        "initial_window_height_pixels": state.initial_geometry.map(|value| value.1),
        "initially_mapped": state.initially_mapped,
        "observer_currently_mapped": state.currently_mapped,
        "finally_mapped": final_mapped,
        "focus_loss_events": state.focus_loss_events,
        "occlusion_events": state.occlusion_events,
        "measured_focus_loss_events": state.measured_focus_loss_events,
        "measured_occlusion_events": state.measured_occlusion_events,
        "measured_unmap_events": state.measured_unmap_events,
        "map_events": state.map_events,
        "unmap_events": state.unmap_events,
        "invalid_environment": invalid_environment,
        "qualified_identity": state.environment_identity,
    });
    let event_counts = json!({
        "vulkan_present_submitted": state.submitted_present_events,
        "vulkan_present_wait_completed": state.complete_events,
        "vulkan_present_rejected": state.rejected_present_events,
        "vulkan_present_rejected_out_of_date": state.benign_rejected_present_events,
        "vulkan_present_rejected_fatal": state.fatal_rejected_present_events,
        "vulkan_present_wait_timeout": state.wait_timeout_events,
        "vulkan_present_wait_failure": state.wait_failure_events,
        "vulkan_present_timing_queries": state.present_timing_query_events,
        "vulkan_present_timing_completed": state.present_timing_complete_events,
        "vulkan_present_timing_incomplete": state.present_timing_incomplete_events,
        "vulkan_present_timing_duplicate": state.present_timing_duplicate_events,
        "vulkan_present_timing_unknown_id": state.present_timing_unknown_id_events,
        "vulkan_present_timing_rejected_result_ignored": state.present_timing_rejected_result_events,
        "vulkan_present_timing_out_of_order": state.present_timing_out_of_order_events,
        "vulkan_present_timing_zero_or_missing_stage": state.present_timing_zero_stage_events,
        "vulkan_present_timing_zero_time": state.present_timing_zero_time_events,
        "vulkan_present_timing_failure": state.present_timing_failure_events,
        "vulkan_present_timing_timeout": state.present_timing_timeout_events,
        "vulkan_present_timing_queue_full": state.present_timing_queue_full_events,
        "vulkan_timing_properties_changes": state.timing_properties_change_events,
        "vulkan_time_domain_changes": state.time_domain_change_events,
        "vulkan_timing_configured_swapchains": state.configured_swapchain_events,
        "vulkan_swapchain_changes": state.swapchain_changes,
        "correct_changed_presented": state.presented.len(),
        "unchanged_present": state.unchanged_present_events,
        "superseded_before_present": state.superseded_before_present,
        "superseded_before_marker_observation": state.superseded_before_marker_observation,
        "ambiguous_completion": state.ambiguous_completion_events,
        "nonqualifying_completion": state.nonqualifying_completion_events,
        "window_unavailable_completion": state.window_unavailable_completion_events,
        "configure": state.configure_events,
        "application_frame_generation_changes": state.surface_generation_changes,
        "pending_at_close": state.pending.len(),
        "present_wait_bindings_at_close": state.present_bindings.len(),
        "rejected_present_tombstones_at_close": state.rejected_present_bindings.len(),
        "present_timing_bindings_at_close": state.timing_bindings.len(),
        "early_present_timing_records_at_close": state.early_timing_records.len(),
        "present_timing_records_at_close": state.timing_records.len(),
        "marker_outcomes_at_close": state.marker_outcomes.len(),
        "dropped_records": state.dropped_records,
    });
    let configured_swapchain_history = state
        .configured_swapchain_history
        .iter()
        .map(|(generation, configuration)| {
            json!({
                "generation": generation,
                "present_id2_supported": configuration.present_id2_supported,
                "present_timing_supported": configuration.present_timing_supported,
                "present_stage_queries": configuration.present_stage_queries,
                "time_domain": configuration.time_domain.as_raw(),
                "time_domain_id": configuration.time_domain_id,
            })
        })
        .collect::<Vec<_>>();
    let present_timing = json!({
        "extension": "VK_EXT_present_timing",
        "extension_revision": 3,
        "stage": "VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT",
        "queue_size": wgpu::hal::vulkan::present_wait_observer::PRESENT_TIMING_QUEUE_SIZE,
        "configured_swapchain_history": configured_swapchain_history,
        "per_swapchain_generation_counters": state.timing_counters,
    });
    let exact_cadence = json!({
        "status": exact_cadence_status,
        "metric": "first_pixel_out_scanout_cadence",
        "physical_photon_visibility_claimed": false,
    });
    let report = json!({
        "schema": REPORT_SCHEMA,
        "schema_version": REPORT_SCHEMA_VERSION,
        "authority": AUTHORITY,
        "status": status,
        "probe_kind": if is_probe { "controlled_boundary_activation" } else { "representative_product_session" },
        "scenario": scenario,
        "clocks": clocks,
        "environment": environment,
        "boundary_proof": boundary_proof,
        "event_counts": event_counts,
        "present_timing": present_timing,
        "work_identity": state.latest_work_identity,
        "metrics": metrics,
        "exact_cadence": exact_cadence,
        "presented_records": state.presented,
        "state_transitions": state.transitions,
        "first_error": state.event_error,
        "first_ambiguous_completion": state.first_ambiguous_completion,
        "internal_publication_metrics_are_diagnostics_only": true,
        "presentation_visibility_claimed": status == "pass",
        "exact_cadence_claimed": exact_cadence_status == "pass",
        "scanout_cadence_claimed": exact_cadence_status == "pass",
        "click_to_photon_claimed": false,
    });
    let bytes = serde_json::to_vec_pretty(&report)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn product_metrics(state: &ObserverState) -> Option<Value> {
    let maximum_active_gap = maximum_active_gap(&state.presented, &state.transitions)?;
    let standalone_intervals = scanout_intervals(
        &state.presented,
        &state.transitions,
        "standalone_interaction",
    )?;
    let four_panel_intervals = scanout_intervals(
        &state.presented,
        &state.transitions,
        "four_panel_interaction",
    )?;
    let resident_input_response = resident_input_response_samples(&state.presented)?;
    let standalone_interval_p95 = nearest_rank(&standalone_intervals, 95)?;
    let four_panel_interval_p95 = nearest_rank(&four_panel_intervals, 95)?;
    let resident_input_response_p99 = nearest_rank(&resident_input_response, 99)?;
    let resident_exact_samples =
        resident_exact_settlement_samples(&state.presented, &state.transitions)?;
    let resident_exact = resident_exact_samples.iter().copied().max()?;
    let prepared_exact = phase_to_first_exact_after_new_input(
        &state.presented,
        &state.transitions,
        "prepared_nonresident_replacement",
    )?;
    let startup_coarse = state
        .presented
        .iter()
        .find(|record| record.phase == "startup")?
        .input_to_visible_coarse_ns;
    let startup_exact = state
        .presented
        .iter()
        .find(|record| record.phase == "startup" && record.exact)?
        .input_to_visible_coarse_ns;
    Some(json!({
        "standalone_present_interval_p95_ns": standalone_interval_p95,
        "four_panel_present_interval_p95_ns": four_panel_interval_p95,
        "resident_input_response_p99_ns": resident_input_response_p99,
        "maximum_active_visible_gap_ns": maximum_active_gap,
        "resident_exact_settlement_ns": resident_exact,
        "prepared_nonresident_exact_replacement_ns": prepared_exact,
        "startup_complete_coarse_ns": startup_coarse,
        "startup_exact_settlement_ns": startup_exact,
        "raw_vectors": {
            "standalone_present_intervals_ns": standalone_intervals,
            "four_panel_present_intervals_ns": four_panel_intervals,
            "resident_input_response_ns": resident_input_response,
            "resident_exact_settlement_ns": resident_exact_samples,
            "prepared_nonresident_exact_replacement_ns": [prepared_exact],
            "maximum_active_visible_gap_ns": [maximum_active_gap],
            "startup_complete_coarse_ns": [startup_coarse],
            "startup_exact_settlement_ns": [startup_exact],
        },
    }))
}

fn scanout_intervals(
    records: &[PresentedRecord],
    transitions: &[StateTransition],
    phase: &str,
) -> Option<Vec<u64>> {
    let mut intervals = Vec::new();
    for (start, end, _) in active_intervals(transitions, phase)? {
        let active = records
            .iter()
            .filter(|record| {
                record.phase == phase
                    && record.active_input
                    && record.observed_at_ns >= start
                    && record.observed_at_ns <= end
            })
            .collect::<Vec<_>>();
        for pair in active.windows(2) {
            if pair[0].swapchain_generation != pair[1].swapchain_generation
                || pair[0].time_domain != pair[1].time_domain
                || pair[0].time_domain_id != pair[1].time_domain_id
            {
                continue;
            }
            let interval = pair[1]
                .first_pixel_out_ns
                .checked_sub(pair[0].first_pixel_out_ns)?;
            if interval == 0 {
                return None;
            }
            intervals.push(interval);
        }
    }
    (!intervals.is_empty()).then_some(intervals)
}

fn resident_input_response_samples(records: &[PresentedRecord]) -> Option<Vec<u64>> {
    let mut first_for_generation = BTreeMap::<u64, u64>::new();
    for record in records.iter().filter(|record| {
        record.active_input
            && matches!(
                record.phase.as_str(),
                "standalone_interaction" | "four_panel_interaction"
            )
    }) {
        first_for_generation
            .entry(record.input_generation)
            .or_insert(record.input_to_visible_coarse_ns);
    }
    (!first_for_generation.is_empty()).then(|| first_for_generation.into_values().collect())
}

fn nearest_rank(values: &[u64], percentile: usize) -> Option<u64> {
    if values.is_empty() || !(1..=100).contains(&percentile) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = percentile.checked_mul(sorted.len())?.checked_add(99)? / 100;
    sorted.get(rank.saturating_sub(1)).copied()
}

fn has_strict_present_pair(records: &[PresentedRecord]) -> bool {
    records.windows(2).any(|pair| {
        pair[1].marker_sequence > pair[0].marker_sequence
            && pair[1].present_id > pair[0].present_id
            && pair[1].observed_at_ns >= pair[0].observed_at_ns
    })
}

fn maximum_active_gap(records: &[PresentedRecord], transitions: &[StateTransition]) -> Option<u64> {
    let mut maximum = 0_u64;
    for phase in ["standalone_interaction", "four_panel_interaction"] {
        for (start, end, _) in active_intervals(transitions, phase)? {
            let presented = records
                .iter()
                .filter(|record| {
                    record.phase == phase
                        && record.active_input
                        && record.observed_at_ns >= start
                        && record.observed_at_ns <= end
                })
                .collect::<Vec<_>>();
            let first = presented.first()?;
            let last = presented.last()?;
            maximum = maximum.max(first.observed_at_ns.saturating_sub(start));
            maximum = maximum.max(end.saturating_sub(last.observed_at_ns));
            for pair in presented.windows(2) {
                maximum = maximum.max(
                    pair[1]
                        .observed_at_ns
                        .saturating_sub(pair[0].observed_at_ns),
                );
            }
        }
    }
    Some(maximum)
}

fn resident_exact_settlement_samples(
    records: &[PresentedRecord],
    transitions: &[StateTransition],
) -> Option<Vec<u64>> {
    let mut samples = Vec::new();
    for phase in ["standalone_interaction", "four_panel_interaction"] {
        for (_, release_at_ns, input_generation) in active_intervals(transitions, phase)? {
            let exact = records
                .iter()
                .find(|record| {
                    record.exact
                        && record.observed_at_ns >= release_at_ns
                        && record.input_generation >= input_generation
                })
                .or_else(|| {
                    records.iter().rev().find(|record| {
                        record.exact
                            && record.observed_at_ns < release_at_ns
                            && record.input_generation >= input_generation
                    })
                })?;
            samples.push(exact.observed_at_ns.saturating_sub(release_at_ns));
        }
    }
    Some(samples)
}

fn active_intervals(transitions: &[StateTransition], phase: &str) -> Option<Vec<(u64, u64, u64)>> {
    let mut active_start = None;
    let mut intervals = Vec::new();
    for transition in transitions {
        if transition.phase == phase && transition.active_input {
            active_start.get_or_insert(transition.observed_at_ns);
        } else if let Some(start) = active_start.take() {
            intervals.push((
                start,
                transition.observed_at_ns,
                transition.input_generation,
            ));
        }
    }
    if active_start.is_some() || intervals.is_empty() {
        return None;
    }
    Some(intervals)
}

fn phase_to_first_exact_after_new_input(
    records: &[PresentedRecord],
    transitions: &[StateTransition],
    phase: &str,
) -> Option<u64> {
    let start = transitions
        .iter()
        .find(|transition| transition.phase == phase)?;
    let exact = records.iter().find(|record| {
        record.phase == phase
            && record.exact
            && record.observed_at_ns >= start.observed_at_ns
            && record.input_generation > start.input_generation
    })?;
    Some(exact.observed_at_ns.saturating_sub(start.observed_at_ns))
}

fn record_error(shared: &Shared, error: String) {
    if let Ok(mut state) = shared.state.lock() {
        state.event_error.get_or_insert(error);
    }
}

fn record_window_unavailable_completion(shared: &Shared) {
    if let Ok(mut state) = shared.state.lock() {
        state.window_unavailable_completion_events =
            state.window_unavailable_completion_events.saturating_add(1);
        state.nonqualifying_completion_events =
            state.nonqualifying_completion_events.saturating_add(1);
    }
}

fn record_ambiguous_completion(shared: &Shared, error: String) {
    if let Ok(mut state) = shared.state.lock() {
        state.ambiguous_completion_events = state.ambiguous_completion_events.saturating_add(1);
        state
            .first_ambiguous_completion
            .get_or_insert_with(|| error.clone());
        state.event_error.get_or_insert(error);
    }
}

fn elapsed_ns(epoch: Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(phase: &str, at: u64, active: bool, exact: bool) -> PresentedRecord {
        PresentedRecord {
            marker_sequence: u32::try_from(at).unwrap(),
            present_id: at,
            observed_at_ns: at,
            wait_return_delay_ns: 1,
            phase: phase.to_owned(),
            command_index: 0,
            active_input: active,
            exact,
            identity: json!(at),
            input_generation: at,
            input_to_visible_coarse_ns: at,
            surface_generation: 1,
            work_identity: json!({"profile": "unit"}),
            swapchain_generation: 1,
            first_pixel_out_ns: at,
            time_domain: vk::TimeDomainKHR::CLOCK_MONOTONIC.as_raw(),
            time_domain_id: 7,
        }
    }

    #[test]
    fn marker_payload_round_trips_and_detects_sequence_corruption() {
        for sequence in [0, 1, 42, u16::MAX as u32, u32::MAX] {
            let bits = marker_bits(sequence);
            assert!(!bits[0]);
            assert!(bits[1]);
            assert_eq!(decode_bits(&bits[4..36]) as u32, sequence);
            assert_eq!(decode_bits(&bits[36..52]) as u16, marker_checksum(sequence));
            assert_eq!(decode_bits(&bits[52..60]), 0xd7);
        }
    }

    #[test]
    fn presentation_metrics_keep_interaction_stalls_and_refinement_separate() {
        let records = vec![
            record("startup", 10, false, false),
            record("startup", 20, false, true),
            record("standalone_interaction", 100, true, false),
            record("standalone_interaction", 130, true, false),
            record("standalone_interaction", 160, true, false),
            record("four_panel_interaction", 200, true, false),
            record("four_panel_interaction", 240, true, false),
            record("four_panel_interaction", 280, true, false),
            record("resident_settlement", 350, false, true),
            record("prepared_nonresident_replacement", 500, false, true),
        ];
        let transitions = vec![
            StateTransition {
                observed_at_ns: 90,
                phase: "standalone_interaction".to_owned(),
                active_input: true,
                command_index: 0,
                input_generation: 1,
            },
            StateTransition {
                observed_at_ns: 170,
                phase: "standalone_interaction".to_owned(),
                active_input: false,
                command_index: 0,
                input_generation: 2,
            },
            StateTransition {
                observed_at_ns: 190,
                phase: "four_panel_interaction".to_owned(),
                active_input: true,
                command_index: 0,
                input_generation: 3,
            },
            StateTransition {
                observed_at_ns: 290,
                phase: "four_panel_interaction".to_owned(),
                active_input: false,
                command_index: 0,
                input_generation: 4,
            },
            StateTransition {
                observed_at_ns: 300,
                phase: "resident_settlement".to_owned(),
                active_input: false,
                command_index: 0,
                input_generation: 4,
            },
            StateTransition {
                observed_at_ns: 400,
                phase: "prepared_nonresident_replacement".to_owned(),
                active_input: false,
                command_index: 0,
                input_generation: 4,
            },
        ];
        let state = ObserverState {
            presented: records,
            transitions,
            ..ObserverState::default()
        };
        let metrics = product_metrics(&state).unwrap();
        assert_eq!(metrics["maximum_active_visible_gap_ns"], 40);
        assert_eq!(metrics["standalone_present_interval_p95_ns"], 30);
        assert_eq!(metrics["four_panel_present_interval_p95_ns"], 40);
        assert_eq!(metrics["resident_input_response_p99_ns"], 280);
        assert_eq!(metrics["resident_exact_settlement_ns"], 180);
        assert_eq!(metrics["prepared_nonresident_exact_replacement_ns"], 100);
        assert_eq!(metrics["startup_complete_coarse_ns"], 10);
        assert_eq!(metrics["startup_exact_settlement_ns"], 20);

        let no_startup = ObserverState {
            presented: state
                .presented
                .iter()
                .filter(|record| record.phase != "startup")
                .cloned()
                .collect(),
            transitions: state.transitions.clone(),
            ..ObserverState::default()
        };
        assert!(product_metrics(&no_startup).is_none());
    }

    #[test]
    fn present_ids_classify_superseded_and_nonmonotonic_visibility() {
        let mut state = ObserverState {
            marker_sequence: 2,
            ..ObserverState::default()
        };
        state.pending.insert(1, pending(1));
        state.pending.insert(2, pending(2));
        let first = record_completed_marker(&mut state, 2, 20_000, completion(2)).unwrap();
        assert_eq!(first.marker_sequence, 2);
        assert!(state.pending.is_empty());
        assert_eq!(state.superseded_before_present, 1);
        assert_eq!(state.ambiguous_completion_events, 0);

        state.marker_sequence = 3;
        state.pending.insert(3, pending(3));
        assert!(record_completed_marker(&mut state, 3, 30_000, completion(3)).is_some());
        assert!(record_completed_marker(&mut state, 3, 31_000, completion(4)).is_none());
        assert_eq!(state.unchanged_present_events, 1);

        state.marker_sequence = 4;
        state.pending.insert(4, pending(4));
        assert!(record_completed_marker(&mut state, 4, 40_000, completion(5)).is_some());

        state.marker_sequence = 5;
        state.pending.insert(5, pending(5));
        assert!(record_completed_marker(&mut state, 5, 50_000, completion(4)).is_none());
        assert_eq!(state.nonqualifying_completion_events, 1);
    }

    fn pending(sequence: u32) -> PendingFrame {
        PendingFrame {
            marker_sequence: sequence,
            enqueued_at_ns: u64::from(sequence) * 1_000,
            observation: PresentationObservation {
                scenario: "representative_gpu_presentation_probe".to_owned(),
                phase: "startup".to_owned(),
                command_index: sequence as usize,
                active_input: false,
                eligible: true,
                exact: false,
                identity: json!(sequence),
                input_generation: u64::from(sequence),
                input_age_ns: 100,
                surface_generation: 1,
                work_identity: json!({"profile": "unit"}),
                environment_identity: json!({"adapter": "unit"}),
            },
        }
    }

    fn completion(present_id: u64) -> CompletionStamp {
        CompletionStamp {
            present_id,
            wait_return_delay_ns: 1,
            swapchain_generation: 1,
        }
    }

    fn present_binding(
        swapchain_generation: u64,
        window_lifecycle_generation: u64,
    ) -> PresentBinding {
        PresentBinding {
            swapchain: 11,
            swapchain_generation,
            marker_sequence: 1,
            marker_spec: MarkerSpec {
                x: 4,
                y: 4,
                cell_pixels: 2,
            },
            window_lifecycle_generation,
            submitted_at_ns: Some(1),
        }
    }

    fn timing_configuration(generation: u64) -> ConfiguredSwapchain {
        ConfiguredSwapchain {
            generation,
            configuration: HalTimingConfiguration {
                present_id2_supported: true,
                present_timing_supported: true,
                present_stage_queries:
                    wgpu::hal::vulkan::present_wait_observer::PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT,
                queue_size: wgpu::hal::vulkan::present_wait_observer::PRESENT_TIMING_QUEUE_SIZE,
                time_domain: vk::TimeDomainKHR::CLOCK_MONOTONIC,
                time_domain_id: 7,
            },
        }
    }

    #[test]
    fn probe_recovery_is_scoped_to_lifecycle_changes_after_each_present() {
        let mut state = ObserverState {
            scenario: Some("representative_gpu_presentation_probe".to_owned()),
            window_lifecycle_generation: 7,
            ..ObserverState::default()
        };
        state
            .configured_swapchain_history
            .insert(1, timing_configuration(1).configuration);

        let stable = present_binding(1, 7);
        assert!(!binding_crossed_controlled_lifecycle(&state, stable));

        state.window_lifecycle_generation = 8;
        assert!(binding_crossed_controlled_lifecycle(&state, stable));

        let after_window_transition = present_binding(1, 8);
        assert!(!binding_crossed_controlled_lifecycle(
            &state,
            after_window_transition
        ));

        state
            .configured_swapchain_history
            .insert(2, timing_configuration(2).configuration);
        assert!(binding_crossed_controlled_lifecycle(
            &state,
            after_window_transition
        ));

        let stable_after_recreation = present_binding(2, 8);
        assert!(!binding_crossed_controlled_lifecycle(
            &state,
            stable_after_recreation
        ));

        state.scenario = Some("representative_gpu_interaction".to_owned());
        state.window_lifecycle_generation = 9;
        assert!(!binding_crossed_controlled_lifecycle(&state, stable));
    }

    fn timing_record(present_id: u64, time_ns: u64) -> HalTimingRecord {
        HalTimingRecord {
            present_id,
            present_stage_count: 1,
            stage: wgpu::hal::vulkan::present_wait_observer::PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT,
            time_ns,
            time_domain: vk::TimeDomainKHR::CLOCK_MONOTONIC,
            time_domain_id: 7,
            report_complete: true,
        }
    }

    fn bind_timing(state: &mut ObserverState, present_id: u64, generation: u64) {
        state.timing_bindings.insert(
            present_id,
            TimingBinding {
                swapchain: 11,
                swapchain_generation: generation,
                submitted_at_ns: 1,
            },
        );
    }

    fn qualified(present_id: u64, marker_sequence: u32, generation: u64) -> MarkerOutcome {
        MarkerOutcome::Qualified(Box::new(QualifiedPresent {
            marker_sequence,
            present_id,
            observed_at_ns: present_id * 100,
            wait_return_delay_ns: 1,
            phase: "standalone_interaction".to_owned(),
            command_index: 0,
            active_input: true,
            exact: false,
            identity: json!(present_id),
            input_generation: present_id,
            input_to_visible_coarse_ns: 1,
            surface_generation: 1,
            work_identity: json!({"profile": "unit"}),
            swapchain_generation: generation,
        }))
    }

    #[test]
    fn timing_and_marker_correlate_in_either_arrival_order() {
        let configured = timing_configuration(1);
        let mut state = ObserverState::default();

        bind_timing(&mut state, 1, 1);
        accept_timing_record(&mut state, 11, configured, timing_record(1, 1_000));
        assert!(state.presented.is_empty());
        state.marker_outcomes.insert(1, qualified(1, 1, 1));
        finalize_timing_correlation(&mut state, 1);
        assert_eq!(state.presented.len(), 1);
        assert_eq!(state.presented[0].first_pixel_out_ns, 1_000);

        bind_timing(&mut state, 2, 1);
        state.marker_outcomes.insert(2, qualified(2, 2, 1));
        accept_timing_record(&mut state, 11, configured, timing_record(2, 2_000));
        assert_eq!(state.presented.len(), 2);
        assert_eq!(state.presented[1].present_id, 2);
    }

    #[test]
    fn timing_rejects_duplicate_unknown_zero_out_of_order_and_cross_generation_data() {
        let configured = timing_configuration(1);
        let mut state = ObserverState::default();
        bind_timing(&mut state, 1, 1);
        accept_timing_record(&mut state, 11, configured, timing_record(1, 1_000));
        accept_timing_record(&mut state, 11, configured, timing_record(1, 1_000));
        assert_eq!(state.present_timing_duplicate_events, 1);

        accept_timing_record(&mut state, 11, configured, timing_record(9, 9_000));
        assert_eq!(state.present_timing_unknown_id_events, 1);

        state.present_bindings.insert(
            11,
            PresentBinding {
                swapchain: 11,
                swapchain_generation: 1,
                marker_sequence: 11,
                marker_spec: MarkerSpec {
                    x: 0,
                    y: 0,
                    cell_pixels: 1,
                },
                window_lifecycle_generation: 0,
                submitted_at_ns: None,
            },
        );
        accept_timing_record(&mut state, 11, configured, timing_record(11, 11_000));
        let early = state.early_timing_records.remove(&11).unwrap();
        state.present_bindings.get_mut(&11).unwrap().submitted_at_ns = Some(1);
        bind_timing(&mut state, 11, 1);
        accept_timing_record(
            &mut state,
            early.raw_swapchain,
            early.configured,
            early.record,
        );
        assert_eq!(state.present_timing_complete_events, 2);
        assert_eq!(state.present_timing_unknown_id_events, 1);

        state.rejected_present_bindings.insert(
            10,
            PresentBinding {
                swapchain: 11,
                swapchain_generation: 1,
                marker_sequence: 10,
                marker_spec: MarkerSpec {
                    x: 0,
                    y: 0,
                    cell_pixels: 1,
                },
                window_lifecycle_generation: 0,
                submitted_at_ns: None,
            },
        );
        accept_timing_record(&mut state, 11, configured, timing_record(10, 10_000));
        assert_eq!(state.present_timing_rejected_result_events, 1);
        assert_eq!(state.present_timing_unknown_id_events, 1);

        bind_timing(&mut state, 2, 1);
        accept_timing_record(&mut state, 11, configured, timing_record(2, 0));
        assert_eq!(state.present_timing_zero_time_events, 1);

        let mut out_of_order = ObserverState::default();
        bind_timing(&mut out_of_order, 1, 1);
        bind_timing(&mut out_of_order, 2, 1);
        accept_timing_record(&mut out_of_order, 11, configured, timing_record(2, 2_000));
        accept_timing_record(&mut out_of_order, 11, configured, timing_record(1, 1_000));
        assert_eq!(out_of_order.present_timing_out_of_order_events, 1);

        let mut crossed = ObserverState::default();
        bind_timing(&mut crossed, 1, 1);
        accept_timing_record(
            &mut crossed,
            11,
            timing_configuration(2),
            timing_record(1, 1_000),
        );
        assert_eq!(crossed.time_domain_change_events, 1);
    }

    #[test]
    fn nearest_rank_distinguishes_a_60_hz_tail_from_a_30_hz_tail() {
        let mut samples = vec![16_667_000; 19];
        samples.push(33_333_000);
        assert_eq!(nearest_rank(&samples, 95), Some(16_667_000));
        assert_eq!(nearest_rank(&samples, 99), Some(33_333_000));
    }

    #[test]
    fn prepared_replacement_cannot_finish_on_the_preexisting_exact_frame() {
        let mut stale = record("prepared_nonresident_replacement", 110, false, true);
        stale.input_generation = 7;
        let mut replacement = record("prepared_nonresident_replacement", 160, false, true);
        replacement.input_generation = 8;
        let transitions = [StateTransition {
            observed_at_ns: 100,
            phase: "prepared_nonresident_replacement".to_owned(),
            active_input: false,
            command_index: 0,
            input_generation: 7,
        }];
        assert_eq!(
            phase_to_first_exact_after_new_input(
                &[stale, replacement],
                &transitions,
                "prepared_nonresident_replacement"
            ),
            Some(60)
        );
    }
}
