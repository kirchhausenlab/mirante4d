use std::{env, path::Path};

use anyhow::{Context, bail};

use crate::product_validate::{is_product_validation_scenario_name, product_validate};
use crate::workflow_audit::workflow_audit;

const PRODUCT_VALIDATE_USAGE: &str = "usage: cargo xtask product-validate [target-package] \
     [target_fixture_camera_smoke|target_fixture_render_modes|target_fixture_resident_navigation_no_readback|target_source_verification|import_preprocessing|b4_project_persistence]";

mod arch;
#[cfg(test)]
mod build_contract;
mod deps;
mod dev;
mod documentation;
mod host;
mod import_performance;
mod import_performance_t5;
mod package;
mod private_evidence;
mod process;
mod product_automation_progress;
mod product_validate;
mod reports;
mod t5_sentinel_oracle;
mod target_fixture;
mod verification;
mod workflow_audit;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "verify-leaf" => {
            let leaf = args
                .next()
                .context("usage: cargo xtask verify-leaf policy|lint|unit|contract|ui")?;
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
            let lane = args.next().context(
                "usage: cargo xtask verify-local <format-lifecycle|project-store-lifecycle|trusted-gpu>",
            )?;
            if args.next().is_some() {
                bail!(
                    "usage: cargo xtask verify-local <format-lifecycle|project-store-lifecycle|trusted-gpu>"
                );
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
        "verify-deps" => deps::verify_deps(),
        "package-linux-release" => {
            package::package_linux_release().map(|path| println!("{}", path.display()))
        }
        "product-validate" => match product_validate_args(args.collect())? {
            ProductValidateArgs::Help => {
                print_product_validate_help();
                Ok(())
            }
            ProductValidateArgs::Run { package, scenario } => {
                product_validate(package.as_deref().map(Path::new), scenario.as_deref())
                    .map(|path| println!("{}", path.display()))
            }
        },
        "workflow-audit" => workflow_audit().map(|path| println!("{}", path.display())),
        "import-performance-t2" => {
            import_performance::run(args.collect()).map(|path| println!("{}", path.display()))
        }
        "import-performance-t5" => {
            import_performance_t5::run(args.collect()).map(|path| println!("{}", path.display()))
        }
        "import-performance-t5-publish" => import_performance_t5::publish(args.collect())
            .map(|path| println!("{}", path.display())),
        "import-performance-t5-oracle-audit" => {
            import_performance_t5::run_oracle_audit(args.collect())
        }
        "__import-performance-t2-worker" => import_performance::run_worker(args.collect()),
        "docs-check" => documentation::docs_check(),
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

Launches the normal release Mirante4D application and writes a scoped report
under target/mirante4d/product-validation/. With no package argument, the
bounded promoted target U16 fixture is extracted locally.

The ordinary bounded scenarios are target_fixture_camera_smoke,
target_fixture_render_modes, target_fixture_resident_navigation_no_readback,
target_source_verification, and import_preprocessing. The resident-navigation
scenario proves warm camera reuse without framebuffer readback. The import scenario generates a bounded public TIFF
fixture, cancels and resumes preprocessing, waits for verified publication,
then renders the imported package. The retained b4_project_persistence
scenario checks project save, recovery, and reopen behavior across three
application launches.

Useful controls:
  MIRANTE4D_PRODUCT_VALIDATE_TIMEOUT_SECS=<seconds>
  MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display|virtual_display
  MIRANTE4D_PRODUCT_VALIDATE_PREFLIGHT_ONLY=1
  MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=<packaged-executable> (uses it directly; skips build)
  MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD=1"
    );
}

fn print_help() {
    println!(
        "\
Mirante4D developer tasks

  cargo xtask verify-leaf policy|lint|unit|contract|ui
  cargo xtask verify-pr [policy|rust]
  cargo xtask verify-local <format-lifecycle|project-store-lifecycle|trusted-gpu>
  cargo xtask verification-sync [--check]
  cargo xtask verify-deps
  cargo xtask package-linux-release
  cargo xtask product-validate [target-package] [scenario]
  cargo xtask workflow-audit
  cargo run --release -p xtask -- import-performance-t2 [--samples 5] [--qualification-profile PATH]
  cargo run --release -p xtask -- import-performance-t5 --config /absolute/private/config.json [--performance | --diagnostic]
  cargo run --release -p xtask -- import-performance-t5-publish --config /absolute/private/config.json --raw-report /absolute/private/raw-private-report.json
  cargo run --release -p xtask -- import-performance-t5-oracle-audit --config /absolute/private/config.json
  cargo xtask docs-check
  cargo xtask run-dev

T2 and T5 qualification require matching local, non-repository qualification profiles.
T5 also requires an owner-pinned private configuration; --diagnostic never qualifies.
The T5 publisher only replays sanitized publication from a finalized private raw report.
The T5 oracle audit is an explicit offline check of that pinned v2 configuration.

Run cargo xtask product-validate --help for scenario details."
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
            product_validate_args(args(&["target_fixture_render_modes"])).unwrap(),
            ProductValidateArgs::Run {
                package: None,
                scenario: Some("target_fixture_render_modes".to_owned())
            }
        );
        assert_eq!(
            product_validate_args(args(&["target_fixture_resident_navigation_no_readback",]))
                .unwrap(),
            ProductValidateArgs::Run {
                package: None,
                scenario: Some("target_fixture_resident_navigation_no_readback".to_owned())
            }
        );
        assert_eq!(
            product_validate_args(args(&["b4_project_persistence"])).unwrap(),
            ProductValidateArgs::Run {
                package: None,
                scenario: Some("b4_project_persistence".to_owned())
            }
        );
        assert_eq!(
            product_validate_args(args(&["import_preprocessing"])).unwrap(),
            ProductValidateArgs::Run {
                package: None,
                scenario: Some("import_preprocessing".to_owned())
            }
        );
    }

    #[test]
    fn product_validate_args_reject_too_many_non_help_args() {
        let error = product_validate_args(args(&[
            "sample.m4d",
            "target_fixture_camera_smoke",
            "extra",
        ]))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("usage: cargo xtask product-validate")
        );
    }
}
