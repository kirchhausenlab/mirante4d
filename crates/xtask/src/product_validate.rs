use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt, process::ExitStatusExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_import_pipeline::{TiffChannelSourceKind, deterministic_tiff_destination};
use mirante4d_storage::{LocalPackageCatalog, PACKAGE_VALIDATION_WORKING_BYTES};
use serde_json::{Value, json};

use crate::{
    host::benchmark_host_context,
    process::{
        BoundedOutputPolicy, isolate_process_tree, run_cargo, run_command_with_bounded_output,
        terminate_process_tree,
    },
    product_automation_progress::{
        FILE_POLL_INTERVAL, ProductAutomationProgressLaunch, ProductAutomationProgressPlan,
        ProgressMonitorAction, SafeProgressSnapshot, SafeProgressState,
        safe_automation_progress_line,
    },
    reports::{read_json_file, write_json_file},
    target_fixture::extract_target_u16_fixture,
};

const PRODUCT_VALIDATION_SCHEMA: &str = "mirante4d-product-validation-report";
pub(crate) const PRODUCT_AUTOMATION_SCRIPT_SCHEMA: &str = "mirante4d-product-automation-script";
pub(crate) const PRODUCT_AUTOMATION_REPORT_SCHEMA: &str = "mirante4d-product-automation-report";
pub(crate) const SCRIPT_SCHEMA_VERSION: u32 = 11;
pub(crate) const REPORT_SCHEMA_VERSION: u32 = 9;
pub(crate) const IMPORT_OPEN_READY_COMPLETE_STATUS: &str = "open_ready_complete";
pub(crate) const PRODUCT_AUTOMATION_HARD_SAFETY_LIMIT_FIELDS: [&str; 12] = [
    "max_cpu_total_bytes",
    "max_cpu_decoded_residency_bytes",
    "max_cpu_upload_staging_bytes",
    "max_cpu_in_flight_decode_bytes",
    "max_cpu_metadata_and_indexes_bytes",
    "max_cpu_queues_and_results_bytes",
    "max_cpu_prefetch_bytes",
    "max_cpu_import_working_set_bytes",
    "max_runtime_queued_requests",
    "max_runtime_in_flight_decodes",
    "max_runtime_pending_completions",
    "max_runtime_resident_resources",
];
const PRODUCT_VALIDATION_SCHEMA_VERSION: u32 = 2;
const PUBLICATION_CURRENTNESS_CONTRACT_ID: &str =
    "mirante4d-publication-currentness-inventory-snapshot-inventory-1";
const OUTPUT_DIR: &str = "target/mirante4d/product-validation";
const OUTPUT_DIR_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_OUTPUT_DIR";
const TIMEOUT_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS";
const ALLOW_NO_DISPLAY_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_ALLOW_NO_DISPLAY";
const SKIP_RELEASE_BUILD_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD";
const APP_BINARY_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY";
const DISPLAY_CLASS_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS";
const SCENARIO_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_SCENARIO";
const PREFLIGHT_ONLY_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_PREFLIGHT_ONLY";
const GENERATED_FIXTURE_SCENARIO: &str = "target_fixture_camera_smoke";
const GENERATED_RENDER_MODES_SCENARIO: &str = "target_fixture_render_modes";
const REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO: &str = "representative_native_navigation";
const REPRESENTATIVE_TEMPORAL_PLAYBACK_SCENARIO: &str = "representative_temporal_playback";
const REPRESENTATIVE_GPU_INTERACTION_SCENARIO: &str = "representative_gpu_interaction";
const REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO: &str =
    "representative_gpu_presentation_probe";
const PRESENTATION_OBSERVER_REPORT_ENV: &str = "MIRANTE4D_PRESENTATION_OBSERVER_REPORT";
const B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO: &str = "target_package_integrity_audit";
const IMPORT_PREPROCESSING_SCENARIO: &str = "import_preprocessing";
const B4_PROJECT_PERSISTENCE_SCENARIO: &str = "b4_project_persistence";
const PRE_ALPHA_RELIABILITY_SCENARIO: &str = "pre_alpha_reliability";
const B4_TRUSTED_REPORT_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_PROJECT_STORE_LIFECYCLE_REPORT";
const B4_CHECKPOINT_SCHEMA: &str = "mirante4d-product-external-kill-checkpoint";
const B4_CHECKPOINT_STAGE: &str = "after_real_autosave_before_external_kill";
const PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE: &str =
    "after_provisional_autosave_before_external_kill";
const PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE: &str = "mapped_clean_window_ready_for_native_close";
const B4_AUTOSAVE_MIN_ELAPSED_MS: u64 = 30_000;
const B4_PHASE_TIMEOUT_SECS: u64 = 90;
const NATIVE_CLOSE_EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const B3_SCENARIO_TIMEOUT_SECS: u64 = 180;
const IMPORT_SCENARIO_TIMEOUT_SECS: u64 = 600;
const B4_PRIMARY_CLIENT_WIDTH: u32 = 1280;
const B4_PRIMARY_CLIENT_HEIGHT: u32 = 720;
const B4_SECONDARY_CLIENT_WIDTH: u32 = 1920;
const B4_SECONDARY_CLIENT_HEIGHT: u32 = 1080;
const GENERATED_VIEWPORT_WIDTH: u32 = 1280;
const GENERATED_VIEWPORT_HEIGHT: u32 = 720;
const GENERATED_RESIZED_VIEWPORT_WIDTH: u32 = 1920;
const GENERATED_RESIZED_VIEWPORT_HEIGHT: u32 = 1080;
const B3_VIEWPORT_WIDTH: u32 = 1280;
const B3_VIEWPORT_HEIGHT: u32 = 720;
const B3_SECOND_VIEWPORT_WIDTH: u32 = 1920;
const B3_SECOND_VIEWPORT_HEIGHT: u32 = 1080;
const B3_PRIMARY_E1_CAPTURE: &str = "b3-after-success-1280x720";
const B3_SECONDARY_E1_CAPTURE: &str = "b3-after-success-1920x1080";
const IMPORT_FIXTURE_Z: u32 = 65;
const IMPORT_FIXTURE_Y: u32 = 12_737;
const IMPORT_FIXTURE_X: u32 = 65;
const IMPORT_DURABLE_PREFIX_WORK_UNITS: u64 = 512;
const IMPORT_CPU_TOTAL_LIMIT_BYTES: u64 = 1_024 * MIB;
// The broker accounts its non-stealable complete-progress reservation as
// ImportWorkingSet even while most of the reservation is unused. Keep that
// authority separate from the actual importer peak asserted in the receipt.
const IMPORT_PROGRESS_RESERVATION_LIMIT_BYTES: u64 = 768 * MIB;
const IMPORT_PEAK_WORKING_BYTES_LIMIT: u64 = 512 * MIB;
const IMPORT_RESIDENT_RESOURCE_LIMIT: u64 = 1_024;
const IMPORT_VIEWPORT_WIDTH: u32 = 1_280;
const IMPORT_VIEWPORT_HEIGHT: u32 = 720;
const MIB: u64 = 1024 * 1024;
const PREFLIGHT_ONLY_DISPLAY_SOURCE: &str = "preflight_only";
const SOURCE_CLOSURE_EVIDENCE_ENTRY_MAX: usize = 131_072;
const SOURCE_CLOSURE_EVIDENCE_BYTES_MAX: u64 = 256 * MIB;
const PRE_ALPHA_RECOVERY_ROOT_ENTRIES_MAX: usize = 64;
const PRE_ALPHA_STDERR_BYTES_MAX: u64 = 4 * MIB;
const X11_AUTOMATION_OUTPUT_POLICY: BoundedOutputPolicy = BoundedOutputPolicy {
    scope: "x11_automation",
    inactivity_timeout: Duration::from_secs(2),
    absolute_timeout: Duration::from_secs(3),
    progress_interval: Duration::from_secs(1),
    max_stdout_bytes: 64 * 1024,
    max_stderr_bytes: 64 * 1024,
};
const PRESENTATION_PROBE_MINIMIZED_HOLD: Duration = Duration::from_millis(250);
const PRESENTATION_PROBE_WINDOW_CONTROL_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceClosureSnapshot {
    entries: Vec<SourceClosureEntry>,
    regular_files: u64,
    source_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceClosureEntry {
    Directory(Vec<u8>),
    File {
        relative_path: Vec<u8>,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceFileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl SourceClosureSnapshot {
    fn capture(root: &Path) -> anyhow::Result<Self> {
        let root_metadata = fs::symlink_metadata(root)
            .with_context(|| format!("failed to inspect source closure root {}", root.display()))?;
        if !root_metadata.file_type().is_dir() {
            bail!("source closure root is not a directory");
        }

        let mut entries = Vec::new();
        let mut stack = vec![(root.to_path_buf(), Vec::<u8>::new())];
        let mut regular_files = 0_u64;
        let mut source_bytes = 0_u64;
        while let Some((directory, relative)) = stack.pop() {
            let mut children = fs::read_dir(&directory)
                .context("failed to traverse source closure directory")?
                .collect::<Result<Vec<_>, _>>()
                .context("failed to enumerate source closure")?;
            children.sort_by(|left, right| {
                left.file_name()
                    .as_bytes()
                    .cmp(right.file_name().as_bytes())
            });
            for child in children.into_iter().rev() {
                if entries.len() >= SOURCE_CLOSURE_EVIDENCE_ENTRY_MAX {
                    bail!("source closure exceeds the evidence entry bound");
                }
                let name = child.file_name();
                let name = name.as_bytes();
                if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
                    bail!("source closure contains an invalid relative name");
                }
                let mut child_relative = relative.clone();
                if !child_relative.is_empty() {
                    child_relative.push(b'/');
                }
                child_relative.extend_from_slice(name);
                let child_path = child.path();
                let before = fs::symlink_metadata(&child_path)
                    .context("failed to inspect source closure entry")?;
                if before.file_type().is_dir() {
                    entries.push(SourceClosureEntry::Directory(child_relative.clone()));
                    stack.push((child_path, child_relative));
                } else if before.file_type().is_file() {
                    let stamp = source_file_stamp(&before);
                    source_bytes = source_bytes
                        .checked_add(stamp.length)
                        .context("source closure byte count overflowed")?;
                    if source_bytes > SOURCE_CLOSURE_EVIDENCE_BYTES_MAX {
                        bail!("source closure exceeds the evidence byte bound");
                    }
                    let bytes = fs::read(&child_path)
                        .context("failed to read source closure evidence bytes")?;
                    let after = fs::symlink_metadata(&child_path)
                        .context("failed to re-inspect source closure entry")?;
                    if source_file_stamp(&after) != stamp
                        || u64::try_from(bytes.len()).ok() != Some(stamp.length)
                    {
                        bail!("source closure changed while evidence was captured");
                    }
                    regular_files = regular_files
                        .checked_add(1)
                        .context("source closure file count overflowed")?;
                    entries.push(SourceClosureEntry::File {
                        relative_path: child_relative,
                        bytes,
                    });
                } else {
                    bail!("source closure contains a symlink or special entry");
                }
            }
        }
        entries.sort_by(|left, right| {
            source_closure_entry_path(left).cmp(source_closure_entry_path(right))
        });
        Ok(Self {
            entries,
            regular_files,
            source_bytes,
        })
    }

    fn pending_json(&self) -> Value {
        json!({
            "required": true,
            "comparison": "exact_relative_entry_and_regular_file_bytes",
            "byte_identical": Value::Null,
            "before": {
                "entries": self.entries.len(),
                "regular_files": self.regular_files,
                "source_bytes": self.source_bytes,
            },
            "after": Value::Null,
        })
    }

    fn compare_json(&self, root: &Path) -> anyhow::Result<Value> {
        let after = Self::capture(root)?;
        Ok(json!({
            "required": true,
            "comparison": "exact_relative_entry_and_regular_file_bytes",
            "byte_identical": self == &after,
            "before": {
                "entries": self.entries.len(),
                "regular_files": self.regular_files,
                "source_bytes": self.source_bytes,
            },
            "after": {
                "entries": after.entries.len(),
                "regular_files": after.regular_files,
                "source_bytes": after.source_bytes,
            },
            "bounds": {
                "maximum_entries": SOURCE_CLOSURE_EVIDENCE_ENTRY_MAX,
                "maximum_source_bytes": SOURCE_CLOSURE_EVIDENCE_BYTES_MAX,
            },
            "paths_or_source_bytes_reported": false,
        }))
    }
}

fn source_file_stamp(metadata: &fs::Metadata) -> SourceFileStamp {
    SourceFileStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn source_closure_entry_path(entry: &SourceClosureEntry) -> &[u8] {
    match entry {
        SourceClosureEntry::Directory(path) => path,
        SourceClosureEntry::File { relative_path, .. } => relative_path,
    }
}

pub(crate) fn product_validate(
    package: Option<&Path>,
    scenario: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let outcome = product_validate_report_with_scenario(package, scenario)?;
    if outcome.status.is_failure() {
        bail!(
            "product validation finished with status {}; see {}",
            outcome.status.name(),
            outcome.report_path.display()
        );
    }
    Ok(outcome.report_path)
}

pub(crate) fn is_product_validation_scenario_name(name: &str) -> bool {
    ProductValidationScenario::is_named_scenario(name)
}

pub(crate) fn product_validate_report_with_scenario(
    package: Option<&Path>,
    scenario: Option<&str>,
) -> anyhow::Result<ProductValidationOutcome> {
    validate_product_validation_output_root()?;
    let scenario =
        ProductValidationScenario::resolve(scenario, env::var(SCENARIO_ENV).ok().as_deref())?;
    product_validate_report_inner(package, &scenario)
}

fn product_validate_report_inner(
    package: Option<&Path>,
    scenario: &ProductValidationScenario,
) -> anyhow::Result<ProductValidationOutcome> {
    if matches!(scenario, ProductValidationScenario::B4ProjectPersistence) {
        return product_validate_b4_project_persistence(package, scenario);
    }
    if matches!(scenario, ProductValidationScenario::PreAlphaReliability) {
        return product_validate_pre_alpha_reliability(package, scenario);
    }
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    let binary = ProductValidationAppBinary::from_environment()?;
    let output_dir = product_validation_output_dir(scenario);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let (package, script, preserved_source) =
        product_validation_package_and_script(package, scenario)?;
    validate_product_automation_script(&script)
        .context("generated product automation script is invalid")?;
    let source_closure_before = preserved_source
        .as_deref()
        .map(SourceClosureSnapshot::capture)
        .transpose()?;
    let pending_source_closure_evidence = source_closure_before
        .as_ref()
        .map_or(Value::Null, SourceClosureSnapshot::pending_json);
    let script_path = output_dir.join("product-automation-script.json");
    let automation_report_path = output_dir.join("product-automation-report.json");
    let wrapper_report_path = output_dir.join("product-validation-report.json");
    let stdout_path = output_dir.join("mirante4d-app.stdout.log");
    let stderr_path = output_dir.join("mirante4d-app.stderr.log");
    let runtime_log_path = output_dir.join("mirante4d-app.runtime.log");
    if automation_report_path.exists() {
        fs::remove_file(&automation_report_path).with_context(|| {
            format!(
                "failed to remove stale {} before product validation",
                automation_report_path.display()
            )
        })?;
    }
    write_json_file(&script_path, &script)?;
    let timeout_seconds = timeout_secs(scenario);
    let preflight_only = env_flag(PREFLIGHT_ONLY_ENV);

    if preflight_only {
        write_wrapper_report(WrapperReport {
            path: &wrapper_report_path,
            scenario_name: scenario.name(),
            status: ProductValidationStatus::Unsupported,
            failure_reason: Some(
                "product validation preflight requested; generated the automation script and \
                 wrapper report without building or launching the native app"
                    .to_owned(),
            ),
            started_at_epoch_ms,
            duration_ms: duration_ms(started_at.elapsed()),
            timeout_secs: timeout_seconds,
            package: &package,
            binary: binary.path(),
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            runtime_log: &runtime_log_path,
            display: DisplayClassification {
                class: DisplayClass::Unsupported,
                source: PREFLIGHT_ONLY_DISPLAY_SOURCE,
            },
            preflight_only,
            source_closure_evidence: pending_source_closure_evidence.clone(),
            automation_status: None,
            exit_status: None,
            exit_success: None,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::Unsupported,
        });
    }

    let display = display_status();
    if display.class == DisplayClass::Unsupported && !env_flag(ALLOW_NO_DISPLAY_ENV) {
        write_wrapper_report(WrapperReport {
            path: &wrapper_report_path,
            scenario_name: scenario.name(),
            status: ProductValidationStatus::Unsupported,
            failure_reason: Some(
                "product validation requires DISPLAY or WAYLAND_DISPLAY; set \
                 MIRANTE4D_PRODUCT_VALIDATE_ALLOW_NO_DISPLAY=1 to attempt launch anyway"
                    .to_owned(),
            ),
            started_at_epoch_ms,
            duration_ms: duration_ms(started_at.elapsed()),
            timeout_secs: timeout_seconds,
            package: &package,
            binary: binary.path(),
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            runtime_log: &runtime_log_path,
            display,
            preflight_only,
            source_closure_evidence: pending_source_closure_evidence.clone(),
            automation_status: None,
            exit_status: None,
            exit_success: None,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::Unsupported,
        });
    }

    if binary.should_build_default_release()
        && let Err(err) = run_cargo(["build", "--release", "-p", "mirante4d-app"])
    {
        write_wrapper_report(WrapperReport {
            path: &wrapper_report_path,
            scenario_name: scenario.name(),
            status: ProductValidationStatus::Failed,
            failure_reason: Some(format!("release app build failed: {err}")),
            started_at_epoch_ms,
            duration_ms: duration_ms(started_at.elapsed()),
            timeout_secs: timeout_seconds,
            package: &package,
            binary: binary.path(),
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            runtime_log: &runtime_log_path,
            display,
            preflight_only,
            source_closure_evidence: pending_source_closure_evidence.clone(),
            automation_status: None,
            exit_status: None,
            exit_success: None,
        })?;
        return Err(err);
    }

    binary.validate_for_launch()?;
    fs::File::create(&runtime_log_path)
        .with_context(|| format!("failed to create {}", runtime_log_path.display()))?;
    let timeout = Duration::from_secs(timeout_seconds);
    let progress_root = fs::canonicalize(&output_dir)
        .context("product validation output directory is unavailable for progress monitoring")?;
    let status = run_product_automation(ProductAutomationRun {
        binary: binary.path(),
        package: &package,
        script: &script_path,
        automation_report: &automation_report_path,
        stdout_path: &stdout_path,
        stderr_path: &stderr_path,
        runtime_log_path: &runtime_log_path,
        timeout,
        scenario: scenario.name(),
        progress_plan: ProductAutomationProgressPlan::from_script(&script)?,
        progress_launch: ProductAutomationProgressLaunch::new_replacing_stale(&progress_root)?,
        external_control: ProductAutomationExternalControlPlan::from_script(
            scenario.name(),
            &script,
        )?,
    })?;
    let source_closure_evidence = source_closure_before
        .as_ref()
        .zip(preserved_source.as_deref())
        .map(|(before, source)| before.compare_json(source))
        .transpose()?
        .unwrap_or(Value::Null);
    let source_closure_changed = source_closure_evidence
        .get("byte_identical")
        .and_then(Value::as_bool)
        == Some(false);

    if status.progress_failure_reason.is_some() || status.external_control_failure_reason.is_some()
    {
        let failure_reason = match (
            status.progress_failure_reason,
            status.external_control_failure_reason.as_deref(),
        ) {
            (Some(progress_failure), Some(control_failure)) => format!(
                "native app automation progress protocol failed: {progress_failure}; external window control failed: {control_failure}"
            ),
            (Some(progress_failure), None) => {
                format!("native app automation progress protocol failed: {progress_failure}")
            }
            (None, Some(control_failure)) => {
                format!("external window control failed: {control_failure}")
            }
            (None, None) => unreachable!("guard requires at least one process control failure"),
        };
        write_wrapper_report(WrapperReport {
            path: &wrapper_report_path,
            scenario_name: scenario.name(),
            status: ProductValidationStatus::Failed,
            failure_reason: Some(failure_reason),
            started_at_epoch_ms,
            duration_ms: duration_ms(started_at.elapsed()),
            timeout_secs: timeout_seconds,
            package: &package,
            binary: binary.path(),
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            runtime_log: &runtime_log_path,
            display,
            preflight_only,
            source_closure_evidence: source_closure_evidence.clone(),
            automation_status: None,
            exit_status: status.exit_status,
            exit_success: status.exit_success,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::Failed,
        });
    }

    if status.timed_out {
        write_wrapper_report(WrapperReport {
            path: &wrapper_report_path,
            scenario_name: scenario.name(),
            status: ProductValidationStatus::TimedOut,
            failure_reason: Some(format!(
                "native app did not finish product automation within {} seconds",
                timeout.as_secs()
            )),
            started_at_epoch_ms,
            duration_ms: duration_ms(started_at.elapsed()),
            timeout_secs: timeout_seconds,
            package: &package,
            binary: binary.path(),
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            runtime_log: &runtime_log_path,
            display,
            preflight_only,
            source_closure_evidence: source_closure_evidence.clone(),
            automation_status: None,
            exit_status: status.exit_status,
            exit_success: status.exit_success,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::TimedOut,
        });
    }

    let automation_report = if automation_report_path.exists() {
        Some(read_json_file(&automation_report_path)?)
    } else {
        None
    };
    let automation_status = automation_report
        .as_ref()
        .and_then(|report| report.get("status"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let app_exited_successfully = status.exit_success.unwrap_or(false);
    let (mut validation_status, mut failure_reason) = completed_product_validation_outcome(
        app_exited_successfully,
        automation_report.as_ref(),
        &script,
        &script_path,
    );
    if validation_status == ProductValidationStatus::Passed
        && matches!(scenario, ProductValidationScenario::B3PackageIntegrityAudit)
        && let Err(reason) = b3_exact_e1_capture_evidence(automation_report.as_ref())
    {
        validation_status = ProductValidationStatus::Failed;
        failure_reason = Some(reason);
    }
    if validation_status == ProductValidationStatus::Passed
        && matches!(scenario, ProductValidationScenario::ImportPreprocessing)
        && let Err(reason) = import_preprocessing_evidence(automation_report.as_ref())
    {
        validation_status = ProductValidationStatus::Failed;
        failure_reason = Some(reason);
    }
    if validation_status == ProductValidationStatus::Passed
        && matches!(
            scenario,
            ProductValidationScenario::RepresentativeTemporalPlayback
        )
        && let Err(reason) = representative_temporal_playback_evidence(automation_report.as_ref())
    {
        validation_status = ProductValidationStatus::Failed;
        failure_reason = Some(reason);
    }
    if validation_status == ProductValidationStatus::Passed
        && matches!(
            scenario,
            ProductValidationScenario::RepresentativeTemporalPlayback
        )
        && let Err(reason) = representative_temporal_playback_log_evidence(&runtime_log_path)
    {
        validation_status = ProductValidationStatus::Failed;
        failure_reason = Some(reason);
    }
    if source_closure_changed {
        validation_status = ProductValidationStatus::Failed;
        failure_reason = Some(
            "source closure changed during product validation; source bytes must remain identical"
                .to_owned(),
        );
    }
    write_wrapper_report(WrapperReport {
        path: &wrapper_report_path,
        scenario_name: scenario.name(),
        status: validation_status,
        failure_reason,
        started_at_epoch_ms,
        duration_ms: duration_ms(started_at.elapsed()),
        timeout_secs: timeout_seconds,
        package: &package,
        binary: binary.path(),
        script: &script_path,
        script_value: &script,
        automation_report: &automation_report_path,
        automation_report_value: automation_report.as_ref(),
        stdout: &stdout_path,
        stderr: &stderr_path,
        runtime_log: &runtime_log_path,
        display,
        preflight_only,
        source_closure_evidence,
        automation_status,
        exit_status: status.exit_status,
        exit_success: status.exit_success,
    })?;

    Ok(ProductValidationOutcome {
        report_path: wrapper_report_path,
        status: validation_status,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProductValidationScenario {
    GeneratedFixtureCameraSmoke,
    GeneratedFixtureRenderModes,
    RepresentativeNativeNavigation,
    RepresentativeTemporalPlayback,
    RepresentativeGpuInteraction,
    RepresentativeGpuPresentationProbe,
    B3PackageIntegrityAudit,
    ImportPreprocessing,
    B4ProjectPersistence,
    PreAlphaReliability,
}

impl ProductValidationScenario {
    fn name(&self) -> &'static str {
        match self {
            Self::GeneratedFixtureCameraSmoke => GENERATED_FIXTURE_SCENARIO,
            Self::GeneratedFixtureRenderModes => GENERATED_RENDER_MODES_SCENARIO,
            Self::RepresentativeNativeNavigation => REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO,
            Self::RepresentativeTemporalPlayback => REPRESENTATIVE_TEMPORAL_PLAYBACK_SCENARIO,
            Self::RepresentativeGpuInteraction => REPRESENTATIVE_GPU_INTERACTION_SCENARIO,
            Self::RepresentativeGpuPresentationProbe => {
                REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO
            }
            Self::B3PackageIntegrityAudit => B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO,
            Self::ImportPreprocessing => IMPORT_PREPROCESSING_SCENARIO,
            Self::B4ProjectPersistence => B4_PROJECT_PERSISTENCE_SCENARIO,
            Self::PreAlphaReliability => PRE_ALPHA_RELIABILITY_SCENARIO,
        }
    }

    fn resolve(explicit: Option<&str>, env_value: Option<&str>) -> anyhow::Result<Self> {
        let requested = explicit.or(env_value);
        match requested.unwrap_or(GENERATED_FIXTURE_SCENARIO) {
            GENERATED_FIXTURE_SCENARIO => Ok(Self::GeneratedFixtureCameraSmoke),
            GENERATED_RENDER_MODES_SCENARIO => Ok(Self::GeneratedFixtureRenderModes),
            REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO => Ok(Self::RepresentativeNativeNavigation),
            REPRESENTATIVE_TEMPORAL_PLAYBACK_SCENARIO => Ok(Self::RepresentativeTemporalPlayback),
            REPRESENTATIVE_GPU_INTERACTION_SCENARIO => Ok(Self::RepresentativeGpuInteraction),
            REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO => {
                Ok(Self::RepresentativeGpuPresentationProbe)
            }
            B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO => Ok(Self::B3PackageIntegrityAudit),
            IMPORT_PREPROCESSING_SCENARIO => Ok(Self::ImportPreprocessing),
            B4_PROJECT_PERSISTENCE_SCENARIO => Ok(Self::B4ProjectPersistence),
            PRE_ALPHA_RELIABILITY_SCENARIO => Ok(Self::PreAlphaReliability),
            other => bail!(
                "unknown product validation scenario {other:?}; expected \
                 {GENERATED_FIXTURE_SCENARIO}, {GENERATED_RENDER_MODES_SCENARIO}, \
                 {REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO}, \
                 {REPRESENTATIVE_TEMPORAL_PLAYBACK_SCENARIO}, \
                 {REPRESENTATIVE_GPU_INTERACTION_SCENARIO}, \
                 {REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO}, \
                 {B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO}, {IMPORT_PREPROCESSING_SCENARIO}, or \
                 {B4_PROJECT_PERSISTENCE_SCENARIO}, or {PRE_ALPHA_RELIABILITY_SCENARIO}"
            ),
        }
    }

    fn is_named_scenario(name: &str) -> bool {
        matches!(
            name,
            GENERATED_FIXTURE_SCENARIO
                | GENERATED_RENDER_MODES_SCENARIO
                | REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO
                | REPRESENTATIVE_TEMPORAL_PLAYBACK_SCENARIO
                | REPRESENTATIVE_GPU_INTERACTION_SCENARIO
                | REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO
                | B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO
                | IMPORT_PREPROCESSING_SCENARIO
                | B4_PROJECT_PERSISTENCE_SCENARIO
                | PRE_ALPHA_RELIABILITY_SCENARIO
        )
    }

    fn default_timeout_secs(&self) -> u64 {
        match self {
            Self::GeneratedFixtureCameraSmoke | Self::GeneratedFixtureRenderModes => 60,
            Self::RepresentativeNativeNavigation => B3_SCENARIO_TIMEOUT_SECS,
            Self::RepresentativeTemporalPlayback => B3_SCENARIO_TIMEOUT_SECS,
            Self::RepresentativeGpuInteraction => 15 * 60,
            Self::RepresentativeGpuPresentationProbe => B3_SCENARIO_TIMEOUT_SECS,
            Self::B3PackageIntegrityAudit => B3_SCENARIO_TIMEOUT_SECS,
            Self::ImportPreprocessing => IMPORT_SCENARIO_TIMEOUT_SECS,
            Self::B4ProjectPersistence => B4_PHASE_TIMEOUT_SECS * 3,
            Self::PreAlphaReliability => B4_PHASE_TIMEOUT_SECS * 3,
        }
    }
}

fn product_validation_output_dir(scenario: &ProductValidationScenario) -> PathBuf {
    env::var_os(OUTPUT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(OUTPUT_DIR))
        .join(scenario.name())
}

fn validate_product_validation_output_root() -> anyhow::Result<()> {
    let Some(path) = env::var_os(OUTPUT_DIR_ENV).map(PathBuf::from) else {
        return Ok(());
    };
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                Component::RootDir | Component::Normal(_) | Component::Prefix(_)
            )
        })
    {
        bail!("{OUTPUT_DIR_ENV} must be an absolute path containing only normal components");
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("{OUTPUT_DIR_ENV} must name a real directory when it exists");
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ProductValidationOutcome {
    pub(crate) report_path: PathBuf,
    pub(crate) status: ProductValidationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductValidationStatus {
    Passed,
    Unsupported,
    Failed,
    TimedOut,
}

impl ProductValidationStatus {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        }
    }

    pub(crate) fn is_failure(self) -> bool {
        matches!(self, Self::Failed | Self::TimedOut)
    }
}

struct B4AggregateReport<'a> {
    path: &'a Path,
    binary: &'a Path,
    status: ProductValidationStatus,
    failure_reason: Option<String>,
    started_at_epoch_ms: u128,
    started_at: Instant,
    package: &'a Path,
    run_dir: &'a Path,
    state_home: &'a Path,
    original_project: &'a Path,
    save_as_project: &'a Path,
    scripts: &'a [Value],
    attempts: &'a [Value],
    revision_identity: &'a Value,
    trusted_project_store_evidence: &'a Value,
    display: DisplayClassification,
    preflight_only: bool,
}

fn product_validate_b4_project_persistence(
    package: Option<&Path>,
    scenario: &ProductValidationScenario,
) -> anyhow::Result<ProductValidationOutcome> {
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    let binary = ProductValidationAppBinary::from_environment()?;
    let output_dir = product_validation_output_dir(scenario);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let run_dir = output_dir.join(format!("run-{}-{}", epoch_ms(), std::process::id()));
    fs::create_dir(&run_dir).with_context(|| {
        format!(
            "failed to create unique B4 run directory {}",
            run_dir.display()
        )
    })?;
    let work_dir = run_dir.join("work");
    let state_home = work_dir.join("xdg-state-home");
    fs::create_dir_all(&state_home)
        .with_context(|| format!("failed to create {}", state_home.display()))?;
    let package = match package {
        Some(package) => package.to_path_buf(),
        None => default_target_fixture()?,
    };
    let original_project = work_dir.join("original.m4dproj");
    let save_as_project = work_dir.join("recovered-save-as.m4dproj");
    let checkpoint = run_dir
        .join("launch-1")
        .join("external-kill-checkpoint.json");
    let scripts = [
        b4_launch_one_script(&package, &original_project, &checkpoint),
        b4_launch_two_script(&package, &original_project, &save_as_project),
        b4_launch_three_script(&package, &save_as_project),
    ];
    let phase_names = ["launch-1", "launch-2", "launch-3"];
    let mut script_records = Vec::with_capacity(3);
    for (phase, script) in phase_names.iter().zip(&scripts) {
        validate_product_automation_script(script)
            .with_context(|| format!("invalid fixed B4 automation script for {phase}"))?;
        let phase_dir = run_dir.join(phase);
        fs::create_dir_all(&phase_dir)
            .with_context(|| format!("failed to create {}", phase_dir.display()))?;
        let path = phase_dir.join("product-automation-script.json");
        write_json_file(&path, script)?;
        script_records.push(json!({
            "phase": phase,
            "path": path,
            "scenario": script.get("scenario").and_then(Value::as_str),
        }));
    }
    let report_path = run_dir.join("product-validation-report.json");
    let display = display_status();
    let preflight_only = env_flag(PREFLIGHT_ONLY_ENV);
    let revision_identity = b4_revision_identity(false).unwrap_or_else(|err| {
        json!({
            "available": false,
            "error": err.to_string(),
        })
    });
    let mut trusted_project_store_evidence = Value::Null;
    let mut attempts = Vec::new();

    if preflight_only {
        write_b4_aggregate_report(B4AggregateReport {
            path: &report_path,
            binary: binary.path(),
            status: ProductValidationStatus::Unsupported,
            failure_reason: Some(
                "B4 preflight generated all three scripts without building or launching the app"
                    .to_owned(),
            ),
            started_at_epoch_ms,
            started_at,
            package: &package,
            run_dir: &run_dir,
            state_home: &state_home,
            original_project: &original_project,
            save_as_project: &save_as_project,
            scripts: &script_records,
            attempts: &attempts,
            revision_identity: &revision_identity,
            trusted_project_store_evidence: &trusted_project_store_evidence,
            display: DisplayClassification {
                class: DisplayClass::Unsupported,
                source: PREFLIGHT_ONLY_DISPLAY_SOURCE,
            },
            preflight_only,
        })?;
        return Ok(ProductValidationOutcome {
            report_path,
            status: ProductValidationStatus::Unsupported,
        });
    }

    let setup = (|| -> anyhow::Result<(Value, Value)> {
        if display.class != DisplayClass::RealDisplay || env::var_os("DISPLAY").is_none() {
            bail!("B4 product validation requires a real X11 display");
        }
        require_b4_x11_tools()?;
        let identity = b4_revision_identity(true)?;
        let trusted_path = env::var_os(B4_TRUSTED_REPORT_ENV)
            .map(PathBuf::from)
            .with_context(|| {
                format!(
                    "B4 product validation requires {B4_TRUSTED_REPORT_ENV}=<same-revision verify-local project-store-lifecycle report>"
                )
            })?;
        let trusted = load_b4_trusted_project_store_evidence(&trusted_path, &identity)?;
        if binary.should_build_default_release() {
            run_cargo(["build", "--release", "-p", "mirante4d-app"])?;
        }
        binary.validate_for_launch()?;
        Ok((identity, trusted))
    })();
    let (identity, trusted) = match setup {
        Ok(values) => values,
        Err(err) => {
            write_b4_aggregate_report(B4AggregateReport {
                path: &report_path,
                binary: binary.path(),
                status: ProductValidationStatus::Failed,
                failure_reason: Some(err.to_string()),
                started_at_epoch_ms,
                started_at,
                package: &package,
                run_dir: &run_dir,
                state_home: &state_home,
                original_project: &original_project,
                save_as_project: &save_as_project,
                scripts: &script_records,
                attempts: &attempts,
                revision_identity: &revision_identity,
                trusted_project_store_evidence: &trusted_project_store_evidence,
                display,
                preflight_only,
            })?;
            return Ok(ProductValidationOutcome {
                report_path,
                status: ProductValidationStatus::Failed,
            });
        }
    };
    trusted_project_store_evidence = trusted;

    let specs = [
        B4AttemptSpec {
            number: 1,
            phase: "launch-1",
            script: &script_records[0]["path"],
            expected_client_width: B4_PRIMARY_CLIENT_WIDTH,
            expected_client_height: B4_PRIMARY_CLIENT_HEIGHT,
            expected_project: &original_project,
            termination: B4Termination::ExternalSigkill {
                checkpoint: &checkpoint,
                expected_stage: B4_CHECKPOINT_STAGE,
            },
        },
        B4AttemptSpec {
            number: 2,
            phase: "launch-2",
            script: &script_records[1]["path"],
            expected_client_width: B4_SECONDARY_CLIENT_WIDTH,
            expected_client_height: B4_SECONDARY_CLIENT_HEIGHT,
            expected_project: &save_as_project,
            termination: B4Termination::Normal,
        },
        B4AttemptSpec {
            number: 3,
            phase: "launch-3",
            script: &script_records[2]["path"],
            expected_client_width: B4_SECONDARY_CLIENT_WIDTH,
            expected_client_height: B4_SECONDARY_CLIENT_HEIGHT,
            expected_project: &save_as_project,
            termination: B4Termination::Normal,
        },
    ];

    for spec in specs {
        let attempt = run_b4_attempt(binary.path(), &package, &state_home, &run_dir, spec);
        let passed = attempt.get("status").and_then(Value::as_str) == Some("passed");
        attempts.push(attempt);
        let partial_failure = (!passed).then(|| {
            attempts
                .last()
                .and_then(|attempt| attempt.get("failure_reason"))
                .and_then(Value::as_str)
                .unwrap_or("B4 phase failed")
                .to_owned()
        });
        write_b4_aggregate_report(B4AggregateReport {
            path: &report_path,
            binary: binary.path(),
            status: ProductValidationStatus::Failed,
            failure_reason: partial_failure
                .or_else(|| Some("B4 scenario is incomplete".to_owned())),
            started_at_epoch_ms,
            started_at,
            package: &package,
            run_dir: &run_dir,
            state_home: &state_home,
            original_project: &original_project,
            save_as_project: &save_as_project,
            scripts: &script_records,
            attempts: &attempts,
            revision_identity: &identity,
            trusted_project_store_evidence: &trusted_project_store_evidence,
            display: display.clone(),
            preflight_only,
        })?;
        if !passed {
            return Ok(ProductValidationOutcome {
                report_path,
                status: ProductValidationStatus::Failed,
            });
        }
    }

    let (status, failure_reason) = match validate_b4_aggregate_attempts(&attempts) {
        Ok(()) => (ProductValidationStatus::Passed, None),
        Err(err) => (ProductValidationStatus::Failed, Some(err.to_string())),
    };
    write_b4_aggregate_report(B4AggregateReport {
        path: &report_path,
        binary: binary.path(),
        status,
        failure_reason,
        started_at_epoch_ms,
        started_at,
        package: &package,
        run_dir: &run_dir,
        state_home: &state_home,
        original_project: &original_project,
        save_as_project: &save_as_project,
        scripts: &script_records,
        attempts: &attempts,
        revision_identity: &identity,
        trusted_project_store_evidence: &trusted_project_store_evidence,
        display,
        preflight_only,
    })?;
    Ok(ProductValidationOutcome {
        report_path,
        status,
    })
}

struct PreAlphaReliabilityReport<'a> {
    path: &'a Path,
    binary: &'a Path,
    status: ProductValidationStatus,
    failure_reason: Option<String>,
    started_at_epoch_ms: u128,
    started_at: Instant,
    package: &'a Path,
    run_dir: &'a Path,
    recovery_state_home: &'a Path,
    clean_close_state_home: &'a Path,
    scripts: &'a [Value],
    attempts: &'a [Value],
    display: DisplayClassification,
    preflight_only: bool,
}

fn product_validate_pre_alpha_reliability(
    package: Option<&Path>,
    scenario: &ProductValidationScenario,
) -> anyhow::Result<ProductValidationOutcome> {
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    let binary = ProductValidationAppBinary::from_environment()?;
    let output_dir = product_validation_output_dir(scenario);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let run_dir = output_dir.join(format!("run-{}-{}", epoch_ms(), std::process::id()));
    fs::create_dir(&run_dir).with_context(|| {
        format!(
            "failed to create unique pre-alpha reliability directory {}",
            run_dir.display()
        )
    })?;
    let work_dir = run_dir.join("work");
    let recovery_state_home = work_dir.join("recovery-xdg-state-home");
    let clean_close_state_home = work_dir.join("clean-close-xdg-state-home");
    fs::create_dir_all(&recovery_state_home)
        .with_context(|| format!("failed to create {}", recovery_state_home.display()))?;
    fs::create_dir_all(&clean_close_state_home)
        .with_context(|| format!("failed to create {}", clean_close_state_home.display()))?;
    let package = match package {
        Some(package) => package.to_path_buf(),
        None => default_target_fixture()?,
    };
    let provisional_checkpoint = run_dir
        .join("launch-1-provisional")
        .join("external-process-checkpoint.json");
    let native_close_checkpoint = run_dir
        .join("launch-3-native-close")
        .join("external-process-checkpoint.json");
    let scripts = [
        pre_alpha_provisional_launch_script(&package, &provisional_checkpoint),
        pre_alpha_recovery_launch_script(&package),
        pre_alpha_native_close_launch_script(&package, &native_close_checkpoint),
    ];
    let phase_names = [
        "launch-1-provisional",
        "launch-2-recovery",
        "launch-3-native-close",
    ];
    let mut script_records = Vec::with_capacity(scripts.len());
    for (phase, script) in phase_names.iter().zip(&scripts) {
        validate_product_automation_script(script)
            .with_context(|| format!("invalid pre-alpha reliability script for {phase}"))?;
        let phase_dir = run_dir.join(phase);
        fs::create_dir_all(&phase_dir)
            .with_context(|| format!("failed to create {}", phase_dir.display()))?;
        let path = phase_dir.join("product-automation-script.json");
        write_json_file(&path, script)?;
        script_records.push(json!({
            "phase": phase,
            "path": path,
            "scenario": script.get("scenario").and_then(Value::as_str),
        }));
    }

    let report_path = run_dir.join("product-validation-report.json");
    let display = display_status();
    let preflight_only = env_flag(PREFLIGHT_ONLY_ENV);
    let mut attempts = Vec::new();
    if preflight_only {
        write_pre_alpha_reliability_report(PreAlphaReliabilityReport {
            path: &report_path,
            binary: binary.path(),
            status: ProductValidationStatus::Unsupported,
            failure_reason: Some(
                "pre-alpha reliability preflight generated all three scripts without launching the app"
                    .to_owned(),
            ),
            started_at_epoch_ms,
            started_at,
            package: &package,
            run_dir: &run_dir,
            recovery_state_home: &recovery_state_home,
            clean_close_state_home: &clean_close_state_home,
            scripts: &script_records,
            attempts: &attempts,
            display: DisplayClassification {
                class: DisplayClass::Unsupported,
                source: PREFLIGHT_ONLY_DISPLAY_SOURCE,
            },
            preflight_only,
        })?;
        return Ok(ProductValidationOutcome {
            report_path,
            status: ProductValidationStatus::Unsupported,
        });
    }

    let setup = (|| -> anyhow::Result<()> {
        if display.class != DisplayClass::RealDisplay || env::var_os("DISPLAY").is_none() {
            bail!("pre-alpha reliability validation requires a real X11 display");
        }
        require_b4_x11_tools()?;
        if binary.should_build_default_release() {
            run_cargo(["build", "--release", "-p", "mirante4d-app"])?;
        }
        binary.validate_for_launch()
    })();
    if let Err(err) = setup {
        write_pre_alpha_reliability_report(PreAlphaReliabilityReport {
            path: &report_path,
            binary: binary.path(),
            status: ProductValidationStatus::Failed,
            failure_reason: Some(err.to_string()),
            started_at_epoch_ms,
            started_at,
            package: &package,
            run_dir: &run_dir,
            recovery_state_home: &recovery_state_home,
            clean_close_state_home: &clean_close_state_home,
            scripts: &script_records,
            attempts: &attempts,
            display,
            preflight_only,
        })?;
        return Ok(ProductValidationOutcome {
            report_path,
            status: ProductValidationStatus::Failed,
        });
    }

    let specs = [
        PreAlphaAttemptSpec {
            number: 1,
            phase: phase_names[0],
            script: &script_records[0]["path"],
            state_home: &recovery_state_home,
            termination: B4Termination::ExternalSigkill {
                checkpoint: &provisional_checkpoint,
                expected_stage: PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE,
            },
        },
        PreAlphaAttemptSpec {
            number: 2,
            phase: phase_names[1],
            script: &script_records[1]["path"],
            state_home: &recovery_state_home,
            termination: B4Termination::Normal,
        },
        PreAlphaAttemptSpec {
            number: 3,
            phase: phase_names[2],
            script: &script_records[2]["path"],
            state_home: &clean_close_state_home,
            termination: B4Termination::ExternalNativeClose {
                checkpoint: &native_close_checkpoint,
                expected_stage: PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE,
            },
        },
    ];

    for spec in specs {
        let attempt = run_pre_alpha_attempt(binary.path(), &package, &run_dir, spec);
        let passed = attempt.get("status").and_then(Value::as_str) == Some("passed");
        attempts.push(attempt);
        let failure_reason = (!passed).then(|| {
            attempts
                .last()
                .and_then(|attempt| attempt.get("failure_reason"))
                .and_then(Value::as_str)
                .unwrap_or("pre-alpha reliability phase failed")
                .to_owned()
        });
        write_pre_alpha_reliability_report(PreAlphaReliabilityReport {
            path: &report_path,
            binary: binary.path(),
            status: ProductValidationStatus::Failed,
            failure_reason: failure_reason
                .or_else(|| Some("pre-alpha reliability scenario is incomplete".to_owned())),
            started_at_epoch_ms,
            started_at,
            package: &package,
            run_dir: &run_dir,
            recovery_state_home: &recovery_state_home,
            clean_close_state_home: &clean_close_state_home,
            scripts: &script_records,
            attempts: &attempts,
            display: display.clone(),
            preflight_only,
        })?;
        if !passed {
            return Ok(ProductValidationOutcome {
                report_path,
                status: ProductValidationStatus::Failed,
            });
        }
    }

    let (status, failure_reason) = match validate_pre_alpha_attempts(&attempts) {
        Ok(()) => (ProductValidationStatus::Passed, None),
        Err(err) => (ProductValidationStatus::Failed, Some(err.to_string())),
    };
    write_pre_alpha_reliability_report(PreAlphaReliabilityReport {
        path: &report_path,
        binary: binary.path(),
        status,
        failure_reason,
        started_at_epoch_ms,
        started_at,
        package: &package,
        run_dir: &run_dir,
        recovery_state_home: &recovery_state_home,
        clean_close_state_home: &clean_close_state_home,
        scripts: &script_records,
        attempts: &attempts,
        display,
        preflight_only,
    })?;
    Ok(ProductValidationOutcome {
        report_path,
        status,
    })
}

fn write_pre_alpha_reliability_report(report: PreAlphaReliabilityReport<'_>) -> anyhow::Result<()> {
    let all_source_byte_identical = report.attempts.len() == 3
        && report.attempts.iter().all(|attempt| {
            attempt
                .pointer("/source_closure_evidence/byte_identical")
                .and_then(Value::as_bool)
                == Some(true)
        });
    let value = json!({
        "schema": PRODUCT_VALIDATION_SCHEMA,
        "schema_version": PRODUCT_VALIDATION_SCHEMA_VERSION,
        "command": "product-validate",
        "status": report.status.name(),
        "failure_reason": report.failure_reason,
        "started_at_epoch_ms": report.started_at_epoch_ms,
        "started_at_utc": unix_epoch_ms_to_utc_rfc3339(report.started_at_epoch_ms),
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": duration_ms(report.started_at.elapsed()),
        "host": benchmark_host_context(),
        "build_profile": "release",
        "binary": report.binary,
        "dataset": dataset_context_json(report.package),
        "scenario": {
            "name": PRE_ALPHA_RELIABILITY_SCENARIO,
            "kind": "fixed_three_launch_packaged_recovery_and_native_close",
            "required_launches": 3,
            "scripts": report.scripts,
            "mapped_client_pixels": {
                "width": B4_PRIMARY_CLIENT_WIDTH,
                "height": B4_PRIMARY_CLIENT_HEIGHT,
            },
            "native_close_exit_deadline_seconds": NATIVE_CLOSE_EXIT_TIMEOUT.as_secs(),
        },
        "claim_boundary": {
            "evidence_type": "normal_release_multi_launch_with_external_sigkill_and_mapped_x11_close",
            "real_display_required": true,
            "provisional_autosave_recovery": true,
            "native_window_manager_close": true,
            "public_release_claim": false,
        },
        "environment": {
            "display_class": report.display.class.name(),
            "display_class_source": report.display.source,
            "display_env_present": env::var_os("DISPLAY").is_some(),
            "isolated_recovery_xdg_state_home": report.recovery_state_home,
            "isolated_clean_close_xdg_state_home": report.clean_close_state_home,
            "preflight_only": report.preflight_only,
        },
        "retry_policy": {
            "automatic_retries": 0,
            "attempts_per_phase_max": 1,
            "observed_retries": 0,
        },
        "source_nonmutation": {
            "required_per_launch": true,
            "all_three_launches_byte_identical": all_source_byte_identical,
        },
        "attempts": report.attempts,
        "artifacts": {
            "run_directory": report.run_dir,
            "retention": "all_phase_scripts_logs_reports_checkpoints_and_capture",
        },
    });
    write_json_file(report.path, &value)
}

fn write_b4_aggregate_report(report: B4AggregateReport<'_>) -> anyhow::Result<()> {
    let all_source_byte_identical = report.attempts.len() == 3
        && report.attempts.iter().all(|attempt| {
            attempt
                .pointer("/source_closure_evidence/byte_identical")
                .and_then(Value::as_bool)
                == Some(true)
        });
    let value = json!({
        "schema": PRODUCT_VALIDATION_SCHEMA,
        "schema_version": PRODUCT_VALIDATION_SCHEMA_VERSION,
        "command": "product-validate",
        "status": report.status.name(),
        "failure_reason": report.failure_reason,
        "started_at_epoch_ms": report.started_at_epoch_ms,
        "started_at_utc": unix_epoch_ms_to_utc_rfc3339(report.started_at_epoch_ms),
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": duration_ms(report.started_at.elapsed()),
        "revision": report.revision_identity,
        "host": benchmark_host_context(),
        "build_profile": "release",
        "binary": report.binary,
        "dataset": dataset_context_json(report.package),
        "scenario": {
            "name": B4_PROJECT_PERSISTENCE_SCENARIO,
            "kind": "fixed_three_launch_project_persistence_cutover",
            "required_launches": 3,
            "scripts": report.scripts,
            "client_area_requirements": [
                {"launch": 1, "width": B4_PRIMARY_CLIENT_WIDTH, "height": B4_PRIMARY_CLIENT_HEIGHT},
                {"launch": 2, "width": B4_SECONDARY_CLIENT_WIDTH, "height": B4_SECONDARY_CLIENT_HEIGHT},
                {"launch": 3, "width": B4_SECONDARY_CLIENT_WIDTH, "height": B4_SECONDARY_CLIENT_HEIGHT}
            ]
        },
        "claim_boundary": {
            "evidence_type": "native_multi_launch_internal_automation_with_external_x11_observation",
            "real_display_required": true,
            "externally_observed_client_pixels": true,
            "external_sigkill": true,
            "e4_product_open_satisfied": false,
            "reason": "final E4 additionally requires frozen external OS input and pixel oracles"
        },
        "environment": {
            "display_class": report.display.class.name(),
            "display_class_source": report.display.source,
            "display_env_present": env::var_os("DISPLAY").is_some(),
            "isolated_xdg_state_home": report.state_home,
            "preflight_only": report.preflight_only
        },
        "project_paths": {
            "original": report.original_project,
            "save_as": report.save_as_project
        },
        "retry_policy": {
            "automatic_retries": 0,
            "attempts_per_phase_max": 1,
            "observed_retries": 0
        },
        "trusted_project_store_evidence": report.trusted_project_store_evidence,
        "source_nonmutation": {
            "required_per_launch": true,
            "all_three_launches_byte_identical": all_source_byte_identical
        },
        "attempts": report.attempts,
        "artifacts": {
            "run_directory": report.run_dir,
            "retention": "all_phase_scripts_logs_reports_and_checkpoint"
        }
    });
    write_json_file(report.path, &value)
}

fn b4_revision_identity(require_clean: bool) -> anyhow::Result<Value> {
    let commit = git_stdout(&["rev-parse", "HEAD"])?;
    let tree = git_stdout(&["rev-parse", "HEAD^{tree}"])?;
    let status = git_stdout(&["status", "--porcelain=v1", "--untracked-files=normal"])?;
    let clean = status.is_empty();
    if require_clean && !clean {
        bail!("B4 product validation requires a clean committed worktree");
    }
    Ok(json!({
        "available": true,
        "commit": commit,
        "tree": tree,
        "clean": clean,
    }))
}

fn git_stdout(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("git {} failed with {}", args.join(" "), output.status);
    }
    String::from_utf8(output.stdout)
        .context("git output was not UTF-8")
        .map(|value| value.trim().to_owned())
}

fn load_b4_trusted_project_store_evidence(
    path: &Path,
    expected_identity: &Value,
) -> anyhow::Result<Value> {
    let report = read_json_file(path).with_context(|| {
        format!(
            "failed to read trusted project-store lifecycle report {}",
            path.display()
        )
    })?;
    if report.get("schema").and_then(Value::as_str) != Some("mirante4d-verification-run")
        || report.get("schema_version").and_then(Value::as_u64) != Some(1)
        || report.get("group").and_then(Value::as_str) != Some("project-store-lifecycle")
        || report.get("native_status").and_then(Value::as_str) != Some("passed")
    {
        bail!("trusted project-store report identity or result is not accepted");
    }
    let expected_commit = expected_identity
        .get("commit")
        .and_then(Value::as_str)
        .context("current B4 identity lacks commit")?;
    let expected_tree = expected_identity
        .get("tree")
        .and_then(Value::as_str)
        .context("current B4 identity lacks tree")?;
    let report_identity = report
        .get("identity")
        .context("trusted project-store report lacks identity")?;
    if report_identity.get("commit").and_then(Value::as_str) != Some(expected_commit)
        || report_identity.get("tree").and_then(Value::as_str) != Some(expected_tree)
        || report_identity.get("clean").and_then(Value::as_bool) != Some(true)
        || report_identity.get("qualifying").and_then(Value::as_bool) != Some(true)
    {
        bail!("trusted project-store report is not bound to this exact clean B4 revision");
    }
    let phases = report
        .get("phases")
        .and_then(Value::as_array)
        .context("trusted project-store report lacks phases")?;
    if phases.len() != 3
        || !phases
            .iter()
            .all(|phase| phase.get("status").and_then(Value::as_str) == Some("passed"))
    {
        bail!("trusted project-store report does not retain three passing phases");
    }
    let lifecycle = report
        .pointer("/evidence/wp10b_project_store_lifecycle")
        .context("trusted project-store report lacks lifecycle evidence")?;
    if lifecycle.get("schema").and_then(Value::as_str)
        != Some("mirante4d-wp10b-project-store-lifecycle-evidence")
        || lifecycle.get("schema_version").and_then(Value::as_u64) != Some(1)
        || lifecycle.get("result").and_then(Value::as_str) != Some("passed")
        || !lifecycle
            .get("failures")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || lifecycle
            .pointer("/identity/commit")
            .and_then(Value::as_str)
            != Some(expected_commit)
        || lifecycle.pointer("/identity/tree").and_then(Value::as_str) != Some(expected_tree)
        || lifecycle
            .pointer("/identity/clean")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("trusted project-store lifecycle evidence identity or result drifted");
    }
    let counters = lifecycle
        .get("counters")
        .context("trusted project-store lifecycle evidence lacks counters")?;
    let unchanged_bytes = counters
        .get("incremental_unchanged_artifact_bytes_rewritten")
        .and_then(Value::as_u64)
        .context("trusted project-store unchanged-byte counter is missing")?;
    if unchanged_bytes != 0
        || lifecycle
            .pointer("/harness/retries")
            .and_then(Value::as_u64)
            != Some(0)
    {
        bail!("trusted project-store zero-rewrite or zero-retry facts did not pass");
    }
    let mut lifecycle_evidence = lifecycle.clone();
    lifecycle_evidence["counters"] = json!({
        "incremental_unchanged_artifact_bytes_rewritten": unchanged_bytes,
    });
    Ok(json!({
        "source_report": path,
        "binding": "explicit_path_exact_commit_tree_clean_identity",
        "aggregate_identity": report_identity,
        "lifecycle_evidence": lifecycle_evidence,
    }))
}

fn require_b4_x11_tools() -> anyhow::Result<()> {
    for (program, arg) in [
        ("xdotool", "version"),
        ("xwininfo", "-version"),
        ("wmctrl", "-h"),
    ] {
        let mut command = Command::new(program);
        command.arg(arg);
        run_command_with_bounded_output(&mut command, X11_AUTOMATION_OUTPUT_POLICY)
            .with_context(|| format!("B4 product validation requires {program}"))?;
    }
    Ok(())
}

fn completed_product_validation_outcome(
    app_exited_successfully: bool,
    automation_report: Option<&Value>,
    automation_script: &Value,
    automation_script_path: &Path,
) -> (ProductValidationStatus, Option<String>) {
    let automation_status = automation_report
        .and_then(|report| report.get("status"))
        .and_then(Value::as_str);
    let automation_failure = automation_report
        .and_then(|report| report.get("failure_reason"))
        .and_then(Value::as_str);
    if !app_exited_successfully || automation_status != Some("passed") {
        return (
            ProductValidationStatus::Failed,
            Some(automation_failure.map_or_else(
                || {
                    format!(
                        "native app exit success={app_exited_successfully}, automation status={automation_status:?}"
                    )
                },
                str::to_owned,
            )),
        );
    }
    let Some(automation_report) = automation_report else {
        return (
            ProductValidationStatus::Failed,
            Some("native app exited without an automation report".to_owned()),
        );
    };
    if let Err(error) = validate_product_automation_report_contract(
        automation_report,
        automation_script,
        automation_script_path,
    ) {
        return (
            ProductValidationStatus::Failed,
            Some(format!(
                "product automation report contract is invalid: {error}"
            )),
        );
    }

    let evidence = qualifying_nonblank_viewport_capture(Some(automation_report)).map(|_| ());
    match evidence {
        Ok(_) => (ProductValidationStatus::Passed, None),
        Err(reason) => (ProductValidationStatus::Failed, Some(reason)),
    }
}

fn qualifying_nonblank_viewport_capture(
    automation_report: Option<&Value>,
) -> Result<&Value, String> {
    let artifacts = automation_report
        .and_then(|report| report.get("artifacts"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "same-run automation report is missing a nonblank GPU viewport_capture artifact"
                .to_owned()
        })?;

    artifacts
        .iter()
        .find(|artifact| nonblank_gpu_viewport_capture(artifact))
        .ok_or_else(|| {
            "same-run automation report is missing a nonblank GPU viewport_capture artifact"
                .to_owned()
        })
}

fn nonblank_gpu_viewport_capture(artifact: &Value) -> bool {
    if artifact.get("kind").and_then(Value::as_str) != Some("viewport_capture")
        || artifact.get("capture_source").and_then(Value::as_str)
            != Some("gpu_display_frame_readback")
    {
        return false;
    }
    let Some(width) = artifact.get("width").and_then(Value::as_u64) else {
        return false;
    };
    let Some(height) = artifact.get("height").and_then(Value::as_u64) else {
        return false;
    };
    let Some(path) = artifact.get("path").and_then(Value::as_str) else {
        return false;
    };
    let target_is_explicit = artifact
        .get("target")
        .and_then(Value::as_str)
        .is_some_and(valid_product_presentation_target);
    let frame_identity_is_explicit = artifact
        .get("frame_identity")
        .and_then(Value::as_u64)
        .is_some();
    let surface_generation_is_explicit = artifact
        .get("surface_generation")
        .and_then(Value::as_u64)
        .is_some();
    let pixel_stats = artifact.get("pixel_stats");
    let pixel_count = pixel_stats
        .and_then(|stats| stats.get("pixel_count"))
        .and_then(Value::as_u64);
    let nonzero_rgb_pixels = pixel_stats
        .and_then(|stats| stats.get("nonzero_rgb_pixels"))
        .and_then(Value::as_u64);
    let max_rgb = pixel_stats
        .and_then(|stats| stats.get("max_rgb"))
        .and_then(Value::as_u64);

    width > 0
        && height > 0
        && !path.trim().is_empty()
        && target_is_explicit
        && frame_identity_is_explicit
        && surface_generation_is_explicit
        && width.checked_mul(height) == pixel_count
        && nonzero_rgb_pixels.is_some_and(|count| count > 0 && Some(count) <= pixel_count)
        && max_rgb.is_some_and(|value| value > 0)
}

fn b3_exact_e1_capture_evidence(automation_report: Option<&Value>) -> Result<Value, String> {
    let artifacts = automation_report
        .and_then(|report| report.get("artifacts"))
        .and_then(Value::as_array)
        .ok_or_else(|| "B3 E1 report is missing its exact-size GPU captures".to_owned())?;
    let requirements = [
        (
            B3_PRIMARY_E1_CAPTURE,
            u64::from(B3_VIEWPORT_WIDTH),
            u64::from(B3_VIEWPORT_HEIGHT),
        ),
        (
            B3_SECONDARY_E1_CAPTURE,
            u64::from(B3_SECOND_VIEWPORT_WIDTH),
            u64::from(B3_SECOND_VIEWPORT_HEIGHT),
        ),
    ];
    let mut accepted = Vec::with_capacity(requirements.len());
    for (label, expected_width, expected_height) in requirements {
        let artifact = artifacts
            .iter()
            .find(|artifact| {
                artifact
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|path| Path::new(path).file_stem())
                    .and_then(|stem| stem.to_str())
                    == Some(label)
            })
            .ok_or_else(|| format!("B3 E1 report is missing capture {label}.ppm"))?;
        if !nonblank_gpu_viewport_capture(artifact) {
            return Err(format!(
                "B3 E1 capture {label}.ppm is not a valid nonblank GPU readback"
            ));
        }
        let width = artifact
            .get("width")
            .and_then(Value::as_u64)
            .expect("qualifying viewport capture has a width");
        let height = artifact
            .get("height")
            .and_then(Value::as_u64)
            .expect("qualifying viewport capture has a height");
        if width != expected_width || height != expected_height {
            return Err(format!(
                "B3 E1 capture {label}.ppm is {width}x{height}, expected exact {expected_width}x{expected_height} render-target pixels"
            ));
        }
        let command_index = artifact
            .get("command_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("B3 E1 capture {label}.ppm is missing command_index"))?;
        accepted.push(json!({
            "label": label,
            "width": expected_width,
            "height": expected_height,
            "path": artifact.get("path").and_then(Value::as_str),
            "command_index": command_index,
            "capture_source": "gpu_display_frame_readback",
        }));
    }

    let distinct_paths = accepted[0].get("path") != accepted[1].get("path");
    let distinct_commands = accepted[0].get("command_index") != accepted[1].get("command_index");
    if !distinct_paths || !distinct_commands {
        return Err(
            "B3 E1 exact-size captures must be distinct artifacts from distinct commands"
                .to_owned(),
        );
    }

    Ok(json!({
        "required": true,
        "accepted": true,
        "evidence_level": "E1",
        "evidence_scope": "automation_only_internal_gpu_render_target_readback",
        "e4_product_open_satisfied": false,
        "captures": accepted,
    }))
}

fn import_preprocessing_evidence(automation_report: Option<&Value>) -> Result<Value, String> {
    let report = automation_report
        .ok_or_else(|| "import scenario is missing its automation report".to_owned())?;
    if report.get("schema").and_then(Value::as_str) != Some(PRODUCT_AUTOMATION_REPORT_SCHEMA)
        || report.get("schema_version").and_then(Value::as_u64)
            != Some(u64::from(REPORT_SCHEMA_VERSION))
    {
        return Err("import scenario used an unsupported automation report schema".to_owned());
    }
    let evidence = report
        .get("import_workflow_evidence")
        .ok_or_else(|| "import scenario is missing import workflow evidence".to_owned())?;
    let stages = evidence
        .get("worker_emitted_stage_names")
        .and_then(Value::as_array)
        .ok_or_else(|| "import scenario is missing emitted stage names".to_owned())?;
    for required in [
        "planning-and-preflight",
        "source-revalidation",
        "checkpoint-open-or-resume",
        "source-ingest",
        "base-production",
        "pyramid-production",
        "shard-publication",
        "staged-structure-validation",
        "staged-exact-validation",
        "staged-scientific-validation",
        "commit",
    ] {
        if !stages.iter().any(|stage| stage.as_str() == Some(required)) {
            return Err(format!(
                "import scenario did not emit required named stage {required:?}"
            ));
        }
    }
    let projected_stage_count = evidence
        .get("projected_named_stage_observations")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let cancelled_runs = evidence
        .get("cancelled_runs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let successful_runs = evidence
        .get("successful_runs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let failed_runs = evidence
        .get("failed_runs")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let published_events = evidence
        .get("published_events")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let resumed_work_units = evidence
        .get("maximum_resumed_work_units")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let peak_working_bytes = evidence
        .get("maximum_peak_working_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let elapsed_ms = evidence
        .get("maximum_elapsed_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let projected_elapsed_ms = evidence
        .get("maximum_projected_elapsed_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if projected_stage_count < 2
        || cancelled_runs < 1
        || successful_runs < 1
        || failed_runs != 0
        || published_events < successful_runs
        || resumed_work_units < IMPORT_DURABLE_PREFIX_WORK_UNITS
        || peak_working_bytes > IMPORT_PEAK_WORKING_BYTES_LIMIT
        || elapsed_ms == 0
        || projected_elapsed_ms == 0
    {
        return Err(format!(
            "import scenario evidence failed: projected_stages={projected_stage_count}, cancelled={cancelled_runs}, successful={successful_runs}, failed={failed_runs}, published_events={published_events}, resumed={resumed_work_units}, peak_working_bytes={peak_working_bytes}, elapsed_ms={elapsed_ms}, projected_elapsed_ms={projected_elapsed_ms}"
        ));
    }
    let open_ready = report
        .get("events")
        .and_then(Value::as_array)
        .is_some_and(|events| {
            events.iter().any(|event| {
                event.get("command").and_then(Value::as_str) == Some("wait_for_imported_open_ready")
                    && event.get("status").and_then(Value::as_str) == Some("passed")
                    && event
                        .pointer("/details/content_id_computed_during_import")
                        .and_then(Value::as_bool)
                        == Some(true)
                    && event
                        .pointer("/details/normal_product_open_path")
                        .and_then(Value::as_bool)
                        == Some(true)
            })
        });
    if !open_ready {
        return Err(
            "import scenario did not prove admitted publication through the normal open path"
                .to_owned(),
        );
    }
    let transfer = evidence
        .get("publication_to_open_ready_clock")
        .ok_or_else(|| {
            "import scenario is missing publication-to-open-ready transfer evidence".to_owned()
        })?;
    if transfer.get("status").and_then(Value::as_str) != Some(IMPORT_OPEN_READY_COMPLETE_STATUS)
        || transfer.get("transfer_mode").and_then(Value::as_str)
            != Some("staged_self_consistent_capability")
        || transfer
            .get("included_in_primary_clock")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(
            "import scenario did not prove the self-consistent publication capability transfer"
                .to_owned(),
        );
    }
    validate_publication_currentness_execution(transfer)?;
    for field in [
        "package_integrity_audit_started_runs",
        "package_integrity_audit_progress_updates",
        "package_integrity_audit_cancelled_runs",
        "package_integrity_audit_failed_runs",
        "package_integrity_audit_completed_runs",
    ] {
        if transfer.get(field).and_then(Value::as_u64) != Some(0) {
            return Err(format!(
                "import scenario unexpectedly reported nonzero {field}"
            ));
        }
    }
    Ok(evidence.clone())
}

fn validate_publication_currentness_execution(transfer: &Value) -> Result<(), String> {
    let execution = transfer
        .get("publication_currentness_execution")
        .ok_or_else(|| {
            "import scenario is missing storage publication-currentness execution evidence"
                .to_owned()
        })?;
    if execution.get("contract_id").and_then(Value::as_str)
        != Some(PUBLICATION_CURRENTNESS_CONTRACT_ID)
    {
        return Err(
            "import scenario used an unknown publication-currentness execution contract".to_owned(),
        );
    }
    let expected = execution
        .get("expected_snapshot_object_reads")
        .and_then(Value::as_u64)
        .ok_or_else(|| "import scenario omitted expected snapshot object reads".to_owned())?;
    let first_inventory = execution
        .get("first_inventory_object_reads")
        .and_then(Value::as_u64)
        .ok_or_else(|| "import scenario omitted first-inventory object reads".to_owned())?;
    let observed_snapshot = execution
        .get("observed_snapshot_object_reads")
        .and_then(Value::as_u64)
        .ok_or_else(|| "import scenario omitted observed snapshot object reads".to_owned())?;
    let second_inventory = execution
        .get("second_inventory_object_reads")
        .and_then(Value::as_u64)
        .ok_or_else(|| "import scenario omitted second-inventory object reads".to_owned())?;
    let observed_total = execution
        .get("observed_total_object_reads")
        .and_then(Value::as_u64)
        .ok_or_else(|| "import scenario omitted total publication object reads".to_owned())?;
    let codec_decodes = execution
        .get("observed_codec_decode_calls")
        .and_then(Value::as_u64)
        .ok_or_else(|| "import scenario omitted observed publication codec decodes".to_owned())?;
    let reconciled_total = first_inventory
        .checked_add(observed_snapshot)
        .and_then(|value| value.checked_add(second_inventory));
    if expected == 0
        || first_inventory == 0
        || first_inventory != second_inventory
        || observed_snapshot != expected
        || reconciled_total != Some(observed_total)
        || codec_decodes != 0
    {
        return Err(
            "import scenario publication-currentness execution disagreed with the storage contract"
                .to_owned(),
        );
    }
    Ok(())
}

fn product_validation_package_and_script(
    package: Option<&Path>,
    scenario: &ProductValidationScenario,
) -> anyhow::Result<(PathBuf, Value, Option<PathBuf>)> {
    match scenario {
        ProductValidationScenario::GeneratedFixtureCameraSmoke => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => default_target_fixture()?,
            };
            let script = target_fixture_camera_smoke_script(&package);
            Ok((package, script, None))
        }
        ProductValidationScenario::GeneratedFixtureRenderModes => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => default_target_fixture()?,
            };
            let script = target_fixture_render_modes_script(&package);
            Ok((package, script, None))
        }
        ProductValidationScenario::RepresentativeNativeNavigation => {
            let package = package
                .context("representative_native_navigation requires an explicit target package")?
                .to_path_buf();
            let script = representative_native_navigation_script(&package);
            Ok((package, script, None))
        }
        ProductValidationScenario::RepresentativeTemporalPlayback => {
            let package = package
                .context("representative_temporal_playback requires an explicit target package")?
                .to_path_buf();
            let script = representative_temporal_playback_script(&package);
            Ok((package, script, None))
        }
        ProductValidationScenario::RepresentativeGpuInteraction => {
            let package = package
                .context("representative_gpu_interaction requires an explicit target package")?
                .to_path_buf();
            let script = representative_gpu_interaction_script(&package);
            Ok((package, script, None))
        }
        ProductValidationScenario::RepresentativeGpuPresentationProbe => {
            let package = package
                .context(
                    "representative_gpu_presentation_probe requires an explicit target package",
                )?
                .to_path_buf();
            let script = representative_gpu_presentation_probe_script(&package);
            Ok((package, script, None))
        }
        ProductValidationScenario::B3PackageIntegrityAudit => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => default_target_fixture()?,
            };
            let script = target_package_integrity_audit_script(&package);
            Ok((package.clone(), script, Some(package)))
        }
        ProductValidationScenario::ImportPreprocessing => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => default_target_fixture()?,
            };
            let fixture = prepare_import_product_fixture(scenario)?;
            let script = import_preprocessing_script(
                &package,
                &fixture.channel_source,
                fixture.source_kind,
                &fixture.output_parent,
                &fixture.destination,
            );
            Ok((package, script, Some(fixture.source_root)))
        }
        ProductValidationScenario::B4ProjectPersistence => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => default_target_fixture()?,
            };
            let placeholder = Path::new("target/b4-project-placeholder.m4dproj");
            let checkpoint = Path::new("target/b4-checkpoint-placeholder.json");
            let script = b4_launch_one_script(&package, placeholder, checkpoint);
            Ok((package, script, None))
        }
        ProductValidationScenario::PreAlphaReliability => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => default_target_fixture()?,
            };
            let checkpoint = Path::new("target/pre-alpha-reliability-checkpoint.json");
            let script = pre_alpha_provisional_launch_script(&package, checkpoint);
            Ok((package, script, None))
        }
    }
}

struct ImportProductFixture {
    source_root: PathBuf,
    channel_source: PathBuf,
    source_kind: TiffChannelSourceKind,
    output_parent: PathBuf,
    destination: PathBuf,
}

fn prepare_import_product_fixture(
    scenario: &ProductValidationScenario,
) -> anyhow::Result<ImportProductFixture> {
    let root = product_validation_output_dir(scenario).join("public-import-fixture");
    prepare_import_product_fixture_at(&root, IMPORT_FIXTURE_Z, IMPORT_FIXTURE_Y, IMPORT_FIXTURE_X)
}

fn prepare_import_product_fixture_at(
    root: &Path,
    z: u32,
    y: u32,
    x: u32,
) -> anyhow::Result<ImportProductFixture> {
    match fs::symlink_metadata(root) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!(
                    "owned public import fixture path is not a real directory: {}",
                    root.display()
                );
            }
            fs::remove_dir_all(root)
                .with_context(|| format!("failed to reset owned fixture {}", root.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect owned fixture {}", root.display()));
        }
    }
    fs::create_dir(root).with_context(|| format!("failed to create {}", root.display()))?;
    let source_root = root.join("public-full-strip-source");
    crate::import_performance::generate_t2_source(&source_root, z, y, x)?;
    let source_manifest = crate::import_performance::t2_source_manifest(&source_root)?;
    let channels = source_manifest.channels();
    if channels.len() != 1 {
        bail!("generated import product fixture must contain exactly one explicit channel");
    }
    let channel_source = channels[0].path().to_path_buf();
    let source_kind = channels[0].kind();
    let output_parent = root.join("output");
    fs::create_dir(&output_parent)
        .with_context(|| format!("failed to create {}", output_parent.display()))?;
    let destination = deterministic_tiff_destination(&source_manifest, &output_parent);
    Ok(ImportProductFixture {
        source_root,
        channel_source,
        source_kind,
        output_parent,
        destination,
    })
}

fn dataset_runtime_hard_safety_limits(
    max_cpu_total_bytes: u64,
    max_resident_resources: u64,
) -> Value {
    let max_cpu_in_flight_decode_bytes = (max_cpu_total_bytes / 8)
        .saturating_add(PACKAGE_VALIDATION_WORKING_BYTES)
        .min(max_cpu_total_bytes);
    json!({
        "max_cpu_total_bytes": max_cpu_total_bytes,
        "max_cpu_decoded_residency_bytes": max_cpu_total_bytes / 2,
        "max_cpu_upload_staging_bytes": max_cpu_total_bytes / 8,
        "max_cpu_in_flight_decode_bytes": max_cpu_in_flight_decode_bytes,
        "max_cpu_metadata_and_indexes_bytes": max_cpu_total_bytes / 10,
        "max_cpu_queues_and_results_bytes": max_cpu_total_bytes / 20,
        "max_cpu_prefetch_bytes": max_cpu_total_bytes / 20,
        "max_cpu_import_working_set_bytes": max_cpu_total_bytes / 20,
        "max_runtime_queued_requests": 1_024,
        "max_runtime_in_flight_decodes": 8,
        "max_runtime_pending_completions": 1_024,
        "max_runtime_resident_resources": max_resident_resources,
    })
}

fn temporal_playback_hard_safety_limits() -> Value {
    let mut limits = dataset_runtime_hard_safety_limits(4_096 * MIB, 16_384);
    // A one-second 24 FPS runway is intentionally charged as prefetch. Its
    // valid bounded footprint is larger than the generic five-percent
    // diagnostic threshold, while the aggregate four-GiB ceiling and the
    // runtime's own policy remain unchanged.
    limits["max_cpu_prefetch_bytes"] = json!(512 * MIB);
    limits
}

fn default_target_fixture() -> anyhow::Result<PathBuf> {
    extract_target_u16_fixture(Path::new("target/mirante4d/fixtures"))
}

fn target_fixture_camera_smoke_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": GENERATED_FIXTURE_SCENARIO,
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "sleep_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "camera_fit_data" },
            { "command": "set_active_tool", "tool": "inspect" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "camera_orbit", "yaw_points": 120.0, "pitch_points": 32.0 },
            { "command": "camera_pan", "x_points": 40.0, "y_points": -24.0 },
            { "command": "camera_zoom", "scroll_y_points": -120.0 },
            { "command": "sleep_frames", "frames": 2 },
            { "command": "probe_hover", "x_fraction": 0.42, "y_fraction": 0.58 },
            { "command": "capture_screenshot", "target": "three_d", "name": "post-camera-sequence" },
            { "command": "copy_diagnostics" },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "quit" }
        ]
    })
}

fn representative_native_navigation_script(package: &Path) -> Value {
    let mut script = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": REPRESENTATIVE_NATIVE_NAVIGATION_SCENARIO,
        "gpu_timing": false,
        "hard_safety_limits": dataset_runtime_hard_safety_limits(2_048 * MIB, 16_384),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "camera_fit_data" },
            { "command": "sleep_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "representative-mip-voxel-settled" }
        ]
    });
    let Value::Array(four_panel) = json!([
            { "command": "set_viewer_layout", "layout": "four_panel" },
            { "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "cross_section_rotate_sequence", "panel": "xy", "samples": 24, "duration_ms": 400, "x_points_per_sample": 0.5, "y_points_per_sample": 0.125, "radians_per_point": 0.005 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "camera_zoom_sequence", "samples": 24, "duration_ms": 400, "scroll_y_points_per_sample": -1.0 },
            { "command": "camera_orbit_sequence", "samples": 48, "duration_ms": 800, "yaw_points_per_sample": 1.0, "pitch_points_per_sample": 0.25 },
            { "command": "cross_section_rotate_sequence", "panel": "xy", "samples": 24, "duration_ms": 400, "x_points_per_sample": 0.5, "y_points_per_sample": 0.125, "radians_per_point": 0.005 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 60000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": { "four_panel_images_distinct": { "min_different_pixels": 1 } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "representative-four-panel-3d" },
            { "command": "capture_screenshot", "target": "xy", "name": "representative-four-panel-xy" },
            { "command": "capture_screenshot", "target": "xz", "name": "representative-four-panel-xz" },
            { "command": "capture_screenshot", "target": "yz", "name": "representative-four-panel-yz" }
    ]) else {
        unreachable!("the representative four-panel commands are an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the representative script has commands")
        .extend(four_panel);
    let Value::Array(smooth_and_dvr) = json!([
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "smooth_linear" },
            { "command": "camera_orbit_sequence", "samples": 48, "duration_ms": 800, "yaw_points_per_sample": -1.0, "pitch_points_per_sample": 0.25 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": { "layer_sampling": { "layer_index": 0, "sampling": "smooth_linear" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "representative-mip-smooth-settled" },

            { "command": "set_render_mode", "mode": "dvr" },
            { "command": "set_dvr_density_scale", "density_scale": 12.0 },
            { "command": "camera_orbit_sequence", "samples": 48, "duration_ms": 800, "yaw_points_per_sample": 0.75, "pitch_points_per_sample": -0.25 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": { "render_mode": { "mode": "dvr" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "representative-dvr-smooth-settled" }
    ]) else {
        unreachable!("the representative smooth and DVR commands are an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the representative script has commands")
        .extend(smooth_and_dvr);
    let Value::Array(iso_and_tail) = json!([
            { "command": "set_render_mode", "mode": "iso" },
            { "command": "set_layer_iso_shading", "layer_index": 0, "shading": "gradient_lighting" },
            { "command": "set_iso_display_level", "display_level": 0.5 },
            { "command": "camera_orbit_sequence", "samples": 48, "duration_ms": 800, "yaw_points_per_sample": -0.75, "pitch_points_per_sample": -0.25 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": { "render_mode": { "mode": "iso" } } },
            { "command": "assert", "condition": { "layer_iso_shading": { "layer_index": 0, "shading": "gradient_lighting" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "representative-iso-smooth-settled" },

            { "command": "set_viewer_layout", "layout": "single3d" },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 60000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": "cross_section_retired" },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "representative-single3d-final" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
    ]) else {
        unreachable!("the representative ISO and closeout commands are an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the representative script has commands")
        .extend(iso_and_tail);
    script
}

pub(crate) fn representative_gpu_interaction_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": REPRESENTATIVE_GPU_INTERACTION_SCENARIO,
        "gpu_timing": true,
        "hard_safety_limits": dataset_runtime_hard_safety_limits(4_096 * MIB, 16_384),
        "commands": [
            { "command": "set_gpu_performance_phase", "phase": "startup" },
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_mapped_client_pixels", "width": 1920, "height": 1080 },
            { "command": "set_viewer_layout", "layout": "single3d" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "camera_fit_data" },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 120000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics", "checkpoint": "resident_standalone_before" },

            { "command": "set_gpu_performance_phase", "phase": "standalone_interaction" },
            { "command": "camera_orbit_sequence", "samples": 900, "duration_ms": 15000, "yaw_points_per_sample": 0.20, "pitch_points_per_sample": 0.05 },
            { "command": "camera_zoom_sequence", "samples": 900, "duration_ms": 15000, "scroll_y_points_per_sample": -0.08 },
            { "command": "set_gpu_performance_phase", "phase": "resident_settlement" },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics", "checkpoint": "resident_standalone_after" },

            { "command": "set_viewer_layout", "layout": "four_panel" },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "copy_diagnostics", "checkpoint": "resident_four_panel_before" },
            { "command": "set_gpu_performance_phase", "phase": "four_panel_interaction" },
            { "command": "cross_section_pan_sequence", "panel": "xy", "samples": 900, "duration_ms": 15000, "x_points_per_sample": 0.08, "y_points_per_sample": -0.04 },
            { "command": "cross_section_rotate_sequence", "panel": "xy", "samples": 900, "duration_ms": 15000, "x_points_per_sample": 0.08, "y_points_per_sample": 0.04, "radians_per_point": 0.0025 },
            { "command": "set_gpu_performance_phase", "phase": "resident_settlement" },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "assert", "condition": { "four_panel_images_distinct": { "min_different_pixels": 1 } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics", "checkpoint": "resident_four_panel_after" },

            { "command": "copy_diagnostics", "checkpoint": "prepared_nonresident_before" },
            { "command": "set_gpu_performance_phase", "phase": "prepared_nonresident_replacement" },
            { "command": "camera_zoom", "scroll_y_points": -480.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics", "checkpoint": "prepared_nonresident_after" },

            { "command": "set_gpu_performance_phase", "phase": "mode_sampling_matrix" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": 0.4, "pitch_points_per_sample": 0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "smooth_linear" },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": -0.4, "pitch_points_per_sample": 0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_render_mode", "mode": "dvr" },
            { "command": "set_dvr_density_scale", "density_scale": 12.0 },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": 0.4, "pitch_points_per_sample": -0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": -0.4, "pitch_points_per_sample": -0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_render_mode", "mode": "iso" },
            { "command": "set_iso_display_level", "display_level": 0.5 },
            { "command": "set_layer_iso_shading", "layer_index": 0, "shading": "gradient_lighting" },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": 0.4, "pitch_points_per_sample": 0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "smooth_linear" },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": -0.4, "pitch_points_per_sample": 0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_layer_render_mode", "layer_index": 0, "mode": "mip" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "iso" },
            { "command": "camera_orbit_sequence", "samples": 30, "duration_ms": 500, "yaw_points_per_sample": 0.4, "pitch_points_per_sample": -0.1 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "set_active_tool", "tool": "inspect" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "primary_click", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "assert", "condition": "no_render_error" },

            { "command": "copy_diagnostics", "checkpoint": "interrupted_refinement_before" },
            { "command": "set_gpu_performance_phase", "phase": "interrupted_refinement" },
            { "command": "camera_zoom", "scroll_y_points": 720.0 },
            { "command": "camera_orbit_sequence", "samples": 120, "duration_ms": 2000, "yaw_points_per_sample": 0.25, "pitch_points_per_sample": 0.05 },
            { "command": "camera_pan_sequence", "samples": 120, "duration_ms": 2000, "x_points_per_sample": -0.10, "y_points_per_sample": 0.05 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics", "checkpoint": "interrupted_refinement_after" },

            { "command": "set_gpu_performance_phase", "phase": "settled_idle" },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 120000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "copy_diagnostics", "checkpoint": "settled_idle_before" },
            { "command": "sleep_frames", "frames": 30 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "gpu-performance-final" },
            { "command": "copy_diagnostics", "checkpoint": "settled_idle_after" },
            { "command": "quit" }
        ]
    })
}

pub(crate) fn representative_gpu_presentation_probe_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO,
        "gpu_timing": false,
        "hard_safety_limits": dataset_runtime_hard_safety_limits(4_096 * MIB, 16_384),
        "commands": [
            { "command": "set_gpu_performance_phase", "phase": "startup" },
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_mapped_client_pixels", "width": 1920, "height": 1080 },
            { "command": "set_render_target_size", "width": 1920, "height": 1080 },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "camera_fit_data" },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "sleep_frames", "frames": 12 },

            { "command": "set_render_target_size", "width": 1600, "height": 900 },
            { "command": "set_mapped_client_pixels", "width": 1600, "height": 900 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "sleep_frames", "frames": 6 },

            { "command": "set_window_minimized", "minimized": true },
            { "command": "camera_orbit", "yaw_points": 12.0, "pitch_points": 3.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120000 },
            { "command": "camera_orbit", "yaw_points": 12.0, "pitch_points": -3.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 120000 },
            { "command": "sleep_frames", "frames": 6 },

            { "command": "set_render_target_size", "width": 1920, "height": 1080 },
            { "command": "set_mapped_client_pixels", "width": 1920, "height": 1080 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 120000 },
            { "command": "sleep_frames", "frames": 12 },
            { "command": "capture_screenshot", "target": "three_d", "name": "presentation-probe-final-1920x1080" },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
        ]
    })
}

fn representative_temporal_playback_script(package: &Path) -> Value {
    let mut script = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": REPRESENTATIVE_TEMPORAL_PLAYBACK_SCENARIO,
        "gpu_timing": false,
        "hard_safety_limits": temporal_playback_hard_safety_limits(),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_mapped_client_pixels", "width": 960, "height": 640 },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "camera_fit_data" },
            { "command": "wait_for", "condition": "initial_auto_dense_applied", "timeout_ms": 60000 },
            { "command": "wait_for_presented_time_index", "time_index": 0, "timeout_ms": 60000 },
            { "command": "capture_temporal_frame", "target": "three_d", "name": "temporal-t0", "min_different_pixels_from_previous": null },

            { "command": "set_time_index", "time_index": 1 },
            { "command": "wait_for_presented_time_index", "time_index": 1, "timeout_ms": 60000 },
            { "command": "capture_temporal_frame", "target": "three_d", "name": "temporal-t1", "min_different_pixels_from_previous": 1 },
            { "command": "assert", "condition": "no_render_error" },

            { "command": "set_time_index", "time_index": 0 },
            { "command": "wait_for_presented_time_index", "time_index": 0, "timeout_ms": 60000 },
            { "command": "set_viewer_layout", "layout": "four_panel" },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
            { "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } },

            { "command": "set_playback_fps", "fps": 24 },
            { "command": "set_playback_active", "active": true },
            { "command": "wait_for_temporal_transitions", "minimum_transitions": 3, "timeout_ms": 60000 },
            { "command": "camera_orbit_sequence", "samples": 96, "duration_ms": 2000, "yaw_points_per_sample": 0.5, "pitch_points_per_sample": 0.125 },
            { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
            { "command": "set_playback_active", "active": false },
            { "command": "wait_for", "condition": "playback_residency_released", "timeout_ms": 60000 },
            { "command": "assert", "condition": "playback_stopped_and_released" },
            { "command": "assert", "condition": "temporal_continuity" },

            { "command": "set_playback_fps", "fps": 12 },
            { "command": "set_playback_active", "active": true },
            { "command": "wait_for_temporal_transitions", "minimum_transitions": 3, "timeout_ms": 60000 },
            { "command": "camera_zoom_sequence", "samples": 96, "duration_ms": 2000, "scroll_y_points_per_sample": -0.25 },
            { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
            { "command": "set_playback_active", "active": false },
            { "command": "wait_for", "condition": "playback_residency_released", "timeout_ms": 60000 },
            { "command": "assert", "condition": "playback_stopped_and_released" },
            { "command": "assert", "condition": "temporal_continuity" },

            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "xy" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "xz" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "yz" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
        ]
    });
    {
        let commands = script["commands"]
            .as_array_mut()
            .expect("the temporal playback script has commands");
        let input_indices = commands
            .iter()
            .enumerate()
            .filter(|(_, command)| {
                matches!(
                    command["command"].as_str(),
                    Some("camera_orbit_sequence" | "camera_zoom_sequence")
                )
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in input_indices.into_iter().rev() {
            commands.insert(
                index,
                json!({ "command": "observe_playback_cadence", "duration_ms": 2000 }),
            );
        }
        let release_indices = commands
            .iter()
            .enumerate()
            .filter(|(_, command)| {
                command["command"] == "wait_for"
                    && command["condition"] == "playback_residency_released"
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in release_indices.into_iter().rev() {
            commands.insert(
                index + 1,
                json!({ "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 }),
            );
        }
    }
    let standalone_input = json!([
        { "command": "set_playback_fps", "fps": 24 },
        { "command": "set_playback_active", "active": true },
        { "command": "wait_for_temporal_transitions", "minimum_transitions": 3, "timeout_ms": 60000 },
        { "command": "observe_playback_cadence", "duration_ms": 2000 },
        { "command": "camera_orbit_sequence", "samples": 96, "duration_ms": 2000, "yaw_points_per_sample": 0.5, "pitch_points_per_sample": 0.125 },
        { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
        { "command": "observe_playback_cadence", "duration_ms": 2000 },
        { "command": "camera_zoom_sequence", "samples": 96, "duration_ms": 2000, "scroll_y_points_per_sample": -0.25 },
        { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
        { "command": "set_playback_active", "active": false },
        { "command": "wait_for", "condition": "playback_residency_released", "timeout_ms": 60000 },
        { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 60000 },
        { "command": "assert", "condition": "playback_stopped_and_released" },
        { "command": "assert", "condition": "temporal_continuity" },
    ]);
    let commands = script["commands"]
        .as_array_mut()
        .expect("the temporal playback script has commands");
    let four_panel_start = commands
        .iter()
        .position(|command| {
            command["command"] == "set_viewer_layout" && command["layout"] == "four_panel"
        })
        .expect("the temporal script has a four-panel phase");
    commands.splice(
        four_panel_start..four_panel_start,
        standalone_input
            .as_array()
            .expect("standalone temporal input is an array")
            .iter()
            .cloned(),
    );
    let linked_input = json!([
        { "command": "observe_playback_cadence", "duration_ms": 2000 },
        { "command": "cross_section_pan_sequence", "panel": "xy", "samples": 96, "duration_ms": 2000, "x_points_per_sample": 0.25, "y_points_per_sample": -0.125 },
        { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
        { "command": "observe_playback_cadence", "duration_ms": 2000 },
        { "command": "cross_section_zoom_sequence", "panel": "xy", "samples": 96, "duration_ms": 2000, "x_fraction": 0.5, "y_fraction": 0.5, "factor_per_sample": 0.999 },
        { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
        { "command": "observe_playback_cadence", "duration_ms": 2000 },
        { "command": "cross_section_rotate_sequence", "panel": "xy", "samples": 96, "duration_ms": 2000, "x_points_per_sample": 0.25, "y_points_per_sample": 0.125, "radians_per_point": 0.005 },
        { "command": "assert", "condition": { "playback_advanced_during_previous_input": { "minimum_transitions": 3 } } },
    ]);
    let commands = script["commands"]
        .as_array_mut()
        .expect("the temporal playback script has commands");
    let four_panel_start = commands
        .iter()
        .position(|command| {
            command["command"] == "set_viewer_layout" && command["layout"] == "four_panel"
        })
        .expect("the temporal script has a four-panel phase");
    let first_stop = commands
        .iter()
        .enumerate()
        .skip(four_panel_start + 1)
        .find(|(_, command)| {
            command["command"] == "set_playback_active" && command["active"] == false
        })
        .map(|(index, _)| index)
        .expect("the four-panel 24 FPS playback phase has a stop boundary");
    commands.splice(
        first_stop..first_stop,
        linked_input
            .as_array()
            .expect("linked temporal input is an array")
            .iter()
            .cloned(),
    );
    script
}

fn target_fixture_render_modes_script(package: &Path) -> Value {
    let mut script = json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": GENERATED_RENDER_MODES_SCENARIO,
        "gpu_timing": false,
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 192),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "set_layer_window", "layer_index": 0, "low": 0.0, "high": 4096.0 },
            { "command": "set_layer_window", "layer_index": 1, "low": 20000.0, "high": 24096.0 },
            { "command": "set_layer_opacity", "layer_index": 0, "opacity": 1.0 },
            { "command": "set_layer_opacity", "layer_index": 1, "opacity": 1.0 },
            { "command": "sleep_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30000 },
            { "command": "camera_fit_data" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "mip" },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "render_mode": { "mode": "mip" } } },
            { "command": "assert", "condition": { "layer_render_mode": { "layer_index": 1, "mode": "mip" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30000 },
            { "command": "set_active_tool", "tool": "inspect" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "assert", "condition": { "pick_evidence": { "policy": "mip_argmax" } } },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-mip" }
        ]
    });
    let Value::Array(middle) = json!([
            { "command": "set_projection", "projection": "perspective" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "smooth_linear" },
            { "command": "set_layer_sampling", "layer_index": 1, "sampling": "smooth_linear" },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "projection": { "projection": "perspective" } } },
            { "command": "assert", "condition": { "layer_sampling": { "layer_index": 0, "sampling": "smooth_linear" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-perspective-smooth" },
            { "command": "set_render_mode", "mode": "dvr" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "dvr" },
            { "command": "set_dvr_density_scale", "density_scale": 12.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "render_mode": { "mode": "dvr" } } },
            { "command": "assert", "condition": { "layer_render_mode": { "layer_index": 1, "mode": "dvr" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "assert", "condition": { "pick_evidence": { "policy": "maximum_opacity_contribution" } } },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-dvr" }
    ]) else {
        unreachable!("the product validation command middle is an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the generated product validation script has commands")
        .extend(middle);
    let Value::Array(iso) = json!([
            { "command": "set_render_mode", "mode": "iso" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "iso" },
            { "command": "set_layer_iso_shading", "layer_index": 0, "shading": "gradient_lighting" },
            { "command": "set_layer_iso_shading", "layer_index": 1, "shading": "gradient_lighting" },
            { "command": "set_iso_display_level", "display_level": 0.05 },
            { "command": "set_iso_light", "light": { "kind": "detached_screen", "x": 0.25, "y": -0.35 } },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "render_mode": { "mode": "iso" } } },
            { "command": "assert", "condition": { "layer_render_mode": { "layer_index": 1, "mode": "iso" } } },
            { "command": "assert", "condition": { "layer_iso_shading": { "layer_index": 0, "shading": "gradient_lighting" } } },
            { "command": "assert", "condition": { "iso_light": { "light": { "kind": "detached_screen", "x": 0.25, "y": -0.35 } } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "assert", "condition": { "pick_evidence": { "policy": "first_threshold_hit" } } },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-iso-detached-light" },
            { "command": "set_iso_light", "light": { "kind": "attached_camera" } },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "iso_light": { "light": { "kind": "attached_camera" } } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-iso-attached-light" }
    ]) else {
        unreachable!("the product validation ISO commands are an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the generated product validation script has commands")
        .extend(iso);
    let Value::Array(mixed) = json!([
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "dvr" },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "render_mode": { "mode": "mip" } } },
            { "command": "assert", "condition": { "layer_render_mode": { "layer_index": 1, "mode": "dvr" } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-mixed-mip-dvr" }
    ]) else {
        unreachable!("the product validation mixed commands are an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the generated product validation script has commands")
        .extend(mixed);
    let Value::Array(tail) = json!([
            { "command": "set_projection", "projection": "orthographic" },
            { "command": "set_layer_sampling", "layer_index": 0, "sampling": "voxel_exact" },
            { "command": "set_layer_sampling", "layer_index": 1, "sampling": "voxel_exact" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "mip" },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "layer_render_mode": { "layer_index": 1, "mode": "mip" } } },
            { "command": "set_active_tool", "tool": "inspect" },
            { "command": "assert", "condition": { "active_tool": { "tool": "inspect" } } },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "assert", "condition": { "pick_evidence": { "policy": "mip_argmax" } } },
            { "command": "set_active_tool", "tool": "crosshair" },
            { "command": "primary_click", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "assert", "condition": "crosshair_linked" },
            { "command": "set_active_tool", "tool": "roi_box" },
            { "command": "primary_click", "x_fraction": 0.44, "y_fraction": 0.44 },
            { "command": "primary_click", "x_fraction": 0.56, "y_fraction": 0.56 },
            { "command": "assert", "condition": "roi_committed" },
            { "command": "set_active_tool", "tool": "measure_distance" },
            { "command": "primary_click", "x_fraction": 0.46, "y_fraction": 0.5 },
            { "command": "primary_click", "x_fraction": 0.54, "y_fraction": 0.5 },
            { "command": "assert", "condition": "distance_committed" },
            { "command": "set_active_tool", "tool": "navigate" },
            { "command": "set_viewer_layout", "layout": "four_panel" },
            { "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } },
            { "command": "wait_for", "condition": "coordinated_presentation_settled", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "frame_fidelity": { "scale_level": 0, "complete": true, "exact": true } } },
            { "command": "assert", "condition": { "cross_section_panel_schedule": {
                "panel": "xz",
                "min_generation": 1,
                "min_selected_resources": 1
            } } },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "four_panel_images_distinct": {
                "min_different_pixels": 1
            } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-linked-panels-3d" },
            { "command": "capture_screenshot", "target": "xy", "name": "generated-linked-panels-xy" },
            { "command": "capture_screenshot", "target": "xz", "name": "generated-linked-panels-xz" },
            { "command": "capture_screenshot", "target": "yz", "name": "generated-linked-panels-yz" },
            { "command": "set_viewer_layout", "layout": "single3d" },
            { "command": "sleep_frames", "frames": 3 },
            { "command": "assert", "condition": "cross_section_retired" },
            { "command": "set_viewport_size", "width": GENERATED_RESIZED_VIEWPORT_WIDTH, "height": GENERATED_RESIZED_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": GENERATED_RESIZED_VIEWPORT_WIDTH, "height": GENERATED_RESIZED_VIEWPORT_HEIGHT },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "render_target_pixels": {
                "width": GENERATED_RESIZED_VIEWPORT_WIDTH,
                "height": GENERATED_RESIZED_VIEWPORT_HEIGHT
            } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "generated-resized-1920x1080" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
    ]) else {
        unreachable!("the product validation command tail is an array")
    };
    script["commands"]
        .as_array_mut()
        .expect("the generated product validation script has commands")
        .extend(tail);
    script
}

fn target_package_integrity_audit_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO,
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": B3_VIEWPORT_WIDTH, "height": B3_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": B3_VIEWPORT_WIDTH, "height": B3_VIEWPORT_HEIGHT },
            { "command": "wait_for", "condition": "package_integrity_audit_not_run", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": { "render_target_pixels": { "width": B3_VIEWPORT_WIDTH, "height": B3_VIEWPORT_HEIGHT } } },
            { "command": "capture_screenshot", "target": "three_d", "name": "b3-before-cancel-1280x720" },
            { "command": "cancel_package_integrity_audit" },
            { "command": "wait_for", "condition": "package_integrity_audit_inactive", "timeout_ms": 30000 },
            { "command": "assert", "condition": {
                "package_integrity_audit_evidence": {
                    "min_progress_updates": 0,
                    "min_cancelled_runs": 1,
                    "min_completed_runs": 0
                }
            } },
            // The optional audit never revokes source or presentation
            // authority. The same complete frame must remain available across
            // cancellation and the subsequent explicit audit.
            { "command": "request_package_integrity_audit" },
            { "command": "wait_for", "condition": "package_integrity_audit_self_consistent", "timeout_ms": 30000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": {
                "package_integrity_audit_evidence": {
                    "min_progress_updates": 1,
                    "min_cancelled_runs": 1,
                    "min_completed_runs": 1
                }
            } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": { "render_target_pixels": { "width": B3_VIEWPORT_WIDTH, "height": B3_VIEWPORT_HEIGHT } } },
            { "command": "capture_screenshot", "target": "three_d", "name": "b3-after-success-1280x720" },
            { "command": "set_viewport_size", "width": B3_SECOND_VIEWPORT_WIDTH, "height": B3_SECOND_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": B3_SECOND_VIEWPORT_WIDTH, "height": B3_SECOND_VIEWPORT_HEIGHT },
            { "command": "sleep_frames", "frames": 3 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": { "render_target_pixels": { "width": B3_SECOND_VIEWPORT_WIDTH, "height": B3_SECOND_VIEWPORT_HEIGHT } } },
            { "command": "capture_screenshot", "target": "three_d", "name": "b3-after-success-1920x1080" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
        ]
    })
}

fn import_preprocessing_script(
    startup_package: &Path,
    source: &Path,
    source_kind: TiffChannelSourceKind,
    output_parent: &Path,
    destination: &Path,
) -> Value {
    // The bounded 65x12737x65 fixture has 800 base-production work units. Keep the
    // resident-record ceiling structural and finite while allowing its exact
    // s0 cohort to settle; byte residency remains independently capped below.
    let mut hard_safety_limits = dataset_runtime_hard_safety_limits(
        IMPORT_CPU_TOTAL_LIMIT_BYTES,
        IMPORT_RESIDENT_RESOURCE_LIMIT,
    );
    hard_safety_limits["max_cpu_import_working_set_bytes"] =
        json!(IMPORT_PROGRESS_RESERVATION_LIMIT_BYTES);
    let source_kind = match source_kind {
        TiffChannelSourceKind::Single3dTiff => "single_3d_tiff",
        TiffChannelSourceKind::FolderOf3dTiffs => "folder_of_3d_tiffs",
        TiffChannelSourceKind::FolderOf2dTiffs => "folder_of_2d_tiffs",
    };
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": IMPORT_PREPROCESSING_SCENARIO,
        "hard_safety_limits": hard_safety_limits,
        "commands": [
            { "command": "open_dataset", "path": startup_package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5_000 },
            { "command": "set_mapped_client_pixels", "width": IMPORT_VIEWPORT_WIDTH, "height": IMPORT_VIEWPORT_HEIGHT },
            { "command": "set_render_target_size", "width": IMPORT_VIEWPORT_WIDTH, "height": IMPORT_VIEWPORT_HEIGHT },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 30_000 },
            { "command": "begin_tiff_import_setup", "source": source, "output_parent": output_parent, "source_kind": source_kind },
            { "command": "wait_for", "condition": "import_review_ready", "timeout_ms": 30_000 },
            { "command": "start_reviewed_import", "spacing_zyx_um": [0.4, 0.2, 0.1], "time_step_seconds": null, "no_data_value_rule": { "kind": "manual_uint8", "value": 255 }, "hide_constant_z_planes": false },
            { "command": "wait_for_import_progress", "stage": "base-production", "minimum_completed_work_units": IMPORT_DURABLE_PREFIX_WORK_UNITS, "timeout_ms": 120_000 },
            { "command": "cancel_import" },
            { "command": "wait_for", "condition": "import_idle", "timeout_ms": 30_000 },
            { "command": "assert", "condition": { "import_workflow_evidence": {
                "required_stage_names": ["planning-and-preflight", "source-revalidation", "checkpoint-open-or-resume", "source-ingest", "base-production"],
                "min_projected_named_stages": 1,
                "min_cancelled_runs": 1,
                "min_successful_runs": 0,
                "min_resumed_work_units": 0,
                "min_elapsed_ms": 1,
                "min_projected_elapsed_ms": 1,
                "max_peak_working_bytes": IMPORT_PEAK_WORKING_BYTES_LIMIT
            } } },
            { "command": "begin_tiff_import_setup", "source": source, "output_parent": output_parent, "source_kind": source_kind },
            { "command": "wait_for", "condition": "import_review_ready", "timeout_ms": 30_000 },
            { "command": "start_reviewed_import", "spacing_zyx_um": [0.4, 0.2, 0.1], "time_step_seconds": null, "no_data_value_rule": { "kind": "manual_uint8", "value": 255 }, "hide_constant_z_planes": false },
            { "command": "wait_for_imported_open_ready", "path": destination, "timeout_ms": 180_000 },
            { "command": "assert", "condition": { "import_workflow_evidence": {
                "required_stage_names": [
                    "planning-and-preflight",
                    "source-revalidation",
                    "checkpoint-open-or-resume",
                    "source-ingest",
                    "base-production",
                    "pyramid-production",
                    "shard-publication",
                    "staged-structure-validation",
                    "staged-exact-validation",
                    "staged-scientific-validation",
                    "commit"
                ],
                "min_projected_named_stages": 2,
                "min_cancelled_runs": 1,
                "min_successful_runs": 1,
                "min_resumed_work_units": IMPORT_DURABLE_PREFIX_WORK_UNITS,
                "min_elapsed_ms": 1,
                "min_projected_elapsed_ms": 1,
                "max_peak_working_bytes": IMPORT_PEAK_WORKING_BYTES_LIMIT
            } } },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 60_000 },
            { "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60_000 },
            { "command": "camera_fit_data" },
            { "command": "camera_orbit", "yaw_points": 48.0, "pitch_points": 16.0 },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 60_000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "import-preprocessing-open-ready-navigation" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
        ]
    })
}

fn pre_alpha_provisional_launch_script(package: &Path, checkpoint: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "pre_alpha_reliability_provisional_autosave",
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5_000 },
            { "command": "set_mapped_client_pixels", "width": B4_PRIMARY_CLIENT_WIDTH, "height": B4_PRIMARY_CLIENT_HEIGHT },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30_000 },
            { "command": "new_project" },
            { "command": "camera_pan", "x_points": 8.0, "y_points": -4.0 },
            { "command": "wait_for", "condition": "project_autosaved", "timeout_ms": 45_000 },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 30_000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": true,
                "lifecycle": "provisional",
                "can_save": true,
                "can_save_as": false,
                "manual": false,
                "autosave": true
            } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "write_external_kill_checkpoint", "path": checkpoint, "stage": PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE },
            { "command": "hold_for_external_kill" }
        ]
    })
}

fn pre_alpha_recovery_launch_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "pre_alpha_reliability_recover_unsaved_autosave",
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5_000 },
            { "command": "set_mapped_client_pixels", "width": B4_PRIMARY_CLIENT_WIDTH, "height": B4_PRIMARY_CLIENT_HEIGHT },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30_000 },
            { "command": "wait_for", "condition": "unsaved_autosave_recovery_exposed", "timeout_ms": 30_000 },
            { "command": "recover_exposed_unsaved_autosave" },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 30_000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": true,
                "lifecycle": "recovery_selected",
                "can_save": false,
                "can_save_as": true,
                "manual": false,
                "autosave": true
            } } },
            { "command": "wait_for", "condition": "frame_freshness_current", "timeout_ms": 30_000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "pre-alpha-recovered-unsaved-autosave" },
            { "command": "close_project_store" },
            { "command": "wait_for", "condition": "project_store_closed", "timeout_ms": 30_000 },
            { "command": "quit" }
        ]
    })
}

fn pre_alpha_native_close_launch_script(package: &Path, checkpoint: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "pre_alpha_reliability_native_close",
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5_000 },
            { "command": "set_mapped_client_pixels", "width": B4_PRIMARY_CLIENT_WIDTH, "height": B4_PRIMARY_CLIENT_HEIGHT },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30_000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "write_external_kill_checkpoint", "path": checkpoint, "stage": PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE },
            { "command": "hold_for_external_kill" }
        ]
    })
}

fn b4_launch_one_script(package: &Path, project: &Path, checkpoint: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "b4_project_persistence_launch_1",
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_mapped_client_pixels", "width": B4_PRIMARY_CLIENT_WIDTH, "height": B4_PRIMARY_CLIENT_HEIGHT },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "new_project" },
            { "command": "initial_save_with_edit", "path": project },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": true,
                "lifecycle": "established",
                "can_save": true,
                "can_save_as": true,
                "manual": true,
                "autosave": false
            } } },
            { "command": "wait_for", "condition": "project_autosaved", "timeout_ms": 45000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": true,
                "lifecycle": "established",
                "can_save": true,
                "can_save_as": true,
                "manual": true,
                "autosave": true
            } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "b4-launch-1-before-kill-1280x720" },
            { "command": "write_external_kill_checkpoint", "path": checkpoint, "stage": B4_CHECKPOINT_STAGE },
            { "command": "hold_for_external_kill" }
        ]
    })
}

fn b4_launch_two_script(package: &Path, original: &Path, save_as: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "b4_project_persistence_launch_2",
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_mapped_client_pixels", "width": B4_SECONDARY_CLIENT_WIDTH, "height": B4_SECONDARY_CLIENT_HEIGHT },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "open_project", "path": original },
            { "command": "wait_for", "condition": "recovery_review_required", "timeout_ms": 30000 },
            { "command": "recover_automatic_autosave" },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": true,
                "lifecycle": "recovery_selected",
                "can_save": false,
                "can_save_as": true,
                "manual": true,
                "autosave": true
            } } },
            { "command": "save_project_as", "path": save_as },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": false,
                "lifecycle": "established",
                "can_save": true,
                "can_save_as": true,
                "manual": true,
                "autosave": false
            } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "b4-launch-2-recovered-save-as-1920x1080" },
            { "command": "close_project_store" },
            { "command": "wait_for", "condition": "project_store_closed", "timeout_ms": 30000 },
            { "command": "quit" }
        ]
    })
}

fn b4_launch_three_script(package: &Path, save_as: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": SCRIPT_SCHEMA_VERSION,
        "scenario": "b4_project_persistence_launch_3",
        "hard_safety_limits": dataset_runtime_hard_safety_limits(128 * MIB, 128),
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_mapped_client_pixels", "width": B4_SECONDARY_CLIENT_WIDTH, "height": B4_SECONDARY_CLIENT_HEIGHT },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "open_project", "path": save_as },
            { "command": "wait_for", "condition": "project_store_idle", "timeout_ms": 30000 },
            { "command": "assert", "condition": { "project_state": {
                "bound": true,
                "dirty": false,
                "lifecycle": "established",
                "can_save": true,
                "can_save_as": true,
                "manual": true,
                "autosave": false
            } } },
            { "command": "assert", "condition": { "nonblank_panel": { "target": "three_d" } } },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "capture_screenshot", "target": "three_d", "name": "b4-launch-3-final-reopen-clean-1920x1080" },
            { "command": "close_project_store" },
            { "command": "wait_for", "condition": "project_store_closed", "timeout_ms": 30000 },
            { "command": "quit" }
        ]
    })
}

pub(crate) fn validate_product_automation_script(script: &Value) -> anyhow::Result<()> {
    let fields = script
        .as_object()
        .context("automation script must be an object")?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_fields = BTreeSet::from([
        "schema",
        "schema_version",
        "scenario",
        "gpu_timing",
        "hard_safety_limits",
        "commands",
    ]);
    if !fields.is_subset(&allowed_fields) {
        bail!("automation script contains an unknown or removed top-level field");
    }
    if script.get("schema").and_then(Value::as_str) != Some(PRODUCT_AUTOMATION_SCRIPT_SCHEMA) {
        bail!("automation script schema must be {PRODUCT_AUTOMATION_SCRIPT_SCHEMA}");
    }
    if script.get("schema_version").and_then(Value::as_u64) != Some(SCRIPT_SCHEMA_VERSION as u64) {
        bail!("automation script schema_version must be {SCRIPT_SCHEMA_VERSION}");
    }
    let scenario = script
        .get("scenario")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if scenario.trim().is_empty() {
        bail!("automation script scenario must be a nonempty string");
    }
    let commands = script
        .get("commands")
        .and_then(Value::as_array)
        .context("automation script commands must be a nonempty array")?;
    if commands.is_empty() {
        bail!("automation script commands must be a nonempty array");
    }
    let has_open_dataset = commands
        .iter()
        .any(|command| command.get("command").and_then(Value::as_str) == Some("open_dataset"));
    if !has_open_dataset {
        bail!("automation script must include open_dataset");
    }
    let terminal_command = commands
        .last()
        .and_then(|command| command.get("command"))
        .and_then(Value::as_str);
    if !matches!(terminal_command, Some("quit" | "hold_for_external_kill")) {
        bail!("automation script final command must be quit or hold_for_external_kill");
    }
    for command in commands {
        let command_name = command
            .get("command")
            .and_then(Value::as_str)
            .context("automation command must name its command")?;
        if matches!(
            command_name,
            "capture_screenshot" | "capture_temporal_frame"
        ) {
            let target = command
                .get("target")
                .and_then(Value::as_str)
                .context("capture command requires an explicit target")?;
            if !valid_product_presentation_target(target) {
                bail!("capture target {target:?} is invalid");
            }
        }
        if command_name == "assert"
            && command.get("condition").and_then(Value::as_str) == Some("nonblank_frame")
        {
            bail!("nonblank_frame was removed; use target-explicit nonblank_panel");
        }
        if let Some(nonblank) = command.pointer("/condition/nonblank_panel") {
            let target = nonblank
                .get("target")
                .and_then(Value::as_str)
                .context("nonblank_panel requires an explicit target")?;
            if !valid_product_presentation_target(target) {
                bail!("nonblank_panel target {target:?} is invalid");
            }
        }
    }
    validate_product_automation_hard_safety_limits(script)?;
    Ok(())
}

fn valid_product_presentation_target(target: &str) -> bool {
    matches!(target, "three_d" | "xy" | "xz" | "yz")
}

fn representative_temporal_playback_evidence(report: Option<&Value>) -> Result<(), String> {
    let report =
        report.ok_or_else(|| "temporal scenario produced no automation report".to_owned())?;
    let temporal = report
        .get("temporal_evidence")
        .and_then(Value::as_object)
        .ok_or_else(|| "temporal scenario omitted presentation evidence".to_owned())?;
    let transitions = temporal
        .get("presented_transition_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "temporal scenario omitted its presented transition count".to_owned())?;
    if transitions < 8 {
        return Err(format!(
            "temporal scenario presented only {transitions} transitions; at least eight are required across 24 and 12 FPS runs"
        ));
    }
    if temporal.get("continuity_violation") != Some(&Value::Null) {
        return Err(format!(
            "temporal scenario recorded a continuity violation: {:?}",
            temporal.get("continuity_violation")
        ));
    }
    let observations = temporal
        .get("observations")
        .and_then(Value::as_array)
        .ok_or_else(|| "temporal scenario omitted bounded presentation observations".to_owned())?;
    if observations.len() < 9 {
        return Err("temporal scenario did not retain enough presentation observations".to_owned());
    }

    let events = report
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "temporal scenario omitted command events".to_owned())?;
    let temporal_captures = events
        .iter()
        .filter(|event| {
            event.get("command").and_then(Value::as_str) == Some("capture_temporal_frame")
        })
        .collect::<Vec<_>>();
    if temporal_captures.len() != 2
        || temporal_captures
            .iter()
            .any(|event| event.get("status").and_then(Value::as_str) != Some("passed"))
    {
        return Err("temporal scenario did not pass exactly two semantic GPU captures".to_owned());
    }
    let first_timepoint = temporal_captures[0]
        .pointer("/details/presented_time_index")
        .and_then(Value::as_u64);
    let second_timepoint = temporal_captures[1]
        .pointer("/details/presented_time_index")
        .and_then(Value::as_u64);
    let changed_pixels = temporal_captures[1]
        .pointer("/details/different_pixels_from_previous")
        .and_then(Value::as_u64);
    let meaningful_pixels = temporal_captures.iter().all(|event| {
        event
            .pointer("/details/intermediate_rgb_pixels")
            .and_then(Value::as_u64)
            .is_some_and(|count| count > 0)
    });
    if first_timepoint != Some(0)
        || second_timepoint != Some(1)
        || changed_pixels.is_none_or(|count| count == 0)
        || !meaningful_pixels
    {
        return Err(
            "temporal scenario captures did not prove non-clipped, pixel-distinct t0 and t1 GPU presentations"
                .to_owned(),
        );
    }

    let transition_waits = events
        .iter()
        .filter(|event| {
            event.get("command").and_then(Value::as_str) == Some("wait_for_temporal_transitions")
                && event.get("status").and_then(Value::as_str) == Some("passed")
                && event
                    .pointer("/details/observed_transitions")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count >= 3)
        })
        .count();
    let fps_values = events
        .iter()
        .filter(|event| event.get("command").and_then(Value::as_str) == Some("set_playback_fps"))
        .filter_map(|event| event.pointer("/details/fps").and_then(Value::as_u64))
        .collect::<BTreeSet<_>>();
    if transition_waits != 3 || fps_values != BTreeSet::from([12, 24]) {
        return Err(
            "temporal scenario did not prove real transitions in standalone and four-panel 24 FPS runs plus the four-panel 12 FPS run"
                .to_owned(),
        );
    }
    let input_commands = BTreeSet::from([
        "camera_orbit_sequence",
        "camera_zoom_sequence",
        "cross_section_pan_sequence",
        "cross_section_zoom_sequence",
        "cross_section_rotate_sequence",
    ]);
    let input_events = events
        .iter()
        .filter(|event| {
            event
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| input_commands.contains(command))
        })
        .collect::<Vec<_>>();
    if input_events.len() != 7 {
        return Err(
            "temporal scenario omitted one or more required held-input workloads".to_owned(),
        );
    }
    let cadence_events = events
        .iter()
        .filter(|event| {
            event.get("command").and_then(Value::as_str) == Some("observe_playback_cadence")
                && event.get("status").and_then(Value::as_str) == Some("passed")
        })
        .collect::<Vec<_>>();
    if cadence_events.len() != input_events.len()
        || cadence_events.iter().any(|event| {
            event
                .pointer("/details/requested_duration_ms")
                .and_then(Value::as_u64)
                != Some(2_000)
                || event
                    .pointer("/details/presented_temporal_transitions")
                    .and_then(Value::as_u64)
                    .is_none_or(|count| count < 3)
        })
    {
        return Err(
            "temporal scenario did not record one valid same-duration stationary cadence baseline for every held input"
                .to_owned(),
        );
    }
    for event in input_events {
        let command = event
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("unknown input");
        let observed = event
            .pointer("/details/observed_counter_delta/presented_temporal_transitions")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{command} omitted its temporal transition count"))?;
        let timing = event
            .pointer("/details/temporal_presentation_timing")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{command} omitted monotonic temporal timing evidence"))?;
        let elapsed = timing
            .get("transition_elapsed_ns")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{command} omitted temporal presentation timestamps"))?;
        let first_half = timing
            .get("first_half_transitions")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let second_half = timing
            .get("second_half_transitions")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let window = timing
            .get("observation_window_ns")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let timestamps = elapsed
            .iter()
            .map(|value| value.as_u64())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("{command} contains a non-integer temporal timestamp"))?;
        let cadence_passed = event
            .pointer("/details/stationary_cadence_comparison/passed")
            .and_then(Value::as_bool)
            == Some(true);
        let baseline_transitions = event
            .pointer("/details/stationary_cadence_comparison/baseline_transitions")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let minimum_input_transitions = event
            .pointer("/details/stationary_cadence_comparison/minimum_input_transitions")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let maximum_input_gap = event
            .pointer("/details/stationary_cadence_comparison/input_maximum_gap_ns")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let maximum_allowed_gap = event
            .pointer("/details/stationary_cadence_comparison/maximum_allowed_input_gap_ns")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if observed < 3
            || timestamps.len() != usize::try_from(observed).unwrap_or(usize::MAX)
            || first_half == 0
            || second_half == 0
            || window == 0
            || timestamps.windows(2).any(|pair| pair[0] >= pair[1])
            || timestamps.last().is_some_and(|last| *last > window)
            || baseline_transitions < 3
            || observed < minimum_input_transitions
            || maximum_input_gap > maximum_allowed_gap
            || !cadence_passed
        {
            return Err(format!(
                "{command} did not prove temporal commits distributed throughout held input"
            ));
        }
    }

    let stop_events = events
        .iter()
        .filter(|event| {
            event.get("command").and_then(Value::as_str) == Some("assert")
                && event.pointer("/details/condition").and_then(Value::as_str)
                    == Some("playback_stopped_and_released")
                && event.get("status").and_then(Value::as_str) == Some("passed")
        })
        .collect::<Vec<_>>();
    if stop_events.len() != 3 {
        return Err("temporal scenario omitted a retained-front trace for one Stop".to_owned());
    }
    for event in stop_events {
        let trace = event
            .pointer("/details/playback_stop_handoff_trace")
            .and_then(Value::as_object)
            .ok_or_else(|| "playback Stop omitted its retained-front handoff trace".to_owned())?;
        if trace.get("violation") != Some(&Value::Null) {
            return Err(format!(
                "playback Stop recorded a handoff violation: {:?}",
                trace.get("violation")
            ));
        }
        let expected = trace
            .get("playback_layer_scales")
            .and_then(Value::as_array)
            .ok_or_else(|| "playback Stop omitted its fixed scale map".to_owned())?
            .iter()
            .map(|layer| {
                Some((
                    layer.get("layer_ordinal")?.as_u64()?,
                    layer.get("scale_level")?.as_u64()?,
                ))
            })
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or_else(|| "playback Stop contains an invalid fixed scale map".to_owned())?;
        let states = trace
            .get("states")
            .and_then(Value::as_array)
            .filter(|states| !states.is_empty())
            .ok_or_else(|| "playback Stop trace contains no visible state".to_owned())?;
        let mut previous = BTreeMap::<(String, u64), u64>::new();
        for (state_index, state) in states.iter().enumerate() {
            let panels = state
                .get("panels")
                .and_then(Value::as_array)
                .filter(|panels| !panels.is_empty())
                .ok_or_else(|| "playback Stop exposed an empty target group".to_owned())?;
            for panel in panels {
                let label = panel
                    .get("panel")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "playback Stop panel omitted its identity".to_owned())?;
                if panel.get("time_index").and_then(Value::as_u64)
                    != trace.get("expected_time_index").and_then(Value::as_u64)
                {
                    return Err(format!(
                        "playback Stop exposed {label} at the wrong timepoint"
                    ));
                }
                for layer in panel
                    .get("layers")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "playback Stop panel omitted layer scales".to_owned())?
                {
                    let ordinal = layer
                        .get("layer_ordinal")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "playback Stop layer omitted its ordinal".to_owned())?;
                    let displayed = layer
                        .get("displayed_scale_level")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "playback Stop exposed a layer without scale".to_owned())?;
                    let fixed = *expected.get(&ordinal).ok_or_else(|| {
                        "playback Stop exposed a layer outside its fixed scale map".to_owned()
                    })?;
                    let key = (label.to_owned(), ordinal);
                    if layer.get("mixed").and_then(Value::as_bool) != Some(false)
                        || displayed > fixed
                        || (state_index == 0 && displayed != fixed)
                        || previous
                            .get(&key)
                            .is_some_and(|previous| displayed > *previous)
                    {
                        return Err(format!(
                            "playback Stop trace downgraded or mixed {label} layer {ordinal}: fixed=s{fixed}, displayed=s{displayed}"
                        ));
                    }
                    previous.insert(key, displayed);
                }
            }
        }
    }
    Ok(())
}

fn representative_temporal_playback_log_evidence(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("temporal scenario runtime log is unavailable: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("temporal scenario runtime log is not a regular file".to_owned());
    }
    if metadata.len() > PRE_ALPHA_STDERR_BYTES_MAX {
        return Err(format!(
            "temporal scenario runtime log exceeded its {}-byte evidence bound",
            PRE_ALPHA_STDERR_BYTES_MAX
        ));
    }
    let log = fs::read_to_string(path)
        .map_err(|error| format!("temporal scenario runtime log is not bounded UTF-8: {error}"))?;
    let forbidden = [
        "ERROR",
        "the requirement set changed within one frame generation",
        "runtime lease delivery violated retained-demand requirements",
    ];
    if let Some(line) = log
        .lines()
        .find(|line| forbidden.iter().any(|marker| line.contains(marker)))
    {
        return Err(format!(
            "temporal scenario runtime log contains a renderer/runtime failure: {line}"
        ));
    }
    Ok(())
}

fn validate_product_automation_hard_safety_limits(script: &Value) -> anyhow::Result<()> {
    let hard_safety_limits = script
        .get("hard_safety_limits")
        .context("automation script hard_safety_limits must be present")?;
    canonical_product_automation_hard_safety_limits(hard_safety_limits)?;
    Ok(())
}

pub(crate) fn canonical_product_automation_hard_safety_limits(
    hard_safety_limits: &Value,
) -> anyhow::Result<Value> {
    let map = hard_safety_limits
        .as_object()
        .context("automation script hard_safety_limits must be an object")?;
    for (name, value) in map {
        if !PRODUCT_AUTOMATION_HARD_SAFETY_LIMIT_FIELDS.contains(&name.as_str()) {
            bail!("unknown automation script hard-safety limit {name:?}");
        }
        if !value.is_null() && value.as_u64().is_none() {
            bail!(
                "automation script hard-safety limit {name:?} must be null or an unsigned integer"
            );
        }
    }
    let canonical = PRODUCT_AUTOMATION_HARD_SAFETY_LIMIT_FIELDS
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                map.get(name).cloned().unwrap_or(Value::Null),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(Value::Object(canonical))
}

pub(crate) fn validate_product_automation_report_contract(
    report: &Value,
    script: &Value,
    script_path: &Path,
) -> anyhow::Result<()> {
    validate_product_automation_script(script)?;
    if report.get("schema").and_then(Value::as_str) != Some(PRODUCT_AUTOMATION_REPORT_SCHEMA)
        || report.get("schema_version").and_then(Value::as_u64)
            != Some(u64::from(REPORT_SCHEMA_VERSION))
        || report.get("status").and_then(Value::as_str) != Some("passed")
        || report.get("failure_reason") != Some(&Value::Null)
    {
        bail!("automation report schema, status, or failure contract is invalid");
    }
    if report.get("limits").is_some() {
        bail!("automation report contains the removed limits field");
    }
    let expected_limits = canonical_product_automation_hard_safety_limits(
        script
            .get("hard_safety_limits")
            .context("automation script hard_safety_limits must be present")?,
    )?;
    if report.get("hard_safety_limits") != Some(&expected_limits) {
        bail!("automation report hard_safety_limits do not exactly echo the script");
    }

    let script_report = report
        .get("script")
        .and_then(Value::as_object)
        .context("automation report script binding is missing")?;
    if script_report
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != BTreeSet::from([
            "path",
            "schema",
            "schema_version",
            "scenario",
            "command_count",
        ])
        || script_report.get("schema").and_then(Value::as_str)
            != script.get("schema").and_then(Value::as_str)
        || script_report.get("schema_version").and_then(Value::as_u64)
            != script.get("schema_version").and_then(Value::as_u64)
        || script_report.get("scenario").and_then(Value::as_str)
            != script.get("scenario").and_then(Value::as_str)
        || script_report.get("command_count").and_then(Value::as_u64)
            != script
                .get("commands")
                .and_then(Value::as_array)
                .and_then(|commands| u64::try_from(commands.len()).ok())
    {
        bail!("automation report script identity is invalid");
    }
    let reported_script_path = script_report
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .context("automation report script path is missing")?;
    let expected_script_path =
        fs::canonicalize(script_path).context("executed automation script path is unavailable")?;
    let reported_script_path = fs::canonicalize(reported_script_path)
        .context("reported automation script path is unavailable")?;
    if reported_script_path != expected_script_path {
        bail!("automation report script path does not match the executed script");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum B4Termination<'a> {
    Normal,
    ExternalSigkill {
        checkpoint: &'a Path,
        expected_stage: &'a str,
    },
    ExternalNativeClose {
        checkpoint: &'a Path,
        expected_stage: &'a str,
    },
}

#[derive(Debug, Clone, Copy)]
struct B4AttemptSpec<'a> {
    number: u64,
    phase: &'a str,
    script: &'a Value,
    expected_client_width: u32,
    expected_client_height: u32,
    expected_project: &'a Path,
    termination: B4Termination<'a>,
}

#[derive(Debug, Clone, Copy)]
struct PreAlphaAttemptSpec<'a> {
    number: u64,
    phase: &'a str,
    script: &'a Value,
    state_home: &'a Path,
    termination: B4Termination<'a>,
}

#[derive(Debug)]
struct B4ProcessStatus {
    timed_out: bool,
    exit_status: Option<String>,
    exit_success: Option<bool>,
    signal: Option<i32>,
    external_sigkill_sent: bool,
    external_native_close_sent: bool,
    checkpoint: Option<Value>,
    observed_client_area_pixels: Option<Value>,
    fullscreen_action: Option<Value>,
    control_failure: Option<String>,
}

struct B4FinishedProcessStatus {
    exit_status: std::process::ExitStatus,
    timed_out: bool,
    external_sigkill_sent: bool,
    external_native_close_sent: bool,
    checkpoint: Option<Value>,
    observed_client_area_pixels: Option<Value>,
    fullscreen_action: Option<Value>,
    control_failure: Option<String>,
}

fn run_b4_attempt(
    binary: &Path,
    package: &Path,
    state_home: &Path,
    run_dir: &Path,
    spec: B4AttemptSpec<'_>,
) -> Value {
    let phase_dir = run_dir.join(spec.phase);
    let automation_report_path = phase_dir.join("product-automation-report.json");
    let stdout_path = phase_dir.join("mirante4d-app.stdout.log");
    let stderr_path = phase_dir.join("mirante4d-app.stderr.log");
    let script_path = spec
        .script
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| phase_dir.join("invalid-script-path"));
    let source_before = SourceClosureSnapshot::capture(package);
    let started_at_epoch_ms = epoch_ms();
    let started_at = Instant::now();
    let progress = read_json_file(&script_path).and_then(|script| {
        let progress_plan = ProductAutomationProgressPlan::from_script(&script)?;
        let phase_root = fs::canonicalize(&phase_dir)
            .context("B4 phase directory is unavailable for progress monitoring")?;
        let progress_launch = ProductAutomationProgressLaunch::new_replacing_stale(&phase_root)?;
        Ok((progress_plan, progress_launch))
    });
    let process_result = match (&source_before, progress) {
        (Ok(_), Ok((progress_plan, progress_launch))) => run_b4_product_process(B4ProductRun {
            binary,
            package,
            script: &script_path,
            automation_report: &automation_report_path,
            stdout_path: &stdout_path,
            stderr_path: &stderr_path,
            state_home,
            timeout: Duration::from_secs(B4_PHASE_TIMEOUT_SECS),
            expected_client_width: spec.expected_client_width,
            expected_client_height: spec.expected_client_height,
            termination: spec.termination,
            phase: spec.phase,
            progress_plan,
            progress_launch,
        }),
        (Err(err), _) => Err(anyhow::anyhow!(err.to_string())),
        (_, Err(err)) => Err(err),
    };
    let source_closure_evidence = source_before
        .as_ref()
        .map_err(|err| anyhow::anyhow!(err.to_string()))
        .and_then(|before| before.compare_json(package))
        .unwrap_or_else(|err| {
            json!({
                "required": true,
                "byte_identical": Value::Null,
                "error": err.to_string(),
            })
        });
    let automation_report = if automation_report_path.exists() {
        read_json_file(&automation_report_path).unwrap_or_else(|err| {
            json!({
                "status": "invalid_report",
                "failure_reason": err.to_string(),
            })
        })
    } else {
        Value::Null
    };
    let process = match process_result {
        Ok(process) => b4_process_status_json(process),
        Err(err) => json!({
            "timed_out": false,
            "exit_status": Value::Null,
            "exit_success": Value::Null,
            "signal": Value::Null,
            "external_sigkill_sent": false,
            "external_native_close_sent": false,
            "checkpoint": Value::Null,
            "observed_client_area_pixels": Value::Null,
            "fullscreen_action": Value::Null,
            "control_failure": format!("B4 process runner failed: {err}"),
        }),
    };
    let mut attempt = json!({
        "attempt": spec.number,
        "phase": spec.phase,
        "retry_index": 0,
        "status": "pending",
        "failure_reason": Value::Null,
        "started_at_epoch_ms": started_at_epoch_ms,
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": duration_ms(started_at.elapsed()),
        "script": script_path,
        "automation_report_path": automation_report_path,
        "stdout": stdout_path,
        "stderr": stderr_path,
        "requested_client_area_pixels": {
            "width": spec.expected_client_width,
            "height": spec.expected_client_height,
        },
        "process": process,
        "automation_report": automation_report,
        "source_closure_evidence": source_closure_evidence,
        "project_package_evidence": {
            "path": spec.expected_project,
            "exists": spec.expected_project.exists(),
            "is_directory": spec.expected_project.is_dir(),
        },
    });
    match validate_b4_attempt(&attempt, spec.number) {
        Ok(()) => attempt["status"] = Value::String("passed".to_owned()),
        Err(err) => {
            attempt["status"] = Value::String("failed".to_owned());
            attempt["failure_reason"] = Value::String(err.to_string());
        }
    }
    attempt
}

fn run_pre_alpha_attempt(
    binary: &Path,
    package: &Path,
    run_dir: &Path,
    spec: PreAlphaAttemptSpec<'_>,
) -> Value {
    let phase_dir = run_dir.join(spec.phase);
    let automation_report_path = phase_dir.join("product-automation-report.json");
    let stdout_path = phase_dir.join("mirante4d-app.stdout.log");
    let stderr_path = phase_dir.join("mirante4d-app.stderr.log");
    let script_path = spec
        .script
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| phase_dir.join("invalid-script-path"));
    let source_before = SourceClosureSnapshot::capture(package);
    let started_at_epoch_ms = epoch_ms();
    let started_at = Instant::now();
    let progress = read_json_file(&script_path).and_then(|script| {
        let progress_plan = ProductAutomationProgressPlan::from_script(&script)?;
        let phase_root = fs::canonicalize(&phase_dir).context(
            "pre-alpha reliability phase directory is unavailable for progress monitoring",
        )?;
        let progress_launch = ProductAutomationProgressLaunch::new_replacing_stale(&phase_root)?;
        Ok((progress_plan, progress_launch))
    });
    let process_result = match (&source_before, progress) {
        (Ok(_), Ok((progress_plan, progress_launch))) => run_b4_product_process(B4ProductRun {
            binary,
            package,
            script: &script_path,
            automation_report: &automation_report_path,
            stdout_path: &stdout_path,
            stderr_path: &stderr_path,
            state_home: spec.state_home,
            timeout: Duration::from_secs(B4_PHASE_TIMEOUT_SECS),
            expected_client_width: B4_PRIMARY_CLIENT_WIDTH,
            expected_client_height: B4_PRIMARY_CLIENT_HEIGHT,
            termination: spec.termination,
            phase: spec.phase,
            progress_plan,
            progress_launch,
        }),
        (Err(err), _) => Err(anyhow::anyhow!(err.to_string())),
        (_, Err(err)) => Err(err),
    };
    let source_closure_evidence = source_before
        .as_ref()
        .map_err(|err| anyhow::anyhow!(err.to_string()))
        .and_then(|before| before.compare_json(package))
        .unwrap_or_else(|err| {
            json!({
                "required": true,
                "byte_identical": Value::Null,
                "error": err.to_string(),
            })
        });
    let automation_report = if automation_report_path.exists() {
        read_json_file(&automation_report_path).unwrap_or_else(|err| {
            json!({
                "status": "invalid_report",
                "failure_reason": err.to_string(),
            })
        })
    } else {
        Value::Null
    };
    let process = match process_result {
        Ok(process) => b4_process_status_json(process),
        Err(err) => json!({
            "timed_out": false,
            "exit_status": Value::Null,
            "exit_success": Value::Null,
            "signal": Value::Null,
            "external_sigkill_sent": false,
            "external_native_close_sent": false,
            "checkpoint": Value::Null,
            "observed_client_area_pixels": Value::Null,
            "fullscreen_action": Value::Null,
            "control_failure": format!("pre-alpha reliability process runner failed: {err}"),
        }),
    };
    let stderr_evidence = pre_alpha_stderr_evidence(&stderr_path).unwrap_or_else(|err| {
        json!({
            "checked": false,
            "panic_free": Value::Null,
            "error": err.to_string(),
        })
    });
    let recovery_store_evidence = pre_alpha_recovery_store_evidence(spec.state_home)
        .unwrap_or_else(|err| {
            json!({
                "checked": false,
                "error": err.to_string(),
            })
        });
    let mut attempt = json!({
        "attempt": spec.number,
        "phase": spec.phase,
        "retry_index": 0,
        "status": "pending",
        "failure_reason": Value::Null,
        "started_at_epoch_ms": started_at_epoch_ms,
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": duration_ms(started_at.elapsed()),
        "script": script_path,
        "automation_report_path": automation_report_path,
        "stdout": stdout_path,
        "stderr": stderr_path,
        "state_home": spec.state_home,
        "requested_client_area_pixels": {
            "width": B4_PRIMARY_CLIENT_WIDTH,
            "height": B4_PRIMARY_CLIENT_HEIGHT,
        },
        "process": process,
        "automation_report": automation_report,
        "source_closure_evidence": source_closure_evidence,
        "stderr_evidence": stderr_evidence,
        "recovery_store_evidence": recovery_store_evidence,
    });
    match validate_pre_alpha_attempt(&attempt, spec.number) {
        Ok(()) => attempt["status"] = Value::String("passed".to_owned()),
        Err(err) => {
            attempt["status"] = Value::String("failed".to_owned());
            attempt["failure_reason"] = Value::String(err.to_string());
        }
    }
    attempt
}

fn pre_alpha_stderr_evidence(path: &Path) -> anyhow::Result<Value> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("pre-alpha stderr evidence is not a regular file");
    }
    if metadata.len() > PRE_ALPHA_STDERR_BYTES_MAX {
        bail!(
            "pre-alpha stderr exceeded its {}-byte evidence bound",
            PRE_ALPHA_STDERR_BYTES_MAX
        );
    }
    let stderr =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let lowercase = stderr.to_ascii_lowercase();
    let panic_free = !lowercase.contains("panicked at")
        && !lowercase.contains("thread 'main' panicked")
        && !lowercase.contains("thread \"main\" panicked");
    Ok(json!({
        "checked": true,
        "bytes": metadata.len(),
        "maximum_bytes": PRE_ALPHA_STDERR_BYTES_MAX,
        "panic_free": panic_free,
        "matching_policy": "bounded_complete_utf8_log_has_no_rust_panic_marker",
    }))
}

fn pre_alpha_recovery_store_evidence(state_home: &Path) -> anyhow::Result<Value> {
    let recovery_root = state_home.join("mirante4d").join("recovery");
    let metadata = fs::symlink_metadata(&recovery_root)
        .with_context(|| format!("failed to inspect {}", recovery_root.display()))?;
    if !metadata.file_type().is_dir() {
        bail!("pre-alpha recovery root is not a real directory");
    }
    let mut entries = 0_usize;
    let mut canonical_store_directories = 0_usize;
    for entry in fs::read_dir(&recovery_root)
        .with_context(|| format!("failed to enumerate {}", recovery_root.display()))?
    {
        if entries >= PRE_ALPHA_RECOVERY_ROOT_ENTRIES_MAX {
            bail!(
                "pre-alpha recovery root exceeded its {}-entry evidence bound",
                PRE_ALPHA_RECOVERY_ROOT_ENTRIES_MAX
            );
        }
        let entry = entry.context("failed to read a pre-alpha recovery-root entry")?;
        entries += 1;
        let file_type = entry
            .file_type()
            .context("failed to inspect a pre-alpha recovery-root entry")?;
        let name = entry.file_name();
        let canonical_name = name
            .to_str()
            .is_some_and(is_canonical_project_store_directory_name);
        if file_type.is_dir() && canonical_name {
            canonical_store_directories += 1;
        } else {
            bail!(
                "isolated pre-alpha recovery root contains a noncanonical or non-directory entry"
            );
        }
    }
    Ok(json!({
        "checked": true,
        "root": recovery_root,
        "entries": entries,
        "canonical_store_directories": canonical_store_directories,
        "maximum_entries": PRE_ALPHA_RECOVERY_ROOT_ENTRIES_MAX,
        "all_entries_canonical_directories": true,
    }))
}

fn is_canonical_project_store_directory_name(name: &str) -> bool {
    let Some(project_id) = name.strip_suffix(".m4dproj") else {
        return false;
    };
    if project_id.len() != 36 {
        return false;
    }
    project_id.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
        }
    })
}

fn b4_process_status_json(status: B4ProcessStatus) -> Value {
    json!({
        "timed_out": status.timed_out,
        "exit_status": status.exit_status,
        "exit_success": status.exit_success,
        "signal": status.signal,
        "external_sigkill_sent": status.external_sigkill_sent,
        "external_native_close_sent": status.external_native_close_sent,
        "checkpoint": status.checkpoint,
        "observed_client_area_pixels": status.observed_client_area_pixels,
        "fullscreen_action": status.fullscreen_action,
        "control_failure": status.control_failure,
    })
}

struct B4ProductRun<'a> {
    binary: &'a Path,
    package: &'a Path,
    script: &'a Path,
    automation_report: &'a Path,
    stdout_path: &'a Path,
    stderr_path: &'a Path,
    state_home: &'a Path,
    timeout: Duration,
    expected_client_width: u32,
    expected_client_height: u32,
    termination: B4Termination<'a>,
    phase: &'a str,
    progress_plan: ProductAutomationProgressPlan,
    progress_launch: ProductAutomationProgressLaunch,
}

fn run_b4_product_process(run: B4ProductRun<'_>) -> anyhow::Result<B4ProcessStatus> {
    let stdout = fs::File::create(run.stdout_path)
        .with_context(|| format!("failed to create {}", run.stdout_path.display()))?;
    let stderr = fs::File::create(run.stderr_path)
        .with_context(|| format!("failed to create {}", run.stderr_path.display()))?;
    let mut command = Command::new(run.binary);
    command
        .env("MIRANTE4D_DEV_DATASET", run.package)
        .env("MIRANTE4D_ENABLE_AUTOMATION", "1")
        .env("MIRANTE4D_AUTOMATION_SCRIPT", run.script)
        .env("MIRANTE4D_AUTOMATION_REPORT", run.automation_report)
        .env("XDG_STATE_HOME", run.state_home)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    isolate_process_tree(&mut command);
    run.progress_launch.apply_to_command(&mut command);
    println!("running B4 product validation phase: {:?}", command);
    let started = Instant::now();
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch native app product validation binary {}",
            run.binary.display()
        )
    })?;
    let deadline = started + run.timeout;
    let mut progress_monitor = run.progress_launch.monitor(run.progress_plan, started);
    let mut next_progress_poll = started;
    let mut observed_client_area_pixels = None;
    let mut fullscreen_action = None;
    let mut checkpoint = None;
    let mut external_native_close_sent = false;
    let mut external_native_close_sent_at = None;
    loop {
        if observed_client_area_pixels.is_none() {
            match probe_b4_x11_client_geometry(
                child.id(),
                run.expected_client_width,
                run.expected_client_height,
                &mut fullscreen_action,
            ) {
                Ok(observed) => observed_client_area_pixels = observed,
                Err(err) => {
                    if let Some(exit_status) = child
                        .try_wait()
                        .context("failed to poll B4 product child after geometry failure")?
                    {
                        let progress_failure = progress_monitor
                            .finalize_at_exit(Instant::now())
                            .map(|failure| failure.reason_code());
                        return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                            exit_status,
                            timed_out: false,
                            external_sigkill_sent: false,
                            external_native_close_sent: false,
                            checkpoint,
                            observed_client_area_pixels,
                            fullscreen_action,
                            control_failure: Some(progress_failure.map_or_else(
                                || format!("external X11 geometry observation failed: {err}"),
                                |failure| format!("automation progress protocol failed: {failure}"),
                            )),
                        }));
                    }
                }
            }
        }

        let checkpoint_request = match run.termination {
            B4Termination::ExternalSigkill {
                checkpoint,
                expected_stage,
            }
            | B4Termination::ExternalNativeClose {
                checkpoint,
                expected_stage,
            } => Some((checkpoint, expected_stage)),
            B4Termination::Normal => None,
        };
        if let Some((checkpoint_path, expected_stage)) = checkpoint_request
            && checkpoint.is_none()
            && checkpoint_path.exists()
        {
            match read_json_file(checkpoint_path).and_then(|value| {
                validate_external_checkpoint_identity(&value, expected_stage).map(|()| value)
            }) {
                Ok(value) => checkpoint = Some(value),
                Err(err) => {
                    terminate_process_tree(&mut child);
                    let exit_status = child
                        .wait()
                        .context("failed to reap B4 child after invalid checkpoint")?;
                    return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                        exit_status,
                        timed_out: false,
                        external_sigkill_sent: true,
                        external_native_close_sent: false,
                        checkpoint: None,
                        observed_client_area_pixels,
                        fullscreen_action,
                        control_failure: Some(format!(
                            "external kill checkpoint failed validation: {err}"
                        )),
                    }));
                }
            }
        }

        if matches!(run.termination, B4Termination::ExternalSigkill { .. })
            && checkpoint.is_some()
            && observed_client_area_pixels.is_some()
        {
            terminate_process_tree(&mut child);
            let exit_status = child
                .wait()
                .context("failed to reap externally killed B4 product child")?;
            return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                exit_status,
                timed_out: false,
                external_sigkill_sent: true,
                external_native_close_sent: false,
                checkpoint,
                observed_client_area_pixels,
                fullscreen_action,
                control_failure: None,
            }));
        }

        if matches!(run.termination, B4Termination::ExternalNativeClose { .. })
            && checkpoint.is_some()
            && !external_native_close_sent
            && let Some(observed_client) = observed_client_area_pixels.as_ref()
        {
            if let Err(err) = request_b4_native_x11_close(observed_client) {
                terminate_process_tree(&mut child);
                let exit_status = child
                    .wait()
                    .context("failed to reap product child after native-close request failure")?;
                return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                    exit_status,
                    timed_out: false,
                    external_sigkill_sent: false,
                    external_native_close_sent: false,
                    checkpoint,
                    observed_client_area_pixels,
                    fullscreen_action,
                    control_failure: Some(format!(
                        "external native X11 close request failed: {err}"
                    )),
                }));
            }
            external_native_close_sent = true;
            external_native_close_sent_at = Some(Instant::now());
        }

        if let Some(exit_status) = child
            .try_wait()
            .context("failed to poll B4 product validation child")?
        {
            let external_native_close_completed =
                matches!(run.termination, B4Termination::ExternalNativeClose { .. })
                    && external_native_close_sent;
            let early_exit = matches!(
                run.termination,
                B4Termination::ExternalSigkill { .. } | B4Termination::ExternalNativeClose { .. }
            ) && !external_native_close_completed;
            let progress_failure = (!external_native_close_completed)
                .then(|| {
                    progress_monitor
                        .finalize_at_exit(Instant::now())
                        .map(|failure| failure.reason_code())
                })
                .flatten();
            return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                exit_status,
                timed_out: false,
                external_sigkill_sent: false,
                external_native_close_sent,
                checkpoint,
                observed_client_area_pixels,
                fullscreen_action,
                control_failure: progress_failure
                    .map(|failure| format!("automation progress protocol failed: {failure}"))
                    .or_else(|| {
                        early_exit.then(|| {
                            "native app exited before its synced checkpoint and external lifecycle action"
                                .to_owned()
                        })
                    }),
            }));
        }
        let now = Instant::now();
        if external_native_close_sent_at
            .is_some_and(|sent_at| now.duration_since(sent_at) >= NATIVE_CLOSE_EXIT_TIMEOUT)
        {
            terminate_process_tree(&mut child);
            let exit_status = child
                .wait()
                .context("failed to reap product child after native-close exit deadline")?;
            return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                exit_status,
                timed_out: false,
                external_sigkill_sent: false,
                external_native_close_sent: true,
                checkpoint,
                observed_client_area_pixels,
                fullscreen_action,
                control_failure: Some(format!(
                    "mapped native X11 close did not exit within {} seconds; fallback termination was failure cleanup",
                    NATIVE_CLOSE_EXIT_TIMEOUT.as_secs()
                )),
            }));
        }
        if now >= next_progress_poll {
            match progress_monitor.poll_at(now) {
                ProgressMonitorAction::Continue => {}
                ProgressMonitorAction::Emit(snapshot) => {
                    let line =
                        safe_automation_progress_line("product_validate_b4", run.phase, &snapshot)?;
                    eprintln!("{line}");
                }
                ProgressMonitorAction::Terminate(failure) => {
                    terminate_process_tree(&mut child);
                    let exit_status = child
                        .wait()
                        .context("failed to reap B4 child after progress failure")?;
                    return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                        exit_status,
                        timed_out: false,
                        external_sigkill_sent: false,
                        external_native_close_sent,
                        checkpoint,
                        observed_client_area_pixels,
                        fullscreen_action,
                        control_failure: Some(format!(
                            "automation progress protocol failed: {}",
                            failure.reason_code()
                        )),
                    }));
                }
            }
            next_progress_poll = now + FILE_POLL_INTERVAL;
        }
        if now >= deadline {
            terminate_process_tree(&mut child);
            let exit_status = child
                .wait()
                .context("failed to reap timed-out B4 product child")?;
            return Ok(b4_finished_process_status(B4FinishedProcessStatus {
                exit_status,
                timed_out: true,
                external_sigkill_sent: false,
                external_native_close_sent,
                checkpoint,
                observed_client_area_pixels,
                fullscreen_action,
                control_failure: Some(format!(
                    "B4 product phase exceeded its {}-second timeout",
                    run.timeout.as_secs()
                )),
            }));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn b4_finished_process_status(finished: B4FinishedProcessStatus) -> B4ProcessStatus {
    B4ProcessStatus {
        timed_out: finished.timed_out,
        exit_status: Some(finished.exit_status.to_string()),
        exit_success: Some(finished.exit_status.success()),
        signal: finished.exit_status.signal(),
        external_sigkill_sent: finished.external_sigkill_sent,
        external_native_close_sent: finished.external_native_close_sent,
        checkpoint: finished.checkpoint,
        observed_client_area_pixels: finished.observed_client_area_pixels,
        fullscreen_action: finished.fullscreen_action,
        control_failure: finished.control_failure,
    }
}

fn probe_b4_x11_client_geometry(
    pid: u32,
    expected_width: u32,
    expected_height: u32,
    fullscreen_action: &mut Option<Value>,
) -> anyhow::Result<Option<Value>> {
    let mut search = Command::new("xdotool");
    search.args(["search", "--onlyvisible", "--pid", &pid.to_string()]);
    let search = run_command_with_bounded_output(&mut search, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to run xdotool window search")?;
    if !search.status.success() {
        return Ok(None);
    }
    let window_ids = String::from_utf8(search.stdout).context("xdotool output was not UTF-8")?;
    for raw_id in window_ids
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(window_id) = raw_id.parse::<u64>() else {
            continue;
        };
        let id_hex = format!("0x{window_id:x}");
        let mut info = Command::new("xwininfo");
        info.args(["-id", &id_hex]);
        let info = run_command_with_bounded_output(&mut info, X11_AUTOMATION_OUTPUT_POLICY)
            .context("failed to run xwininfo")?;
        if !info.status.success() {
            continue;
        }
        let encoded = String::from_utf8(info.stdout).context("xwininfo output was not UTF-8")?;
        let Some((width, height, is_viewable)) = parse_xwininfo_client_geometry(&encoded) else {
            continue;
        };
        if width == expected_width && height == expected_height && is_viewable {
            return Ok(Some(json!({
                "width": width,
                "height": height,
                "window_id": id_hex,
                "map_state": "is_viewable",
                "observation": "xdotool_pid_search_plus_xwininfo_client_geometry",
                "observed_at_epoch_ms": epoch_ms(),
            })));
        }
        if expected_width == B4_SECONDARY_CLIENT_WIDTH
            && expected_height == B4_SECONDARY_CLIENT_HEIGHT
            && fullscreen_action.is_none()
        {
            let mut action = Command::new("wmctrl");
            action.args(["-i", "-r", &id_hex, "-b", "add,fullscreen"]);
            let action = run_command_with_bounded_output(&mut action, X11_AUTOMATION_OUTPUT_POLICY)
                .context("failed to request external fullscreen through wmctrl")?;
            if action.status.success() {
                *fullscreen_action = Some(json!({
                    "tool": "wmctrl",
                    "window_id": id_hex,
                    "action": "add_fullscreen",
                    "status": "succeeded",
                }));
            }
        }
    }
    Ok(None)
}

fn request_b4_native_x11_close(observed_client: &Value) -> anyhow::Result<()> {
    if observed_client.get("map_state").and_then(Value::as_str) != Some("is_viewable") {
        bail!("native close requires an externally observed mapped X11 client");
    }
    let window_id = observed_client
        .get("window_id")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("0x") && value.len() > 2)
        .context("mapped X11 client observation lacks its window ID")?;
    let mut close = Command::new("wmctrl");
    close.args(["-i", "-c", window_id]);
    let output = run_command_with_bounded_output(&mut close, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to request mapped X11 client close through wmctrl")?;
    if !output.status.success() {
        bail!("wmctrl native close failed with status {}", output.status);
    }
    Ok(())
}

fn parse_xwininfo_client_geometry(output: &str) -> Option<(u32, u32, bool)> {
    let width = output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Width:"))?
        .trim()
        .parse::<u32>()
        .ok()?;
    let height = output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Height:"))?
        .trim()
        .parse::<u32>()
        .ok()?;
    let is_viewable = output
        .lines()
        .map(str::trim)
        .any(|line| line == "Map State: IsViewable");
    Some((width, height, is_viewable))
}

fn validate_external_checkpoint_identity(
    checkpoint: &Value,
    expected_stage: &str,
) -> anyhow::Result<()> {
    if checkpoint.get("schema").and_then(Value::as_str) != Some(B4_CHECKPOINT_SCHEMA)
        || checkpoint.get("schema_version").and_then(Value::as_u64) != Some(1)
        || checkpoint.get("stage").and_then(Value::as_str) != Some(expected_stage)
    {
        bail!("external process checkpoint identity or stage drifted");
    }
    Ok(())
}

fn validate_b4_checkpoint(checkpoint: &Value, expected_stage: &str) -> anyhow::Result<()> {
    validate_external_checkpoint_identity(checkpoint, expected_stage)?;
    let requested_pixels = checkpoint
        .pointer("/viewport_evidence/requested_mapped_client_pixels")
        .context("B4 checkpoint lacks requested mapped-client pixels")?;
    if requested_pixels.get("width").and_then(Value::as_u64)
        != Some(u64::from(B4_PRIMARY_CLIENT_WIDTH))
        || requested_pixels.get("height").and_then(Value::as_u64)
            != Some(u64::from(B4_PRIMARY_CLIENT_HEIGHT))
    {
        bail!("B4 checkpoint mapped-client request is not exact 1280x720");
    }
    let state = checkpoint
        .get("project_state")
        .context("B4 checkpoint lacks project state")?;
    for (field, expected) in [
        ("bound", true),
        ("dirty", true),
        ("can_save", true),
        ("can_save_as", true),
        ("manual", true),
        ("autosave", true),
    ] {
        if state.get(field).and_then(Value::as_bool) != Some(expected) {
            bail!("B4 checkpoint project_state.{field} drifted");
        }
    }
    if state.get("lifecycle").and_then(Value::as_str) != Some("established")
        || state.get("current_manual").is_none_or(Value::is_null)
        || state.get("current_autosave").is_none_or(Value::is_null)
    {
        bail!("B4 checkpoint does not retain established manual and autosave heads");
    }
    let evidence = checkpoint
        .get("project_evidence")
        .context("B4 checkpoint lacks project evidence")?;
    if evidence.get("autosave_wait_mode").and_then(Value::as_str)
        != Some("scheduled_deadline_no_busy_poll")
        || evidence
            .get("autosave_elapsed_from_durable_edit_ms")
            .and_then(Value::as_u64)
            .is_none_or(|elapsed| elapsed < B4_AUTOSAVE_MIN_ELAPSED_MS)
    {
        bail!("B4 checkpoint does not prove a passive real 30-second autosave deadline");
    }
    let current = b4_revision_fact(state.get("current_revision"), "current revision")?;
    let saved = b4_revision_fact(state.get("saved_revision"), "saved revision")?;
    let initial = b4_revision_fact(
        evidence.get("initial_save_captured_revision"),
        "initial Save captured revision",
    )?;
    let autosave = b4_revision_fact(
        evidence.get("latest_autosave_captured_revision"),
        "latest autosave captured revision",
    )?;
    if saved != initial || current != autosave || current.0 != saved.0 || current.1 <= saved.1 {
        bail!(
            "B4 checkpoint does not prove initial captured Save, later dirty edit, and autosave of the current revision"
        );
    }
    Ok(())
}

fn b4_revision_fact(value: Option<&Value>, context: &str) -> anyhow::Result<(String, u64)> {
    let value = value.with_context(|| format!("B4 checkpoint lacks {context}"))?;
    let project_id = value
        .get("project_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("B4 checkpoint {context} lacks project_id"))?;
    let sequence = value
        .get("sequence")
        .and_then(Value::as_u64)
        .with_context(|| format!("B4 checkpoint {context} lacks sequence"))?;
    Ok((project_id.to_owned(), sequence))
}

fn validate_b4_attempt(attempt: &Value, expected_number: u64) -> anyhow::Result<()> {
    if attempt.get("attempt").and_then(Value::as_u64) != Some(expected_number)
        || attempt.get("retry_index").and_then(Value::as_u64) != Some(0)
    {
        bail!("B4 attempt identity or retry index drifted");
    }
    if attempt
        .pointer("/source_closure_evidence/byte_identical")
        .and_then(Value::as_bool)
        != Some(true)
    {
        bail!("B4 launch {expected_number} changed or failed to compare the source closure");
    }
    if attempt
        .pointer("/project_package_evidence/exists")
        .and_then(Value::as_bool)
        != Some(true)
        || attempt
            .pointer("/project_package_evidence/is_directory")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("B4 launch {expected_number} did not retain its expected project package");
    }
    if attempt
        .pointer("/process/timed_out")
        .and_then(Value::as_bool)
        != Some(false)
        || !attempt
            .pointer("/process/control_failure")
            .is_some_and(Value::is_null)
    {
        bail!("B4 launch {expected_number} process control failed");
    }
    let requested = attempt
        .get("requested_client_area_pixels")
        .context("B4 attempt lacks requested client pixels")?;
    let observed = attempt
        .pointer("/process/observed_client_area_pixels")
        .context("B4 attempt lacks externally observed client pixels")?;
    if observed.get("width") != requested.get("width")
        || observed.get("height") != requested.get("height")
        || observed.get("map_state").and_then(Value::as_str) != Some("is_viewable")
        || observed.get("observation").and_then(Value::as_str)
            != Some("xdotool_pid_search_plus_xwininfo_client_geometry")
    {
        bail!("B4 launch {expected_number} mapped client geometry was not externally exact");
    }

    if expected_number == 1 {
        if attempt
            .pointer("/process/external_sigkill_sent")
            .and_then(Value::as_bool)
            != Some(true)
            || attempt.pointer("/process/signal").and_then(Value::as_i64) != Some(9)
            || attempt
                .pointer("/process/exit_success")
                .and_then(Value::as_bool)
                != Some(false)
        {
            bail!("B4 launch 1 was not terminated by the parent's external SIGKILL signal 9");
        }
        let checkpoint = attempt
            .pointer("/process/checkpoint")
            .context("B4 launch 1 lacks the synced pre-kill checkpoint")?;
        validate_b4_checkpoint(checkpoint, B4_CHECKPOINT_STAGE)?;
        return Ok(());
    }

    if attempt
        .pointer("/process/exit_success")
        .and_then(Value::as_bool)
        != Some(true)
        || !attempt
            .pointer("/process/signal")
            .is_some_and(Value::is_null)
    {
        bail!("B4 launch {expected_number} did not exit normally");
    }
    let automation = attempt
        .get("automation_report")
        .context("B4 normal launch lacks its automation report")?;
    let script_path = attempt
        .get("script")
        .and_then(Value::as_str)
        .map(Path::new)
        .context("B4 normal launch lacks its exact automation script path")?;
    let script = read_json_file(script_path)
        .context("B4 normal launch automation script could not be read")?;
    validate_product_automation_report_contract(automation, &script, script_path)
        .context("B4 normal launch automation report contract is invalid")?;
    let expected_width = requested.get("width").and_then(Value::as_u64);
    let expected_height = requested.get("height").and_then(Value::as_u64);
    if automation
        .pointer("/viewport_evidence/requested_mapped_client_pixels/width")
        .and_then(Value::as_u64)
        != expected_width
        || automation
            .pointer("/viewport_evidence/requested_mapped_client_pixels/height")
            .and_then(Value::as_u64)
            != expected_height
    {
        bail!("B4 launch {expected_number} app request and external client geometry disagree");
    }
    qualifying_nonblank_viewport_capture(Some(automation)).map_err(anyhow::Error::msg)?;
    if automation
        .pointer("/project_store_evidence/close_result/status")
        .and_then(Value::as_str)
        != Some("succeeded")
        || automation
            .pointer("/project_store_evidence/actor_join/status")
            .and_then(Value::as_str)
            != Some("succeeded")
    {
        bail!("B4 launch {expected_number} does not prove normal Close and actor join");
    }
    Ok(())
}

fn validate_b4_aggregate_attempts(attempts: &[Value]) -> anyhow::Result<()> {
    if attempts.len() != 3 {
        bail!("B4 aggregate requires exactly three retained launch attempts");
    }
    for (index, attempt) in attempts.iter().enumerate() {
        let expected_number = u64::try_from(index + 1).expect("three attempts fit u64");
        if attempt.get("status").and_then(Value::as_str) != Some("passed") {
            bail!("B4 launch {expected_number} is not passed");
        }
        validate_b4_attempt(attempt, expected_number)?;
    }
    if attempts
        .iter()
        .any(|attempt| attempt.get("retry_index").and_then(Value::as_u64) != Some(0))
    {
        bail!("B4 aggregate observed a retry");
    }
    Ok(())
}

fn validate_pre_alpha_checkpoint(
    checkpoint: &Value,
    expected_stage: &str,
    expected_number: u64,
) -> anyhow::Result<()> {
    validate_external_checkpoint_identity(checkpoint, expected_stage)?;
    let requested_pixels = checkpoint
        .pointer("/viewport_evidence/requested_mapped_client_pixels")
        .context("pre-alpha checkpoint lacks requested mapped-client pixels")?;
    if requested_pixels.get("width").and_then(Value::as_u64)
        != Some(u64::from(B4_PRIMARY_CLIENT_WIDTH))
        || requested_pixels.get("height").and_then(Value::as_u64)
            != Some(u64::from(B4_PRIMARY_CLIENT_HEIGHT))
    {
        bail!("pre-alpha checkpoint mapped-client request is not exact 1280x720");
    }
    let state = checkpoint
        .get("project_state")
        .context("pre-alpha checkpoint lacks project state")?;
    match expected_number {
        1 => {
            for (field, expected) in [
                ("bound", true),
                ("dirty", true),
                ("can_save", true),
                ("can_save_as", false),
                ("manual", false),
                ("autosave", true),
            ] {
                if state.get(field).and_then(Value::as_bool) != Some(expected) {
                    bail!("pre-alpha provisional checkpoint project_state.{field} drifted");
                }
            }
            if state.get("lifecycle").and_then(Value::as_str) != Some("provisional")
                || !state.get("current_manual").is_some_and(Value::is_null)
                || state.get("current_autosave").is_none_or(Value::is_null)
            {
                bail!(
                    "pre-alpha provisional checkpoint does not retain provisional autosave-only state"
                );
            }
            let current = b4_revision_fact(state.get("current_revision"), "current revision")?;
            let autosave = b4_revision_fact(
                checkpoint.pointer("/project_evidence/latest_autosave_captured_revision"),
                "latest autosave captured revision",
            )?;
            if current != autosave {
                bail!(
                    "pre-alpha provisional checkpoint autosave does not capture the current revision"
                );
            }
        }
        3 => {
            for field in ["bound", "dirty", "can_save_as", "manual", "autosave"] {
                if state.get(field).and_then(Value::as_bool) != Some(false) {
                    bail!("pre-alpha clean-close checkpoint project_state.{field} drifted");
                }
            }
            if state.get("can_save").and_then(Value::as_bool) != Some(true) {
                bail!("pre-alpha clean-close checkpoint project_state.can_save drifted");
            }
            for field in [
                "current_revision",
                "saved_revision",
                "current_manual",
                "current_autosave",
            ] {
                if !state.get(field).is_some_and(Value::is_null) {
                    bail!("pre-alpha clean-close checkpoint project_state.{field} is not unbound");
                }
            }
            if state.get("lifecycle").and_then(Value::as_str) != Some("unbound") {
                bail!("pre-alpha clean-close checkpoint lifecycle is not unbound");
            }
        }
        _ => bail!("pre-alpha checkpoint validation was requested for an invalid launch"),
    }
    Ok(())
}

fn validate_pre_alpha_attempt(attempt: &Value, expected_number: u64) -> anyhow::Result<()> {
    if attempt.get("attempt").and_then(Value::as_u64) != Some(expected_number)
        || attempt.get("retry_index").and_then(Value::as_u64) != Some(0)
    {
        bail!("pre-alpha reliability attempt identity or retry index drifted");
    }
    if attempt
        .pointer("/source_closure_evidence/byte_identical")
        .and_then(Value::as_bool)
        != Some(true)
    {
        bail!("pre-alpha launch {expected_number} changed or failed to compare the source closure");
    }
    if attempt
        .pointer("/process/timed_out")
        .and_then(Value::as_bool)
        != Some(false)
        || !attempt
            .pointer("/process/control_failure")
            .is_some_and(Value::is_null)
    {
        bail!("pre-alpha launch {expected_number} process control failed");
    }
    if attempt
        .pointer("/stderr_evidence/checked")
        .and_then(Value::as_bool)
        != Some(true)
        || attempt
            .pointer("/stderr_evidence/panic_free")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("pre-alpha launch {expected_number} did not retain panic-free bounded stderr");
    }
    if attempt
        .pointer("/recovery_store_evidence/checked")
        .and_then(Value::as_bool)
        != Some(true)
        || attempt
            .pointer("/recovery_store_evidence/all_entries_canonical_directories")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("pre-alpha launch {expected_number} recovery-root evidence is invalid");
    }
    let requested = attempt
        .get("requested_client_area_pixels")
        .context("pre-alpha attempt lacks requested client pixels")?;
    let observed = attempt
        .pointer("/process/observed_client_area_pixels")
        .context("pre-alpha attempt lacks externally observed client pixels")?;
    if observed.get("width") != requested.get("width")
        || observed.get("height") != requested.get("height")
        || observed.get("map_state").and_then(Value::as_str) != Some("is_viewable")
        || observed.get("observation").and_then(Value::as_str)
            != Some("xdotool_pid_search_plus_xwininfo_client_geometry")
    {
        bail!("pre-alpha launch {expected_number} mapped client geometry was not externally exact");
    }
    let recovery_store_count = attempt
        .pointer("/recovery_store_evidence/canonical_store_directories")
        .and_then(Value::as_u64)
        .context("pre-alpha attempt lacks its recovery-store count")?;

    match expected_number {
        1 => {
            if attempt
                .pointer("/process/external_sigkill_sent")
                .and_then(Value::as_bool)
                != Some(true)
                || attempt
                    .pointer("/process/external_native_close_sent")
                    .and_then(Value::as_bool)
                    != Some(false)
                || attempt.pointer("/process/signal").and_then(Value::as_i64) != Some(9)
                || attempt
                    .pointer("/process/exit_success")
                    .and_then(Value::as_bool)
                    != Some(false)
            {
                bail!(
                    "pre-alpha launch 1 was not terminated by the parent's external SIGKILL signal 9"
                );
            }
            let checkpoint = attempt
                .pointer("/process/checkpoint")
                .context("pre-alpha launch 1 lacks its synced provisional checkpoint")?;
            validate_pre_alpha_checkpoint(
                checkpoint,
                PRE_ALPHA_PROVISIONAL_CHECKPOINT_STAGE,
                expected_number,
            )?;
            if recovery_store_count != 1 {
                bail!(
                    "pre-alpha launch 1 did not leave exactly one canonical provisional recovery store"
                );
            }
        }
        2 => {
            if attempt
                .pointer("/process/external_sigkill_sent")
                .and_then(Value::as_bool)
                != Some(false)
                || attempt
                    .pointer("/process/external_native_close_sent")
                    .and_then(Value::as_bool)
                    != Some(false)
                || attempt
                    .pointer("/process/exit_success")
                    .and_then(Value::as_bool)
                    != Some(true)
                || !attempt
                    .pointer("/process/signal")
                    .is_some_and(Value::is_null)
            {
                bail!("pre-alpha recovery launch did not exit normally");
            }
            let automation = attempt
                .get("automation_report")
                .context("pre-alpha recovery launch lacks its automation report")?;
            let script_path = attempt
                .get("script")
                .and_then(Value::as_str)
                .map(Path::new)
                .context("pre-alpha recovery launch lacks its exact automation script path")?;
            let script = read_json_file(script_path)
                .context("pre-alpha recovery automation script could not be read")?;
            validate_product_automation_report_contract(automation, &script, script_path)
                .context("pre-alpha recovery automation report contract is invalid")?;
            validate_pre_alpha_recovery_script(&script)?;
            validate_pre_alpha_recovery_events(automation)?;
            qualifying_nonblank_viewport_capture(Some(automation)).map_err(anyhow::Error::msg)?;
            if automation
                .pointer("/project_store_evidence/close_result/status")
                .and_then(Value::as_str)
                != Some("succeeded")
                || automation
                    .pointer("/project_store_evidence/actor_join/status")
                    .and_then(Value::as_str)
                    != Some("succeeded")
            {
                bail!(
                    "pre-alpha recovery launch does not prove normal project-store Close and actor join"
                );
            }
            if recovery_store_count != 1 {
                bail!(
                    "pre-alpha recovery launch did not retain exactly one unchanged provisional recovery store"
                );
            }
        }
        3 => {
            if attempt
                .pointer("/process/external_sigkill_sent")
                .and_then(Value::as_bool)
                != Some(false)
                || attempt
                    .pointer("/process/external_native_close_sent")
                    .and_then(Value::as_bool)
                    != Some(true)
                || attempt
                    .pointer("/process/exit_success")
                    .and_then(Value::as_bool)
                    != Some(true)
                || !attempt
                    .pointer("/process/signal")
                    .is_some_and(Value::is_null)
            {
                bail!(
                    "pre-alpha clean-close launch did not exit successfully from the external mapped X11 close"
                );
            }
            let checkpoint = attempt
                .pointer("/process/checkpoint")
                .context("pre-alpha clean-close launch lacks its synced checkpoint")?;
            validate_pre_alpha_checkpoint(
                checkpoint,
                PRE_ALPHA_NATIVE_CLOSE_CHECKPOINT_STAGE,
                expected_number,
            )?;
            if recovery_store_count != 0 {
                bail!("pre-alpha clean-close launch unexpectedly created a recovery store");
            }
        }
        _ => bail!("pre-alpha reliability requires launch numbers 1 through 3"),
    }
    Ok(())
}

fn validate_pre_alpha_recovery_script(script: &Value) -> anyhow::Result<()> {
    let commands = script
        .get("commands")
        .and_then(Value::as_array)
        .context("pre-alpha recovery script lacks commands")?;
    let recovery_assertion = commands
        .iter()
        .find_map(|command| command.pointer("/condition/project_state"))
        .context("pre-alpha recovery script lacks its dirty recovery assertion")?;
    for (field, expected) in [
        ("bound", true),
        ("dirty", true),
        ("can_save", false),
        ("can_save_as", true),
        ("manual", false),
        ("autosave", true),
    ] {
        if recovery_assertion.get(field).and_then(Value::as_bool) != Some(expected) {
            bail!("pre-alpha recovery script project_state.{field} drifted");
        }
    }
    if recovery_assertion.get("lifecycle").and_then(Value::as_str) != Some("recovery_selected") {
        bail!("pre-alpha recovery script does not require recovery_selected lifecycle");
    }
    Ok(())
}

fn validate_pre_alpha_recovery_events(report: &Value) -> anyhow::Result<()> {
    let events = report
        .get("events")
        .and_then(Value::as_array)
        .context("pre-alpha recovery report lacks command events")?;
    let recovery_exposed = events.iter().any(|event| {
        event.get("command").and_then(Value::as_str) == Some("wait_for")
            && event.get("status").and_then(Value::as_str) == Some("passed")
            && event.pointer("/details/condition").and_then(Value::as_str)
                == Some("unsaved_autosave_recovery_exposed")
    });
    if !recovery_exposed {
        bail!("pre-alpha recovery report does not prove startup recovery exposure");
    }
    let normal_recovery_route = events.iter().any(|event| {
        event.get("command").and_then(Value::as_str) == Some("recover_exposed_unsaved_autosave")
            && event.get("status").and_then(Value::as_str) == Some("passed")
            && event
                .pointer("/details/startup_panel_was_open")
                .and_then(Value::as_bool)
                == Some(true)
            && event
                .pointer("/details/normal_reducer_service_path")
                .and_then(Value::as_bool)
                == Some(true)
            && event
                .pointer("/details/foreground_started_or_completed")
                .and_then(Value::as_bool)
                == Some(true)
    });
    if !normal_recovery_route {
        bail!(
            "pre-alpha recovery report does not prove the exposed locator used the normal application/service route"
        );
    }
    let recovery_state_asserted = events.iter().any(|event| {
        event.get("command").and_then(Value::as_str) == Some("assert")
            && event.get("status").and_then(Value::as_str) == Some("passed")
            && event.pointer("/details/condition").and_then(Value::as_str) == Some("project_state")
    });
    if !recovery_state_asserted {
        bail!("pre-alpha recovery report lacks its passing dirty-state assertion");
    }
    Ok(())
}

fn validate_pre_alpha_attempts(attempts: &[Value]) -> anyhow::Result<()> {
    if attempts.len() != 3 {
        bail!("pre-alpha reliability requires exactly three retained launch attempts");
    }
    for (index, attempt) in attempts.iter().enumerate() {
        let expected_number = u64::try_from(index + 1).expect("three attempts fit u64");
        if attempt.get("status").and_then(Value::as_str) != Some("passed") {
            bail!("pre-alpha launch {expected_number} is not passed");
        }
        validate_pre_alpha_attempt(attempt, expected_number)?;
    }
    if attempts
        .iter()
        .any(|attempt| attempt.get("retry_index").and_then(Value::as_u64) != Some(0))
    {
        bail!("pre-alpha reliability observed an automatic retry");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProductAutomationExternalControlPlan {
    None,
    PresentationProbe { minimize_command_index: usize },
}

impl ProductAutomationExternalControlPlan {
    fn from_script(scenario: &str, script: &Value) -> anyhow::Result<Self> {
        if scenario != REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO {
            return Ok(Self::None);
        }
        let commands = script
            .get("commands")
            .and_then(Value::as_array)
            .context("presentation probe script commands are unavailable")?;
        let mut minimize_command_index = None;
        for (index, command) in commands.iter().enumerate() {
            if command.get("command").and_then(Value::as_str) != Some("set_window_minimized") {
                continue;
            }
            match command.get("minimized").and_then(Value::as_bool) {
                Some(true) if minimize_command_index.replace(index).is_none() => {}
                Some(true) => bail!("presentation probe requires exactly one minimize request"),
                Some(false) => bail!(
                    "presentation probe restoration must be owned by the external X11 controller"
                ),
                None => bail!("presentation probe minimize request is malformed"),
            }
        }
        Ok(Self::PresentationProbe {
            minimize_command_index: minimize_command_index
                .context("presentation probe requires one minimize request")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct X11ClientWindow {
    id_hex: String,
    id_decimal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum X11ClientWindowDiscovery {
    Found(X11ClientWindow),
    NotFound,
    ListingUnavailable { status: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct X11WindowManagerState {
    hidden: bool,
    iconic: bool,
}

impl X11WindowManagerState {
    fn minimized(self) -> bool {
        self.hidden && self.iconic
    }

    fn restored(self) -> bool {
        !self.hidden && !self.iconic
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentationProbeWindowPhase {
    WaitingForMinimizeCommand,
    WaitingForMinimized { armed_at: Instant },
    HoldingMinimized { observed_at: Instant },
    RestoreRequested { requested_at: Instant },
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentationProbeWindowDecision {
    None,
    Unmap,
    Restore,
}

#[derive(Debug)]
struct PresentationProbeWindowState {
    minimize_command_index: usize,
    phase: PresentationProbeWindowPhase,
}

impl PresentationProbeWindowState {
    fn new(minimize_command_index: usize) -> Self {
        Self {
            minimize_command_index,
            phase: PresentationProbeWindowPhase::WaitingForMinimizeCommand,
        }
    }

    fn observe_progress(&mut self, snapshot: &SafeProgressSnapshot, now: Instant) {
        let SafeProgressState::Command { index, .. } = &snapshot.state else {
            return;
        };
        if matches!(
            self.phase,
            PresentationProbeWindowPhase::WaitingForMinimizeCommand
        ) && *index >= self.minimize_command_index
        {
            self.phase = PresentationProbeWindowPhase::WaitingForMinimized { armed_at: now };
        }
    }

    fn observe_window(
        &mut self,
        state: X11WindowManagerState,
        now: Instant,
    ) -> PresentationProbeWindowDecision {
        match self.phase {
            PresentationProbeWindowPhase::WaitingForMinimizeCommand
            | PresentationProbeWindowPhase::Complete => PresentationProbeWindowDecision::None,
            PresentationProbeWindowPhase::WaitingForMinimized { .. } if state.minimized() => {
                self.phase = PresentationProbeWindowPhase::HoldingMinimized { observed_at: now };
                PresentationProbeWindowDecision::Unmap
            }
            PresentationProbeWindowPhase::HoldingMinimized { observed_at }
                if now.saturating_duration_since(observed_at)
                    >= PRESENTATION_PROBE_MINIMIZED_HOLD =>
            {
                self.phase = PresentationProbeWindowPhase::RestoreRequested { requested_at: now };
                PresentationProbeWindowDecision::Restore
            }
            PresentationProbeWindowPhase::RestoreRequested { .. } if state.restored() => {
                self.phase = PresentationProbeWindowPhase::Complete;
                PresentationProbeWindowDecision::None
            }
            _ => PresentationProbeWindowDecision::None,
        }
    }

    fn deadline_failure(&self, now: Instant) -> Option<&'static str> {
        match self.phase {
            PresentationProbeWindowPhase::WaitingForMinimized { armed_at }
                if now.saturating_duration_since(armed_at)
                    >= PRESENTATION_PROBE_WINDOW_CONTROL_TIMEOUT =>
            {
                Some("the external controller did not observe the requested minimized window")
            }
            PresentationProbeWindowPhase::RestoreRequested { requested_at }
                if now.saturating_duration_since(requested_at)
                    >= PRESENTATION_PROBE_WINDOW_CONTROL_TIMEOUT =>
            {
                Some("the externally restored window did not return to normal state")
            }
            _ => None,
        }
    }

    fn finish(&self) -> anyhow::Result<()> {
        if self.phase != PresentationProbeWindowPhase::Complete {
            bail!("presentation probe exited before external minimize/unmap/restore completed");
        }
        Ok(())
    }
}

#[derive(Debug)]
enum ProductAutomationExternalControl {
    None,
    PresentationProbe {
        state: PresentationProbeWindowState,
        window: Option<X11ClientWindow>,
        last_window_listing_failure: Option<String>,
    },
}

impl ProductAutomationExternalControl {
    fn new(plan: ProductAutomationExternalControlPlan) -> anyhow::Result<Self> {
        match plan {
            ProductAutomationExternalControlPlan::None => Ok(Self::None),
            ProductAutomationExternalControlPlan::PresentationProbe {
                minimize_command_index,
            } => {
                require_presentation_probe_x11_tools()?;
                Ok(Self::PresentationProbe {
                    state: PresentationProbeWindowState::new(minimize_command_index),
                    window: None,
                    last_window_listing_failure: None,
                })
            }
        }
    }

    fn observe_progress(&mut self, snapshot: &SafeProgressSnapshot, now: Instant) {
        if let Self::PresentationProbe { state, .. } = self {
            state.observe_progress(snapshot, now);
        }
    }

    fn poll(&mut self, pid: u32, now: Instant) -> anyhow::Result<()> {
        let Self::PresentationProbe {
            state,
            window,
            last_window_listing_failure,
        } = self
        else {
            return Ok(());
        };
        if let Some(reason) = state.deadline_failure(now) {
            if let Some(failure) = last_window_listing_failure.as_deref() {
                bail!("{reason}; most recent wmctrl listing failure: {failure}");
            }
            bail!(reason);
        }
        if window.is_none() {
            match find_x11_client_window(pid)? {
                X11ClientWindowDiscovery::Found(discovered) => {
                    *window = Some(discovered);
                    *last_window_listing_failure = None;
                }
                X11ClientWindowDiscovery::NotFound => {
                    *last_window_listing_failure = None;
                }
                X11ClientWindowDiscovery::ListingUnavailable { status } => {
                    *last_window_listing_failure = Some(status);
                    return Ok(());
                }
            }
        }
        let Some(window) = window.as_ref() else {
            return Ok(());
        };
        let Some(window_state) = inspect_x11_window_manager_state(window)? else {
            return Ok(());
        };
        match state.observe_window(window_state, now) {
            PresentationProbeWindowDecision::None => {}
            PresentationProbeWindowDecision::Unmap => request_x11_window_unmap(window)?,
            PresentationProbeWindowDecision::Restore => {
                request_x11_window_restore(window)?;
                let restored = inspect_x11_window_manager_state(window)?.context(
                    "restored presentation-probe window disappeared before state confirmation",
                )?;
                state.observe_window(restored, Instant::now());
                if !matches!(state.phase, PresentationProbeWindowPhase::Complete) {
                    bail!("external X11 restore returned without normal window state");
                }
                eprintln!(
                    "automation_window_control scope=product_validate scenario={} action=minimize_unmap_restore status=completed",
                    REPRESENTATIVE_GPU_PRESENTATION_PROBE_SCENARIO,
                );
            }
        }
        Ok(())
    }

    fn finish(&self) -> anyhow::Result<()> {
        match self {
            Self::None => Ok(()),
            Self::PresentationProbe { state, .. } => state.finish(),
        }
    }
}

fn require_presentation_probe_x11_tools() -> anyhow::Result<()> {
    if env::var_os("DISPLAY").is_none() {
        bail!("presentation probe external window control requires a real X11 display");
    }
    let observer_report = env::var_os(PRESENTATION_OBSERVER_REPORT_ENV)
        .map(PathBuf::from)
        .context("presentation probe requires an independent presentation observer report path")?;
    if !observer_report.is_absolute() {
        bail!("{PRESENTATION_OBSERVER_REPORT_ENV} must be absolute");
    }
    for (program, argument) in [
        ("xdotool", "version"),
        ("xprop", "-version"),
        ("wmctrl", "-h"),
    ] {
        let mut command = Command::new(program);
        command.arg(argument);
        run_command_with_bounded_output(&mut command, X11_AUTOMATION_OUTPUT_POLICY)
            .with_context(|| format!("presentation probe requires {program}"))?;
    }
    Ok(())
}

fn find_x11_client_window(pid: u32) -> anyhow::Result<X11ClientWindowDiscovery> {
    let mut command = Command::new("wmctrl");
    command.args(["-l", "-p"]);
    let output = run_command_with_bounded_output(&mut command, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to list X11 client windows")?;
    classify_wmctrl_client_window_listing(
        output.status.success(),
        &output.status.to_string(),
        &output.stdout,
        pid,
    )
}

fn classify_wmctrl_client_window_listing(
    status_success: bool,
    status: &str,
    stdout: &[u8],
    pid: u32,
) -> anyhow::Result<X11ClientWindowDiscovery> {
    if !status_success {
        return Ok(X11ClientWindowDiscovery::ListingUnavailable {
            status: status.to_owned(),
        });
    }
    let encoded = std::str::from_utf8(stdout).context("wmctrl output was not UTF-8")?;
    Ok(match parse_wmctrl_client_window(encoded, pid)? {
        Some(window) => X11ClientWindowDiscovery::Found(window),
        None => X11ClientWindowDiscovery::NotFound,
    })
}

fn parse_wmctrl_client_window(output: &str, pid: u32) -> anyhow::Result<Option<X11ClientWindow>> {
    let mut matches = Vec::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let Some(id_hex) = fields.next() else {
            continue;
        };
        let _desktop = fields.next();
        let Some(raw_pid) = fields.next() else {
            continue;
        };
        if raw_pid.parse::<u32>().ok() != Some(pid) {
            continue;
        }
        let encoded_id = id_hex
            .strip_prefix("0x")
            .context("wmctrl returned a non-hexadecimal X11 window ID")?;
        let id = u64::from_str_radix(encoded_id, 16)
            .context("wmctrl returned an invalid X11 window ID")?;
        matches.push(X11ClientWindow {
            id_hex: format!("0x{id:x}"),
            id_decimal: id.to_string(),
        });
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        count => bail!("presentation probe found {count} X11 client windows for one app process"),
    }
}

fn inspect_x11_window_manager_state(
    window: &X11ClientWindow,
) -> anyhow::Result<Option<X11WindowManagerState>> {
    let mut command = Command::new("xprop");
    command.args(["-id", &window.id_hex, "_NET_WM_STATE", "WM_STATE"]);
    let output = run_command_with_bounded_output(&mut command, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to inspect X11 window-manager state")?;
    if !output.status.success() {
        return Ok(None);
    }
    let encoded = String::from_utf8(output.stdout).context("xprop output was not UTF-8")?;
    parse_xprop_window_manager_state(&encoded).map(Some)
}

fn parse_xprop_window_manager_state(output: &str) -> anyhow::Result<X11WindowManagerState> {
    if !output.contains("_NET_WM_STATE") || !output.contains("window state:") {
        bail!("xprop output omitted required window-manager state");
    }
    Ok(X11WindowManagerState {
        hidden: output.contains("_NET_WM_STATE_HIDDEN"),
        iconic: output
            .lines()
            .map(str::trim)
            .any(|line| line == "window state: Iconic"),
    })
}

fn request_x11_window_unmap(window: &X11ClientWindow) -> anyhow::Result<()> {
    let mut command = Command::new("xdotool");
    command.args(["windowunmap", "--sync", &window.id_decimal]);
    let output = run_command_with_bounded_output(&mut command, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to unmap the minimized presentation-probe window")?;
    if !output.status.success() {
        bail!("xdotool window unmap failed with {}", output.status);
    }
    Ok(())
}

fn request_x11_window_restore(window: &X11ClientWindow) -> anyhow::Result<()> {
    let mut map = Command::new("xdotool");
    map.args(["windowmap", "--sync", &window.id_decimal]);
    let output = run_command_with_bounded_output(&mut map, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to remap the presentation-probe window")?;
    if !output.status.success() {
        bail!("xdotool window map failed with {}", output.status);
    }
    let mut restore = Command::new("wmctrl");
    restore.args(["-i", "-R", &window.id_hex]);
    let output = run_command_with_bounded_output(&mut restore, X11_AUTOMATION_OUTPUT_POLICY)
        .context("failed to restore the presentation-probe window")?;
    if !output.status.success() {
        bail!("wmctrl window restore failed with {}", output.status);
    }
    Ok(())
}

struct ProductAutomationRun<'a> {
    binary: &'a Path,
    package: &'a Path,
    script: &'a Path,
    automation_report: &'a Path,
    stdout_path: &'a Path,
    stderr_path: &'a Path,
    runtime_log_path: &'a Path,
    timeout: Duration,
    scenario: &'a str,
    progress_plan: ProductAutomationProgressPlan,
    progress_launch: ProductAutomationProgressLaunch,
    external_control: ProductAutomationExternalControlPlan,
}

fn run_product_automation(run: ProductAutomationRun<'_>) -> anyhow::Result<ProductProcessStatus> {
    let mut external_control = ProductAutomationExternalControl::new(run.external_control)?;
    let stdout = fs::File::create(run.stdout_path)
        .with_context(|| format!("failed to create {}", run.stdout_path.display()))?;
    let stderr = fs::File::create(run.stderr_path)
        .with_context(|| format!("failed to create {}", run.stderr_path.display()))?;
    let mut command = Command::new(run.binary);
    command
        .env("MIRANTE4D_DEV_DATASET", run.package)
        .env("MIRANTE4D_ENABLE_AUTOMATION", "1")
        .env("MIRANTE4D_AUTOMATION_SCRIPT", run.script)
        .env("MIRANTE4D_AUTOMATION_REPORT", run.automation_report)
        .env("MIRANTE4D_LOG_FILE", run.runtime_log_path)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    isolate_process_tree(&mut command);
    run.progress_launch.apply_to_command(&mut command);
    println!(
        "running normal-app product validation scenario {:?}",
        run.scenario
    );
    let started = Instant::now();
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch native app product validation binary {}",
            run.binary.display()
        )
    })?;
    let deadline = started + run.timeout;
    let mut progress_monitor = run.progress_launch.monitor(run.progress_plan, started);
    let mut next_progress_poll = started;
    loop {
        let now = Instant::now();
        if let Some(exit_status) = child
            .try_wait()
            .context("failed to poll product validation child process")?
        {
            let external_control_failure_reason = external_control
                .finish()
                .err()
                .map(|error| error.to_string());
            return Ok(ProductProcessStatus {
                timed_out: false,
                exit_status: Some(exit_status.to_string()),
                exit_success: Some(exit_status.success()),
                progress_failure_reason: progress_monitor
                    .finalize_at_exit(now)
                    .map(|failure| failure.reason_code()),
                external_control_failure_reason,
            });
        }
        if now >= next_progress_poll {
            match progress_monitor.poll_at(now) {
                ProgressMonitorAction::Continue => {}
                ProgressMonitorAction::Emit(snapshot) => {
                    external_control.observe_progress(&snapshot, now);
                    let line =
                        safe_automation_progress_line("product_validate", run.scenario, &snapshot)?;
                    eprintln!("{line}");
                }
                ProgressMonitorAction::Terminate(failure) => {
                    terminate_process_tree(&mut child);
                    let exit_status = child.wait().ok();
                    return Ok(ProductProcessStatus {
                        timed_out: false,
                        exit_status: exit_status.map(|status| status.to_string()),
                        exit_success: exit_status.map(|status| status.success()),
                        progress_failure_reason: Some(failure.reason_code()),
                        external_control_failure_reason: None,
                    });
                }
            }
            next_progress_poll = now + FILE_POLL_INTERVAL;
        }
        if let Err(error) = external_control.poll(child.id(), now) {
            terminate_process_tree(&mut child);
            let exit_status = child.wait().ok();
            return Ok(ProductProcessStatus {
                timed_out: false,
                exit_status: exit_status.map(|status| status.to_string()),
                exit_success: exit_status.map(|status| status.success()),
                progress_failure_reason: None,
                external_control_failure_reason: Some(error.to_string()),
            });
        }
        if now >= deadline {
            terminate_process_tree(&mut child);
            let exit_status = child.wait().ok().map(|status| status.to_string());
            return Ok(ProductProcessStatus {
                timed_out: true,
                exit_status,
                exit_success: None,
                progress_failure_reason: None,
                external_control_failure_reason: None,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug)]
struct ProductProcessStatus {
    timed_out: bool,
    exit_status: Option<String>,
    exit_success: Option<bool>,
    progress_failure_reason: Option<&'static str>,
    external_control_failure_reason: Option<String>,
}

struct WrapperReport<'a> {
    path: &'a Path,
    scenario_name: &'a str,
    status: ProductValidationStatus,
    failure_reason: Option<String>,
    started_at_epoch_ms: u128,
    duration_ms: f64,
    timeout_secs: u64,
    package: &'a Path,
    binary: &'a Path,
    script: &'a Path,
    script_value: &'a Value,
    automation_report: &'a Path,
    automation_report_value: Option<&'a Value>,
    stdout: &'a Path,
    stderr: &'a Path,
    runtime_log: &'a Path,
    display: DisplayClassification,
    preflight_only: bool,
    source_closure_evidence: Value,
    automation_status: Option<String>,
    exit_status: Option<String>,
    exit_success: Option<bool>,
}

fn write_wrapper_report(report: WrapperReport<'_>) -> anyhow::Result<()> {
    let path = report.path.to_path_buf();
    let value = wrapper_report_json(report);
    write_json_file(&path, &value)
}

fn wrapper_report_json(report: WrapperReport<'_>) -> Value {
    let host = benchmark_host_context();
    let git_commit = host.get("git_commit").cloned().unwrap_or(Value::Null);
    let dirty_worktree = host.get("dirty_worktree").cloned().unwrap_or(Value::Null);
    let finished_at_epoch_ms = epoch_ms();
    let automation_artifacts = report
        .automation_report_value
        .and_then(|value| value.get("artifacts"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let gpu_adapter = report
        .automation_report_value
        .and_then(|value| value.get("final_diagnostics"))
        .and_then(|value| {
            value
                .get("gpu_adapter")
                .or_else(|| value.get("render").and_then(|render| render.get("adapter")))
        })
        .cloned()
        .unwrap_or(Value::Null);
    let dataset_runtime_metrics =
        product_validation_dataset_runtime_metrics(report.automation_report_value);
    let lease_bridge_metrics =
        product_validation_lease_bridge_metrics(report.automation_report_value);
    let cross_section_panel_metrics =
        product_validation_cross_section_panel_metrics(report.automation_report_value);
    let automation_script_scenario = report
        .script_value
        .get("scenario")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let scenario_name = report.scenario_name;
    let claim_source = "instrumented_application_commands_internal_state_and_readback";
    let pixel_content_observed = Value::Null;
    let b3_e1_capture_evidence = if scenario_name == B3_PACKAGE_INTEGRITY_AUDIT_SCENARIO {
        match b3_exact_e1_capture_evidence(report.automation_report_value) {
            Ok(evidence) => evidence,
            Err(reason) => json!({
                "required": true,
                "accepted": false,
                "evidence_level": "E1",
                "evidence_scope": "automation_only_internal_gpu_render_target_readback",
                "e4_product_open_satisfied": false,
                "failure_reason": reason,
                "captures": [],
            }),
        }
    } else {
        Value::Null
    };
    let import_evidence = if scenario_name == IMPORT_PREPROCESSING_SCENARIO {
        match import_preprocessing_evidence(report.automation_report_value) {
            Ok(evidence) => json!({
                "required": true,
                "accepted": true,
                "workflow": evidence,
            }),
            Err(reason) => json!({
                "required": true,
                "accepted": false,
                "failure_reason": reason,
            }),
        }
    } else {
        Value::Null
    };
    let requested_window_inner_size_points =
        script_requested_window_inner_size_points_json(report.script_value);
    let pixels_per_point = report
        .automation_report_value
        .and_then(|value| value.pointer("/viewport_evidence/pixels_per_point"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(Value::from)
        .unwrap_or(Value::Null);
    let render_target_pixels = qualifying_nonblank_viewport_capture(report.automation_report_value)
        .map(|artifact| {
            json!({
                "width": artifact.get("width").and_then(Value::as_u64),
                "height": artifact.get("height").and_then(Value::as_u64),
            })
        })
        .unwrap_or(Value::Null);
    let render_modes = script_render_modes_json(report.script_value);
    let max_cpu_total_bytes = script_limit_u64(report.script_value, "max_cpu_total_bytes");
    let cpu_category_byte_limits = json!({
        "decoded_residency": script_limit_u64(report.script_value, "max_cpu_decoded_residency_bytes"),
        "upload_staging": script_limit_u64(report.script_value, "max_cpu_upload_staging_bytes"),
        "in_flight_decode": script_limit_u64(report.script_value, "max_cpu_in_flight_decode_bytes"),
        "metadata_and_indexes": script_limit_u64(report.script_value, "max_cpu_metadata_and_indexes_bytes"),
        "queues_and_results": script_limit_u64(report.script_value, "max_cpu_queues_and_results_bytes"),
        "prefetch": script_limit_u64(report.script_value, "max_cpu_prefetch_bytes"),
        "import_working_set": script_limit_u64(report.script_value, "max_cpu_import_working_set_bytes"),
    });
    let runtime_work_limits = json!({
        "queued_requests": script_limit_u64(report.script_value, "max_runtime_queued_requests"),
        "in_flight_decodes": script_limit_u64(report.script_value, "max_runtime_in_flight_decodes"),
        "pending_completions": script_limit_u64(report.script_value, "max_runtime_pending_completions"),
        "resident_resources": script_limit_u64(report.script_value, "max_runtime_resident_resources"),
    });
    let cpu_byte_limit_enforced = script_has_any_limit(
        report.script_value,
        &[
            "max_cpu_total_bytes",
            "max_cpu_decoded_residency_bytes",
            "max_cpu_upload_staging_bytes",
            "max_cpu_in_flight_decode_bytes",
            "max_cpu_metadata_and_indexes_bytes",
            "max_cpu_queues_and_results_bytes",
            "max_cpu_prefetch_bytes",
            "max_cpu_import_working_set_bytes",
        ],
    );
    let runtime_work_limit_enforced = script_has_any_limit(
        report.script_value,
        &[
            "max_runtime_queued_requests",
            "max_runtime_in_flight_decodes",
            "max_runtime_pending_completions",
            "max_runtime_resident_resources",
        ],
    );
    json!({
        "schema": PRODUCT_VALIDATION_SCHEMA,
        "schema_version": PRODUCT_VALIDATION_SCHEMA_VERSION,
        "command": "product-validate",
        "evidence_level": "E1",
        "claim_boundary": {
            "evidence_type": "internal_native_window_product_automation",
            "source": claim_source,
            "pixel_content_observed": pixel_content_observed,
            "closure_authority": "integration_support_only_not_black_box_product_open",
            "e4_product_open_satisfied": false,
        },
        "status": report.status.name(),
        "failure_reason": report.failure_reason,
        "started_at_epoch_ms": report.started_at_epoch_ms,
        "started_at_utc": unix_epoch_ms_to_utc_rfc3339(report.started_at_epoch_ms),
        "finished_at_epoch_ms": finished_at_epoch_ms,
        "finished_at_utc": unix_epoch_ms_to_utc_rfc3339(finished_at_epoch_ms),
        "duration_ms": report.duration_ms,
        "git_commit": git_commit,
        "dirty_worktree": dirty_worktree,
        "build_profile": "release",
        "binary": report.binary,
        "host": host,
        "gpu_adapter": gpu_adapter,
        "dataset": dataset_context_json(report.package),
        "scenario": {
            "name": scenario_name,
            "automation_script_scenario": automation_script_scenario,
            "automation_script": report.script,
            "automation_status": report.automation_status,
            "requested_window_inner_size_points": requested_window_inner_size_points,
            "pixels_per_point": pixels_per_point,
            "render_target_pixels": render_target_pixels,
            "b3_exact_e1_capture_evidence": b3_e1_capture_evidence,
            "import_preprocessing_evidence": import_evidence,
            "render_modes": render_modes,
        },
        "limits": {
            "timeout_secs": report.timeout_secs,
            "cpu_total_byte_limit_bytes": max_cpu_total_bytes,
            "cpu_category_byte_limits": cpu_category_byte_limits,
            "runtime_work_limits": runtime_work_limits,
            "cpu_byte_limit_enforced": cpu_byte_limit_enforced,
            "runtime_work_limit_enforced": runtime_work_limit_enforced,
        },
        "metrics": {
            "duration_ms": report.duration_ms,
            "dataset_runtime": dataset_runtime_metrics,
            "lease_bridge": lease_bridge_metrics,
            "cross_section_panels": cross_section_panel_metrics,
        },
        "artifacts": {
            "automation_report": report.automation_report,
            "automation_artifacts": automation_artifacts,
            "stdout": report.stdout,
            "stderr": report.stderr,
            "runtime_log": report.runtime_log,
        },
        "logs": {
            "stdout": report.stdout,
            "stderr": report.stderr,
            "runtime": report.runtime_log,
        },
        "environment": {
            "display": report.display.class.name(),
            "display_class": report.display.class.name(),
            "display_class_source": report.display.source,
            "display_env_present": env::var_os("DISPLAY").is_some(),
            "wayland_display_env_present": env::var_os("WAYLAND_DISPLAY").is_some(),
            "display_class_override_env": env::var(DISPLAY_CLASS_ENV).ok(),
            "product_validate_preflight_only_env": env::var(PREFLIGHT_ONLY_ENV).ok(),
            "product_validate_preflight_only": report.preflight_only,
        },
        "process": {
            "exit_status": report.exit_status,
            "exit_success": report.exit_success,
        },
        "source_closure_evidence": report.source_closure_evidence,
    })
}

fn product_validation_dataset_runtime_metrics(automation_report_value: Option<&Value>) -> Value {
    let final_snapshot = automation_report_value
        .and_then(|value| value.get("final_diagnostics"))
        .and_then(|value| value.get("dataset_runtime"))
        .cloned()
        .unwrap_or(Value::Null);
    let snapshots = automation_report_value
        .and_then(|value| value.get("diagnostics"))
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|diagnostics| diagnostics.get("dataset_runtime").cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "kind": "dataset_runtime_metrics",
        "taxonomy_version": 1,
        "snapshot_source": "automation_copy_diagnostics_and_final_diagnostics",
        "snapshot_count": snapshots.len(),
        "final": final_snapshot,
        "latest": snapshots.last().cloned().unwrap_or(Value::Null),
    })
}

fn product_validation_lease_bridge_metrics(automation_report_value: Option<&Value>) -> Value {
    let final_snapshot = automation_report_value
        .and_then(|value| value.get("final_diagnostics"))
        .and_then(|value| value.get("lease_bridge"))
        .cloned()
        .unwrap_or(Value::Null);
    let snapshots = automation_report_value
        .and_then(|value| value.get("diagnostics"))
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|diagnostics| diagnostics.get("lease_bridge").cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "kind": "lease_bridge_metrics",
        "taxonomy_version": 1,
        "snapshot_source": "automation_copy_diagnostics_and_final_diagnostics",
        "snapshot_count": snapshots.len(),
        "final": final_snapshot,
        "latest": snapshots.last().cloned().unwrap_or(Value::Null),
    })
}

fn product_validation_cross_section_panel_metrics(
    automation_report_value: Option<&Value>,
) -> Value {
    let final_snapshot = automation_report_value
        .and_then(|value| value.get("final_diagnostics"))
        .and_then(|value| value.get("cross_section"))
        .cloned()
        .unwrap_or(Value::Null);
    let snapshots = automation_report_value
        .and_then(|value| value.get("events"))
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    event
                        .get("details")
                        .and_then(|details| details.get("cross_section_snapshot"))
                        .cloned()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "kind": "cross_section_panel_metrics",
        "taxonomy_version": 1,
        "snapshot_source": "automation_assertion_details_and_final_diagnostics",
        "snapshot_count": snapshots.len(),
        "final": final_snapshot,
        "latest_assertion": snapshots.last().cloned().unwrap_or(Value::Null),
    })
}

fn script_limit_u64(script: &Value, name: &str) -> Value {
    script
        .get("hard_safety_limits")
        .and_then(|limits| limits.get(name))
        .and_then(Value::as_u64)
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn script_has_any_limit(script: &Value, names: &[&str]) -> bool {
    names.iter().any(|name| {
        script
            .get("hard_safety_limits")
            .and_then(|limits| limits.get(name))
            .and_then(Value::as_u64)
            .is_some()
    })
}

fn script_requested_window_inner_size_points_json(script: &Value) -> Value {
    script_commands(script)
        .and_then(|commands| {
            commands.iter().find_map(|command| {
                if command.get("command").and_then(Value::as_str) != Some("set_viewport_size") {
                    return None;
                }
                Some(json!({
                    "width": command.get("width").and_then(Value::as_u64),
                    "height": command.get("height").and_then(Value::as_u64),
                }))
            })
        })
        .unwrap_or(Value::Null)
}

fn script_render_modes_json(script: &Value) -> Value {
    let mut modes = Vec::new();
    if let Some(commands) = script_commands(script) {
        for command in commands {
            if command.get("command").and_then(Value::as_str) == Some("set_render_mode")
                && let Some(mode) = command.get("mode").and_then(Value::as_str)
                && !modes.iter().any(|existing: &String| existing == mode)
            {
                modes.push(mode.to_owned());
            }
        }
    }
    if modes.is_empty() {
        Value::Null
    } else {
        json!(modes)
    }
}

fn script_commands(script: &Value) -> Option<&Vec<Value>> {
    script.get("commands").and_then(Value::as_array)
}

fn dataset_context_json(package: &Path) -> Value {
    match LocalPackageCatalog::open(package) {
        Ok(catalog) => {
            let active_layer = catalog.science().layers().first().map(|layer| {
                let shape = layer.base_shape();
                let scale_count = catalog
                    .profile()
                    .images()
                    .iter()
                    .find(|image| {
                        image
                            .logical_layers()
                            .iter()
                            .any(|candidate| candidate.logical_layer() == layer.logical_layer())
                    })
                    .map_or(0, |image| image.levels().len());
                json!({
                    "logical_layer": layer.logical_layer().ordinal(),
                    "shape": {
                        "t": shape.t(),
                        "z": shape.z(),
                        "y": shape.y(),
                        "x": shape.x(),
                    },
                    "dtype": format!("{:?}", layer.dtype()),
                    "scale_count": scale_count,
                    "timepoint_count": shape.t(),
                })
            });
            let timepoint_count = active_layer
                .as_ref()
                .and_then(|layer| layer.get("timepoint_count"))
                .cloned();
            json!({
                "package_path": package,
                "manifest_status": "loaded",
                "format_family": mirante4d_storage::PROFILE.format_family,
                "semantic_schema": mirante4d_storage::PROFILE.semantic_schema,
                "package_id": catalog.declared_package_id().to_string(),
                "scientific_content_id": catalog.science().scientific_content_id().to_string(),
                "layer_count": catalog.science().layers().len(),
                "active_layer": active_layer,
                "timepoint_count": timepoint_count,
            })
        }
        Err(err) => json!({
            "package_path": package,
            "manifest_status": "load_failed",
            "error": err.to_string(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProductValidationAppBinary {
    path: PathBuf,
    overridden: bool,
}

impl ProductValidationAppBinary {
    fn from_environment() -> anyhow::Result<Self> {
        Self::resolve(env::var_os(APP_BINARY_ENV).map(PathBuf::from))
    }

    fn resolve(override_path: Option<PathBuf>) -> anyhow::Result<Self> {
        match override_path {
            Some(path) => {
                validate_app_binary_file(&path, APP_BINARY_ENV)?;
                Ok(Self {
                    path,
                    overridden: true,
                })
            }
            None => Ok(Self {
                path: release_app_binary(),
                overridden: false,
            }),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn should_build_default_release(&self) -> bool {
        !self.overridden && !env_flag(SKIP_RELEASE_BUILD_ENV)
    }

    fn validate_for_launch(&self) -> anyhow::Result<()> {
        let description = if self.overridden {
            APP_BINARY_ENV
        } else {
            "release app binary"
        };
        validate_app_binary_file(&self.path, description).with_context(|| {
            if self.overridden {
                format!("set {APP_BINARY_ENV} to an existing packaged executable")
            } else {
                "run cargo build --release -p mirante4d-app".to_owned()
            }
        })
    }
}

fn validate_app_binary_file(path: &Path, description: &str) -> anyhow::Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {description} at {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{description} is not a file at {}", path.display());
    }
    Ok(())
}

fn release_app_binary() -> PathBuf {
    Path::new("target")
        .join("release")
        .join(format!("mirante4d-app{}", env::consts::EXE_SUFFIX))
}

fn timeout_secs(scenario: &ProductValidationScenario) -> u64 {
    env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or_else(|| scenario.default_timeout_secs())
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayClassification {
    class: DisplayClass,
    source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayClass {
    RealDisplay,
    VirtualDisplay,
    Unsupported,
}

impl DisplayClass {
    fn name(self) -> &'static str {
        match self {
            Self::RealDisplay => "real_display",
            Self::VirtualDisplay => "virtual_display",
            Self::Unsupported => "unsupported",
        }
    }
}

fn display_status() -> DisplayClassification {
    classify_display(
        env::var_os("DISPLAY").is_some(),
        env::var_os("WAYLAND_DISPLAY").is_some(),
        env::var(DISPLAY_CLASS_ENV).ok().as_deref(),
        env_flag("CI") || env_flag("GITHUB_ACTIONS"),
    )
}

fn classify_display(
    display_env_present: bool,
    wayland_display_env_present: bool,
    explicit_class: Option<&str>,
    ci_env_present: bool,
) -> DisplayClassification {
    if !display_env_present && !wayland_display_env_present {
        return DisplayClassification {
            class: DisplayClass::Unsupported,
            source: "no_display_environment",
        };
    }
    match explicit_class {
        Some("real_display") => {
            return DisplayClassification {
                class: DisplayClass::RealDisplay,
                source: DISPLAY_CLASS_ENV,
            };
        }
        Some("virtual_display") => {
            return DisplayClassification {
                class: DisplayClass::VirtualDisplay,
                source: DISPLAY_CLASS_ENV,
            };
        }
        Some(_) | None => {}
    }
    if ci_env_present && display_env_present && !wayland_display_env_present {
        return DisplayClassification {
            class: DisplayClass::VirtualDisplay,
            source: "ci_x11_heuristic",
        };
    }
    DisplayClassification {
        class: DisplayClass::RealDisplay,
        source: "display_environment_heuristic",
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn unix_epoch_ms_to_utc_rfc3339(epoch_ms: u128) -> String {
    let seconds = (epoch_ms / 1000) as i64;
    let millis = (epoch_ms % 1000) as u32;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_unix_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_unix_days(days: i64) -> (i32, u32, u32) {
    // Inverse of days-from-civil for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests;
