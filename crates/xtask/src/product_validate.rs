use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_format::load_and_validate_dataset_quick;
use serde_json::{Value, json};

use crate::{
    fixtures::generate_fixture,
    host::benchmark_host_context,
    process::{run_cargo, with_heavy_benchmark_guard},
    reports::{read_json_file, write_json_file},
};

const PRODUCT_VALIDATION_SCHEMA: &str = "mirante4d-product-validation-report";
const PRODUCT_AUTOMATION_SCRIPT_SCHEMA: &str = "mirante4d-product-automation-script";
const PRODUCT_AUTOMATION_SCHEMA_VERSION: u32 = 1;
const PRODUCT_VALIDATION_SCHEMA_VERSION: u32 = 1;
const DEFAULT_FIXTURE: &str = "basic-u16-16cube";
const RENDER_MODES_FIXTURE: &str = "time-multichannel-u16-8cube-3t-2c";
const OUTPUT_DIR: &str = "target/mirante4d/product-validation";
const TIMEOUT_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS";
const ALLOW_NO_DISPLAY_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_ALLOW_NO_DISPLAY";
const SKIP_RELEASE_BUILD_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD";
const DISPLAY_CLASS_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS";
const SCENARIO_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_SCENARIO";
const CUSTOM_SCRIPT_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_SCRIPT";
const PREFLIGHT_ONLY_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_PREFLIGHT_ONLY";
const PRODUCT_VALIDATE_GPU_TIMESTAMPS_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_GPU_TIMESTAMPS";
const PRODUCT_VALIDATE_MAX_RSS_BYTES_ENV: &str = "MIRANTE4D_PRODUCT_VALIDATE_MAX_RSS_BYTES";
const APP_GPU_TIMESTAMPS_ENV: &str = "MIRANTE4D_GPU_TIMESTAMPS";
const GENERATED_FIXTURE_SCENARIO: &str = "generated_fixture_camera_smoke";
const GENERATED_RENDER_MODES_SCENARIO: &str = "generated_fixture_render_modes";
const T5_QUAL_001_INTERACTION_MIP_SCENARIO: &str = "t5_qual_001_interaction_mip";
const T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO: &str = "t5_qual_001_interaction_render_modes";
const T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO: &str = "t5_qual_001_interaction_continuous";
const T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO: &str = "t5_qual_001_four_panel_cross_section";
const T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO: &str = "t5_qual_001_four_panel_fine_scale";
const T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO: &str =
    "t5_qual_001_four_panel_continuous_cross_section";
const T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO: &str = "t5_qual_002_four_panel_timepoint";
const T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO: &str = "t5_qual_002_four_panel_autoplay";
const CUSTOM_SCRIPT_SCENARIO: &str = "custom_script";
const GENERATED_VIEWPORT_WIDTH: u32 = 960;
const GENERATED_VIEWPORT_HEIGHT: u32 = 720;
const T5_QUAL_001_VIEWPORT_WIDTH: u32 = 1280;
const T5_QUAL_001_VIEWPORT_HEIGHT: u32 = 720;
const T5_QUAL_002_VIEWPORT_WIDTH: u32 = 1280;
const T5_QUAL_002_VIEWPORT_HEIGHT: u32 = 720;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const PREFLIGHT_ONLY_DISPLAY_SOURCE: &str = "preflight_only";
const LEGACY_ROOT_PRODUCT_VALIDATION_ARTIFACTS: &[&str] = &[
    "product-automation-script.json",
    "product-automation-report.json",
    "product-validation-report.json",
    "mirante4d-app.stdout.log",
    "mirante4d-app.stderr.log",
];

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

pub(crate) fn t5_qual_001_product_validation_completion_plan_json(
    scenario_name: &str,
    current_report: Option<&Value>,
) -> Value {
    let scenario = ProductValidationScenario::resolve(Some(scenario_name), None, None)
        .ok()
        .filter(ProductValidationScenario::is_t5_qual_001_scenario);
    let package = current_report
        .and_then(|report| report.pointer("/dataset/package_path"))
        .and_then(Value::as_str)
        .unwrap_or("<t5-qual-001-package.m4d>");
    let scenario_label = scenario
        .as_ref()
        .map(ProductValidationScenario::name)
        .unwrap_or(scenario_name);
    let timeout_secs = scenario
        .as_ref()
        .map(ProductValidationScenario::default_timeout_secs)
        .unwrap_or(0);
    let default_process_rss_limit_bytes = scenario
        .as_ref()
        .and_then(ProductValidationScenario::default_process_rss_limit_bytes);
    let output_dir = scenario
        .as_ref()
        .map(product_validation_output_dir)
        .unwrap_or_else(|| Path::new(OUTPUT_DIR).join(scenario_label));
    let current_evidence = current_report.map(|report| {
        json!({
            "status": report.get("status").and_then(Value::as_str),
            "failure_reason": report.get("failure_reason").and_then(Value::as_str),
            "preflight_only": report.pointer("/environment/product_validate_preflight_only").and_then(Value::as_bool),
            "display_class": report.pointer("/environment/display_class").and_then(Value::as_str),
            "dataset_id": report.pointer("/dataset/id").and_then(Value::as_str),
            "dataset_name": report.pointer("/dataset/name").and_then(Value::as_str),
            "package_path": report.pointer("/dataset/package_path").and_then(Value::as_str),
            "stored_dtype": report.pointer("/dataset/active_layer/dtype/stored").and_then(Value::as_str),
            "shape": report.pointer("/dataset/active_layer/shape"),
            "command_count": report.pointer("/scenario/command_count").and_then(Value::as_u64),
            "render_modes": report.pointer("/scenario/render_modes"),
            "viewport": report.pointer("/scenario/viewport"),
            "automation_limits": report.pointer("/scenario/automation_limits"),
            "process_rss_limit_bytes": report.pointer("/process/rss_limit_bytes").and_then(Value::as_u64),
            "stdout_log": report.pointer("/logs/stdout").and_then(Value::as_str),
            "stderr_log": report.pointer("/logs/stderr").and_then(Value::as_str),
        })
    });
    json!({
        "schema": "mirante4d-t5-qual-001-product-validation-completion-plan",
        "schema_version": PRODUCT_VALIDATION_SCHEMA_VERSION,
        "scenario": scenario_label,
        "status": "pending_real_native_window_product_validation",
        "requires_product_open_validation": true,
        "requires_explicit_heavy_opt_in": true,
        "requires_real_display": true,
        "default_timeout_secs": timeout_secs,
        "default_process_rss_limit_bytes": default_process_rss_limit_bytes,
        "output_dir": output_dir,
        "required_report": output_dir.join("product-validation-report.json"),
        "required_script": output_dir.join("product-automation-script.json"),
        "current_evidence": current_evidence,
        "preflight_command": product_validation_shell_command(
            package,
            scenario_label,
            true,
            false,
            default_process_rss_limit_bytes,
        ),
        "product_open_command": product_validation_shell_command(
            package,
            scenario_label,
            false,
            false,
            default_process_rss_limit_bytes,
        ),
        "timestamp_product_open_command": product_validation_shell_command(
            package,
            scenario_label,
            false,
            true,
            default_process_rss_limit_bytes,
        ),
        "acceptance": {
            "required_status": "passed",
            "must_launch_native_app": true,
            "must_not_be_preflight": true,
            "must_record_exit_success": true,
            "must_record_automation_status_passed": true,
            "must_record_process_rss_limit": true,
            "must_inspect_stdout_stderr_logs": true,
        },
    })
}

pub(crate) fn product_validate_report_with_scenario(
    package: Option<&Path>,
    scenario: Option<&str>,
) -> anyhow::Result<ProductValidationOutcome> {
    let scenario = ProductValidationScenario::resolve(
        scenario,
        env::var(SCENARIO_ENV).ok().as_deref(),
        env::var_os(CUSTOM_SCRIPT_ENV).map(PathBuf::from),
    )?;
    scenario.validate_package_arg(package)?;
    if scenario.requires_heavy_opt_in() {
        let command_name = format!("product-validate {}", scenario.name());
        with_heavy_benchmark_guard(&command_name, || {
            product_validate_report_inner(package, &scenario)
        })
    } else {
        product_validate_report_inner(package, &scenario)
    }
}

fn product_validation_shell_command(
    package: &str,
    scenario: &str,
    preflight_only: bool,
    gpu_timestamps: bool,
    process_rss_limit_bytes: Option<u64>,
) -> Value {
    let mut env = vec![("MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK", "1".to_owned())];
    if preflight_only {
        env.push((PREFLIGHT_ONLY_ENV, "1".to_owned()));
    }
    if gpu_timestamps {
        env.push((PRODUCT_VALIDATE_GPU_TIMESTAMPS_ENV, "1".to_owned()));
    }
    if let Some(limit) = process_rss_limit_bytes {
        env.push((PRODUCT_VALIDATE_MAX_RSS_BYTES_ENV, limit.to_string()));
    }
    let argv = vec![
        "cargo".to_owned(),
        "xtask".to_owned(),
        "product-validate".to_owned(),
        package.to_owned(),
        scenario.to_owned(),
    ];
    let shell_command = env
        .iter()
        .map(|(name, value)| format!("{name}={}", shell_quote(value)))
        .chain(argv.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ");
    let env_json = env
        .into_iter()
        .map(|(name, value)| (name.to_owned(), Value::String(value)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "env": env_json,
        "argv": argv,
        "shell_command": shell_command,
    })
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) fn product_validate_report_with_custom_script(
    package: Option<&Path>,
    script_path: &Path,
) -> anyhow::Result<ProductValidationOutcome> {
    product_validate_report_inner(
        package,
        &ProductValidationScenario::CustomScript(script_path.to_path_buf()),
    )
}

fn product_validate_report_inner(
    package: Option<&Path>,
    scenario: &ProductValidationScenario,
) -> anyhow::Result<ProductValidationOutcome> {
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    let gpu_timestamps_requested = product_validate_gpu_timestamps_requested();
    let base_output_dir = PathBuf::from(OUTPUT_DIR);
    fs::create_dir_all(&base_output_dir)
        .with_context(|| format!("failed to create {}", base_output_dir.display()))?;
    remove_legacy_root_product_validation_artifacts(&base_output_dir)?;
    let output_dir = product_validation_output_dir(scenario);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let (package, script) = product_validation_package_and_script(package, scenario)?;
    let script_path = output_dir.join("product-automation-script.json");
    let automation_report_path = output_dir.join("product-automation-report.json");
    let wrapper_report_path = output_dir.join("product-validation-report.json");
    let stdout_path = output_dir.join("mirante4d-app.stdout.log");
    let stderr_path = output_dir.join("mirante4d-app.stderr.log");
    write_json_file(&script_path, &script)?;
    let timeout_seconds = timeout_secs(scenario);
    let process_rss_limit_bytes = process_rss_limit_bytes(scenario);
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
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            display: DisplayClassification {
                class: DisplayClass::Unsupported,
                source: PREFLIGHT_ONLY_DISPLAY_SOURCE,
            },
            gpu_timestamps_requested,
            preflight_only,
            process_rss_limit_bytes,
            process_peak_rss_bytes: None,
            process_rss_limit_exceeded: false,
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
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            display,
            gpu_timestamps_requested,
            preflight_only,
            process_rss_limit_bytes,
            process_peak_rss_bytes: None,
            process_rss_limit_exceeded: false,
            automation_status: None,
            exit_status: None,
            exit_success: None,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::Unsupported,
        });
    }

    if !env_flag(SKIP_RELEASE_BUILD_ENV)
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
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            display,
            gpu_timestamps_requested,
            preflight_only,
            process_rss_limit_bytes,
            process_peak_rss_bytes: None,
            process_rss_limit_exceeded: false,
            automation_status: None,
            exit_status: None,
            exit_success: None,
        })?;
        return Err(err);
    }

    let binary = release_app_binary();
    if !binary.exists() {
        bail!(
            "release app binary does not exist at {}; run cargo build --release -p mirante4d-app",
            binary.display()
        );
    }
    let timeout = Duration::from_secs(timeout_seconds);
    let status = run_product_automation(ProductAutomationRun {
        binary: &binary,
        package: &package,
        script: &script_path,
        automation_report: &automation_report_path,
        stdout_path: &stdout_path,
        stderr_path: &stderr_path,
        timeout,
        gpu_timestamps_requested,
        max_rss_bytes: process_rss_limit_bytes,
    })?;

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
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            display,
            gpu_timestamps_requested,
            preflight_only,
            process_rss_limit_bytes,
            process_peak_rss_bytes: status.peak_rss_bytes,
            process_rss_limit_exceeded: status.rss_limit_exceeded,
            automation_status: None,
            exit_status: status.exit_status,
            exit_success: status.exit_success,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::TimedOut,
        });
    }

    if status.rss_limit_exceeded {
        write_wrapper_report(WrapperReport {
            path: &wrapper_report_path,
            scenario_name: scenario.name(),
            status: ProductValidationStatus::Failed,
            failure_reason: Some(format!(
                "native app exceeded product validation process RSS limit: peak={} limit={}",
                status
                    .peak_rss_bytes
                    .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
                process_rss_limit_bytes
                    .map_or_else(|| "unset".to_owned(), |bytes| bytes.to_string())
            )),
            started_at_epoch_ms,
            duration_ms: duration_ms(started_at.elapsed()),
            timeout_secs: timeout_seconds,
            package: &package,
            script: &script_path,
            script_value: &script,
            automation_report: &automation_report_path,
            automation_report_value: None,
            stdout: &stdout_path,
            stderr: &stderr_path,
            display,
            gpu_timestamps_requested,
            preflight_only,
            process_rss_limit_bytes,
            process_peak_rss_bytes: status.peak_rss_bytes,
            process_rss_limit_exceeded: status.rss_limit_exceeded,
            automation_status: None,
            exit_status: status.exit_status,
            exit_success: status.exit_success,
        })?;
        return Ok(ProductValidationOutcome {
            report_path: wrapper_report_path,
            status: ProductValidationStatus::Failed,
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
    let automation_failure = automation_report
        .as_ref()
        .and_then(|report| report.get("failure_reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let app_exited_successfully = status.exit_success.unwrap_or(false);
    let passed = app_exited_successfully && automation_status.as_deref() == Some("passed");
    let validation_status = if passed {
        ProductValidationStatus::Passed
    } else {
        ProductValidationStatus::Failed
    };
    write_wrapper_report(WrapperReport {
        path: &wrapper_report_path,
        scenario_name: scenario.name(),
        status: validation_status,
        failure_reason: if passed {
            None
        } else {
            Some(automation_failure.unwrap_or_else(|| {
                format!(
                    "native app exit success={app_exited_successfully}, automation status={:?}",
                    automation_status
                )
            }))
        },
        started_at_epoch_ms,
        duration_ms: duration_ms(started_at.elapsed()),
        timeout_secs: timeout_seconds,
        package: &package,
        script: &script_path,
        script_value: &script,
        automation_report: &automation_report_path,
        automation_report_value: automation_report.as_ref(),
        stdout: &stdout_path,
        stderr: &stderr_path,
        display,
        gpu_timestamps_requested,
        preflight_only,
        process_rss_limit_bytes,
        process_peak_rss_bytes: status.peak_rss_bytes,
        process_rss_limit_exceeded: status.rss_limit_exceeded,
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
    T5Qual001InteractionMip,
    T5Qual001InteractionRenderModes,
    T5Qual001InteractionContinuous,
    T5Qual001FourPanelCrossSection,
    T5Qual001FourPanelFineScale,
    T5Qual001FourPanelContinuousCrossSection,
    T5Qual002FourPanelTimepoint,
    T5Qual002FourPanelAutoplay,
    CustomScript(PathBuf),
}

impl ProductValidationScenario {
    fn name(&self) -> &'static str {
        match self {
            Self::GeneratedFixtureCameraSmoke => GENERATED_FIXTURE_SCENARIO,
            Self::GeneratedFixtureRenderModes => GENERATED_RENDER_MODES_SCENARIO,
            Self::T5Qual001InteractionMip => T5_QUAL_001_INTERACTION_MIP_SCENARIO,
            Self::T5Qual001InteractionRenderModes => T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO,
            Self::T5Qual001InteractionContinuous => T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO,
            Self::T5Qual001FourPanelCrossSection => T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO,
            Self::T5Qual001FourPanelFineScale => T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO,
            Self::T5Qual001FourPanelContinuousCrossSection => {
                T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO
            }
            Self::T5Qual002FourPanelTimepoint => T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO,
            Self::T5Qual002FourPanelAutoplay => T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO,
            Self::CustomScript(_) => CUSTOM_SCRIPT_SCENARIO,
        }
    }

    fn resolve(
        explicit: Option<&str>,
        env_value: Option<&str>,
        custom_script: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let requested = explicit.or(env_value);
        if let Some(path) = custom_script {
            match requested {
                Some(CUSTOM_SCRIPT_SCENARIO) | None => return Ok(Self::CustomScript(path)),
                Some(other) => bail!(
                    "{CUSTOM_SCRIPT_ENV} cannot be combined with generated product validation \
                     scenario {other:?}; use {CUSTOM_SCRIPT_SCENARIO:?} or unset {CUSTOM_SCRIPT_ENV}"
                ),
            }
        }
        match requested.unwrap_or(GENERATED_FIXTURE_SCENARIO) {
            GENERATED_FIXTURE_SCENARIO | "generated-fixture-camera-smoke" | "generated" => {
                Ok(Self::GeneratedFixtureCameraSmoke)
            }
            GENERATED_RENDER_MODES_SCENARIO | "generated-fixture-render-modes" | "render-modes" => {
                Ok(Self::GeneratedFixtureRenderModes)
            }
            T5_QUAL_001_INTERACTION_MIP_SCENARIO
            | "t5-qual-001-interaction-mip"
            | "T5-QUAL-001" => Ok(Self::T5Qual001InteractionMip),
            T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO
            | "t5-qual-001-interaction-render-modes"
            | "t5-qual-001-render-modes" => Ok(Self::T5Qual001InteractionRenderModes),
            T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO
            | "t5-qual-001-interaction-continuous"
            | "t5-qual-001-continuous" => Ok(Self::T5Qual001InteractionContinuous),
            T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO
            | "t5-qual-001-four-panel-cross-section"
            | "t5-qual-001-four-panel" => Ok(Self::T5Qual001FourPanelCrossSection),
            T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO
            | "t5-qual-001-four-panel-fine-scale"
            | "t5-qual-001-fine-scale" => Ok(Self::T5Qual001FourPanelFineScale),
            T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO
            | "t5-qual-001-four-panel-continuous-cross-section"
            | "t5-qual-001-four-panel-continuous"
            | "t5-qual-001-cross-section-continuous" => {
                Ok(Self::T5Qual001FourPanelContinuousCrossSection)
            }
            T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO
            | "t5_qual_002-four-panel-timepoint"
            | "t5_qual_002-timepoint" => Ok(Self::T5Qual002FourPanelTimepoint),
            T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO
            | "t5_qual_002-four-panel-autoplay"
            | "t5_qual_002-autoplay" => Ok(Self::T5Qual002FourPanelAutoplay),
            CUSTOM_SCRIPT_SCENARIO => {
                bail!("{CUSTOM_SCRIPT_SCENARIO} requires {CUSTOM_SCRIPT_ENV}=<script.json>")
            }
            other => bail!(
                "unknown product validation scenario {other:?}; expected \
                 {GENERATED_FIXTURE_SCENARIO}, {GENERATED_RENDER_MODES_SCENARIO}, \
                 {T5_QUAL_001_INTERACTION_MIP_SCENARIO}, {T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO}, \
                 {T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO}, {T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO}, \
                 {T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO}, \
                 {T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO}, \
                 {T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO}, \
                 {T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO}, \
                 or {CUSTOM_SCRIPT_SCENARIO}"
            ),
        }
    }

    fn is_named_scenario(name: &str) -> bool {
        matches!(
            name,
            GENERATED_FIXTURE_SCENARIO
                | "generated-fixture-camera-smoke"
                | "generated"
                | GENERATED_RENDER_MODES_SCENARIO
                | "generated-fixture-render-modes"
                | "render-modes"
                | T5_QUAL_001_INTERACTION_MIP_SCENARIO
                | "t5-qual-001-interaction-mip"
                | "T5-QUAL-001"
                | T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO
                | "t5-qual-001-interaction-render-modes"
                | "t5-qual-001-render-modes"
                | T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO
                | "t5-qual-001-interaction-continuous"
                | "t5-qual-001-continuous"
                | T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO
                | "t5-qual-001-four-panel-cross-section"
                | "t5-qual-001-four-panel"
                | T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO
                | "t5-qual-001-four-panel-fine-scale"
                | "t5-qual-001-fine-scale"
                | T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO
                | "t5-qual-001-four-panel-continuous-cross-section"
                | "t5-qual-001-four-panel-continuous"
                | "t5-qual-001-cross-section-continuous"
                | T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO
                | "t5_qual_002-four-panel-timepoint"
                | "t5_qual_002-timepoint"
                | T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO
                | "t5_qual_002-four-panel-autoplay"
                | "t5_qual_002-autoplay"
                | CUSTOM_SCRIPT_SCENARIO
        )
    }

    fn requires_heavy_opt_in(&self) -> bool {
        self.is_heavy_local_sample_scenario()
    }

    fn default_timeout_secs(&self) -> u64 {
        match self {
            Self::GeneratedFixtureCameraSmoke
            | Self::GeneratedFixtureRenderModes
            | Self::CustomScript(_) => 60,
            Self::T5Qual001InteractionMip => 180,
            Self::T5Qual001InteractionRenderModes => 240,
            Self::T5Qual001InteractionContinuous => 300,
            Self::T5Qual001FourPanelCrossSection => 300,
            Self::T5Qual001FourPanelFineScale => 300,
            Self::T5Qual001FourPanelContinuousCrossSection => 300,
            Self::T5Qual002FourPanelTimepoint => 300,
            Self::T5Qual002FourPanelAutoplay => 300,
        }
    }

    fn default_process_rss_limit_bytes(&self) -> Option<u64> {
        if self.is_heavy_local_sample_scenario() {
            Some(8 * GIB)
        } else {
            None
        }
    }

    fn validate_package_arg(&self, package: Option<&Path>) -> anyhow::Result<()> {
        if self.is_heavy_local_sample_scenario() && package.is_none() {
            bail!(
                "{} product validation requires <native-package.m4d>",
                self.name()
            );
        }
        Ok(())
    }

    fn is_t5_qual_001_scenario(&self) -> bool {
        matches!(
            self,
            Self::T5Qual001InteractionMip
                | Self::T5Qual001InteractionRenderModes
                | Self::T5Qual001InteractionContinuous
                | Self::T5Qual001FourPanelCrossSection
                | Self::T5Qual001FourPanelFineScale
                | Self::T5Qual001FourPanelContinuousCrossSection
        )
    }

    fn is_heavy_local_sample_scenario(&self) -> bool {
        self.is_t5_qual_001_scenario()
            || matches!(
                self,
                Self::T5Qual002FourPanelTimepoint | Self::T5Qual002FourPanelAutoplay
            )
    }
}

fn product_validation_output_dir(scenario: &ProductValidationScenario) -> PathBuf {
    Path::new(OUTPUT_DIR).join(scenario.name())
}

fn remove_legacy_root_product_validation_artifacts(base_output_dir: &Path) -> anyhow::Result<()> {
    for artifact in LEGACY_ROOT_PRODUCT_VALIDATION_ARTIFACTS {
        let path = base_output_dir.join(artifact);
        if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove stale {}", path.display()))?;
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

fn product_validation_package_and_script(
    package: Option<&Path>,
    scenario: &ProductValidationScenario,
) -> anyhow::Result<(PathBuf, Value)> {
    match scenario {
        ProductValidationScenario::GeneratedFixtureCameraSmoke => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => generate_fixture(DEFAULT_FIXTURE)?,
            };
            let script = generated_fixture_camera_smoke_script(&package);
            Ok((package, script))
        }
        ProductValidationScenario::GeneratedFixtureRenderModes => {
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => generate_fixture(RENDER_MODES_FIXTURE)?,
            };
            let script = generated_fixture_render_modes_script(&package);
            Ok((package, script))
        }
        ProductValidationScenario::T5Qual001InteractionMip => {
            let package = package
                .context(
                    "t5_qual_001_interaction_mip product validation requires <native-package.m4d>",
                )?
                .to_path_buf();
            let script = t5_qual_001_interaction_mip_script(&package);
            Ok((package, script))
        }
        ProductValidationScenario::T5Qual001InteractionRenderModes => {
            let package = package
                .context(
                    "t5_qual_001_interaction_render_modes product validation requires <native-package.m4d>",
                )?
                .to_path_buf();
            let script = t5_qual_001_interaction_render_modes_script(&package);
            Ok((package, script))
        }
        ProductValidationScenario::T5Qual001InteractionContinuous => {
            let package = package.context(
                "t5_qual_001_interaction_continuous product validation requires <native-package.m4d>",
            )?;
            let script = t5_qual_001_interaction_continuous_script(package);
            Ok((package.to_path_buf(), script))
        }
        ProductValidationScenario::T5Qual001FourPanelCrossSection => {
            let package = package.context(
                "t5_qual_001_four_panel_cross_section product validation requires <native-package.m4d>",
            )?;
            let script = t5_qual_001_four_panel_cross_section_script(package);
            Ok((package.to_path_buf(), script))
        }
        ProductValidationScenario::T5Qual001FourPanelFineScale => {
            let package = package.context(
                "t5_qual_001_four_panel_fine_scale product validation requires <native-package.m4d>",
            )?;
            let script = t5_qual_001_four_panel_fine_scale_script(package);
            Ok((package.to_path_buf(), script))
        }
        ProductValidationScenario::T5Qual001FourPanelContinuousCrossSection => {
            let package = package.context(
                "t5_qual_001_four_panel_continuous_cross_section product validation requires <native-package.m4d>",
            )?;
            let script = t5_qual_001_four_panel_continuous_cross_section_script(package);
            Ok((package.to_path_buf(), script))
        }
        ProductValidationScenario::T5Qual002FourPanelTimepoint => {
            let package = package.context(
                "t5_qual_002_four_panel_timepoint product validation requires <native-package.m4d>",
            )?;
            let script = t5_qual_002_four_panel_timepoint_script(package);
            Ok((package.to_path_buf(), script))
        }
        ProductValidationScenario::T5Qual002FourPanelAutoplay => {
            let package = package.context(
                "t5_qual_002_four_panel_autoplay product validation requires <native-package.m4d>",
            )?;
            let script = t5_qual_002_four_panel_autoplay_script(package);
            Ok((package.to_path_buf(), script))
        }
        ProductValidationScenario::CustomScript(script_path) => {
            let script = load_custom_product_automation_script(script_path)?;
            let package = match package {
                Some(package) => package.to_path_buf(),
                None => script_open_dataset_path(&script).with_context(|| {
                    format!(
                        "{CUSTOM_SCRIPT_ENV}={} does not include an open_dataset command; \
                         pass <native-package.m4d> explicitly",
                        script_path.display()
                    )
                })?,
            };
            Ok((package, script))
        }
    }
}

fn generated_fixture_camera_smoke_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": GENERATED_FIXTURE_SCENARIO,
        "limits": {
            "max_decoded_bytes": 8 * MIB,
            "max_decoded_brick_bytes": 8 * MIB,
            "max_encoded_payload_bytes_read": 8 * MIB,
            "max_cpu_brick_cache_bytes": 64 * MIB,
            "max_brick_requests_queued": 128,
            "max_brick_queue_depth": 128,
            "max_gpu_brick_atlas_uploaded_bytes": 64 * MIB,
            "max_gpu_brick_atlas_resident_bytes": GIB,
            "max_gpu_display_resource_resident_bytes": 256 * MIB,
        },
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "sleep_or_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "camera_fit_data" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "camera_orbit", "yaw_points": 120.0, "pitch_points": 32.0 },
            { "command": "camera_pan", "x_points": 40.0, "y_points": -24.0 },
            { "command": "camera_zoom", "scroll_y_points": -120.0 },
            { "command": "sleep_or_frames", "frames": 2 },
            { "command": "probe_hover", "x_fraction": 0.42, "y_fraction": 0.58 },
            { "command": "capture_screenshot", "name": "post-camera-sequence" },
            { "command": "copy_diagnostics" },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "quit" }
        ]
    })
}

fn generated_fixture_render_modes_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": GENERATED_RENDER_MODES_SCENARIO,
        "limits": {
            "max_decoded_bytes": 12 * MIB,
            "max_decoded_brick_bytes": 12 * MIB,
            "max_encoded_payload_bytes_read": 12 * MIB,
            "max_cpu_brick_cache_bytes": 64 * MIB,
            "max_brick_requests_queued": 192,
            "max_brick_queue_depth": 192,
            "max_gpu_brick_atlas_uploaded_bytes": 96 * MIB,
            "max_gpu_brick_atlas_resident_bytes": GIB,
            "max_gpu_display_resource_resident_bytes": 256 * MIB,
        },
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": GENERATED_VIEWPORT_WIDTH, "height": GENERATED_VIEWPORT_HEIGHT },
            { "command": "set_layer_window", "layer_index": 0, "low": 0.0, "high": 4096.0 },
            { "command": "set_layer_window", "layer_index": 1, "low": 20000.0, "high": 24096.0 },
            { "command": "set_layer_opacity", "layer_index": 0, "opacity": 1.0 },
            { "command": "set_layer_opacity", "layer_index": 1, "opacity": 1.0 },
            { "command": "sleep_or_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 30000 },
            { "command": "camera_fit_data" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "mip" },
            { "command": "assert", "condition": { "render_mode": { "mode": "mip" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "generated-mip" },
            { "command": "set_render_mode", "mode": "dvr" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "dvr" },
            { "command": "set_dvr_density_scale", "density_scale": 12.0 },
            { "command": "assert", "condition": { "render_mode": { "mode": "dvr" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "generated-dvr" },
            { "command": "set_render_mode", "mode": "iso" },
            { "command": "set_layer_render_mode", "layer_index": 1, "mode": "iso" },
            { "command": "set_iso_display_level", "display_level": 0.05 },
            { "command": "assert", "condition": { "render_mode": { "mode": "iso" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "generated-iso" },
            { "command": "copy_diagnostics" },
            { "command": "quit" }
        ]
    })
}

fn t5_qual_001_interaction_mip_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_001_INTERACTION_MIP_SCENARIO,
        "limits": {
            "max_decoded_bytes": GIB,
            "max_decoded_brick_bytes": GIB,
            "max_encoded_payload_bytes_read": 2 * GIB,
            "max_cpu_brick_cache_bytes": 2 * GIB,
            "max_brick_requests_queued": 4096,
            "max_brick_queue_depth": 4096,
            "max_gpu_brick_atlas_uploaded_bytes": GIB,
            "max_gpu_brick_atlas_resident_bytes": 2 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": T5_QUAL_001_VIEWPORT_WIDTH, "height": T5_QUAL_001_VIEWPORT_HEIGHT },
            { "command": "sleep_or_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "assert", "condition": { "render_mode": { "mode": "mip" } } },
            { "command": "camera_fit_data" },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-initial-mip" },
            { "command": "camera_orbit", "yaw_points": 180.0, "pitch_points": 24.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "probe_hover", "x_fraction": 0.45, "y_fraction": 0.55 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-first-orbit-cache-miss" },
            { "command": "camera_orbit", "yaw_points": -180.0, "pitch_points": -24.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-return-orbit-cache-reuse" },
            { "command": "camera_pan", "x_points": 96.0, "y_points": -48.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "copy_diagnostics" },
            { "command": "camera_zoom", "scroll_y_points": -160.0 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "copy_diagnostics" },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "quit" }
        ]
    })
}

fn t5_qual_001_interaction_render_modes_script(package: &Path) -> Value {
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": [
            { "command": "open_dataset", "path": package },
            { "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 },
            { "command": "set_viewport_size", "width": T5_QUAL_001_VIEWPORT_WIDTH, "height": T5_QUAL_001_VIEWPORT_HEIGHT },
            { "command": "sleep_or_frames", "frames": 3 },
            { "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 },
            { "command": "camera_fit_data" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "assert", "condition": { "render_mode": { "mode": "mip" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.5, "y_fraction": 0.5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-render-modes-mip" },
            { "command": "set_render_mode", "mode": "dvr" },
            { "command": "set_dvr_density_scale", "density_scale": 8.0 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "assert", "condition": { "render_mode": { "mode": "dvr" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.47, "y_fraction": 0.53 },
            { "command": "camera_orbit", "yaw_points": 120.0, "pitch_points": 16.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-render-modes-dvr-orbit" },
            { "command": "set_render_mode", "mode": "iso" },
            { "command": "set_iso_display_level", "display_level": 0.02 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "assert", "condition": { "render_mode": { "mode": "iso" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "probe_hover", "x_fraction": 0.52, "y_fraction": 0.48 },
            { "command": "camera_pan", "x_points": 80.0, "y_points": -40.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-render-modes-iso-pan" },
            { "command": "set_render_mode", "mode": "mip" },
            { "command": "sleep_or_frames", "frames": 5 },
            { "command": "assert", "condition": { "render_mode": { "mode": "mip" } } },
            { "command": "assert", "condition": "nonblank_frame" },
            { "command": "assert", "condition": "no_render_error" },
            { "command": "copy_diagnostics" },
            { "command": "capture_screenshot", "name": "t5-qual-001-render-modes-return-mip" },
            { "command": "quit" }
        ]
    })
}

fn t5_qual_001_interaction_continuous_script(package: &Path) -> Value {
    let mut commands = vec![
        json!({ "command": "open_dataset", "path": package }),
        json!({ "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 }),
        json!({ "command": "set_viewport_size", "width": T5_QUAL_001_VIEWPORT_WIDTH, "height": T5_QUAL_001_VIEWPORT_HEIGHT }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 }),
        json!({ "command": "camera_fit_data" }),
        json!({ "command": "set_render_mode", "mode": "mip" }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "mip" } } }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "copy_diagnostics" }),
    ];
    append_continuous_camera_sequence(&mut commands, 18);
    commands.extend([
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-continuous-mip" }),
        json!({ "command": "set_render_mode", "mode": "dvr" }),
        json!({ "command": "set_dvr_density_scale", "density_scale": 8.0 }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "dvr" } } }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
    ]);
    append_continuous_camera_sequence(&mut commands, 18);
    commands.extend([
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-continuous-dvr" }),
        json!({ "command": "set_render_mode", "mode": "iso" }),
        json!({ "command": "set_iso_display_level", "display_level": 0.02 }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "iso" } } }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
    ]);
    append_continuous_camera_sequence(&mut commands, 18);
    commands.extend([
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-continuous-iso" }),
        json!({ "command": "quit" }),
    ]);

    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": commands,
    })
}

fn t5_qual_001_four_panel_cross_section_script(package: &Path) -> Value {
    let commands = vec![
        json!({ "command": "open_dataset", "path": package }),
        json!({ "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 }),
        json!({ "command": "set_viewport_size", "width": T5_QUAL_001_VIEWPORT_WIDTH, "height": T5_QUAL_001_VIEWPORT_HEIGHT }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "set_render_mode", "mode": "mip" }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "mip" } } }),
        json!({ "command": "camera_fit_data" }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-single3d-baseline" }),
        json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
            "panel": "xy",
            "min_generation": 1,
            "min_selected_bricks": 1
        } } }),
        json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
            "panel": "xz",
            "min_generation": 1,
            "min_selected_bricks": 1
        } } }),
        json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
            "panel": "yz",
            "min_generation": 1,
            "min_selected_bricks": 1
        } } }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": { "cross_section_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
        json!({ "command": "assert", "condition": { "four_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-initial" }),
        json!({ "command": "cross_section_pan", "panel": "xz", "x_points": 72.0, "y_points": -24.0 }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "assert", "condition": { "cross_section_active_panel": { "panel": "xz" } } }),
        json!({ "command": "cross_section_slice_step", "panel": "xz", "notches": 2.0 }),
        json!({ "command": "cross_section_slice_step", "panel": "xz", "notches": -1.0, "fast": true }),
        json!({ "command": "cross_section_zoom", "panel": "xz", "x_fraction": 0.45, "y_fraction": 0.55, "scroll_y_points": -120.0 }),
        json!({ "command": "cross_section_rotate", "panel": "xz", "x_points": 28.0, "y_points": -16.0 }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": { "cross_section_active_panel": { "panel": "xz" } } }),
        json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
            "panel": "xz",
            "min_generation": 2,
            "min_selected_bricks": 1
        } } }),
        json!({ "command": "assert", "condition": { "cross_section_stream": {
            "panel": "xz",
            "priority": "current_frame",
            "active_panel_at_submission": "xz",
            "min_requested": 1,
            "min_completed": 1,
            "min_visible_chunks": 1,
            "max_failed": 0
        } } }),
        json!({ "command": "assert", "condition": { "cross_section_stream": {
            "panel": "xy",
            "active_panel_at_submission": "xz",
            "min_requested": 1,
            "min_completed": 1,
            "min_visible_chunks": 1,
            "max_failed": 0
        } } }),
        json!({ "command": "assert", "condition": { "cross_section_stream": {
            "panel": "yz",
            "active_panel_at_submission": "xz",
            "min_requested": 1,
            "min_completed": 1,
            "min_visible_chunks": 1,
            "max_failed": 0
        } } }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": { "cross_section_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
        json!({ "command": "assert", "condition": { "four_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
        json!({ "command": "probe_panel_hover", "panel": "xz", "x_fraction": 0.5, "y_fraction": 0.5, "expected_status": "value", "expect_value": true }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-after-oblique-interaction" }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "set_viewer_layout", "layout": "single3d" }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "single3d" } } }),
        json!({ "command": "assert", "condition": "cross_section_retired" }),
        json!({ "command": "camera_orbit", "yaw_points": 90.0, "pitch_points": 12.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-returned-single3d" }),
        json!({ "command": "quit" }),
    ];
    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": commands,
    })
}

fn t5_qual_001_four_panel_fine_scale_script(package: &Path) -> Value {
    let mut commands = vec![
        json!({ "command": "open_dataset", "path": package }),
        json!({ "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 }),
        json!({ "command": "set_viewport_size", "width": T5_QUAL_001_VIEWPORT_WIDTH, "height": T5_QUAL_001_VIEWPORT_HEIGHT }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "set_render_mode", "mode": "mip" }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "mip" } } }),
        json!({ "command": "camera_fit_data" }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60000 }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-fine-scale-broad-before-zoom" }),
        json!({ "command": "cross_section_zoom", "panel": "xz", "x_fraction": 0.5, "y_fraction": 0.5, "scroll_y_points": 1000.0 }),
        json!({ "command": "cross_section_zoom", "panel": "xz", "x_fraction": 0.5, "y_fraction": 0.5, "scroll_y_points": 1000.0 }),
        json!({ "command": "cross_section_zoom", "panel": "xz", "x_fraction": 0.5, "y_fraction": 0.5, "scroll_y_points": 1000.0 }),
        json!({ "command": "sleep_or_frames", "millis": 250 }),
        json!({ "command": "assert", "condition": { "cross_section_active_panel": { "panel": "xz" } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 120000 }),
    ];
    append_t5_qual_001_fine_scale_panel_assertions(&mut commands, 4);
    commands.extend([
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-fine-scale-s0" }),
        json!({ "command": "set_viewer_layout", "layout": "single3d" }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "single3d" } } }),
        json!({ "command": "assert", "condition": "cross_section_retired" }),
        json!({ "command": "camera_orbit", "yaw_points": 80.0, "pitch_points": 10.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-fine-scale-returned-single3d" }),
        json!({ "command": "quit" }),
    ]);

    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": commands,
    })
}

fn append_t5_qual_001_fine_scale_panel_assertions(commands: &mut Vec<Value>, min_generation: u64) {
    for panel in ["xy", "xz", "yz"] {
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
            "panel": panel,
            "min_generation": min_generation,
            "target_scale_level": 0,
            "render_scale_level": 0,
            "min_selected_bricks": 1,
            "max_missing_occupied_bricks": 0,
            "display_current": true
        } } }),
        );
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_stream": {
            "panel": panel,
            "timepoint": 0,
            "min_requested": 1,
            "min_completed": 1,
            "min_visible_chunks": 1,
            "max_failed": 0
        } } }),
        );
    }
    commands.push(
        json!({ "command": "assert", "condition": { "cross_section_panel_images_distinct": {
        "min_different_pixels": 1
    } } }),
    );
    commands.push(
        json!({ "command": "assert", "condition": { "four_panel_images_distinct": {
        "min_different_pixels": 1
    } } }),
    );
}

fn t5_qual_001_four_panel_continuous_cross_section_script(package: &Path) -> Value {
    let mut commands = vec![
        json!({ "command": "open_dataset", "path": package }),
        json!({ "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 }),
        json!({ "command": "set_viewport_size", "width": T5_QUAL_001_VIEWPORT_WIDTH, "height": T5_QUAL_001_VIEWPORT_HEIGHT }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "set_render_mode", "mode": "mip" }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "mip" } } }),
        json!({ "command": "camera_fit_data" }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 60000 }),
    ];
    append_cross_section_panel_nonblank_assertions(&mut commands);
    commands.extend([
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-continuous-initial" }),
    ]);
    append_continuous_cross_section_sequence(&mut commands, 6);
    commands.extend([
        json!({ "command": "assert", "condition": { "cross_section_active_panel": { "panel": "xz" } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 120000 }),
    ]);
    append_t5_qual_001_continuous_cross_section_settled_assertions(&mut commands, 4);
    commands.extend([
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-continuous-settled" }),
        json!({ "command": "set_viewer_layout", "layout": "single3d" }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "single3d" } } }),
        json!({ "command": "assert", "condition": "cross_section_retired" }),
        json!({ "command": "camera_orbit", "yaw_points": 80.0, "pitch_points": 12.0, "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32 }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5-qual-001-four-panel-continuous-returned-single3d" }),
        json!({ "command": "quit" }),
    ]);

    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": commands,
    })
}

fn append_continuous_cross_section_sequence(commands: &mut Vec<Value>, steps: usize) {
    for step in 0..steps {
        let direction = if step.is_multiple_of(2) { 1.0 } else { -1.0 };
        commands.push(json!({
            "command": "cross_section_rotate",
            "panel": "xz",
            "x_points": direction * 24.0,
            "y_points": -direction * 14.0,
        }));
        commands.push(json!({
            "command": "cross_section_slice_step",
            "panel": "xz",
            "notches": direction,
            "fast": step.is_multiple_of(3),
        }));
        commands.push(json!({
            "command": "cross_section_pan",
            "panel": "xz",
            "x_points": direction * 42.0,
            "y_points": -direction * 18.0,
        }));
        commands.push(json!({
            "command": "cross_section_zoom",
            "panel": "xz",
            "x_fraction": 0.5,
            "y_fraction": 0.5,
            "scroll_y_points": if step.is_multiple_of(2) { 140.0 } else { -105.0 },
        }));
        commands.push(json!({ "command": "sleep_or_frames", "frames": 1 }));
        commands.push(json!({ "command": "assert", "condition": "no_render_error" }));
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_active_panel": {
                "panel": "xz"
            } } }),
        );
        append_cross_section_panel_nonblank_assertions(commands);
        if step == steps / 2 {
            commands.push(json!({ "command": "copy_diagnostics" }));
        }
    }
}

fn append_cross_section_panel_nonblank_assertions(commands: &mut Vec<Value>) {
    for panel in ["xy", "xz", "yz"] {
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_nonblank": {
                "panel": panel,
                "min_nonzero_rgb_pixels": 1
            } } }),
        );
    }
}

fn append_t5_qual_001_continuous_cross_section_settled_assertions(
    commands: &mut Vec<Value>,
    min_generation: u64,
) {
    for panel in ["xy", "xz", "yz"] {
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
                "panel": panel,
                "min_generation": min_generation,
                "min_selected_bricks": 1,
                "max_missing_occupied_bricks": 0,
                "display_current": true
            } } }),
        );
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_stream": {
                "panel": panel,
                "min_requested": 1,
                "min_completed": 1,
                "min_visible_chunks": 1,
                "max_failed": 0
            } } }),
        );
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_nonblank": {
                "panel": panel,
                "min_nonzero_rgb_pixels": 1
            } } }),
        );
    }
    commands.push(
        json!({ "command": "assert", "condition": { "cross_section_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
    );
    commands.push(
        json!({ "command": "assert", "condition": { "four_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
    );
}

fn t5_qual_002_four_panel_timepoint_script(package: &Path) -> Value {
    let mut commands = vec![
        json!({ "command": "open_dataset", "path": package }),
        json!({ "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 }),
        json!({ "command": "set_viewport_size", "width": T5_QUAL_002_VIEWPORT_WIDTH, "height": T5_QUAL_002_VIEWPORT_HEIGHT }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "set_render_mode", "mode": "mip" }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "mip" } } }),
        json!({ "command": "camera_fit_data" }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } }),
        json!({ "command": "assert", "condition": { "active_timepoint": { "timepoint": 0 } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 90000 }),
    ];
    append_t5_qual_002_timepoint_panel_assertions(
        &mut commands,
        0,
        1,
        "t5_qual_002-four-panel-timepoint-0",
    );
    commands.extend([
        json!({ "command": "set_timepoint", "timepoint": 1 }),
        json!({ "command": "assert", "condition": { "active_timepoint": { "timepoint": 1 } } }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 90000 }),
    ]);
    append_t5_qual_002_timepoint_panel_assertions(
        &mut commands,
        1,
        2,
        "t5_qual_002-four-panel-timepoint-1",
    );
    commands.extend([
        json!({ "command": "step_timepoint", "delta": 1 }),
        json!({ "command": "assert", "condition": { "active_timepoint": { "timepoint": 2 } } }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 90000 }),
    ]);
    append_t5_qual_002_timepoint_panel_assertions(
        &mut commands,
        2,
        3,
        "t5_qual_002-four-panel-timepoint-2",
    );
    commands.extend([
        json!({ "command": "set_viewer_layout", "layout": "single3d" }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "single3d" } } }),
        json!({ "command": "assert", "condition": "cross_section_retired" }),
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5_qual_002-four-panel-returned-single3d" }),
        json!({ "command": "quit" }),
    ]);

    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": commands,
    })
}

fn t5_qual_002_four_panel_autoplay_script(package: &Path) -> Value {
    let mut commands = vec![
        json!({ "command": "open_dataset", "path": package }),
        json!({ "command": "wait_for", "condition": "window_ready", "timeout_ms": 5000 }),
        json!({ "command": "set_viewport_size", "width": T5_QUAL_002_VIEWPORT_WIDTH, "height": T5_QUAL_002_VIEWPORT_HEIGHT }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "wait_for", "condition": "first_frame", "timeout_ms": 60000 }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "set_render_mode", "mode": "mip" }),
        json!({ "command": "assert", "condition": { "render_mode": { "mode": "mip" } } }),
        json!({ "command": "camera_fit_data" }),
        json!({ "command": "sleep_or_frames", "frames": 5 }),
        json!({ "command": "set_viewer_layout", "layout": "four_panel" }),
        json!({ "command": "sleep_or_frames", "frames": 8 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "four_panel" } } }),
        json!({ "command": "assert", "condition": { "active_timepoint": { "timepoint": 0 } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 90000 }),
    ];
    append_t5_qual_002_timepoint_panel_assertions(
        &mut commands,
        0,
        1,
        "t5_qual_002-four-panel-autoplay-0",
    );
    commands.extend([
        json!({ "command": "set_playback", "playing": true }),
        json!({ "command": "assert", "condition": { "playback": { "playing": true } } }),
        json!({ "command": "sleep_or_frames", "millis": 350 }),
        json!({ "command": "set_playback", "playing": false }),
        json!({ "command": "assert", "condition": { "playback": { "playing": false } } }),
        json!({ "command": "assert", "condition": { "observed_timepoints": { "min_distinct": 2 } } }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "wait_for", "condition": "runtime_idle", "timeout_ms": 120000 }),
        json!({ "command": "assert", "condition": { "cross_section_streams_match_active_timepoint": {
            "min_completed": 1,
            "min_visible_chunks": 1,
            "max_failed": 0
        } } }),
    ]);
    append_t5_qual_002_autoplay_panel_assertions(
        &mut commands,
        2,
        "t5_qual_002-four-panel-autoplay-settled",
    );
    commands.extend([
        json!({ "command": "set_viewer_layout", "layout": "single3d" }),
        json!({ "command": "sleep_or_frames", "frames": 3 }),
        json!({ "command": "assert", "condition": { "viewer_layout": { "layout": "single3d" } } }),
        json!({ "command": "assert", "condition": "cross_section_retired" }),
        json!({ "command": "assert", "condition": "nonblank_frame" }),
        json!({ "command": "assert", "condition": "no_render_error" }),
        json!({ "command": "copy_diagnostics" }),
        json!({ "command": "capture_screenshot", "name": "t5_qual_002-four-panel-autoplay-returned-single3d" }),
        json!({ "command": "quit" }),
    ]);

    json!({
        "schema": PRODUCT_AUTOMATION_SCRIPT_SCHEMA,
        "schema_version": PRODUCT_AUTOMATION_SCHEMA_VERSION,
        "scenario": T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO,
        "limits": {
            "max_decoded_bytes": 2 * GIB,
            "max_decoded_brick_bytes": 2 * GIB,
            "max_encoded_payload_bytes_read": 4 * GIB,
            "max_cpu_brick_cache_bytes": 3 * GIB,
            "max_brick_requests_queued": 8192,
            "max_brick_queue_depth": 8192,
            "max_gpu_brick_atlas_uploaded_bytes": 2 * GIB,
            "max_gpu_brick_atlas_resident_bytes": 3 * GIB,
            "max_gpu_display_resource_resident_bytes": GIB,
        },
        "commands": commands,
    })
}

fn append_t5_qual_002_timepoint_panel_assertions(
    commands: &mut Vec<Value>,
    timepoint: u64,
    min_generation: u64,
    screenshot_name: &str,
) {
    for panel in ["xy", "xz", "yz"] {
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
            "panel": panel,
            "min_generation": min_generation,
            "min_selected_bricks": 1,
            "max_missing_occupied_bricks": 0,
            "display_current": true
        } } }),
        );
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_stream": {
            "panel": panel,
            "timepoint": timepoint,
            "min_requested": 1,
            "min_completed": 1,
            "min_visible_chunks": 1,
            "max_failed": 0
        } } }),
        );
    }
    commands.push(
        json!({ "command": "assert", "condition": { "cross_section_panel_images_distinct": {
        "min_different_pixels": 1
    } } }),
    );
    commands.push(
        json!({ "command": "assert", "condition": { "four_panel_images_distinct": {
        "min_different_pixels": 1
    } } }),
    );
    commands.push(json!({ "command": "copy_diagnostics" }));
    commands.push(json!({ "command": "capture_screenshot", "name": screenshot_name }));
}

fn append_t5_qual_002_autoplay_panel_assertions(
    commands: &mut Vec<Value>,
    min_generation: u64,
    screenshot_name: &str,
) {
    for panel in ["xy", "xz", "yz"] {
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_schedule": {
                "panel": panel,
                "min_generation": min_generation,
                "min_selected_bricks": 1,
                "max_missing_occupied_bricks": 0,
                "display_current": true
            } } }),
        );
        commands.push(
            json!({ "command": "assert", "condition": { "cross_section_panel_nonblank": {
                "panel": panel,
                "min_nonzero_rgb_pixels": 1
            } } }),
        );
    }
    commands.push(
        json!({ "command": "assert", "condition": { "cross_section_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
    );
    commands.push(
        json!({ "command": "assert", "condition": { "four_panel_images_distinct": {
            "min_different_pixels": 1
        } } }),
    );
    commands.push(json!({ "command": "copy_diagnostics" }));
    commands.push(json!({ "command": "capture_screenshot", "name": screenshot_name }));
}

fn append_continuous_camera_sequence(commands: &mut Vec<Value>, steps: usize) {
    for step in 0..steps {
        let direction = if step.is_multiple_of(2) { 1.0 } else { -1.0 };
        commands.push(json!({
            "command": "camera_orbit",
            "yaw_points": direction * 16.0,
            "pitch_points": direction * 4.0,
            "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32,
        }));
        if step.is_multiple_of(3) {
            commands.push(json!({
                "command": "camera_pan",
                "x_points": direction * 12.0,
                "y_points": -direction * 6.0,
                "viewport_height_points": T5_QUAL_001_VIEWPORT_HEIGHT as f32,
            }));
        }
        if step.is_multiple_of(6) {
            commands.push(json!({
                "command": "camera_zoom",
                "scroll_y_points": -direction * 18.0,
            }));
        }
        if step == steps / 2 {
            commands.push(json!({ "command": "copy_diagnostics" }));
        }
    }
}

fn load_custom_product_automation_script(path: &Path) -> anyhow::Result<Value> {
    let script = read_json_file(path).with_context(|| {
        format!(
            "failed to read custom product validation script {}",
            path.display()
        )
    })?;
    validate_product_automation_script(&script).with_context(|| {
        format!(
            "invalid custom product validation script {}",
            path.display()
        )
    })?;
    Ok(script)
}

fn validate_product_automation_script(script: &Value) -> anyhow::Result<()> {
    if script.get("schema").and_then(Value::as_str) != Some(PRODUCT_AUTOMATION_SCRIPT_SCHEMA) {
        bail!("automation script schema must be {PRODUCT_AUTOMATION_SCRIPT_SCHEMA}");
    }
    if script.get("schema_version").and_then(Value::as_u64)
        != Some(PRODUCT_AUTOMATION_SCHEMA_VERSION as u64)
    {
        bail!("automation script schema_version must be {PRODUCT_AUTOMATION_SCHEMA_VERSION}");
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
    if commands
        .last()
        .and_then(|command| command.get("command"))
        .and_then(Value::as_str)
        != Some("quit")
    {
        bail!("automation script final command must be quit");
    }
    validate_product_automation_limits(script)?;
    Ok(())
}

fn validate_product_automation_limits(script: &Value) -> anyhow::Result<()> {
    let Some(limits) = script.get("limits") else {
        return Ok(());
    };
    let Some(map) = limits.as_object() else {
        bail!("automation script limits must be an object");
    };
    const ALLOWED_LIMITS: &[&str] = &[
        "max_decoded_bytes",
        "max_decoded_brick_bytes",
        "max_encoded_payload_bytes_read",
        "max_cpu_brick_cache_bytes",
        "max_brick_requests_queued",
        "max_brick_queue_depth",
        "max_gpu_brick_atlas_uploaded_bytes",
        "max_gpu_brick_atlas_resident_bytes",
        "max_gpu_display_resource_resident_bytes",
    ];
    for (name, value) in map {
        if !ALLOWED_LIMITS.contains(&name.as_str()) {
            bail!("unknown automation script limit {name:?}");
        }
        if value.as_u64().is_none() {
            bail!("automation script limit {name:?} must be an unsigned integer");
        }
    }
    Ok(())
}

fn script_open_dataset_path(script: &Value) -> Option<PathBuf> {
    script
        .get("commands")
        .and_then(Value::as_array)?
        .iter()
        .find(|command| command.get("command").and_then(Value::as_str) == Some("open_dataset"))?
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn process_rss_limit_bytes(scenario: &ProductValidationScenario) -> Option<u64> {
    env::var(PRODUCT_VALIDATE_MAX_RSS_BYTES_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .or_else(|| scenario.default_process_rss_limit_bytes())
}

fn linux_process_rss_bytes(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_linux_status_rss_bytes(&status)
}

fn parse_linux_status_rss_bytes(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib = line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())?;
    Some(kib * 1024)
}

struct ProductAutomationRun<'a> {
    binary: &'a Path,
    package: &'a Path,
    script: &'a Path,
    automation_report: &'a Path,
    stdout_path: &'a Path,
    stderr_path: &'a Path,
    timeout: Duration,
    gpu_timestamps_requested: bool,
    max_rss_bytes: Option<u64>,
}

fn run_product_automation(run: ProductAutomationRun<'_>) -> anyhow::Result<ProductProcessStatus> {
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
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if run.gpu_timestamps_requested {
        command.env(APP_GPU_TIMESTAMPS_ENV, "1");
    }
    println!("running product validation: {:?}", command);
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch native app product validation binary {}",
            run.binary.display()
        )
    })?;
    let deadline = Instant::now() + run.timeout;
    let mut peak_rss_bytes = None;
    loop {
        if let Some(exit_status) = child
            .try_wait()
            .context("failed to poll product validation child process")?
        {
            return Ok(ProductProcessStatus {
                timed_out: false,
                rss_limit_exceeded: false,
                peak_rss_bytes,
                exit_status: Some(exit_status.to_string()),
                exit_success: Some(exit_status.success()),
            });
        }
        if let Some(max_rss_bytes) = run.max_rss_bytes
            && let Some(current_rss_bytes) = linux_process_rss_bytes(child.id())
        {
            peak_rss_bytes = Some(
                peak_rss_bytes.map_or(current_rss_bytes, |peak: u64| peak.max(current_rss_bytes)),
            );
            if current_rss_bytes > max_rss_bytes {
                let _ = child.kill();
                let exit_status = child.wait().ok().map(|status| status.to_string());
                return Ok(ProductProcessStatus {
                    timed_out: false,
                    rss_limit_exceeded: true,
                    peak_rss_bytes,
                    exit_status,
                    exit_success: None,
                });
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let exit_status = child.wait().ok().map(|status| status.to_string());
            return Ok(ProductProcessStatus {
                timed_out: true,
                rss_limit_exceeded: false,
                peak_rss_bytes,
                exit_status,
                exit_success: None,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug)]
struct ProductProcessStatus {
    timed_out: bool,
    rss_limit_exceeded: bool,
    peak_rss_bytes: Option<u64>,
    exit_status: Option<String>,
    exit_success: Option<bool>,
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
    script: &'a Path,
    script_value: &'a Value,
    automation_report: &'a Path,
    automation_report_value: Option<&'a Value>,
    stdout: &'a Path,
    stderr: &'a Path,
    display: DisplayClassification,
    gpu_timestamps_requested: bool,
    preflight_only: bool,
    process_rss_limit_bytes: Option<u64>,
    process_peak_rss_bytes: Option<u64>,
    process_rss_limit_exceeded: bool,
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
    let automation_summary = report
        .automation_report_value
        .and_then(|value| value.get("display_refresh_timing_summary"))
        .cloned()
        .unwrap_or(Value::Null);
    let app_update_summary = report
        .automation_report_value
        .and_then(|value| value.get("app_update_timing_summary"))
        .cloned()
        .unwrap_or(Value::Null);
    let input_to_present_summary = report
        .automation_report_value
        .and_then(|value| value.get("input_to_present_timing_summary"))
        .cloned()
        .unwrap_or(Value::Null);
    let cross_section_latency_summary = report
        .automation_report_value
        .and_then(|value| value.get("cross_section_latency_summary"))
        .cloned()
        .unwrap_or(Value::Null);
    let presentation_timing = report
        .automation_report_value
        .and_then(|value| {
            value.get("presentation_timing").or_else(|| {
                value
                    .get("final_diagnostics")
                    .and_then(|diagnostics| diagnostics.get("presentation_timing"))
            })
        })
        .cloned()
        .unwrap_or(Value::Null);
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
    let gpu_timestamp_timing = report
        .automation_report_value
        .and_then(|value| value.get("final_diagnostics"))
        .and_then(|value| value.get("gpu_timestamp_timing"))
        .cloned()
        .unwrap_or(Value::Null);
    let cross_section_runtime_metrics =
        product_validation_cross_section_runtime_metrics(report.automation_report_value);
    let cross_section_performance_gate_table =
        product_validation_cross_section_performance_gate_table(report.automation_report_value);
    let automation_script_scenario = report
        .script_value
        .get("scenario")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let scenario_name = report.scenario_name;
    let command_count = report
        .script_value
        .get("commands")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let viewport = script_viewport_json(report.script_value);
    let render_modes = script_render_modes_json(report.script_value);
    let frame_wait_count = script_frame_wait_count(report.script_value);
    let millis_wait_count = script_millis_wait_count(report.script_value);
    let wait_timeout_ms_total = script_wait_timeout_ms_total(report.script_value);
    let heavy_local_evidence = scenario_name == T5_QUAL_001_INTERACTION_MIP_SCENARIO
        || scenario_name == T5_QUAL_001_INTERACTION_RENDER_MODES_SCENARIO
        || scenario_name == T5_QUAL_001_INTERACTION_CONTINUOUS_SCENARIO
        || scenario_name == T5_QUAL_001_FOUR_PANEL_CROSS_SECTION_SCENARIO
        || scenario_name == T5_QUAL_001_FOUR_PANEL_FINE_SCALE_SCENARIO
        || scenario_name == T5_QUAL_001_FOUR_PANEL_CONTINUOUS_CROSS_SECTION_SCENARIO
        || scenario_name == T5_QUAL_002_FOUR_PANEL_TIMEPOINT_SCENARIO
        || scenario_name == T5_QUAL_002_FOUR_PANEL_AUTOPLAY_SCENARIO;
    let automation_limits = script_limits_json(report.script_value);
    let max_decoded_bytes = script_limit_u64(report.script_value, "max_decoded_bytes");
    let max_decoded_brick_bytes = script_limit_u64(report.script_value, "max_decoded_brick_bytes");
    let max_encoded_payload_bytes_read =
        script_limit_u64(report.script_value, "max_encoded_payload_bytes_read");
    let max_gpu_upload_bytes =
        script_limit_u64(report.script_value, "max_gpu_brick_atlas_uploaded_bytes");
    let max_gpu_resident_bytes =
        script_limit_u64(report.script_value, "max_gpu_brick_atlas_resident_bytes");
    let decoded_byte_limit_enforced = max_decoded_bytes.is_number()
        || max_decoded_brick_bytes.is_number()
        || max_encoded_payload_bytes_read.is_number();
    let gpu_upload_byte_limit_enforced = max_gpu_upload_bytes.is_number();
    let gpu_resident_byte_limit_enforced = max_gpu_resident_bytes.is_number();
    json!({
        "schema": PRODUCT_VALIDATION_SCHEMA,
        "schema_version": PRODUCT_VALIDATION_SCHEMA_VERSION,
        "command": "product-validate",
        "evidence_type": "native_window_product_automation",
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
        "binary": release_app_binary(),
        "host": host,
        "gpu_adapter": gpu_adapter,
        "gpu_timestamp_timing": gpu_timestamp_timing.clone(),
        "presentation_timing": presentation_timing.clone(),
        "dataset": dataset_context_json(report.package),
        "scenario": {
            "name": scenario_name,
            "automation_script_scenario": automation_script_scenario,
            "automation_script": report.script,
            "automation_status": report.automation_status,
            "command_count": command_count,
            "viewport": viewport.clone(),
            "render_modes": render_modes.clone(),
            "frame_wait_count": frame_wait_count,
            "millis_wait_count": millis_wait_count,
            "wait_timeout_ms_total": wait_timeout_ms_total,
            "automation_limits": automation_limits.clone(),
        },
        "limits": {
            "timeout_secs": report.timeout_secs,
            "viewport": viewport,
            "render_modes": render_modes,
            "heavy_local_evidence": heavy_local_evidence,
            "command_count": command_count,
            "frame_wait_count": frame_wait_count,
            "millis_wait_count": millis_wait_count,
            "wait_timeout_ms_total": wait_timeout_ms_total,
            "automation_limits": automation_limits,
            "decoded_byte_limit_bytes": max_decoded_bytes,
            "decoded_brick_byte_limit_bytes": max_decoded_brick_bytes,
            "encoded_payload_byte_limit_bytes": max_encoded_payload_bytes_read,
            "decoded_byte_limit_enforced": decoded_byte_limit_enforced,
            "gpu_upload_byte_limit_bytes": max_gpu_upload_bytes,
            "gpu_resident_byte_limit_bytes": max_gpu_resident_bytes,
            "gpu_upload_byte_limit_enforced": gpu_upload_byte_limit_enforced,
            "gpu_resident_byte_limit_enforced": gpu_resident_byte_limit_enforced,
            "process_rss_limit_bytes": report.process_rss_limit_bytes,
        },
        "metrics": {
            "duration_ms": report.duration_ms,
            "app_update_timing_summary": app_update_summary,
            "display_refresh_timing_summary": automation_summary,
            "input_to_present_timing_summary": input_to_present_summary,
            "cross_section_latency_summary": cross_section_latency_summary,
            "cross_section_performance_gate_table": cross_section_performance_gate_table,
            "cross_section_runtime": cross_section_runtime_metrics,
            "gpu_timestamp_timing": gpu_timestamp_timing,
            "presentation_timing": presentation_timing,
        },
        "artifacts": {
            "automation_report": report.automation_report,
            "automation_artifacts": automation_artifacts,
            "stdout": report.stdout,
            "stderr": report.stderr,
        },
        "logs": {
            "stdout": report.stdout,
            "stderr": report.stderr,
        },
        "environment": {
            "display": report.display.class.name(),
            "display_class": report.display.class.name(),
            "display_class_source": report.display.source,
            "display_env_present": env::var_os("DISPLAY").is_some(),
            "wayland_display_env_present": env::var_os("WAYLAND_DISPLAY").is_some(),
            "display_class_override_env": env::var(DISPLAY_CLASS_ENV).ok(),
            "product_validate_gpu_timestamps_env": env::var(PRODUCT_VALIDATE_GPU_TIMESTAMPS_ENV).ok(),
            "product_validate_gpu_timestamps_requested": report.gpu_timestamps_requested,
            "app_gpu_timestamps_env_set_by_wrapper": report.gpu_timestamps_requested,
            "product_validate_preflight_only_env": env::var(PREFLIGHT_ONLY_ENV).ok(),
            "product_validate_preflight_only": report.preflight_only,
            "product_validate_max_rss_bytes_env": env::var(PRODUCT_VALIDATE_MAX_RSS_BYTES_ENV).ok(),
        },
        "process": {
            "exit_status": report.exit_status,
            "exit_success": report.exit_success,
            "rss_limit_bytes": report.process_rss_limit_bytes,
            "peak_rss_bytes": report.process_peak_rss_bytes,
            "rss_limit_exceeded": report.process_rss_limit_exceeded,
        },
    })
}

fn script_limits_json(script: &Value) -> Value {
    script.get("limits").cloned().unwrap_or_else(|| json!({}))
}

fn product_validation_cross_section_performance_gate_table(
    automation_report_value: Option<&Value>,
) -> Value {
    const WARM_INTERACTION_GATE_MS: f64 = 250.0;
    let summary =
        automation_report_value.and_then(|value| value.get("cross_section_latency_summary"));
    let operations = [
        ("pan", "warm_cross_section_pan_input_to_current_partial"),
        ("zoom", "warm_cross_section_zoom_input_to_current_partial"),
        (
            "slice_shift",
            "warm_cross_section_slice_shift_input_to_current_partial",
        ),
        (
            "oblique_rotation",
            "warm_cross_section_oblique_rotation_input_to_current_partial",
        ),
        (
            "timepoint_change",
            "cold_cross_section_timepoint_change_input_to_current_partial",
        ),
    ];
    let rows = operations
        .into_iter()
        .map(|(operation, metric)| {
            let operation_summary = summary
                .and_then(|summary| summary.get("by_operation"))
                .and_then(|by_operation| by_operation.get(operation));
            let latency = operation_summary.and_then(|value| value.get("latency_ms"));
            let gate = operation_summary.and_then(|value| {
                value
                    .get("latency_gate")
                    .or_else(|| value.get("warm_interaction_gate"))
            });
            json!({
                "metric": metric,
                "operation": operation,
                "presentation_proxy": "panel_displayed_generation_with_gpu_display_frame",
                "gate_kind": gate
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str)
                    .unwrap_or(if operation == "timepoint_change" {
                        "cold_current_partial"
                    } else {
                        "warm_interaction"
                    }),
                "sample_count": latency
                    .and_then(|value| value.get("sample_count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                "p50_ms": latency
                    .and_then(|value| value.get("p50"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "p95_ms": latency
                    .and_then(|value| value.get("p95"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "p99_ms": latency
                    .and_then(|value| value.get("p99"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "max_ms": latency
                    .and_then(|value| value.get("max"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "threshold_ms": gate
                    .and_then(|value| value.get("threshold_ms"))
                    .and_then(Value::as_f64)
                    .unwrap_or(if operation == "timepoint_change" {
                        2000.0
                    } else {
                        WARM_INTERACTION_GATE_MS
                    }),
                "status": gate
                    .and_then(|value| value.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("missing_samples"),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "kind": "cross_section_performance_gate_table",
        "taxonomy_version": 1,
        "source": "product_automation_cross_section_latency_summary",
        "measurement_scope": "automation_cross_section_command_start_to_panel_displayed_generation",
        "overall_status": summary
            .and_then(|summary| summary.get("operation_gate").or_else(|| summary.get("warm_interaction_gate")))
            .and_then(|gate| gate.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("missing_samples"),
        "pending_sample_count": summary
            .and_then(|summary| summary.get("pending_sample_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "rows": rows,
    })
}

fn product_validation_cross_section_runtime_metrics(
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
    let latest_assertion = snapshots.last().cloned().unwrap_or(Value::Null);
    let latest_visible_work_assertion = snapshots
        .iter()
        .rev()
        .find(|snapshot| {
            snapshot.get("layout").and_then(Value::as_str) == Some("FourPanel")
                && snapshot
                    .get("runtime")
                    .and_then(|runtime| runtime.get("visible_work"))
                    .and_then(Value::as_bool)
                    == Some(true)
        })
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "kind": "cross_section_runtime_metrics",
        "taxonomy_version": 1,
        "snapshot_source": "automation_assertion_details_and_final_diagnostics",
        "snapshot_count": snapshots.len(),
        "final": final_snapshot,
        "latest_assertion": latest_assertion,
        "latest_visible_work_assertion": latest_visible_work_assertion,
    })
}

fn script_limit_u64(script: &Value, name: &str) -> Value {
    script
        .get("limits")
        .and_then(|limits| limits.get(name))
        .and_then(Value::as_u64)
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn script_viewport_json(script: &Value) -> Value {
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

fn script_frame_wait_count(script: &Value) -> u64 {
    script_commands(script).map_or(0, |commands| {
        commands
            .iter()
            .filter(|command| {
                command.get("command").and_then(Value::as_str) == Some("sleep_or_frames")
                    && command.get("frames").and_then(Value::as_u64).is_some()
            })
            .count() as u64
    })
}

fn script_millis_wait_count(script: &Value) -> u64 {
    script_commands(script).map_or(0, |commands| {
        commands
            .iter()
            .filter(|command| {
                command.get("command").and_then(Value::as_str) == Some("sleep_or_frames")
                    && command.get("millis").and_then(Value::as_u64).is_some()
            })
            .count() as u64
    })
}

fn script_wait_timeout_ms_total(script: &Value) -> u64 {
    script_commands(script).map_or(0, |commands| {
        commands
            .iter()
            .filter(|command| command.get("command").and_then(Value::as_str) == Some("wait_for"))
            .filter_map(|command| command.get("timeout_ms").and_then(Value::as_u64))
            .sum()
    })
}

fn script_commands(script: &Value) -> Option<&Vec<Value>> {
    script.get("commands").and_then(Value::as_array)
}

fn dataset_context_json(package: &Path) -> Value {
    match load_and_validate_dataset_quick(package) {
        Ok(dataset) => {
            let manifest = dataset.manifest;
            let active_layer = manifest.layers.first().map(|layer| {
                json!({
                    "id": layer.id,
                    "name": layer.name,
                    "kind": layer.kind,
                    "shape": layer.shape,
                    "dtype": {
                        "source": layer.dtype.source,
                        "stored": layer.dtype.stored,
                        "conversion": layer.dtype.conversion,
                    },
                    "scale_count": layer.scales.len(),
                    "timepoint_count": layer.shape.t,
                })
            });
            let timepoint_count = active_layer
                .as_ref()
                .and_then(|layer| layer.get("timepoint_count"))
                .cloned();
            json!({
                "package_path": package,
                "manifest_status": "loaded",
                "format": manifest.format,
                "schema_version": manifest.schema_version,
                "id": manifest.dataset.id,
                "name": manifest.dataset.name,
                "layer_count": manifest.layers.len(),
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

fn product_validate_gpu_timestamps_requested() -> bool {
    env::var(PRODUCT_VALIDATE_GPU_TIMESTAMPS_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
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
