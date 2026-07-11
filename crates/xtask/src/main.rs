use std::{
    env,
    path::{Path, PathBuf},
};

use crate::audits::{
    bench_phase14_multichannel, bench_phase15_analysis, phase10_audit, phase12_audit,
    phase14_audit, phase15_audit, phase17_audit, phase19_audit, phase20_extreme_audit,
    phase20_extreme_sample_audit, phase20_smoke_audit,
};
use crate::baseline_audit::baseline_audit;
use crate::baseline_promote::{baseline_promote, baseline_promote_manifest, baseline_refresh_plan};
use crate::bench::{
    bench_check, bench_import_sample, bench_native_package, bench_phase11_interaction,
    bench_phase11_large_view, bench_phase11_synthetic_matrix, bench_phase11_viewport_matrix,
    bench_phase13_renderer, bench_phase13_viewport_matrix, bench_runtime_stress, bench_smoke,
};
use crate::command_audit::command_audit;
use crate::fixtures::generate_fixture;
use crate::neuroglancer_compare::neuroglancer_compare;
use crate::product_validate::{is_product_validation_scenario_name, product_validate};
use crate::smoke::app_smoke;
use crate::workflow_audit::workflow_audit;
use anyhow::{Context, bail};

const PRODUCT_VALIDATE_USAGE: &str = "usage: cargo xtask product-validate [native-package.m4d] \
     [generated_fixture_camera_smoke|generated_fixture_render_modes|\
      t5_qual_001_interaction_mip|t5_qual_001_interaction_render_modes|t5_qual_001_interaction_continuous|\
      t5_qual_001_four_panel_cross_section|t5_qual_001_four_panel_fine_scale|\
      t5_qual_001_four_panel_continuous_cross_section|t5_qual_002_four_panel_timepoint|t5_qual_002_four_panel_autoplay|custom_script]";
mod arch;
mod audits;
mod baseline_audit;
mod baseline_promote;
mod bench;
mod command_audit;
mod deps;
mod dev;
mod documentation;
mod fixtures;
mod host;
mod ids;
mod neuroglancer_compare;
mod package;
mod process;
mod product_validate;
mod reports;
mod smoke;
mod verification;
mod verify;
mod workflow_audit;

pub(crate) use bench::{
    BENCHMARK_PRESENTATION_POINTS, PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS,
    benchmark_camera_for_shape, benchmark_camera_for_volume, benchmark_camera_frame,
    benchmark_camera_orbit, benchmark_camera_pan, benchmark_camera_world_per_screen_point,
    benchmark_camera_zoom, env_u64, phase11_benchmark_viewport_for_shape,
    phase11_brick_pixel_stride, phase11_gpu_brick_cache_budget_bytes,
    phase11_gpu_volume_cache_budget_bytes, phase11_interaction_steps_per_scenario,
    phase11_max_decoded_bytes, phase11_max_visible_bricks,
};
pub(crate) use ids::stable_id_from_name;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "verify-leaf" => {
            let leaf = args
                .next()
                .context("usage: cargo xtask verify-leaf policy|lint|unit|contract|ui|doctest")?;
            if args.next().is_some() {
                bail!("verify-leaf accepts exactly one leaf");
            }
            verification::verify_leaf(verification::Leaf::parse(&leaf)?)
        }
        "verify-pr" => {
            let group = args.next();
            if args.next().is_some() {
                bail!("usage: cargo xtask verify-pr [policy|rust]");
            }
            verification::verify_pr(group.as_deref())
        }
        "verify-local" => {
            let lane = args
                .next()
                .context("usage: cargo xtask verify-local trusted-gpu")?;
            if args.next().is_some() {
                bail!("usage: cargo xtask verify-local trusted-gpu");
            }
            verification::verify_local(&lane)
        }
        "verification-sync" => {
            let option = args.next();
            if args.next().is_some() || option.as_deref().is_some_and(|value| value != "--check") {
                bail!("usage: cargo xtask verification-sync [--check]");
            }
            verification::verification_sync(option.as_deref() == Some("--check"))
        }
        "verify-deps" => verify::verify_deps(),
        "verify-coverage" => verify::verify_coverage(),
        "generate-fixture" => {
            let name = args
                .next()
                .context("usage: cargo xtask generate-fixture <fixture-name>")?;
            generate_fixture(&name).map(|path| {
                println!("{}", path.display());
            })
        }
        "package-dev" => package::package_dev().map(|path| {
            println!("{}", path.display());
        }),
        "package-linux-release" => package::package_linux_release().map(|report| {
            println!("{}", report.display());
        }),
        "bench-smoke" => bench_smoke().map(|path| {
            println!("{}", path.display());
        }),
        "bench-native-package" => {
            let package = args
                .next()
                .context("usage: cargo xtask bench-native-package <native-package.m4d>")?;
            process::with_heavy_benchmark_guard("bench-native-package", || {
                bench_native_package(Path::new(&package))
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "bench-phase11-large-view" => {
            let package = args
                .next()
                .context("usage: cargo xtask bench-phase11-large-view <native-package.m4d>")?;
            process::with_heavy_benchmark_guard("bench-phase11-large-view", || {
                bench_phase11_large_view(Path::new(&package))
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "bench-phase11-interaction" => {
            let package = args
                .next()
                .context("usage: cargo xtask bench-phase11-interaction <native-package.m4d>")?;
            process::with_heavy_benchmark_guard("bench-phase11-interaction", || {
                bench_phase11_interaction(Path::new(&package))
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "bench-phase11-viewport-matrix" => {
            let package = args
                .next()
                .context("usage: cargo xtask bench-phase11-viewport-matrix <native-package.m4d>")?;
            process::with_heavy_benchmark_guard("bench-phase11-viewport-matrix", || {
                bench_phase11_viewport_matrix(Path::new(&package))
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "bench-phase11-synthetic-matrix" => bench_phase11_synthetic_matrix().map(|path| {
            println!("{}", path.display());
        }),
        "bench-phase13-renderer" => {
            let package = args
                .next()
                .context("usage: cargo xtask bench-phase13-renderer <native-package.m4d>")?;
            process::with_heavy_benchmark_guard("bench-phase13-renderer", || {
                bench_phase13_renderer(Path::new(&package))
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "bench-phase13-viewport-matrix" => {
            let package = args
                .next()
                .context("usage: cargo xtask bench-phase13-viewport-matrix <native-package.m4d>")?;
            process::with_heavy_benchmark_guard("bench-phase13-viewport-matrix", || {
                bench_phase13_viewport_matrix(Path::new(&package))
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "app-smoke" => {
            let package = args
                .next()
                .context("usage: cargo xtask app-smoke <native-package.m4d>")?;
            app_smoke(Path::new(&package)).map(|path| {
                println!("{}", path.display());
            })
        }
        "product-validate" => {
            let parsed = product_validate_args(args.collect::<Vec<_>>())?;
            match parsed {
                ProductValidateArgs::Help => {
                    print_product_validate_help();
                    Ok(())
                }
                ProductValidateArgs::Run { package, scenario } => {
                    product_validate(package.as_deref().map(Path::new), scenario.as_deref()).map(
                        |path| {
                            println!("{}", path.display());
                        },
                    )
                }
            }
        }
        "bench-runtime-stress" => bench_runtime_stress().map(|path| {
            println!("{}", path.display());
        }),
        "bench-import-sample" => {
            let experiment = args
                .next()
                .context("usage: cargo xtask bench-import-sample <experiment-folder-name>")?;
            bench_import_sample(&experiment).map(|path| {
                println!("{}", path.display());
            })
        }
        "phase10-audit" => phase10_audit().map(|path| {
            println!("{}", path.display());
        }),
        "phase12-audit" => phase12_audit().map(|path| {
            println!("{}", path.display());
        }),
        "phase14-audit" => phase14_audit().map(|path| {
            println!("{}", path.display());
        }),
        "bench-phase14-multichannel" => bench_phase14_multichannel().map(|path| {
            println!("{}", path.display());
        }),
        "phase15-audit" => phase15_audit().map(|path| {
            println!("{}", path.display());
        }),
        "bench-phase15-analysis" => bench_phase15_analysis().map(|path| {
            println!("{}", path.display());
        }),
        "phase17-audit" => phase17_audit().map(|path| {
            println!("{}", path.display());
        }),
        "phase19-audit" => phase19_audit().map(|path| {
            println!("{}", path.display());
        }),
        "phase20-smoke-audit" => phase20_smoke_audit().map(|path| {
            println!("{}", path.display());
        }),
        "phase20-extreme-audit" => {
            process::with_heavy_benchmark_guard("phase20-extreme-audit", phase20_extreme_audit).map(
                |path| {
                    println!("{}", path.display());
                },
            )
        }
        "phase20-extreme-sample" => {
            let experiment = args.next().context(
                "usage: cargo xtask phase20-extreme-sample <T5-QUAL-001|T5-QUAL-002|T5-QUAL-003>",
            )?;
            process::with_heavy_benchmark_guard("phase20-extreme-sample", || {
                phase20_extreme_sample_audit(&experiment)
            })
            .map(|path| {
                println!("{}", path.display());
            })
        }
        "bench-check" => {
            let current = args.next().context(
                "usage: cargo xtask bench-check <current-benchmark.json> <baseline-benchmark.json>",
            )?;
            let baseline = args.next().context(
                "usage: cargo xtask bench-check <current-benchmark.json> <baseline-benchmark.json>",
            )?;
            bench_check(Path::new(&current), Path::new(&baseline))
        }
        "neuroglancer-compare" => {
            let manifest = args
                .next()
                .context("usage: cargo xtask neuroglancer-compare <comparison-manifest.json>")?;
            if args.next().is_some() {
                bail!("usage: cargo xtask neuroglancer-compare <comparison-manifest.json>");
            }
            neuroglancer_compare(Path::new(&manifest)).map(|path| {
                println!("{}", path.display());
            })
        }
        "baseline-audit" => baseline_audit().map(|path| {
            println!("{}", path.display());
        }),
        "baseline-promote" => {
            let source = args.next().context(
                "usage: cargo xtask baseline-promote <current-benchmark.json> <baseline-name.json>",
            )?;
            let destination = args.next().context(
                "usage: cargo xtask baseline-promote <current-benchmark.json> <baseline-name.json>",
            )?;
            baseline_promote(Path::new(&source), &destination).map(|path| {
                println!("{}", path.display());
            })
        }
        "baseline-promote-manifest" => {
            let manifest = args.next().context(
                "usage: cargo xtask baseline-promote-manifest <promotion-manifest.json>",
            )?;
            baseline_promote_manifest(Path::new(&manifest)).map(|path| {
                println!("{}", path.display());
            })
        }
        "baseline-refresh-plan" => {
            let source_root = args.next().map(PathBuf::from);
            baseline_refresh_plan(source_root.as_deref()).map(|path| {
                println!("{}", path.display());
            })
        }
        "workflow-audit" => workflow_audit().map(|path| {
            println!("{}", path.display());
        }),
        "docs-check" => documentation::docs_check(),
        "command-audit" => command_audit().map(|path| {
            println!("{}", path.display());
        }),
        "run-dev" => dev::run_dev(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => bail!("unknown xtask command {other:?}; run cargo xtask help"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProductValidateArgs {
    Help,
    Run {
        package: Option<String>,
        scenario: Option<String>,
    },
}

fn product_validate_args(args: Vec<String>) -> anyhow::Result<ProductValidateArgs> {
    if args.iter().any(|arg| is_help_arg(arg)) {
        return Ok(ProductValidateArgs::Help);
    }
    if args.len() > 2 {
        bail!("{PRODUCT_VALIDATE_USAGE}");
    }

    let mut args = args.into_iter();
    let first = args.next();
    let second = args.next();
    let first_is_scenario = first
        .as_deref()
        .is_some_and(is_product_validation_scenario_name);
    let (package, scenario) = if first_is_scenario && second.is_none() {
        (None, first)
    } else {
        (first, second)
    };

    Ok(ProductValidateArgs::Run { package, scenario })
}

fn is_help_arg(arg: &str) -> bool {
    matches!(arg, "help" | "--help" | "-h")
}

fn print_product_validate_help() {
    println!(
        "\
{PRODUCT_VALIDATE_USAGE}

Launches the normal release mirante4d-app binary with env-gated semantic
automation and writes scenario-scoped reports under
target/mirante4d/product-validation/<scenario>/.

Scenarios:
  generated_fixture_camera_smoke     bounded generated-fixture camera workflow
  generated_fixture_render_modes     generated-fixture MIP/DVR/ISO workflow
  t5_qual_001_interaction_mip               heavy T5Qual001 MIP workflow; requires package and heavy opt-in
  t5_qual_001_interaction_render_modes      heavy T5Qual001 MIP/DVR/ISO workflow; requires package and heavy opt-in
  t5_qual_001_interaction_continuous        heavy T5Qual001 continuous MIP/DVR/ISO workflow; requires package and heavy opt-in
  t5_qual_001_four_panel_cross_section      heavy T5Qual001 four-panel cross-section workflow; requires package and heavy opt-in
  t5_qual_001_four_panel_fine_scale         heavy T5Qual001 four-panel zoomed s0 workflow; requires package and heavy opt-in
  t5_qual_001_four_panel_continuous_cross_section
                                      heavy T5Qual001 repeated 2D pan/slice/zoom/oblique workflow; requires package and heavy opt-in
  t5_qual_002_four_panel_timepoint         heavy T5Qual002 four-panel timepoint workflow; requires package and heavy opt-in
  t5_qual_002_four_panel_autoplay          heavy T5Qual002 four-panel autoplay workflow; requires package and heavy opt-in
  custom_script                      uses MIRANTE4D_PRODUCT_VALIDATE_SCRIPT

Controls:
  MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS=<seconds>
  MIRANTE4D_PRODUCT_VALIDATE_SCENARIO=<scenario>
  MIRANTE4D_PRODUCT_VALIDATE_SCRIPT=<script.json>
  MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display|virtual_display
  MIRANTE4D_PRODUCT_VALIDATE_PREFLIGHT_ONLY=1
  MIRANTE4D_PRODUCT_VALIDATE_ALLOW_NO_DISPLAY=1
  MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD=1
  MIRANTE4D_PRODUCT_VALIDATE_MAX_RSS_BYTES=<bytes>
  MIRANTE4D_PRODUCT_VALIDATE_GPU_TIMESTAMPS=1"
    );
}

fn print_help() {
    println!(
        "\
Mirante4D developer tasks

Commands:
  cargo xtask verify-leaf policy|lint|unit|contract|ui|doctest
      runs one non-recursive verification leaf with its declared timeout and selector
  cargo xtask verify-pr [policy|rust]
      runs the pull-request policy and/or Rust group without recursively invoking xtask
  cargo xtask verify-local trusted-gpu
      runs the registry-owned ignored GPU union on an explicitly trusted local machine
      env: MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 acknowledges trusted-machine execution
  cargo xtask verification-sync [--check]
      generates or verifies the registry-derived Nextest configuration and selectors
  cargo xtask verify-deps
  cargo xtask verify-coverage
  cargo xtask generate-fixture basic-u16-16cube
  cargo xtask generate-fixture anisotropic-u16-16cube
  cargo xtask generate-fixture time-u16-8cube-3t
  cargo xtask generate-fixture time-multichannel-u16-8cube-3t-2c
  cargo xtask generate-fixture multichannel-u16-8cube-4c
  cargo xtask generate-fixture basic-f32-8cube
  cargo xtask package-dev
      builds the Linux release package directory, tarball, AppImage, contents report, and packaged smoke evidence
      env: MIRANTE4D_APPIMAGETOOL=<path> when appimagetool is not on PATH
  cargo xtask package-linux-release
      same Linux release packaging gate; prints the generated contents report path
      env: MIRANTE4D_APPIMAGETOOL=<path> when appimagetool is not on PATH
  cargo xtask bench-smoke
  cargo xtask bench-native-package <native-package.m4d>
      heavyweight local evidence; requires MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      guarded by a single-process heavyweight benchmark lock under target/mirante4d
      env: MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 acknowledges intentional heavyweight local work
      env: MIRANTE4D_XTASK_HEAVY_BENCHMARK_LOCK=<path> overrides the lock path
      env: MIRANTE4D_BENCH_VIEWPORT_WIDTH=<pixels> (default min(x, 1024))
      env: MIRANTE4D_BENCH_VIEWPORT_HEIGHT=<pixels> (default min(y, 1024))
      env: MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE=<pixels> (default 16)
      env: MIRANTE4D_PHASE11_GPU_VOLUME_CACHE_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_BRICK_CACHE_BYTES=<bytes> (default 2 GiB)
  cargo xtask bench-phase11-large-view <native-package.m4d>
      streaming-first large-view benchmark; does not read a dense source volume before the stream checkpoint
      heavyweight local evidence; requires MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      env: MIRANTE4D_BENCH_VIEWPORT_WIDTH=<pixels> (default min(x, 1024))
      env: MIRANTE4D_BENCH_VIEWPORT_HEIGHT=<pixels> (default min(y, 1024))
      env: MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE=<pixels> (default max(width, height) / 128 rounded up)
      env: MIRANTE4D_PHASE11_MAX_VISIBLE_BRICKS=<bricks> (default 1024)
      env: MIRANTE4D_PHASE11_MAX_DECODED_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_VOLUME_CACHE_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_BRICK_CACHE_BYTES=<bytes> (default 2 GiB)
  cargo xtask bench-phase11-interaction <native-package.m4d>
      deterministic first-frame/orbit/pan/zoom timeline benchmark; records LOD, GPU timings, and p50/p95/p99
      heavyweight local evidence; requires MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      env: MIRANTE4D_PHASE11_INTERACTION_STEPS=<frames per scenario> (default 5)
      env: MIRANTE4D_BENCH_VIEWPORT_WIDTH/HEIGHT=<pixels>
      env: MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE=<pixels>
      env: MIRANTE4D_PHASE11_MAX_VISIBLE_BRICKS=<bricks> (default 1024)
      env: MIRANTE4D_PHASE11_MAX_DECODED_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_VOLUME_CACHE_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_BRICK_CACHE_BYTES=<bytes> (default 2 GiB)
  cargo xtask bench-phase11-viewport-matrix <native-package.m4d>
      runs the Phase 11 interaction benchmark across 512, 720p, 1080p, and default-capped viewports
      heavyweight local evidence; requires MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      env: MIRANTE4D_PHASE11_HIDPI_VIEWPORT_WIDTH/HEIGHT=<pixels> (optional local high-DPI scenario)
      env: MIRANTE4D_PHASE11_MAXIMIZED_VIEWPORT_WIDTH/HEIGHT=<pixels> (optional local maximized scenario)
  cargo xtask bench-phase11-synthetic-matrix
      generates deterministic Phase 11 fixtures and runs the viewport matrix on them
  cargo xtask bench-phase13-renderer <native-package.m4d>
      renders MIP, DVR, and ISO through a Phase 13 schema with CPU/GPU timing and fidelity diagnostics
      heavyweight local evidence; requires MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      env: MIRANTE4D_BENCH_VIEWPORT_WIDTH/HEIGHT=<pixels>
      env: MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE=<pixels>
      env: MIRANTE4D_PHASE11_MAX_VISIBLE_BRICKS=<bricks> (default 1024)
      env: MIRANTE4D_PHASE11_MAX_DECODED_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_VOLUME_CACHE_BYTES=<bytes> (default 1 GiB)
      env: MIRANTE4D_PHASE11_GPU_BRICK_CACHE_BYTES=<bytes> (default 2 GiB)
  cargo xtask bench-phase13-viewport-matrix <native-package.m4d>
      runs the full Phase 13 renderer benchmark across 512, 720p, 1080p, and default-capped viewports
      heavyweight local evidence; requires MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      env: MIRANTE4D_PHASE11_HIDPI_VIEWPORT_WIDTH/HEIGHT=<pixels> (optional local high-DPI scenario)
      env: MIRANTE4D_PHASE11_MAXIMIZED_VIEWPORT_WIDTH/HEIGHT=<pixels> (optional local maximized scenario)
  cargo xtask app-smoke <native-package.m4d>
      developer release-app open smoke using MIRANTE4D_APP_SMOKE; writes JSON and log reports
  cargo xtask product-validate [native-package.m4d] [generated_fixture_camera_smoke|generated_fixture_render_modes|t5_qual_001_interaction_mip|t5_qual_001_interaction_render_modes|t5_qual_001_interaction_continuous|t5_qual_001_four_panel_cross_section|t5_qual_001_four_panel_fine_scale|t5_qual_001_four_panel_continuous_cross_section|t5_qual_002_four_panel_timepoint|t5_qual_002_four_panel_autoplay|custom_script]
      launches the real native app with env-gated semantic automation and writes product-validation artifacts
      help: cargo xtask product-validate --help
      no argument uses the generated basic-u16-16cube fixture; explicit paths target real native packages
      generated_fixture_render_modes switches MIP, DVR, and ISO on a bounded generated fixture
      t5_qual_001_interaction_mip requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_001_interaction_render_modes requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_001_interaction_continuous requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_001_four_panel_cross_section requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_001_four_panel_fine_scale requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_001_four_panel_continuous_cross_section requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_002_four_panel_timepoint requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      t5_qual_002_four_panel_autoplay requires <native-package.m4d> and MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
      env: MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS=<seconds> (default 60; heavy local sample scenarios default 180/240/300)
      env: MIRANTE4D_PRODUCT_VALIDATE_SCENARIO=<scenario> selects a named scenario when no CLI scenario is given
      env: MIRANTE4D_PRODUCT_VALIDATE_SCRIPT=<script.json> runs a custom automation script; pair with custom_script or omit scenario
      env: MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display|virtual_display overrides display classification
      env: MIRANTE4D_PRODUCT_VALIDATE_PREFLIGHT_ONLY=1 writes script/report metadata without building or launching the app
      env: MIRANTE4D_PRODUCT_VALIDATE_ALLOW_NO_DISPLAY=1 attempts launch even when no DISPLAY/WAYLAND_DISPLAY exists
      env: MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD=1 reuses target/release/mirante4d-app
      env: MIRANTE4D_PRODUCT_VALIDATE_MAX_RSS_BYTES=<bytes> kills the launched app if it exceeds the wrapper RSS guard
      env: MIRANTE4D_PRODUCT_VALIDATE_GPU_TIMESTAMPS=1 requests renderer timestamp-query samples when supported
  cargo xtask bench-runtime-stress
      env: MIRANTE4D_BENCH_HARDWARE_NAME=<name> (default local-dev-machine)
      env: MIRANTE4D_BENCH_HARDWARE_CLASS=<class> (default hardware name)
      env: MIRANTE4D_BENCH_BASELINE_CLASS=<synthetic_ci|local_gpu|private_local_heavy> (default local_gpu)
      env: MIRANTE4D_BENCH_STRESS_T/Z/Y/X=<voxels> (default 3/64/128/128)
      env: MIRANTE4D_BENCH_STRESS_BRICK_Z/Y/X=<voxels> (default 16/16/16)
      env: MIRANTE4D_BENCH_STRESS_BRICK_PIXEL_STRIDE=<pixels> (default 1)
      env: MIRANTE4D_BENCH_STRESS_WORKERS=<count> (default 4)
      env: MIRANTE4D_BENCH_STRESS_GPU_SET_SIZE=<bricks> (default 32)
  cargo xtask bench-import-sample <experiment-folder-name>
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
      env: MIRANTE4D_BENCH_IMPORT_MAX_FILES=<count> (default 4)
      env: MIRANTE4D_BENCH_HARDWARE_NAME/CLASS and MIRANTE4D_BENCH_BASELINE_CLASS set comparable report context
  cargo xtask phase10-audit
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
      env: MIRANTE4D_PHASE10_EXPERIMENTS=<comma-separated private resolver folders> (default T5-QUAL-001..003 when available)
      env: MIRANTE4D_BENCH_IMPORT_MAX_FILES=<count> (default 4)
      env: MIRANTE4D_PHASE10_BENCH_VIEWPORT_WIDTH=<pixels> (default 128)
      env: MIRANTE4D_PHASE10_BENCH_VIEWPORT_HEIGHT=<pixels> (default 128)
      env: MIRANTE4D_PHASE10_BENCH_BRICK_PIXEL_STRIDE=<pixels> (default 64)
  cargo xtask phase12-audit
      viewer-usability audit for generated data plus opaque T5 qualification inputs when available
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
      env: MIRANTE4D_PHASE12_EXPERIMENTS=<comma-separated private resolver folders> (default T5-QUAL-003,T5-QUAL-001 when available)
      env: MIRANTE4D_BENCH_IMPORT_MAX_FILES=<count> (default 4)
  cargo xtask phase14-audit
      multi-channel fixture/sample inventory plus visible-vs-hidden channel resource evidence
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
  cargo xtask bench-phase14-multichannel
      writes the Phase 14 synthetic multi-channel benchmark JSON report
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
  cargo xtask phase15-audit
      analysis-workbench audit with typed operation records, source streaming, CSV metadata, and SVG export evidence
  cargo xtask bench-phase15-analysis
      writes the Phase 15 deterministic analysis benchmark JSON report plus exported table/plot artifacts
  cargo xtask phase17-audit
      import metadata hardening audit with approved-format matrix, reviewed TIFF plan, strict validation, and provenance evidence
  cargo xtask phase19-audit
      viewer product hardening audit with generated playback smoke, real-sample import/open smoke, and renderer evidence
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
      env: MIRANTE4D_PHASE19_EXPERIMENTS=<comma-separated private resolver folders> (default T5-QUAL-003,T5-QUAL-001)
      env: MIRANTE4D_BENCH_IMPORT_MAX_FILES=<count> (default 4)
  cargo xtask phase20-smoke-audit
      generated stack-series and plane-series import/open smoke evidence for Phase 20
      env: MIRANTE4D_PHASE20_OUTPUT_ROOT=<directory> (default target/mirante4d/phase20)
  cargo xtask phase20-extreme-audit
      full local T5-QUAL-002/T5-QUAL-001 import/open evidence for Phase 20; intentionally heavy
      env: MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 required
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
      env: MIRANTE4D_PHASE20_OUTPUT_ROOT=<directory> (default target/mirante4d/phase20)
  cargo xtask phase20-extreme-sample <T5-QUAL-001|T5-QUAL-002|T5-QUAL-003>
      one local Phase 20 extreme sample import/open evidence run
      env: MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 required
      env: MIRANTE4D_SAMPLE_DATA=<sample root>
      env: MIRANTE4D_PHASE20_OUTPUT_ROOT=<directory> (default target/mirante4d/phase20)
  cargo xtask bench-check <current-benchmark.json> <baseline-benchmark.json>
      env: MIRANTE4D_BENCH_WARN_SLOWDOWN_PCT=<percent> (default 10)
      env: MIRANTE4D_BENCH_FAIL_SLOWDOWN_PCT=<percent> (default 20)
      report writers also honor MIRANTE4D_BENCH_DATASET_CLASS for deliberate native-package baseline runs
  cargo xtask neuroglancer-compare <comparison-manifest.json>
      compares Mirante product-validation latency rows against a Neuroglancer measurement JSON
      manifest schema: mirante4d-neuroglancer-comparison-input v1 with
        {{mirante_reports: [<product-validation-report.json>], neuroglancer_measurement: <measurement.json>, output_report?: <report.json>}}
      Neuroglancer measurement schema: neuroglancer-cross-section-performance-measurement v1
      writes target/mirante4d/neuroglancer-comparison/neuroglancer-comparison-report.json by default
  cargo xtask baseline-audit
      audits curated benchmark baselines for baseline_class and compatibility context
  cargo xtask baseline-promote <current-benchmark.json> <baseline-name.json>
      promotes a clean release benchmark report into docs/benchmarks/baselines/
      env: MIRANTE4D_BASELINE_PROMOTE_REPLACE=1 required to replace an existing baseline
      env: MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 required to promote private_local_heavy baselines
  cargo xtask baseline-promote-manifest <promotion-manifest.json>
      promotes a clean-worktree batch of release benchmark reports into curated baselines
      manifest schema: mirante4d-baseline-promotion-manifest v1 with entries [{{source_report, baseline_name}}]
      env: MIRANTE4D_BASELINE_PROMOTE_REPLACE=1 required to replace existing baselines
      env: MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 required for any private_local_heavy baseline
  cargo xtask baseline-refresh-plan [benchmark-report-root]
      matches stale curated baselines to benchmark reports and writes a non-mutating refresh plan
      default report root: target/mirante4d/benchmarks
      writes target/mirante4d/baseline-refresh/baseline-refresh-plan.json
      writes target/mirante4d/baseline-refresh/baseline-promotion-manifest.json only when every stale baseline has one unique promotable source
  cargo xtask workflow-audit
      audits GitHub Actions workflow files for evidence-role naming, xtask gates, artifact uploads, and private-data exclusions
  cargo xtask docs-check
      checks the exact documentation inventory, authority graph, read order, local links, and anchors
  cargo xtask command-audit
      writes machine-readable xtask command classification and quarantine reports
  cargo xtask run-dev"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn product_validate_help_args_do_not_become_dataset_paths() {
        for help_arg in ["--help", "-h", "help"] {
            assert_eq!(
                product_validate_args(args(&[help_arg])).unwrap(),
                ProductValidateArgs::Help
            );
        }
    }

    #[test]
    fn product_validate_args_preserve_scenario_shorthand() {
        assert_eq!(
            product_validate_args(args(&["generated_fixture_render_modes"])).unwrap(),
            ProductValidateArgs::Run {
                package: None,
                scenario: Some("generated_fixture_render_modes".to_owned())
            }
        );
        assert_eq!(
            product_validate_args(args(&["sample.m4d", "t5_qual_001_interaction_mip"])).unwrap(),
            ProductValidateArgs::Run {
                package: Some("sample.m4d".to_owned()),
                scenario: Some("t5_qual_001_interaction_mip".to_owned())
            }
        );
        assert_eq!(
            product_validate_args(args(&[
                "sample.m4d",
                "t5_qual_001_four_panel_cross_section"
            ]))
            .unwrap(),
            ProductValidateArgs::Run {
                package: Some("sample.m4d".to_owned()),
                scenario: Some("t5_qual_001_four_panel_cross_section".to_owned())
            }
        );
    }

    #[test]
    fn product_validate_args_reject_too_many_non_help_args() {
        let err = product_validate_args(args(&[
            "sample.m4d",
            "generated_fixture_camera_smoke",
            "extra",
        ]))
        .unwrap_err()
        .to_string();

        assert!(err.contains("usage: cargo xtask product-validate"));
    }
}
