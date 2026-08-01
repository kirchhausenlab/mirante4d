use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rustix::time::{ClockId, clock_gettime};

use crate::{
    ImportError, ImportEvent, ImportStage, ImportStageTiming, ImportStatistics,
    ordered_workers::CPU_WORKERS_MAX,
};

const FILE_DESCRIPTOR_SAMPLE_INTERVAL: Duration = Duration::from_millis(5);

// Descriptor authorities are fixed and phase-scoped:
// - current canonical cache (directory + 2 files), one decode-ahead cache
//   (directory + 2 files), and the spool owner (directory + 4 files) = 11
//   resident;
// - checkpoint recovery can clone at most 2 files at once;
// - positional readers are one canonical and one spool handle shared by all
//   workers; a worker task opens no per-task descriptor;
// - source-native decode holds at most one source file per admitted worker;
// - local publication holds parent + stage (2), plus at most a traversed
//   directory + current object (2);
// - staged validation holds at most one object plus one directory iterator;
// - resource sampling holds one directory iterator; and
// - executable provenance holds one file.
// These maxima are deliberately summed even though their phases cannot all
// overlap. Any authority that adds a descriptor must update this admission
// contract and its pressure test.
const CHECKPOINT_RESIDENT_FILE_DESCRIPTORS_MAX: u64 = 11;
const CHECKPOINT_RECOVERY_CLONES_MAX: u64 = 2;
const PHASE_SHARED_WORKER_READER_FILE_DESCRIPTORS_MAX: u64 = 2;
const WORKER_TASK_FILE_DESCRIPTORS_MAX: u64 = 0;
const SOURCE_TRAVERSAL_FILE_DESCRIPTORS_MAX: u64 = CPU_WORKERS_MAX as u64;
const PUBLICATION_RESIDENT_FILE_DESCRIPTORS_MAX: u64 = 2;
const PUBLICATION_TRANSIENT_FILE_DESCRIPTORS_MAX: u64 = 2;
const STAGED_VALIDATION_FILE_DESCRIPTORS_MAX: u64 = 2;
const RESOURCE_SAMPLING_FILE_DESCRIPTORS_MAX: u64 = 1;
const EXECUTABLE_PROVENANCE_FILE_DESCRIPTORS_MAX: u64 = 1;

pub(crate) const IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND: u64 =
    CHECKPOINT_RESIDENT_FILE_DESCRIPTORS_MAX
        + CHECKPOINT_RECOVERY_CLONES_MAX
        + PHASE_SHARED_WORKER_READER_FILE_DESCRIPTORS_MAX
        + WORKER_TASK_FILE_DESCRIPTORS_MAX
        + SOURCE_TRAVERSAL_FILE_DESCRIPTORS_MAX
        + PUBLICATION_RESIDENT_FILE_DESCRIPTORS_MAX
        + PUBLICATION_TRANSIENT_FILE_DESCRIPTORS_MAX
        + STAGED_VALIDATION_FILE_DESCRIPTORS_MAX
        + RESOURCE_SAMPLING_FILE_DESCRIPTORS_MAX
        + EXECUTABLE_PROVENANCE_FILE_DESCRIPTORS_MAX;

pub(crate) const fn conservative_file_descriptor_peak(sampled_peak: u64) -> u64 {
    if sampled_peak > IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND {
        sampled_peak
    } else {
        IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND
    }
}

/// Samples process descriptors throughout the primary clock and attributes
/// descriptors whose `/proc` targets belong to the reviewed source,
/// checkpoint, destination, or writer-owned private-stage path scopes.
///
/// This captures descriptors created by any import worker thread without
/// instrumenting every file wrapper. Attribution is path-based rather than an
/// ownership token: an unrelated descriptor to the exact same source or
/// destination-parent path is conservatively included. Linux is the qualified
/// product target; other targets report no sample.
pub(crate) struct ImportFileDescriptorMonitor {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl ImportFileDescriptorMonitor {
    pub(crate) fn start(
        source: &Path,
        checkpoint: &Path,
        destination: &Path,
    ) -> Result<Self, ImportError> {
        #[cfg(target_os = "linux")]
        {
            let scope =
                ImportPathScope::new(source, checkpoint, destination).map_err(|source| {
                    ImportError::Io {
                        operation: "prepare import file-descriptor monitor",
                        path: destination.to_path_buf(),
                        source,
                    }
                })?;
            let stop = Arc::new(AtomicBool::new(false));
            let peak = Arc::new(AtomicU64::new(0));
            sample_import_path_file_descriptors(&scope, &peak);
            let worker_stop = Arc::clone(&stop);
            let worker_peak = Arc::clone(&peak);
            let worker = thread::Builder::new()
                .name("mirante4d-import-fd-monitor".to_owned())
                .spawn(move || {
                    while !worker_stop.load(Ordering::Acquire) {
                        sample_import_path_file_descriptors(&scope, &worker_peak);
                        thread::park_timeout(FILE_DESCRIPTOR_SAMPLE_INTERVAL);
                    }
                    sample_import_path_file_descriptors(&scope, &worker_peak);
                })
                .map_err(|source| ImportError::Io {
                    operation: "start import file-descriptor monitor",
                    path: destination.to_path_buf(),
                    source,
                })?;
            Ok(Self {
                stop,
                peak,
                worker: Some(worker),
            })
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (source, checkpoint, destination);
            Ok(Self {
                stop: Arc::new(AtomicBool::new(false)),
                peak: Arc::new(AtomicU64::new(0)),
                worker: None,
            })
        }
    }

    #[cfg(test)]
    pub(crate) fn finish(mut self) -> Result<u64, ImportError> {
        self.stop_and_join()?;
        Ok(self.peak.load(Ordering::Relaxed))
    }

    /// Finalizes evidence after atomic publication without converting a
    /// diagnostics failure into a false import failure. `u64::MAX` is a
    /// fail-closed sentinel for every qualification resource gate.
    pub(crate) fn finish_after_publication(mut self) -> u64 {
        if self.stop_and_join().is_err() {
            return u64::MAX;
        }
        self.peak.load(Ordering::Relaxed)
    }

    fn stop_and_join(&mut self) -> Result<(), ImportError> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            worker.join().map_err(|_| ImportError::Io {
                operation: "join import file-descriptor monitor",
                path: PathBuf::from("/proc/self/fd"),
                source: io::Error::other("import file-descriptor monitor panicked"),
            })?;
        }
        Ok(())
    }
}

impl Drop for ImportFileDescriptorMonitor {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

#[cfg(target_os = "linux")]
struct ImportPathScope {
    source: PathBuf,
    source_is_directory: bool,
    checkpoint: PathBuf,
    destination: PathBuf,
    destination_parent: PathBuf,
    stage_name_prefix: String,
}

#[cfg(target_os = "linux")]
impl ImportPathScope {
    fn new(source: &Path, checkpoint: &Path, destination: &Path) -> io::Result<Self> {
        let source_is_directory = fs::metadata(source)?.is_dir();
        let source = normalize_existing_or_future_path(source)?;
        let checkpoint = normalize_existing_or_future_path(checkpoint)?;
        let destination = normalize_existing_or_future_path(destination)?;
        let destination_parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("import destination has no parent"))?
            .to_path_buf();
        Ok(Self {
            source,
            source_is_directory,
            checkpoint,
            destination,
            destination_parent,
            stage_name_prefix: format!(".mirante4d-stage-{}-", std::process::id()),
        })
    }

    fn contains(&self, target: &Path) -> bool {
        (if self.source_is_directory {
            target.starts_with(&self.source)
        } else {
            target == self.source
        }) || target.starts_with(&self.checkpoint)
            || target.starts_with(&self.destination)
            || target == self.destination_parent
            || self.is_private_stage_path(target)
    }

    fn is_private_stage_path(&self, target: &Path) -> bool {
        let Ok(relative) = target.strip_prefix(&self.destination_parent) else {
            return false;
        };
        let Some(Component::Normal(name)) = relative.components().next() else {
            return false;
        };
        name.to_string_lossy().starts_with(&self.stage_name_prefix)
    }
}

#[cfg(target_os = "linux")]
fn normalize_existing_or_future_path(path: &Path) -> io::Result<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("import path has no final component"))?;
    Ok(canonical_parent.join(name))
}

#[cfg(target_os = "linux")]
fn sample_import_path_file_descriptors(scope: &ImportPathScope, peak: &AtomicU64) {
    let Ok(entries) = fs::read_dir("/proc/self/fd") else {
        return;
    };
    let mut open = 0_u64;
    for entry in entries.flatten() {
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        if target.is_absolute() && scope.contains(&target) {
            open = open.saturating_add(1);
        }
    }
    peak.fetch_max(open, Ordering::Relaxed);
}

pub(crate) struct PrimaryClock {
    wall_started: Instant,
    cpu_started_ns: u64,
}

impl PrimaryClock {
    pub(crate) fn start() -> Result<Self, ImportError> {
        Ok(Self {
            wall_started: Instant::now(),
            cpu_started_ns: process_cpu_time_ns()?,
        })
    }

    /// Records the end of the primary clock after atomic publication. Any
    /// impossible clock-representation failure becomes fail-closed evidence,
    /// never an `ImportError` after the destination is already visible.
    pub(crate) fn finish_after_publication(self, statistics: &mut ImportStatistics) {
        statistics.primary_wall_time_ns =
            u64::try_from(self.wall_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        statistics.primary_cpu_time_ns = process_cpu_time_ns()
            .and_then(|finished| {
                finished
                    .checked_sub(self.cpu_started_ns)
                    .ok_or(ImportError::Overflow)
            })
            .unwrap_or(u64::MAX);
    }
}

pub(crate) struct StageClock {
    stage: ImportStage,
    wall_started: Instant,
    cpu_started_ns: u64,
}

impl StageClock {
    pub(crate) fn start(
        stage: ImportStage,
        completed_work_units: u64,
        total_work_units: Option<u64>,
        progress: &mut impl FnMut(ImportEvent),
    ) -> Result<Self, ImportError> {
        progress(ImportEvent::StageStarted {
            stage,
            completed_work_units,
            total_work_units,
        });
        Ok(Self {
            stage,
            wall_started: Instant::now(),
            cpu_started_ns: process_cpu_time_ns()?,
        })
    }

    pub(crate) fn finish(
        self,
        statistics: &mut ImportStatistics,
        progress: &mut impl FnMut(ImportEvent),
    ) -> Result<ImportStageTiming, ImportError> {
        let timing = ImportStageTiming {
            stage: self.stage,
            wall_time_ns: duration_ns(self.wall_started.elapsed())?,
            cpu_time_ns: process_cpu_time_ns()?
                .checked_sub(self.cpu_started_ns)
                .ok_or(ImportError::Overflow)?,
        };
        statistics.stages.push(timing);
        progress(ImportEvent::StageFinished(timing));
        Ok(timing)
    }
}

pub(crate) fn sample_process_resources(
    statistics: &mut ImportStatistics,
    owned_temporary_paths: &[&Path],
) -> Result<(), ImportError> {
    if let Some(rss) = linux_process_high_water_rss_bytes() {
        statistics.peak_process_rss_bytes = statistics.peak_process_rss_bytes.max(rss);
    }
    let temporary = owned_temporary_paths
        .iter()
        .try_fold(0_u64, |total, path| {
            total
                .checked_add(owned_regular_file_bytes(path)?)
                .ok_or(ImportError::Overflow)
        })?;
    statistics.peak_temporary_bytes = statistics.peak_temporary_bytes.max(temporary);
    Ok(())
}

fn process_cpu_time_ns() -> Result<u64, ImportError> {
    let time = clock_gettime(ClockId::ProcessCPUTime);
    let seconds = u64::try_from(time.tv_sec).map_err(|_| ImportError::Overflow)?;
    let nanoseconds = u64::try_from(time.tv_nsec).map_err(|_| ImportError::Overflow)?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanoseconds))
        .ok_or(ImportError::Overflow)
}

fn duration_ns(duration: Duration) -> Result<u64, ImportError> {
    u64::try_from(duration.as_nanos()).map_err(|_| ImportError::Overflow)
}

fn linux_process_high_water_rss_bytes() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024))
}

pub(crate) fn owned_regular_file_bytes(path: &Path) -> Result<u64, ImportError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(ImportError::Io {
                operation: "sample import-owned bytes",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path).map_err(|source| ImportError::Io {
        operation: "sample import-owned directory",
        path: path.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| ImportError::Io {
            operation: "sample import-owned directory entry",
            path: path.to_path_buf(),
            source,
        })?;
        total = total
            .checked_add(owned_regular_file_bytes(&entry.path())?)
            .ok_or(ImportError::Overflow)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_clock_emits_named_start_and_finish_with_monotonic_measurements() {
        let mut events = Vec::new();
        let mut statistics = ImportStatistics::default();
        let clock = StageClock::start(ImportStage::BaseProduction, 2, Some(9), &mut |event| {
            events.push(event)
        })
        .unwrap();
        let timing = clock
            .finish(&mut statistics, &mut |event| events.push(event))
            .unwrap();

        assert_eq!(timing.stage, ImportStage::BaseProduction);
        assert_eq!(statistics.stages, vec![timing]);
        assert!(matches!(
            events.first(),
            Some(ImportEvent::StageStarted {
                stage: ImportStage::BaseProduction,
                completed_work_units: 2,
                total_work_units: Some(9),
            })
        ));
        assert_eq!(events.last(), Some(&ImportEvent::StageFinished(timing)));
    }

    #[test]
    fn descriptor_structural_admission_bound_is_below_the_product_gate() {
        assert_eq!(IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND, 39);
        assert_eq!(WORKER_TASK_FILE_DESCRIPTORS_MAX, 0);
        const { assert!(IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND <= 64) };
        assert_eq!(
            conservative_file_descriptor_peak(7),
            IMPORT_OPEN_FILE_DESCRIPTOR_STRUCTURAL_BOUND
        );
        assert_eq!(conservative_file_descriptor_peak(42), 42);
    }

    #[test]
    fn resource_sample_counts_only_regular_owned_bytes() {
        let root = tempfile::tempdir().unwrap();
        let checkpoint = root.path().join("checkpoint");
        let destination = root.path().join("destination");
        fs::create_dir_all(checkpoint.join("nested")).unwrap();
        fs::write(checkpoint.join("one"), [0_u8; 3]).unwrap();
        fs::write(checkpoint.join("nested/two"), [0_u8; 5]).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("three"), [0_u8; 7]).unwrap();

        let mut statistics = ImportStatistics::default();
        sample_process_resources(&mut statistics, &[&checkpoint, &destination]).unwrap();

        assert_eq!(statistics.peak_temporary_bytes, 15);
        assert!(statistics.peak_process_rss_bytes > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_monitor_captures_worker_opened_source_checkpoint_and_stage_files() {
        use std::fs::File;
        use std::sync::mpsc;

        const CHECKPOINT_FILES: usize = 40;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let checkpoint = root.path().join("checkpoint");
        let destination = root.path().join("destination.m4d");
        let stage = root.path().join(format!(
            ".mirante4d-stage-{}-monitor-test",
            std::process::id()
        ));
        fs::create_dir(&source).unwrap();
        fs::create_dir(&checkpoint).unwrap();
        fs::create_dir(&stage).unwrap();
        fs::write(source.join("plane.tif"), b"source").unwrap();
        fs::write(stage.join("object"), b"stage").unwrap();
        for index in 0..CHECKPOINT_FILES {
            fs::write(
                checkpoint.join(format!("checkpoint-{index}")),
                b"checkpoint",
            )
            .unwrap();
        }

        let monitor =
            ImportFileDescriptorMonitor::start(&source, &checkpoint, &destination).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut files = Vec::with_capacity(CHECKPOINT_FILES + 2);
            files.push(File::open(source.join("plane.tif")).unwrap());
            files.push(File::open(stage.join("object")).unwrap());
            for index in 0..CHECKPOINT_FILES {
                files.push(File::open(checkpoint.join(format!("checkpoint-{index}"))).unwrap());
            }
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            files
        });
        ready_rx.recv().unwrap();

        let expected = u64::try_from(CHECKPOINT_FILES + 2).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while monitor.peak.load(Ordering::Relaxed) < expected && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        release_tx.send(()).unwrap();
        drop(worker.join().unwrap());

        let sampled = monitor.finish().unwrap();
        assert!(sampled >= expected);
        assert!(conservative_file_descriptor_peak(sampled) >= expected);
    }
}
