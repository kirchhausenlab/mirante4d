use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{Context, bail};
use mirante4d_renderer::gpu::{
    AdapterDiagnostics, GpuLimitDiagnostics, GpuRenderer, GpuRendererStats,
};
use serde_json::{Value, json};

use crate::process::{
    cargo_command, ensure_cargo_subcommand, ensure_nextest, run_cargo, run_command,
    run_command_with_timeout,
};
use crate::reports::{read_json_file, write_json_file};

const COVERAGE_OUTPUT_DIR: &str = "target/mirante4d/coverage";
const COVERAGE_HTML_DIR: &str = "target/mirante4d/coverage/html";
const COVERAGE_SUMMARY_JSON: &str = "target/mirante4d/coverage/summary.json";
const COVERAGE_LCOV: &str = "target/mirante4d/coverage/lcov.info";
const MUTANTS_OUTPUT_DIR: &str = "target/mirante4d/mutants";
const VERIFY_RENDER_OUTPUT_DIR: &str = "target/mirante4d/verify-render";
const VERIFY_RENDER_REPORT_JSON: &str = "target/mirante4d/verify-render/verify-render-report.json";
const VERIFY_RENDER_REPORT_SCHEMA: &str = "mirante4d-verify-render-report";
const VERIFY_RENDER_REPORT_SCHEMA_VERSION: u32 = 1;
const VERIFY_UI_OUTPUT_DIR: &str = "target/mirante4d/verify-ui";
const VERIFY_UI_REPORT_JSON: &str = "target/mirante4d/verify-ui/verify-ui-report.json";
const VERIFY_UI_REPORT_SCHEMA: &str = "mirante4d-verify-ui-report";
const VERIFY_UI_REPORT_SCHEMA_VERSION: u32 = 1;
const VERIFY_UI_TEST_TIMEOUT_SECS_ENV: &str = "MIRANTE4D_VERIFY_UI_TEST_TIMEOUT_SECS";
const VERIFY_UI_SNAPSHOT_TIMEOUT_SECS_ENV: &str = "MIRANTE4D_VERIFY_UI_SNAPSHOT_TIMEOUT_SECS";
const DEFAULT_VERIFY_UI_TEST_TIMEOUT_SECS: u64 = 60;
const DEFAULT_VERIFY_UI_SNAPSHOT_TIMEOUT_SECS: u64 = 240;
const VERIFY_E2E_OUTPUT_DIR: &str = "target/mirante4d/verify-e2e";
const VERIFY_E2E_REPORT_JSON: &str = "target/mirante4d/verify-e2e/verify-e2e-report.json";
const VERIFY_E2E_REPORT_SCHEMA: &str = "mirante4d-verify-e2e-report";
const VERIFY_E2E_REPORT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_NIGHTLY_FUZZ_SECONDS: u64 = 30;
const DEFAULT_MUTATION_PACKAGES: &[&str] = &["mirante4d-core", "mirante4d-format"];
const VERIFY_BOOTSTRAP_TARGET_DIR: &str = "target/mirante4d/verify-bootstrap";
const VERIFY_BOOTSTRAP_FORMAT_TIMEOUT: Duration = Duration::from_secs(60);
const VERIFY_BOOTSTRAP_CHECK_TIMEOUT: Duration = Duration::from_secs(480);
const VERIFY_BOOTSTRAP_TEST_TIMEOUT: Duration = Duration::from_secs(240);
const VERIFY_BOOTSTRAP_EXPECTED_TESTS: u64 = 169;
const VERIFY_BOOTSTRAP_RUSTC_VERSION: &str = "rustc 1.96.1 (31fca3adb 2026-06-26)";
const VERIFY_BOOTSTRAP_CARGO_VERSION: &str = "cargo 1.96.1 (356927216 2026-06-26)";
const VERIFY_BOOTSTRAP_NEXTEST_VERSION: &str = "cargo-nextest 0.9.138 (fc97e97bb 2026-06-21)";
const VERIFY_BOOTSTRAP_RUMDL_VERSION: &str = "rumdl 0.2.30";
const VERIFY_BOOTSTRAP_FILTER: &str = concat!(
    "package(mirante4d-core) | package(mirante4d-format) | ",
    "package(mirante4d-data) | package(mirante4d-import) | ",
    "(package(mirante4d-analysis) & (",
    "test(=operations::tests::operation_record_validates_and_serializes_exact_roi_scope) | ",
    "test(=results::tests::roi_box_intensity_measurement_uses_source_volume_values) | ",
    "test(=scene_artifacts::tests::scene_artifact_store_applies_undoes_and_redoes_commands))) | ",
    "(package(mirante4d-renderer) & (",
    "test(=brick_render::tests::complete_resident_bricks_match_dense_camera_mip) | ",
    "test(=cross_section::tests::oblique_slab_culls_to_local_brick_subset) | ",
    "test(=scene::tests::extraction_filters_hidden_layers_objects_and_timepoints))) | ",
    "(package(mirante4d-app) & (",
    "test(=tests::app_shell_opens_fixture_and_renders_first_frame) | ",
    "test(=tests::project_package_roundtrip_restores_layer_display_states) | ",
    "test(=tests::workbench_commands_update_core_viewer_state))) | ",
    "(package(xtask) & (",
    "test(=command_audit::tests::command_audit_covers_current_xtask_command_surface) | ",
    "test(=verify::tests::bootstrap_scope_is_explicit_and_bounded)))",
);
const NIGHTLY_FUZZ_TARGETS: &[&str] = &["manifest_parser", "project_metadata_parser"];
const E2E_LIBRARY_TESTS: &[(&str, &str)] = &[
    (
        "mirante4d-app",
        "app_shell_opens_fixture_and_renders_first_frame",
    ),
    (
        "mirante4d-app",
        "project_package_rejects_dataset_manifest_fingerprint_mismatch",
    ),
    ("mirante4d-app", "rejects_file_based_m4dproj_sessions"),
    (
        "mirante4d-import",
        "imports_single_uint16_tiff_file_to_native_dataset",
    ),
    (
        "mirante4d-import",
        "imports_directory_with_explicit_grouping_without_filename_tokens",
    ),
    (
        "mirante4d-import",
        "inspection_extracts_ome_tiff_voxel_spacing_metadata",
    ),
    (
        "mirante4d-import",
        "import_rejects_unaccepted_review_plan_before_native_output",
    ),
    (
        "mirante4d-import",
        "import_rejects_review_plan_with_tampered_value_range",
    ),
    (
        "mirante4d-format",
        "validator_rejects_invalid_native_provenance",
    ),
    (
        "mirante4d-app",
        "prepare_tiff_import_prefills_ome_voxel_spacing_without_confirming_review",
    ),
    (
        "mirante4d-app",
        "project_package_roundtrip_restores_layer_display_states",
    ),
    (
        "mirante4d-app",
        "workbench_commands_update_core_viewer_state",
    ),
    (
        "mirante4d-app",
        "frame_fidelity_label_reports_display_staleness_when_known",
    ),
    (
        "mirante4d-app",
        "project_package_roundtrip_restores_analysis_results",
    ),
    (
        "mirante4d-app",
        "app_full_time_series_analysis_uses_data_engine_and_exports_csv",
    ),
    (
        "mirante4d-app",
        "app_roi_analysis_uses_roi_artifacts_and_data_engine_volume_reads",
    ),
];
const E2E_VIRTUAL_PRODUCT_AUTOMATION_TESTS: &[(&str, &str)] = &[
    (
        "mirante4d-app",
        "virtual_product_automation_generated_fixture_camera_sequence",
    ),
    (
        "mirante4d-app",
        "virtual_product_automation_generated_fixture_render_mode_sequence",
    ),
];
const E2E_REAL_WINDOW_PRODUCT_AUTOMATION_SCENARIOS: &[&str] = &[
    "generated_fixture_camera_smoke",
    "generated_fixture_render_modes",
    "custom_script",
];
const E2E_CUSTOM_SCRIPT_SOURCE: &str = "target/mirante4d/product-validation/generated_fixture_camera_smoke/product-automation-script.json";
const VERIFY_RENDER_TESTS: &[VerifyRenderTest] = &[
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_renderer_rejects_existing_default_limit_device",
        evidence_type: "existing_device_limit_rejection",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_renderer_constructs_from_existing_renderer_limit_device",
        evidence_type: "existing_device_product_limits",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_camera_volume_modes_match_cpu_camera_and_preserve_orthographic_invariants",
        evidence_type: "dense_camera_pixel_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_bricked_camera_modes_match_cpu_resident_bricks",
        evidence_type: "resident_brick_pixel_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_float32_bricked_camera_modes_match_cpu_resident_bricks",
        evidence_type: "resident_float32_pixel_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_resident_display_texture_matches_cpu_intensity_compositor",
        evidence_type: "display_texture_pixel_and_resource_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_display_frame_blend_pass_matches_additive_display_frame_reference",
        evidence_type: "display_texture_blend_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_resident_display_texture_matches_cpu_same_ray_multi_channel_dvr",
        evidence_type: "display_texture_dvr_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_resident_display_texture_matches_cpu_depth_sorted_multi_channel_iso",
        evidence_type: "display_texture_iso_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_scene_renderer_draws_world_interaction_and_screen_primitives",
        evidence_type: "scene_overlay_display_texture",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_scene_pick_returns_topmost_selectable_command_id",
        evidence_type: "scene_pick_readback",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_intensity_summary_matches_cpu_volume_summary",
        evidence_type: "gpu_summary_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-renderer",
        filter: "gpu_float32_intensity_summary_matches_cpu_volume_summary",
        evidence_type: "gpu_float32_summary_parity",
    },
    VerifyRenderTest {
        package: "mirante4d-app",
        filter: "app_backend_uses_gpu_for_volume_modes_when_renderer_available",
        evidence_type: "app_dense_gpu_backend",
    },
    VerifyRenderTest {
        package: "mirante4d-app",
        filter: "app_backend_uses_gpu_resident_bricks_when_stream_complete",
        evidence_type: "app_resident_gpu_backend",
    },
    VerifyRenderTest {
        package: "mirante4d-app",
        filter: "app_backend_uses_gpu_resident_bricks_for_float32_layers",
        evidence_type: "app_resident_float32_gpu_backend",
    },
    VerifyRenderTest {
        package: "mirante4d-app",
        filter: "app_backend_uses_gpu_resident_bricks_for_visible_channel_layers",
        evidence_type: "app_multichannel_resident_gpu_backend",
    },
];
const VERIFY_UI_TESTS: &[VerifyUiTest] = &[
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_exposes_primary_regions_at_high_dpi",
        evidence_type: "high_dpi_shell_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_handles_long_dataset_name_in_narrow_layout",
        evidence_type: "long_label_narrow_shell_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_exposes_channel_display_controls",
        evidence_type: "channel_controls_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_exposes_histogram_and_auto_window_controls",
        evidence_type: "histogram_controls_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_exposes_playback_controls_for_time_series",
        evidence_type: "playback_controls_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_runtime_diagnostics_exposes_data_runtime_budgets",
        evidence_type: "diagnostics_panel_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_settings_panel_exposes_runtime_budget_controls",
        evidence_type: "narrow_settings_panel_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "dirty_project_close_prompt_exposes_save_discard_and_cancel",
        evidence_type: "narrow_modal_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "tiff_import_setup_window_exposes_inspection_state",
        evidence_type: "import_modal_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "analysis_workspace_window_exposes_selected_results",
        evidence_type: "analysis_workspace_semantic_layout",
        run_ignored: false,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_image_snapshot_matches_baseline",
        evidence_type: "workbench_shell_visual_snapshot",
        run_ignored: true,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_image_snapshot_iso_hidpi_matches_baseline",
        evidence_type: "workbench_shell_iso_hidpi_visual_snapshot",
        run_ignored: true,
    },
    VerifyUiTest {
        package: "mirante4d-app",
        filter: "workbench_shell_image_snapshot_dvr_hidpi_matches_baseline",
        evidence_type: "workbench_shell_dvr_hidpi_visual_snapshot",
        run_ignored: true,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifyRenderTest {
    package: &'static str,
    filter: &'static str,
    evidence_type: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifyUiTest {
    package: &'static str,
    filter: &'static str,
    evidence_type: &'static str,
    run_ignored: bool,
}

pub(crate) fn verify_bootstrap() -> anyhow::Result<()> {
    ensure_bootstrap_tool_versions()?;

    let cache_state = if Path::new(VERIFY_BOOTSTRAP_TARGET_DIR).exists() {
        "warm"
    } else {
        "cold"
    };
    println!(
        "verify-bootstrap: partial WP-01 feedback only; cache={cache_state}; target={VERIFY_BOOTSTRAP_TARGET_DIR}"
    );
    println!(
        "verify-bootstrap excludes the complete suite, Clippy, doctests, dependencies, GPU, UI snapshots, E2E, packaging, performance, real data, and product-open validation"
    );

    run_bootstrap_cargo(
        &["fmt", "--all", "--check"],
        VERIFY_BOOTSTRAP_FORMAT_TIMEOUT,
    )?;
    run_bootstrap_cargo(
        &["check", "--workspace", "--all-targets", "--frozen"],
        VERIFY_BOOTSTRAP_CHECK_TIMEOUT,
    )?;
    run_bootstrap_tests()?;

    crate::documentation::docs_check()?;

    println!(
        "verify-bootstrap passed its declared partial scope; deeper verification remains required"
    );
    Ok(())
}

fn run_bootstrap_cargo(args: &[&str], timeout: Duration) -> anyhow::Result<()> {
    let mut command = bootstrap_cargo_command(args);
    run_bootstrap_command_with_timeout(&mut command, timeout)
}

fn bootstrap_cargo_command(args: &[&str]) -> Command {
    let mut command = cargo_command();
    command
        .args(args)
        .env("CARGO_TARGET_DIR", VERIFY_BOOTSTRAP_TARGET_DIR)
        .env("CARGO_INCREMENTAL", "0");
    if args.first() == Some(&"nextest") {
        command.env("NEXTEST_USER_CONFIG_FILE", "none");
    }
    command
}

fn run_bootstrap_tests() -> anyhow::Result<()> {
    let started = Instant::now();
    let list_path = PathBuf::from(VERIFY_BOOTSTRAP_TARGET_DIR).join("selected-tests.json");
    if let Some(parent) = list_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let result = (|| {
        let list_output = fs::File::create(&list_path)
            .with_context(|| format!("failed to create {}", list_path.display()))?;
        let mut list = bootstrap_cargo_command(&[
            "nextest",
            "list",
            "--workspace",
            "--frozen",
            "--profile",
            "bootstrap",
            "-E",
            VERIFY_BOOTSTRAP_FILTER,
            "--message-format",
            "json",
        ]);
        list.stdout(Stdio::from(list_output));
        run_bootstrap_command_with_timeout(&mut list, VERIFY_BOOTSTRAP_TEST_TIMEOUT)?;

        let discovery: Value = serde_json::from_slice(
            &fs::read(&list_path)
                .with_context(|| format!("failed to read {}", list_path.display()))?,
        )
        .context("failed to parse verify-bootstrap Nextest discovery")?;
        let selected = discovery
            .get("rust-suites")
            .and_then(Value::as_object)
            .context("verify-bootstrap Nextest discovery is missing rust-suites")?
            .values()
            .map(|suite| {
                suite
                    .get("testcases")
                    .and_then(Value::as_object)
                    .context("verify-bootstrap Nextest suite is missing testcases")
                    .map(|testcases| {
                        testcases
                            .values()
                            .filter(|testcase| {
                                testcase
                                    .pointer("/filter-match/status")
                                    .and_then(Value::as_str)
                                    == Some("matches")
                            })
                            .count() as u64
                    })
            })
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .sum::<u64>();
        if selected != VERIFY_BOOTSTRAP_EXPECTED_TESTS {
            bail!(
                "verify-bootstrap selector drift: expected {VERIFY_BOOTSTRAP_EXPECTED_TESTS} tests, found {selected}"
            );
        }
        println!("verify-bootstrap selector resolved to {selected} tests");

        let remaining = VERIFY_BOOTSTRAP_TEST_TIMEOUT
            .checked_sub(started.elapsed())
            .context("verify-bootstrap test discovery exhausted the 240 second phase timeout")?;
        let mut run = bootstrap_cargo_command(&[
            "nextest",
            "run",
            "--workspace",
            "--frozen",
            "--profile",
            "bootstrap",
            "--no-fail-fast",
            "-E",
            VERIFY_BOOTSTRAP_FILTER,
        ]);
        run_bootstrap_command_with_timeout(&mut run, remaining)
    })();
    let _ = fs::remove_file(&list_path);
    result
}

fn ensure_bootstrap_tool_versions() -> anyhow::Result<()> {
    let mut rustc = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    rustc.arg("--version");
    require_exact_version(
        &mut rustc,
        "Rust",
        VERIFY_BOOTSTRAP_RUSTC_VERSION,
        "use the repository rust-toolchain.toml without an overriding RUSTUP_TOOLCHAIN",
    )?;

    let mut cargo = cargo_command();
    cargo.arg("--version");
    require_exact_version(
        &mut cargo,
        "Cargo",
        VERIFY_BOOTSTRAP_CARGO_VERSION,
        "use the repository rust-toolchain.toml without an overriding RUSTUP_TOOLCHAIN",
    )?;

    let mut nextest = cargo_command();
    nextest.args(["nextest", "--version"]);
    require_exact_version(
        &mut nextest,
        "cargo-nextest",
        VERIFY_BOOTSTRAP_NEXTEST_VERSION,
        "install cargo-nextest 0.9.138 with `cargo install cargo-nextest --version 0.9.138 --locked`",
    )?;

    let mut rumdl = rumdl_command();
    rumdl.arg("--version");
    require_exact_version(
        &mut rumdl,
        "rumdl",
        VERIFY_BOOTSTRAP_RUMDL_VERSION,
        "install rumdl 0.2.30 or set MIRANTE4D_RUMDL to its executable",
    )?;
    Ok(())
}

fn require_exact_version(
    command: &mut Command,
    tool: &str,
    expected: &str,
    remediation: &str,
) -> anyhow::Result<()> {
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            bail!("{tool} {expected:?} is required; {remediation}: {error}")
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual = stdout.lines().next().unwrap_or_default().trim();
    if !output.status.success() || actual != expected {
        bail!("{tool} version mismatch: expected {expected:?}, found {actual:?}; {remediation}")
    }
    println!("verify-bootstrap tool: {actual}");
    Ok(())
}

fn run_bootstrap_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    command.process_group(0);

    println!("running with timeout {timeout:?}: {:?}", command);
    let mut child = command.spawn().context("failed to spawn command")?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("failed to poll command status")? {
            if status.success() {
                return Ok(());
            }
            bail!("command failed with status {status}: {:?}", command);
        }
        if Instant::now() >= deadline {
            terminate_bootstrap_process_tree(&mut child);
            let _ = child.wait();
            bail!("command timed out after {timeout:?}: {:?}", command);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn terminate_bootstrap_process_tree(child: &mut std::process::Child) {
    const SIGKILL: i32 = 9;
    let process_group = -(child.id() as i32);
    // SAFETY: this sends SIGKILL only to the process group created immediately
    // before spawning this bridge phase.
    unsafe {
        kill(process_group, SIGKILL);
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(not(unix))]
fn terminate_bootstrap_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn rumdl_command() -> Command {
    Command::new(env::var_os("MIRANTE4D_RUMDL").unwrap_or_else(|| "rumdl".into()))
}

pub(crate) fn verify_fast() -> anyhow::Result<()> {
    ensure_nextest()?;
    crate::arch::architecture_self_check()?;
    run_cargo(["fmt", "--all", "--check"])?;
    run_cargo([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])?;
    run_cargo(["nextest", "run", "--workspace", "--all-targets"])?;
    run_cargo(["test", "--workspace", "--doc"])?;
    Ok(())
}

pub(crate) fn verify_full() -> anyhow::Result<()> {
    verify_fast()?;
    verify_deps()
}

pub(crate) fn verify_coverage() -> anyhow::Result<()> {
    ensure_nextest()?;
    ensure_cargo_subcommand(
        "llvm-cov",
        "cargo-llvm-cov",
        "cargo install cargo-llvm-cov --locked",
    )?;
    fs::create_dir_all(COVERAGE_OUTPUT_DIR)
        .with_context(|| format!("failed to create {COVERAGE_OUTPUT_DIR}"))?;
    run_cargo(["llvm-cov", "clean", "--workspace"])?;
    run_cargo([
        "llvm-cov",
        "nextest",
        "--workspace",
        "--all-targets",
        "--html",
        "--output-dir",
        COVERAGE_HTML_DIR,
    ])?;
    run_cargo([
        "llvm-cov",
        "report",
        "--json",
        "--summary-only",
        "--output-path",
        COVERAGE_SUMMARY_JSON,
    ])?;
    run_cargo([
        "llvm-cov",
        "report",
        "--lcov",
        "--output-path",
        COVERAGE_LCOV,
    ])
}

pub(crate) fn verify_nightly() -> anyhow::Result<()> {
    verify_full()?;
    verify_e2e()?;
    verify_coverage()?;
    verify_fuzz_targets()?;
    verify_mutation_audit()?;
    crate::bench::bench_smoke()?;
    crate::bench::bench_runtime_stress()?;
    if let Some(experiment) = env::var_os("MIRANTE4D_NIGHTLY_SAMPLE_EXPERIMENT") {
        let experiment = experiment.to_string_lossy().into_owned();
        crate::bench::bench_import_sample(&experiment)?;
    }
    Ok(())
}

pub(crate) fn verify_deps() -> anyhow::Result<()> {
    crate::deps::verify_deps()
}

pub(crate) fn verify_render() -> anyhow::Result<()> {
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    ensure_nextest()?;
    fs::create_dir_all(VERIFY_RENDER_OUTPUT_DIR)
        .with_context(|| format!("failed to create {VERIFY_RENDER_OUTPUT_DIR}"))?;
    let report_path = Path::new(VERIFY_RENDER_REPORT_JSON);

    let renderer_probe = match probe_verify_render_gpu() {
        Ok(probe) => probe,
        Err(err) => {
            write_verify_render_report(VerifyRenderReportInput {
                path: report_path,
                started_at_epoch_ms,
                duration_ms: duration_ms(started_at.elapsed()),
                status: "failed",
                failure_reason: Some(format!("GPU renderer probe failed: {err}")),
                renderer_probe: Value::Null,
                tests: Vec::new(),
            })?;
            bail!(
                "verify-render GPU renderer probe failed; see {}: {err}",
                report_path.display()
            );
        }
    };

    let mut tests = Vec::with_capacity(VERIFY_RENDER_TESTS.len());
    for test in VERIFY_RENDER_TESTS {
        println!("verify-render: {}::{}", test.package, test.filter);
        let result = run_cargo([
            "nextest",
            "run",
            "-p",
            test.package,
            "--run-ignored",
            "only",
            "--no-capture",
            test.filter,
        ]);
        match result {
            Ok(()) => tests.push(verify_render_test_json(*test, "passed", None)),
            Err(err) => {
                let failure_reason = err.to_string();
                tests.push(verify_render_test_json(
                    *test,
                    "failed",
                    Some(failure_reason.clone()),
                ));
                write_verify_render_report(VerifyRenderReportInput {
                    path: report_path,
                    started_at_epoch_ms,
                    duration_ms: duration_ms(started_at.elapsed()),
                    status: "failed",
                    failure_reason: Some(format!(
                        "verify-render test {}::{} failed: {failure_reason}",
                        test.package, test.filter
                    )),
                    renderer_probe,
                    tests,
                })?;
                bail!(
                    "verify-render test {}::{} failed; see {}: {failure_reason}",
                    test.package,
                    test.filter,
                    report_path.display()
                );
            }
        }
    }

    write_verify_render_report(VerifyRenderReportInput {
        path: report_path,
        started_at_epoch_ms,
        duration_ms: duration_ms(started_at.elapsed()),
        status: "passed",
        failure_reason: None,
        renderer_probe,
        tests,
    })?;
    println!("verify-render report: {}", report_path.display());
    Ok(())
}

pub(crate) fn verify_ui() -> anyhow::Result<()> {
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    ensure_nextest()?;
    fs::create_dir_all(VERIFY_UI_OUTPUT_DIR)
        .with_context(|| format!("failed to create {VERIFY_UI_OUTPUT_DIR}"))?;
    let report_path = Path::new(VERIFY_UI_REPORT_JSON);

    let mut tests = Vec::with_capacity(VERIFY_UI_TESTS.len());
    for test in VERIFY_UI_TESTS {
        println!("verify-ui: {}::{}", test.package, test.filter);
        let timeout = verify_ui_test_timeout(*test);
        let result = run_verify_ui_test(*test, timeout);
        match result {
            Ok(()) => tests.push(verify_ui_test_json(*test, "passed", None, timeout)),
            Err(err) => {
                let failure_reason = err.to_string();
                tests.push(verify_ui_test_json(
                    *test,
                    "failed",
                    Some(failure_reason.clone()),
                    timeout,
                ));
                write_verify_ui_report(VerifyUiReportInput {
                    path: report_path,
                    started_at_epoch_ms,
                    duration_ms: duration_ms(started_at.elapsed()),
                    status: "failed",
                    failure_reason: Some(format!(
                        "verify-ui test {}::{} failed: {failure_reason}",
                        test.package, test.filter
                    )),
                    tests,
                })?;
                bail!(
                    "verify-ui test {}::{} failed; see {}: {failure_reason}",
                    test.package,
                    test.filter,
                    report_path.display()
                );
            }
        }
    }

    write_verify_ui_report(VerifyUiReportInput {
        path: report_path,
        started_at_epoch_ms,
        duration_ms: duration_ms(started_at.elapsed()),
        status: "passed",
        failure_reason: None,
        tests,
    })?;
    println!("verify-ui report: {}", report_path.display());
    Ok(())
}

pub(crate) fn verify_e2e() -> anyhow::Result<()> {
    let started_at = Instant::now();
    let started_at_epoch_ms = epoch_ms();
    ensure_nextest()?;
    fs::create_dir_all(VERIFY_E2E_OUTPUT_DIR)
        .with_context(|| format!("failed to create {VERIFY_E2E_OUTPUT_DIR}"))?;

    println!("verify-e2e: library workflow tests");
    let mut library_tests = Vec::with_capacity(E2E_LIBRARY_TESTS.len());
    for &(package, test_filter) in E2E_LIBRARY_TESTS {
        println!("verify-e2e: library {package}::{test_filter}");
        run_cargo(["nextest", "run", "-p", package, "--no-capture", test_filter])?;
        library_tests.push(json!({
            "package": package,
            "filter": test_filter,
            "status": "passed",
        }));
    }

    println!("verify-e2e: virtual-window product automation");
    let mut virtual_product_tests = Vec::with_capacity(E2E_VIRTUAL_PRODUCT_AUTOMATION_TESTS.len());
    for &(package, test_filter) in E2E_VIRTUAL_PRODUCT_AUTOMATION_TESTS {
        println!("verify-e2e: virtual product automation {package}::{test_filter}");
        run_cargo(["nextest", "run", "-p", package, "--no-capture", test_filter])?;
        virtual_product_tests.push(json!({
            "package": package,
            "filter": test_filter,
            "status": "passed",
        }));
    }

    println!("verify-e2e: real-window product automation");
    let mut real_product_scenarios =
        Vec::with_capacity(E2E_REAL_WINDOW_PRODUCT_AUTOMATION_SCENARIOS.len());
    let mut real_product_failed = false;
    let mut first_failed_product_report = None;
    for scenario in E2E_REAL_WINDOW_PRODUCT_AUTOMATION_SCENARIOS {
        println!("verify-e2e: real product automation scenario {scenario}");
        let product_outcome = if *scenario == "custom_script" {
            crate::product_validate::product_validate_report_with_custom_script(
                None,
                Path::new(E2E_CUSTOM_SCRIPT_SOURCE),
            )?
        } else {
            crate::product_validate::product_validate_report_with_scenario(None, Some(scenario))?
        };
        let product_report_path = product_outcome.report_path;
        let product_report = read_json_file(&product_report_path)
            .with_context(|| format!("failed to read {}", product_report_path.display()))?;
        let product_status = product_report
            .get("status")
            .and_then(|status| status.as_str())
            .unwrap_or("missing")
            .to_owned();
        let product_failure = product_report
            .get("failure_reason")
            .and_then(|reason| reason.as_str())
            .map(str::to_owned);
        let product_scenario_failed =
            product_outcome.status.is_failure() || matches!(product_status.as_str(), "missing");
        if product_scenario_failed {
            real_product_failed = true;
            if first_failed_product_report.is_none() {
                first_failed_product_report = Some(product_report_path.clone());
            }
        }
        real_product_scenarios.push(json!({
            "scenario": scenario,
            "status": product_status,
            "failure_reason": product_failure,
            "product_validation_report": product_report_path,
        }));
    }
    let command_status = if real_product_failed {
        "failed"
    } else {
        "passed"
    };

    let report_path = Path::new(VERIFY_E2E_REPORT_JSON);
    let report = verify_e2e_report_json(VerifyE2eReportInput {
        started_at_epoch_ms,
        duration_ms: duration_ms(started_at.elapsed()),
        status: command_status,
        failure_reason: first_failed_product_report.as_ref().map(|path| {
            format!(
                "real-window product automation failed; see {}",
                path.display()
            )
        }),
        library_tests,
        virtual_product_tests,
        real_product_scenarios,
    });
    write_json_file(report_path, &report)?;
    println!("verify-e2e report: {}", report_path.display());

    if real_product_failed {
        let failed_report = first_failed_product_report
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<missing report path>".to_owned());
        bail!("verify-e2e real-window product automation failed; see {failed_report}");
    }
    Ok(())
}

struct VerifyRenderReportInput<'a> {
    path: &'a Path,
    started_at_epoch_ms: u128,
    duration_ms: f64,
    status: &'static str,
    failure_reason: Option<String>,
    renderer_probe: Value,
    tests: Vec<Value>,
}

struct VerifyUiReportInput<'a> {
    path: &'a Path,
    started_at_epoch_ms: u128,
    duration_ms: f64,
    status: &'static str,
    failure_reason: Option<String>,
    tests: Vec<Value>,
}

struct VerifyE2eReportInput {
    started_at_epoch_ms: u128,
    duration_ms: f64,
    status: &'static str,
    failure_reason: Option<String>,
    library_tests: Vec<Value>,
    virtual_product_tests: Vec<Value>,
    real_product_scenarios: Vec<Value>,
}

fn probe_verify_render_gpu() -> anyhow::Result<Value> {
    let renderer = GpuRenderer::new_blocking().context("failed to initialize GPU renderer")?;
    let adapter_diagnostics = renderer.adapter_diagnostics();
    let adapter = adapter_diagnostics_json(adapter_diagnostics);
    let gpu_timestamp_timing = gpu_timestamp_timing_json(adapter_diagnostics);
    let initial_stats = gpu_renderer_stats_json(
        renderer
            .stats()
            .context("failed to collect initial GPU renderer stats")?,
    );

    Ok(json!({
        "status": "available",
        "adapter": adapter,
        "gpu_timestamp_timing": gpu_timestamp_timing,
        "initial_stats": initial_stats,
    }))
}

fn write_verify_render_report(input: VerifyRenderReportInput<'_>) -> anyhow::Result<()> {
    let path = input.path;
    let report = verify_render_report_json(input);
    write_json_file(path, &report)
}

fn verify_render_report_json(input: VerifyRenderReportInput<'_>) -> Value {
    let passed = input
        .tests
        .iter()
        .filter(|test| test.get("status").and_then(Value::as_str) == Some("passed"))
        .count();
    let failed = input
        .tests
        .iter()
        .filter(|test| test.get("status").and_then(Value::as_str) == Some("failed"))
        .count();
    let gpu_adapter = input
        .renderer_probe
        .get("adapter")
        .cloned()
        .unwrap_or(Value::Null);
    let gpu_timestamp_timing = input
        .renderer_probe
        .get("gpu_timestamp_timing")
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "schema": VERIFY_RENDER_REPORT_SCHEMA,
        "schema_version": VERIFY_RENDER_REPORT_SCHEMA_VERSION,
        "command": "verify-render",
        "evidence_type": "gpu_render_verification",
        "status": input.status,
        "failure_reason": input.failure_reason,
        "started_at_epoch_ms": input.started_at_epoch_ms,
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": input.duration_ms,
        "gpu_adapter": gpu_adapter,
        "gpu_timestamp_timing": gpu_timestamp_timing,
        "renderer_probe": input.renderer_probe,
        "requirements": {
            "requires_non_cpu_gpu": true,
            "fails_without_adapter": true,
            "evidence_must_include_pixels_or_resources": true,
            "product_device_limit_coverage": true,
            "display_texture_coverage": true,
            "app_backend_coverage": true,
            "timestamp_capability_reported": true,
        },
        "tests": {
            "test_count": input.tests.len(),
            "passed": passed,
            "failed": failed,
            "items": input.tests,
        },
    })
}

fn verify_render_test_json(
    test: VerifyRenderTest,
    status: &'static str,
    failure_reason: Option<String>,
) -> Value {
    json!({
        "package": test.package,
        "filter": test.filter,
        "evidence_type": test.evidence_type,
        "status": status,
        "failure_reason": failure_reason,
    })
}

fn write_verify_ui_report(input: VerifyUiReportInput<'_>) -> anyhow::Result<()> {
    let path = input.path;
    let report = verify_ui_report_json(input);
    write_json_file(path, &report)
}

fn verify_ui_report_json(input: VerifyUiReportInput<'_>) -> Value {
    let passed = input
        .tests
        .iter()
        .filter(|test| test.get("status").and_then(Value::as_str) == Some("passed"))
        .count();
    let failed = input
        .tests
        .iter()
        .filter(|test| test.get("status").and_then(Value::as_str) == Some("failed"))
        .count();
    let ignored_snapshot_tests = input
        .tests
        .iter()
        .filter(|test| test.get("run_ignored").and_then(Value::as_bool) == Some(true))
        .count();
    let semantic_tests = input
        .tests
        .iter()
        .filter(|test| {
            test.get("evidence_layer").and_then(Value::as_str) == Some("semantic_ui_tree")
        })
        .count();
    let visual_snapshot_tests = input
        .tests
        .iter()
        .filter(|test| {
            test.get("evidence_layer").and_then(Value::as_str) == Some("visual_snapshot")
        })
        .count();
    let high_dpi_tests = verify_ui_category_count(&input.tests, "high_dpi");
    let narrow_layout_tests = verify_ui_category_count(&input.tests, "narrow");
    let long_label_tests = verify_ui_category_count(&input.tests, "long_label");
    let snapshot_artifacts = input
        .tests
        .iter()
        .filter_map(|test| {
            test.get("snapshot_artifacts")
                .filter(|value| !value.is_null())
        })
        .cloned()
        .collect::<Vec<_>>();

    json!({
        "schema": VERIFY_UI_REPORT_SCHEMA,
        "schema_version": VERIFY_UI_REPORT_SCHEMA_VERSION,
        "command": "verify-ui",
        "evidence_type": "ui_visual_and_semantic_verification",
        "status": input.status,
        "failure_reason": input.failure_reason,
        "started_at_epoch_ms": input.started_at_epoch_ms,
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": input.duration_ms,
        "requirements": {
            "semantic_component_coverage": true,
            "high_dpi_coverage": true,
            "narrow_layout_coverage": true,
            "long_label_coverage": true,
            "screenshot_snapshot_coverage": true,
            "product_automation_is_not_ui_substitute": true,
        },
        "coverage_summary": {
            "semantic_ui_tree_tests": semantic_tests,
            "visual_snapshot_tests": visual_snapshot_tests,
            "high_dpi_tests": high_dpi_tests,
            "narrow_layout_tests": narrow_layout_tests,
            "long_label_tests": long_label_tests,
        },
        "artifacts": {
            "snapshot_artifacts": snapshot_artifacts,
            "failure_artifact_patterns": [
                "crates/mirante4d-app/tests/snapshots/*.new.png",
                "crates/mirante4d-app/tests/snapshots/*.diff.png",
            ],
        },
        "tests": {
            "test_count": input.tests.len(),
            "passed": passed,
            "failed": failed,
            "ignored_snapshot_tests": ignored_snapshot_tests,
            "items": input.tests,
        },
    })
}

fn verify_ui_category_count(tests: &[Value], needle: &str) -> usize {
    tests
        .iter()
        .filter(|test| {
            test.get("evidence_type")
                .and_then(Value::as_str)
                .is_some_and(|evidence_type| evidence_type.contains(needle))
        })
        .count()
}

fn verify_ui_test_json(
    test: VerifyUiTest,
    status: &'static str,
    failure_reason: Option<String>,
    timeout: Duration,
) -> Value {
    let snapshot_name = verify_ui_snapshot_name(test);
    let snapshot_artifacts = snapshot_name.map(verify_ui_snapshot_artifacts_json);
    json!({
        "package": test.package,
        "filter": test.filter,
        "evidence_type": test.evidence_type,
        "evidence_layer": if test.run_ignored { "visual_snapshot" } else { "semantic_ui_tree" },
        "run_ignored": test.run_ignored,
        "timeout_secs": timeout.as_secs(),
        "snapshot_name": snapshot_name,
        "snapshot_artifacts": snapshot_artifacts,
        "status": status,
        "failure_reason": failure_reason,
    })
}

fn run_verify_ui_test(test: VerifyUiTest, timeout: Duration) -> anyhow::Result<()> {
    let mut command = cargo_command();
    command.args(["nextest", "run", "-p", test.package]);
    if test.run_ignored {
        command.args(["--run-ignored", "only"]);
    }
    command.args(["--no-capture", test.filter]);
    run_command_with_timeout(&mut command, timeout)
}

fn verify_ui_test_timeout(test: VerifyUiTest) -> Duration {
    let (env_name, default_secs) = if test.run_ignored {
        (
            VERIFY_UI_SNAPSHOT_TIMEOUT_SECS_ENV,
            DEFAULT_VERIFY_UI_SNAPSHOT_TIMEOUT_SECS,
        )
    } else {
        (
            VERIFY_UI_TEST_TIMEOUT_SECS_ENV,
            DEFAULT_VERIFY_UI_TEST_TIMEOUT_SECS,
        )
    };
    Duration::from_secs(env_timeout_secs(env_name, default_secs))
}

fn verify_ui_snapshot_name(test: VerifyUiTest) -> Option<&'static str> {
    match test.filter {
        "workbench_shell_image_snapshot_matches_baseline" => Some("workbench_shell_basic"),
        "workbench_shell_image_snapshot_iso_hidpi_matches_baseline" => {
            Some("workbench_shell_iso_hidpi")
        }
        "workbench_shell_image_snapshot_dvr_hidpi_matches_baseline" => {
            Some("workbench_shell_dvr_hidpi")
        }
        _ => None,
    }
}

fn verify_ui_snapshot_artifacts_json(snapshot_name: &str) -> Value {
    let base = format!("crates/mirante4d-app/tests/snapshots/{snapshot_name}");
    json!({
        "snapshot_name": snapshot_name,
        "baseline": format!("{base}.png"),
        "new": format!("{base}.new.png"),
        "diff": format!("{base}.diff.png"),
    })
}

fn verify_e2e_report_json(input: VerifyE2eReportInput) -> Value {
    let real_product_summary = verify_e2e_real_product_summary(&input.real_product_scenarios);
    json!({
        "schema": VERIFY_E2E_REPORT_SCHEMA,
        "schema_version": VERIFY_E2E_REPORT_SCHEMA_VERSION,
        "command": "verify-e2e",
        "evidence_type": "workflow_e2e_with_product_automation",
        "status": input.status,
        "failure_reason": input.failure_reason,
        "started_at_epoch_ms": input.started_at_epoch_ms,
        "finished_at_epoch_ms": epoch_ms(),
        "duration_ms": input.duration_ms,
        "requirements": {
            "library_workflow_coverage": true,
            "virtual_window_product_automation_coverage": true,
            "real_window_product_automation_is_sectioned": true,
            "unsupported_display_is_explicit": true,
            "failed_product_scenario_fails_gate": true,
        },
        "portions": {
            "library_workflow_tests": {
                "evidence_type": "library_e2e",
                "status": "passed",
                "test_count": input.library_tests.len(),
                "tests": input.library_tests,
            },
            "virtual_window_product_automation": {
                "evidence_type": "virtual_window_product_automation",
                "status": "passed",
                "test_count": input.virtual_product_tests.len(),
                "tests": input.virtual_product_tests,
            },
            "real_window_product_automation": {
                "evidence_type": "real_window_product_automation",
                "status": real_product_summary.status,
                "test_count": input.real_product_scenarios.len(),
                "passed": real_product_summary.passed,
                "unsupported": real_product_summary.unsupported,
                "failed": real_product_summary.failed,
                "scenarios": input.real_product_scenarios,
            },
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifyE2eRealProductSummary {
    status: &'static str,
    passed: usize,
    unsupported: usize,
    failed: usize,
}

fn verify_e2e_real_product_summary(scenarios: &[Value]) -> VerifyE2eRealProductSummary {
    let passed = scenarios
        .iter()
        .filter(|scenario| scenario.get("status").and_then(Value::as_str) == Some("passed"))
        .count();
    let unsupported = scenarios
        .iter()
        .filter(|scenario| scenario.get("status").and_then(Value::as_str) == Some("unsupported"))
        .count();
    let failed = scenarios
        .iter()
        .filter(|scenario| {
            let status = scenario.get("status").and_then(Value::as_str);
            status == Some("failed") || status == Some("timed_out") || status == Some("missing")
        })
        .count();
    let status = if failed > 0 {
        "failed"
    } else if unsupported == scenarios.len() {
        "unsupported"
    } else {
        "passed"
    };

    VerifyE2eRealProductSummary {
        status,
        passed,
        unsupported,
        failed,
    }
}

fn adapter_diagnostics_json(adapter: &AdapterDiagnostics) -> Value {
    json!({
        "name": adapter.name.as_str(),
        "backend": adapter.backend.as_str(),
        "device_type": adapter.device_type.as_str(),
        "driver": adapter.driver.as_str(),
        "driver_info": adapter.driver_info.as_str(),
        "timestamp_queries_supported": adapter.timestamp_queries_supported,
        "timestamp_queries_requested": adapter.timestamp_queries_requested,
        "timestamp_queries_enabled": adapter.timestamp_queries_enabled,
        "adapter_limits": gpu_limit_diagnostics_json(&adapter.adapter_limits),
        "requested_limits": gpu_limit_diagnostics_json(&adapter.requested_limits),
    })
}

fn gpu_timestamp_timing_json(adapter: &AdapterDiagnostics) -> Value {
    json!({
        "kind": "gpu_timestamp_timing",
        "taxonomy_version": 1,
        "status": gpu_timestamp_timing_status(adapter),
        "env_var": "MIRANTE4D_GPU_TIMESTAMPS",
        "measurement_scope": "renderer_compute_pass_elapsed_time_from_wgpu_timestamp_queries",
        "sample_field": "gpu_compute_ms",
        "unit": "milliseconds",
        "adapter_timestamp_queries_supported": adapter.timestamp_queries_supported,
        "timestamp_queries_requested": adapter.timestamp_queries_requested,
        "timestamp_queries_enabled": adapter.timestamp_queries_enabled,
    })
}

fn gpu_timestamp_timing_status(adapter: &AdapterDiagnostics) -> &'static str {
    match (
        adapter.timestamp_queries_supported,
        adapter.timestamp_queries_requested,
        adapter.timestamp_queries_enabled,
    ) {
        (_, _, true) => "enabled",
        (true, true, false) => "requested_but_device_feature_missing",
        (false, true, false) => "requested_but_unsupported",
        (true, false, false) => "supported_not_requested",
        (false, false, false) => "unsupported_not_requested",
    }
}

fn gpu_limit_diagnostics_json(limits: &GpuLimitDiagnostics) -> Value {
    json!({
        "max_buffer_size": limits.max_buffer_size,
        "max_storage_buffer_binding_size": limits.max_storage_buffer_binding_size,
        "max_storage_buffers_per_shader_stage": limits.max_storage_buffers_per_shader_stage,
    })
}

fn gpu_renderer_stats_json(stats: GpuRendererStats) -> Value {
    json!({
        "volume_cache_budget_bytes": stats.volume_cache_budget_bytes,
        "brick_atlas_cache_budget_bytes": stats.brick_atlas_cache_budget_bytes,
        "volume_cache_hits": stats.volume_cache_hits,
        "volume_cache_misses": stats.volume_cache_misses,
        "volume_uploads": stats.volume_uploads,
        "volume_uploaded_bytes": stats.volume_uploaded_bytes,
        "volume_evictions": stats.volume_evictions,
        "volume_resident_bytes": stats.volume_resident_bytes,
        "brick_atlas_cache_hits": stats.brick_atlas_cache_hits,
        "brick_atlas_cache_misses": stats.brick_atlas_cache_misses,
        "brick_atlas_uploads": stats.brick_atlas_uploads,
        "brick_atlas_uploaded_bytes": stats.brick_atlas_uploaded_bytes,
        "brick_atlas_u8_uploaded_bytes": stats.brick_atlas_u8_uploaded_bytes,
        "brick_atlas_u16_uploaded_bytes": stats.brick_atlas_u16_uploaded_bytes,
        "brick_atlas_f32_uploaded_bytes": stats.brick_atlas_f32_uploaded_bytes,
        "brick_atlas_evictions": stats.brick_atlas_evictions,
        "brick_atlas_page_table_rebuilds": stats.brick_atlas_page_table_rebuilds,
        "brick_atlas_page_table_bytes_written": stats.brick_atlas_page_table_bytes_written,
        "brick_atlas_resident_bytes": stats.brick_atlas_resident_bytes,
        "brick_atlas_u8_resident_bytes": stats.brick_atlas_u8_resident_bytes,
        "brick_atlas_u16_resident_bytes": stats.brick_atlas_u16_resident_bytes,
        "brick_atlas_f32_resident_bytes": stats.brick_atlas_f32_resident_bytes,
        "upload_ready_brick_cache_budget_bytes": stats.upload_ready_brick_cache_budget_bytes,
        "upload_ready_brick_cache_hits": stats.upload_ready_brick_cache_hits,
        "upload_ready_brick_cache_misses": stats.upload_ready_brick_cache_misses,
        "upload_ready_brick_cache_evictions": stats.upload_ready_brick_cache_evictions,
        "upload_ready_brick_cache_resident_bytes": stats.upload_ready_brick_cache_resident_bytes,
        "display_resource_cache_hits": stats.display_resource_cache_hits,
        "display_resource_cache_misses": stats.display_resource_cache_misses,
        "display_resource_recreations": stats.display_resource_recreations,
        "display_resource_resident_bytes": stats.display_resource_resident_bytes,
    })
}

fn verify_fuzz_targets() -> anyhow::Result<()> {
    ensure_cargo_subcommand("fuzz", "cargo-fuzz", "cargo install cargo-fuzz --locked")?;
    if !Path::new("fuzz/Cargo.toml").is_file() {
        bail!("fuzz/Cargo.toml is required for `cargo xtask verify-nightly` fuzz targets");
    }
    let max_total_time = crate::env_u64("MIRANTE4D_NIGHTLY_FUZZ_SECONDS")?
        .unwrap_or(DEFAULT_NIGHTLY_FUZZ_SECONDS)
        .to_string();
    for target in NIGHTLY_FUZZ_TARGETS {
        let mut command = cargo_command();
        command.args([
            "+nightly",
            "fuzz",
            "run",
            target,
            "--",
            "-max_total_time",
            &max_total_time,
        ]);
        run_command(&mut command).with_context(|| {
            format!("nightly fuzz target {target:?} failed; artifacts are under fuzz/artifacts/")
        })?;
    }
    Ok(())
}

fn verify_mutation_audit() -> anyhow::Result<()> {
    ensure_nextest()?;
    ensure_cargo_subcommand(
        "mutants",
        "cargo-mutants",
        "cargo install cargo-mutants --locked",
    )?;
    fs::create_dir_all(MUTANTS_OUTPUT_DIR)
        .with_context(|| format!("failed to create {MUTANTS_OUTPUT_DIR}"))?;
    let package_list = env::var("MIRANTE4D_MUTANTS_PACKAGES").ok();
    let packages = mutation_packages(package_list.as_deref());

    let mut command = cargo_command();
    command.args([
        "mutants",
        "--test-tool=nextest",
        "--output",
        MUTANTS_OUTPUT_DIR,
    ]);
    for package in packages {
        command.args(["--package", &package]);
    }
    run_command(&mut command)
}

fn mutation_packages(raw: Option<&str>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|package| !package.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    })
    .filter(|packages| !packages.is_empty())
    .unwrap_or_else(|| {
        DEFAULT_MUTATION_PACKAGES
            .iter()
            .map(|package| (*package).to_owned())
            .collect()
    })
}

fn env_timeout_secs(env_name: &str, default_secs: u64) -> u64 {
    parse_timeout_secs(env::var(env_name).ok().as_deref(), default_secs)
}

fn parse_timeout_secs(raw: Option<&str>, default_secs: u64) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(default_secs)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn bootstrap_scope_is_explicit_and_bounded() {
        for package in [
            "mirante4d-core",
            "mirante4d-format",
            "mirante4d-data",
            "mirante4d-import",
            "mirante4d-analysis",
            "mirante4d-renderer",
            "mirante4d-app",
            "xtask",
        ] {
            assert!(VERIFY_BOOTSTRAP_FILTER.contains(package));
        }
        assert!(!VERIFY_BOOTSTRAP_FILTER.contains("all()"));
        assert_eq!(VERIFY_BOOTSTRAP_EXPECTED_TESTS, 169);

        let total_ceiling = VERIFY_BOOTSTRAP_FORMAT_TIMEOUT
            + VERIFY_BOOTSTRAP_CHECK_TIMEOUT
            + VERIFY_BOOTSTRAP_TEST_TIMEOUT
            + crate::documentation::DOCS_CHECK_TIMEOUT;
        assert_eq!(total_ceiling, Duration::from_secs(14 * 60 + 30));
    }

    #[test]
    fn deep_verification_targets_are_stable() {
        assert_eq!(
            NIGHTLY_FUZZ_TARGETS,
            &["manifest_parser", "project_metadata_parser"]
        );
        assert_eq!(
            mutation_packages(None),
            vec!["mirante4d-core".to_owned(), "mirante4d-format".to_owned()]
        );
        assert_eq!(
            mutation_packages(Some(" mirante4d-core, mirante4d-analysis ,, ")),
            vec!["mirante4d-core".to_owned(), "mirante4d-analysis".to_owned()]
        );
    }

    #[test]
    fn verify_e2e_virtual_product_tests_cover_camera_and_render_modes() {
        let filters = E2E_VIRTUAL_PRODUCT_AUTOMATION_TESTS
            .iter()
            .map(|(_, filter)| *filter)
            .collect::<BTreeSet<_>>();

        assert_eq!(filters.len(), E2E_VIRTUAL_PRODUCT_AUTOMATION_TESTS.len());
        assert!(
            filters.contains("virtual_product_automation_generated_fixture_camera_sequence"),
            "verify-e2e must keep generated-fixture camera automation coverage"
        );
        assert!(
            filters.contains("virtual_product_automation_generated_fixture_render_mode_sequence"),
            "verify-e2e must keep generated-fixture render-mode automation coverage"
        );
    }

    #[test]
    fn verify_e2e_real_product_scenarios_cover_camera_and_render_modes() {
        let scenarios = E2E_REAL_WINDOW_PRODUCT_AUTOMATION_SCENARIOS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();

        assert_eq!(
            scenarios.len(),
            E2E_REAL_WINDOW_PRODUCT_AUTOMATION_SCENARIOS.len()
        );
        assert!(scenarios.contains("generated_fixture_camera_smoke"));
        assert!(scenarios.contains("generated_fixture_render_modes"));
        assert!(scenarios.contains("custom_script"));
    }

    #[test]
    fn verify_e2e_real_product_summary_classifies_unsupported_and_failures() {
        let unsupported = vec![
            json!({"scenario": "a", "status": "unsupported"}),
            json!({"scenario": "b", "status": "unsupported"}),
        ];
        assert_eq!(
            verify_e2e_real_product_summary(&unsupported),
            VerifyE2eRealProductSummary {
                status: "unsupported",
                passed: 0,
                unsupported: 2,
                failed: 0,
            }
        );

        let mixed = vec![
            json!({"scenario": "a", "status": "passed"}),
            json!({"scenario": "b", "status": "unsupported"}),
        ];
        assert_eq!(
            verify_e2e_real_product_summary(&mixed),
            VerifyE2eRealProductSummary {
                status: "passed",
                passed: 1,
                unsupported: 1,
                failed: 0,
            }
        );

        let failed = vec![
            json!({"scenario": "a", "status": "passed"}),
            json!({"scenario": "b", "status": "timed_out"}),
            json!({"scenario": "c", "status": "missing"}),
        ];
        assert_eq!(
            verify_e2e_real_product_summary(&failed),
            VerifyE2eRealProductSummary {
                status: "failed",
                passed: 1,
                unsupported: 0,
                failed: 2,
            }
        );
    }

    #[test]
    fn verify_e2e_report_json_sections_product_scenarios() {
        let report = verify_e2e_report_json(VerifyE2eReportInput {
            started_at_epoch_ms: 123,
            duration_ms: 4.5,
            status: "passed",
            failure_reason: None,
            library_tests: vec![json!({
                "package": "mirante4d-app",
                "filter": "library_test",
                "status": "passed",
            })],
            virtual_product_tests: vec![json!({
                "package": "mirante4d-app",
                "filter": "virtual_test",
                "status": "passed",
            })],
            real_product_scenarios: vec![
                json!({
                    "scenario": "generated_fixture_camera_smoke",
                    "status": "unsupported",
                    "failure_reason": "no display",
                    "product_validation_report": "target/mirante4d/product-validation/generated_fixture_camera_smoke/product-validation-report.json",
                }),
                json!({
                    "scenario": "generated_fixture_render_modes",
                    "status": "unsupported",
                    "failure_reason": "no display",
                    "product_validation_report": "target/mirante4d/product-validation/generated_fixture_render_modes/product-validation-report.json",
                }),
                json!({
                    "scenario": "custom_script",
                    "status": "unsupported",
                    "failure_reason": "no display",
                    "product_validation_report": "target/mirante4d/product-validation/custom_script/product-validation-report.json",
                }),
            ],
        });

        assert_eq!(report["schema"], VERIFY_E2E_REPORT_SCHEMA);
        assert_eq!(report["schema_version"], VERIFY_E2E_REPORT_SCHEMA_VERSION);
        assert_eq!(report["command"], "verify-e2e");
        assert_eq!(
            report["evidence_type"],
            "workflow_e2e_with_product_automation"
        );
        assert_eq!(report["status"], "passed");
        assert_eq!(
            report["requirements"]["real_window_product_automation_is_sectioned"],
            true
        );
        assert_eq!(
            report["portions"]["library_workflow_tests"]["test_count"],
            1
        );
        assert_eq!(
            report["portions"]["virtual_window_product_automation"]["test_count"],
            1
        );
        assert_eq!(
            report["portions"]["real_window_product_automation"]["status"],
            "unsupported"
        );
        assert_eq!(
            report["portions"]["real_window_product_automation"]["unsupported"],
            3
        );
        assert_eq!(
            report["portions"]["real_window_product_automation"]["scenarios"][2]["scenario"],
            "custom_script"
        );
    }

    #[test]
    fn verify_render_targets_are_product_relevant_and_explicit() {
        let packages = VERIFY_RENDER_TESTS
            .iter()
            .map(|test| test.package)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            packages,
            BTreeSet::from(["mirante4d-app", "mirante4d-renderer"])
        );

        let filters = VERIFY_RENDER_TESTS
            .iter()
            .map(|test| test.filter)
            .collect::<BTreeSet<_>>();
        assert_eq!(filters.len(), VERIFY_RENDER_TESTS.len());
        assert!(
            filters.contains("gpu_renderer_constructs_from_existing_renderer_limit_device"),
            "verify-render must keep product-like existing-device coverage"
        );
        assert!(
            filters.contains("gpu_resident_display_texture_matches_cpu_intensity_compositor"),
            "verify-render must keep display-texture resource coverage"
        );
        assert!(
            filters.contains("app_backend_uses_gpu_resident_bricks_when_stream_complete"),
            "verify-render must keep app backend coverage"
        );

        let evidence_types = VERIFY_RENDER_TESTS
            .iter()
            .map(|test| test.evidence_type)
            .collect::<BTreeSet<_>>();
        assert!(
            evidence_types
                .iter()
                .any(|evidence| evidence.contains("display_texture"))
        );
        assert!(evidence_types.contains("existing_device_product_limits"));
        assert!(evidence_types.contains("app_resident_gpu_backend"));
    }

    #[test]
    fn verify_render_report_json_names_requirements_and_test_counts() {
        let tests = vec![
            verify_render_test_json(VERIFY_RENDER_TESTS[0], "passed", None),
            verify_render_test_json(VERIFY_RENDER_TESTS[1], "failed", Some("boom".to_owned())),
        ];
        let report = verify_render_report_json(VerifyRenderReportInput {
            path: Path::new("unused.json"),
            started_at_epoch_ms: 123,
            duration_ms: 4.5,
            status: "failed",
            failure_reason: Some("test failed".to_owned()),
            renderer_probe: json!({
                "status": "available",
                "adapter": {
                    "name": "unit adapter",
                    "backend": "Vulkan",
                    "timestamp_queries_supported": true,
                    "timestamp_queries_requested": true,
                    "timestamp_queries_enabled": true
                },
                "gpu_timestamp_timing": {
                    "kind": "gpu_timestamp_timing",
                    "status": "enabled",
                    "measurement_scope": "renderer_compute_pass_elapsed_time_from_wgpu_timestamp_queries",
                    "sample_field": "gpu_compute_ms"
                },
                "initial_stats": {
                    "volume_uploaded_bytes": 0
                }
            }),
            tests,
        });

        assert_eq!(report["schema"], VERIFY_RENDER_REPORT_SCHEMA);
        assert_eq!(
            report["schema_version"],
            VERIFY_RENDER_REPORT_SCHEMA_VERSION
        );
        assert_eq!(report["command"], "verify-render");
        assert_eq!(report["evidence_type"], "gpu_render_verification");
        assert_eq!(report["status"], "failed");
        assert_eq!(report["gpu_adapter"]["name"], "unit adapter");
        assert_eq!(report["gpu_timestamp_timing"]["status"], "enabled");
        assert_eq!(
            report["gpu_timestamp_timing"]["sample_field"],
            "gpu_compute_ms"
        );
        assert_eq!(report["requirements"]["fails_without_adapter"], true);
        assert_eq!(report["requirements"]["display_texture_coverage"], true);
        assert_eq!(
            report["requirements"]["timestamp_capability_reported"],
            true
        );
        assert_eq!(report["tests"]["test_count"], 2);
        assert_eq!(report["tests"]["passed"], 1);
        assert_eq!(report["tests"]["failed"], 1);
        assert_eq!(report["tests"]["items"][1]["failure_reason"], "boom");
    }

    #[test]
    fn verify_ui_targets_cover_semantic_layout_and_snapshots() {
        let packages = VERIFY_UI_TESTS
            .iter()
            .map(|test| test.package)
            .collect::<BTreeSet<_>>();
        assert_eq!(packages, BTreeSet::from(["mirante4d-app"]));

        let filters = VERIFY_UI_TESTS
            .iter()
            .map(|test| test.filter)
            .collect::<BTreeSet<_>>();
        assert_eq!(filters.len(), VERIFY_UI_TESTS.len());
        assert!(
            filters.contains("workbench_shell_exposes_primary_regions_at_high_dpi"),
            "verify-ui must keep high-DPI workbench shell coverage"
        );
        assert!(
            filters.contains("workbench_shell_handles_long_dataset_name_in_narrow_layout"),
            "verify-ui must keep long-label narrow-layout coverage"
        );
        assert!(
            filters.contains("workbench_settings_panel_exposes_runtime_budget_controls"),
            "verify-ui must keep narrow settings panel coverage"
        );
        assert!(
            filters.contains("workbench_shell_image_snapshot_matches_baseline"),
            "verify-ui must keep visual snapshot coverage"
        );

        let evidence_types = VERIFY_UI_TESTS
            .iter()
            .map(|test| test.evidence_type)
            .collect::<BTreeSet<_>>();
        assert!(evidence_types.contains("high_dpi_shell_semantic_layout"));
        assert!(evidence_types.contains("long_label_narrow_shell_semantic_layout"));
        assert!(evidence_types.contains("workbench_shell_visual_snapshot"));
        assert!(
            VERIFY_UI_TESTS.iter().any(|test| test.run_ignored),
            "verify-ui must explicitly run ignored snapshot tests"
        );
        assert!(
            VERIFY_UI_TESTS
                .iter()
                .filter(|test| test.run_ignored)
                .all(|test| verify_ui_snapshot_name(*test).is_some()),
            "every ignored visual snapshot test must name its baseline artifacts"
        );
        assert!(
            VERIFY_UI_TESTS.iter().any(|test| !test.run_ignored),
            "verify-ui must include semantic UI-tree tests"
        );
    }

    #[test]
    fn verify_ui_report_json_names_requirements_and_test_counts() {
        let basic_snapshot = *VERIFY_UI_TESTS
            .iter()
            .find(|test| test.filter == "workbench_shell_image_snapshot_matches_baseline")
            .expect("verify-ui includes the basic workbench snapshot");
        let tests = vec![
            verify_ui_test_json(VERIFY_UI_TESTS[0], "passed", None, Duration::from_secs(60)),
            verify_ui_test_json(
                basic_snapshot,
                "failed",
                Some("snapshot drift".to_owned()),
                Duration::from_secs(240),
            ),
        ];
        let report = verify_ui_report_json(VerifyUiReportInput {
            path: Path::new("unused.json"),
            started_at_epoch_ms: 123,
            duration_ms: 4.5,
            status: "failed",
            failure_reason: Some("test failed".to_owned()),
            tests,
        });

        assert_eq!(report["schema"], VERIFY_UI_REPORT_SCHEMA);
        assert_eq!(report["schema_version"], VERIFY_UI_REPORT_SCHEMA_VERSION);
        assert_eq!(report["command"], "verify-ui");
        assert_eq!(
            report["evidence_type"],
            "ui_visual_and_semantic_verification"
        );
        assert_eq!(report["status"], "failed");
        assert_eq!(report["requirements"]["semantic_component_coverage"], true);
        assert_eq!(report["requirements"]["high_dpi_coverage"], true);
        assert_eq!(report["requirements"]["narrow_layout_coverage"], true);
        assert_eq!(report["requirements"]["long_label_coverage"], true);
        assert_eq!(report["requirements"]["screenshot_snapshot_coverage"], true);
        assert_eq!(report["coverage_summary"]["semantic_ui_tree_tests"], 1);
        assert_eq!(report["coverage_summary"]["visual_snapshot_tests"], 1);
        assert_eq!(report["coverage_summary"]["high_dpi_tests"], 1);
        assert_eq!(report["coverage_summary"]["narrow_layout_tests"], 0);
        assert_eq!(report["coverage_summary"]["long_label_tests"], 0);
        assert_eq!(
            report["artifacts"]["snapshot_artifacts"][0]["baseline"],
            "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.png"
        );
        assert_eq!(
            report["artifacts"]["snapshot_artifacts"][0]["diff"],
            "crates/mirante4d-app/tests/snapshots/workbench_shell_basic.diff.png"
        );
        assert_eq!(report["tests"]["test_count"], 2);
        assert_eq!(report["tests"]["passed"], 1);
        assert_eq!(report["tests"]["failed"], 1);
        assert_eq!(report["tests"]["ignored_snapshot_tests"], 1);
        assert_eq!(
            report["tests"]["items"][0]["evidence_layer"],
            "semantic_ui_tree"
        );
        assert_eq!(
            report["tests"]["items"][1]["evidence_layer"],
            "visual_snapshot"
        );
        assert_eq!(report["tests"]["items"][0]["timeout_secs"], 60);
        assert_eq!(report["tests"]["items"][1]["timeout_secs"], 240);
        assert_eq!(
            report["tests"]["items"][1]["snapshot_name"],
            "workbench_shell_basic"
        );
        assert_eq!(
            report["tests"]["items"][1]["failure_reason"],
            "snapshot drift"
        );
    }

    #[test]
    fn verify_ui_timeout_parsing_uses_positive_overrides_only() {
        assert_eq!(parse_timeout_secs(None, 60), 60);
        assert_eq!(parse_timeout_secs(Some(""), 60), 60);
        assert_eq!(parse_timeout_secs(Some("0"), 60), 60);
        assert_eq!(parse_timeout_secs(Some("abc"), 60), 60);
        assert_eq!(parse_timeout_secs(Some("240"), 60), 240);
        assert_eq!(parse_timeout_secs(Some(" 180 "), 60), 180);
    }

    #[test]
    fn adapter_diagnostics_json_preserves_limits_and_device_identity() {
        let adapter = AdapterDiagnostics {
            name: "adapter".to_owned(),
            backend: "Vulkan".to_owned(),
            device_type: "DiscreteGpu".to_owned(),
            driver: "driver".to_owned(),
            driver_info: "driver-info".to_owned(),
            timestamp_queries_supported: true,
            timestamp_queries_requested: true,
            timestamp_queries_enabled: false,
            adapter_limits: GpuLimitDiagnostics {
                max_buffer_size: 1024,
                max_storage_buffer_binding_size: 2048,
                max_storage_buffers_per_shader_stage: 8,
            },
            requested_limits: GpuLimitDiagnostics {
                max_buffer_size: 512,
                max_storage_buffer_binding_size: 1024,
                max_storage_buffers_per_shader_stage: 4,
            },
        };

        let value = adapter_diagnostics_json(&adapter);

        assert_eq!(value["name"], "adapter");
        assert_eq!(value["backend"], "Vulkan");
        assert_eq!(value["device_type"], "DiscreteGpu");
        assert_eq!(value["timestamp_queries_supported"], true);
        assert_eq!(value["timestamp_queries_requested"], true);
        assert_eq!(value["timestamp_queries_enabled"], false);
        assert_eq!(value["adapter_limits"]["max_buffer_size"], 1024);
        assert_eq!(
            value["adapter_limits"]["max_storage_buffer_binding_size"],
            2048
        );
        assert_eq!(
            value["requested_limits"]["max_storage_buffers_per_shader_stage"],
            4
        );
    }

    #[test]
    fn gpu_timestamp_timing_json_distinguishes_request_support_and_enablement() {
        let mut adapter = AdapterDiagnostics {
            name: "adapter".to_owned(),
            backend: "Vulkan".to_owned(),
            device_type: "DiscreteGpu".to_owned(),
            driver: "driver".to_owned(),
            driver_info: "driver-info".to_owned(),
            timestamp_queries_supported: true,
            timestamp_queries_requested: false,
            timestamp_queries_enabled: false,
            adapter_limits: GpuLimitDiagnostics {
                max_buffer_size: 1024,
                max_storage_buffer_binding_size: 2048,
                max_storage_buffers_per_shader_stage: 8,
            },
            requested_limits: GpuLimitDiagnostics {
                max_buffer_size: 1024,
                max_storage_buffer_binding_size: 2048,
                max_storage_buffers_per_shader_stage: 8,
            },
        };

        assert_eq!(
            gpu_timestamp_timing_json(&adapter)["status"],
            "supported_not_requested"
        );
        adapter.timestamp_queries_requested = true;
        assert_eq!(
            gpu_timestamp_timing_json(&adapter)["status"],
            "requested_but_device_feature_missing"
        );
        adapter.timestamp_queries_enabled = true;
        assert_eq!(gpu_timestamp_timing_json(&adapter)["status"], "enabled");
        adapter.timestamp_queries_enabled = false;
        adapter.timestamp_queries_supported = false;
        assert_eq!(
            gpu_timestamp_timing_json(&adapter)["status"],
            "requested_but_unsupported"
        );
    }
}
