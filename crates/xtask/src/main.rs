use std::{env, path::Path};

use anyhow::{Context, bail};

use crate::command_audit::command_audit;
use crate::product_validate::{is_product_validation_scenario_name, product_validate};
use crate::smoke::app_smoke;
use crate::workflow_audit::workflow_audit;

const PRODUCT_VALIDATE_USAGE: &str = "usage: cargo xtask product-validate [target-package] \
     [target_fixture_camera_smoke|target_fixture_render_modes|target_source_verification|b4_project_persistence|\
      t5_qual_001_interaction_mip|t5_qual_001_interaction_render_modes|t5_qual_001_interaction_continuous|\
      t5_qual_001_four_panel_cross_section|t5_qual_001_four_panel_fine_scale|\
      t5_qual_001_four_panel_continuous_cross_section|t5_qual_002_four_panel_timepoint|t5_qual_002_four_panel_autoplay|custom_script]";

mod arch;
mod command_audit;
mod deps;
mod dev;
mod documentation;
mod host;
mod ids;
mod package;
mod process;
mod product_validate;
mod reports;
mod smoke;
mod target_fixture;
mod verification;
mod workflow_audit;

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
        "package-dev" => package::package_dev().map(|path| println!("{}", path.display())),
        "package-linux-release" => {
            package::package_linux_release().map(|path| println!("{}", path.display()))
        }
        "app-smoke" => {
            let package = args
                .next()
                .context("usage: cargo xtask app-smoke <target-package>")?;
            if args.next().is_some() {
                bail!("usage: cargo xtask app-smoke <target-package>");
            }
            app_smoke(Path::new(&package)).map(|path| println!("{}", path.display()))
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
        "docs-check" => documentation::docs_check(),
        "command-audit" => command_audit().map(|path| println!("{}", path.display())),
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
target_fixture_render_modes, and target_source_verification. The T5 scenarios
require an explicit local package and the heavy-work opt-in. Use custom_script
with MIRANTE4D_PRODUCT_VALIDATE_SCRIPT=<script.json> for a reviewed script.

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

  cargo xtask verify-leaf policy|lint|unit|contract|ui|doctest
  cargo xtask verify-pr [policy|rust]
  cargo xtask verify-local <format-lifecycle|project-store-lifecycle|trusted-gpu>
  cargo xtask verification-sync [--check]
  cargo xtask verify-deps
  cargo xtask package-dev
  cargo xtask package-linux-release
  cargo xtask app-smoke <target-package>
  cargo xtask product-validate [target-package] [scenario]
  cargo xtask workflow-audit
  cargo xtask docs-check
  cargo xtask command-audit
  cargo xtask run-dev

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
            product_validate_args(args(&["b4_project_persistence"])).unwrap(),
            ProductValidateArgs::Run {
                package: None,
                scenario: Some("b4_project_persistence".to_owned())
            }
        );
        assert_eq!(
            product_validate_args(args(&["sample.m4d", "t5_qual_001_interaction_mip"])).unwrap(),
            ProductValidateArgs::Run {
                package: Some("sample.m4d".to_owned()),
                scenario: Some("t5_qual_001_interaction_mip".to_owned())
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
