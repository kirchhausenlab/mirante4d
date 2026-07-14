use std::{fs, path::Path, process::Stdio};

use anyhow::{Context, bail};
use serde::Deserialize;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const DEPENDENCY_EXCEPTIONS_DOC: &str = "docs/DEPENDENCY_EXCEPTIONS.md";
const DENY_CONFIG: &str = "deny.toml";

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CargoMetadata {
    pub(crate) packages: Vec<CargoMetadataPackage>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CargoMetadataPackage {
    pub(crate) name: String,
    pub(crate) version: String,
    source: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) license_file: Option<String>,
}

pub(crate) fn verify_deps() -> anyhow::Result<()> {
    let metadata = cargo_metadata_all_features()?;
    check_package_source_policy(&metadata)?;
    check_package_license_policy(&metadata)?;
    ensure_dependency_exceptions_doc()?;
    run_cargo_deny()
}

pub(crate) fn cargo_metadata() -> anyhow::Result<CargoMetadata> {
    cargo_metadata_with_args(&[])
}

fn cargo_metadata_all_features() -> anyhow::Result<CargoMetadata> {
    cargo_metadata_with_args(&["--all-features"])
}

fn cargo_metadata_with_args(extra_args: &[&str]) -> anyhow::Result<CargoMetadata> {
    let output = crate::process::cargo_command()
        .args(["metadata", "--format-version", "1", "--locked"])
        .args(extra_args)
        .output()
        .context("failed to run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed with status {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("failed to parse cargo metadata JSON")
}

fn check_package_source_policy(metadata: &CargoMetadata) -> anyhow::Result<()> {
    let mut violations = Vec::new();
    for package in &metadata.packages {
        match package.source.as_deref() {
            None => {}
            Some(CRATES_IO_SOURCE) => {}
            Some(source) => violations.push(format!(
                "{} {} uses unsupported dependency source {source:?}",
                package.name, package.version
            )),
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "dependency source policy failed:\n{}",
            violations.join("\n")
        )
    }
}

fn check_package_license_policy(metadata: &CargoMetadata) -> anyhow::Result<()> {
    let mut violations = Vec::new();
    for package in &metadata.packages {
        match package.license.as_deref() {
            Some(license) if license_expression_allowed(&package.name, license) => {}
            Some(license) => violations.push(format!(
                "{} {} has unsupported license expression {license:?}",
                package.name, package.version
            )),
            None if package.license_file.is_some() => violations.push(format!(
                "{} {} uses a license file without a machine-readable SPDX expression",
                package.name, package.version
            )),
            None => violations.push(format!(
                "{} {} has no machine-readable license",
                package.name, package.version
            )),
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "dependency license policy failed:\n{}",
            violations.join("\n")
        )
    }
}

fn license_expression_allowed(package_name: &str, expression: &str) -> bool {
    normalize_license_expression(expression)
        .split(" AND ")
        .all(|clause| {
            clause
                .split(" OR ")
                .any(|license| license_atom_allowed(package_name, license.trim()))
        })
}

fn normalize_license_expression(expression: &str) -> String {
    expression
        .replace(['(', ')'], "")
        .replace(" / ", " OR ")
        .replace('/', " OR ")
}

fn license_atom_allowed(package_name: &str, license: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "0BSD",
        "Apache-2.0",
        "Apache-2.0 WITH LLVM-exception",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "BSL-1.0",
        "CC0-1.0",
        "ISC",
        "MIT",
        "MIT-0",
        "Unicode-3.0",
        "Unlicense",
        "Zlib",
    ];
    if ALLOWED.contains(&license) {
        return true;
    }
    documented_license_exception(package_name, license)
}

fn documented_license_exception(package_name: &str, license: &str) -> bool {
    matches!(
        (package_name, license),
        ("colored", "MPL-2.0") | ("epaint_default_fonts", "OFL-1.1" | "Ubuntu-font-1.0")
    )
}

fn ensure_dependency_exceptions_doc() -> anyhow::Result<()> {
    let text = fs::read_to_string(DEPENDENCY_EXCEPTIONS_DOC)
        .with_context(|| format!("failed to read {DEPENDENCY_EXCEPTIONS_DOC}"))?;
    let required = [
        "epaint_default_fonts",
        "OFL-1.1",
        "Ubuntu-font-1.0",
        "colored",
        "MPL-2.0",
    ];
    for needle in required {
        if !text.contains(needle) {
            bail!("{DEPENDENCY_EXCEPTIONS_DOC} must document dependency exception {needle:?}");
        }
    }
    Ok(())
}

fn run_cargo_deny() -> anyhow::Result<()> {
    if !Path::new(DENY_CONFIG).is_file() {
        bail!("{DENY_CONFIG} is required for dependency advisory/license/source checks");
    }

    let status = crate::process::cargo_command()
        .args(["deny", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to check cargo-deny")?;
    if !status.success() {
        bail!(
            "cargo-deny is required for advisory checks; install it with `cargo install cargo-deny --locked`"
        );
    }

    crate::process::run_cargo([
        "deny",
        "check",
        "--hide-inclusion-graph",
        "advisories",
        "licenses",
        "bans",
        "sources",
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_policy_accepts_common_permissive_expressions() {
        assert!(license_expression_allowed(
            "self_cell",
            "Apache-2.0 OR GPL-2.0-only"
        ));
        assert!(license_expression_allowed(
            "unicode-ident",
            "(MIT OR Apache-2.0) AND Unicode-3.0"
        ));
        assert!(license_expression_allowed("cgl", "MIT / Apache-2.0"));
    }

    #[test]
    fn license_policy_rejects_unsupported_required_licenses() {
        assert!(!license_expression_allowed("bad", "GPL-3.0-only"));
        assert!(!license_expression_allowed(
            "bad-fonts",
            "(MIT OR Apache-2.0) AND OFL-1.1"
        ));
    }

    #[test]
    fn license_policy_allows_documented_package_specific_font_exception() {
        assert!(license_expression_allowed(
            "epaint_default_fonts",
            "(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0"
        ));
    }

    #[test]
    fn license_policy_allows_documented_package_specific_ui_snapshot_exception() {
        assert!(license_expression_allowed("colored", "MPL-2.0"));
        assert!(!license_expression_allowed(
            "runtime-colored-copy",
            "MPL-2.0"
        ));
    }

    #[test]
    fn source_policy_accepts_only_path_and_crates_io_sources() {
        let metadata = CargoMetadata {
            packages: vec![
                CargoMetadataPackage {
                    name: "local".to_owned(),
                    version: "0.1.0".to_owned(),
                    source: None,
                    license: Some("MIT OR Apache-2.0".to_owned()),
                    license_file: None,
                },
                CargoMetadataPackage {
                    name: "external".to_owned(),
                    version: "1.0.0".to_owned(),
                    source: Some(CRATES_IO_SOURCE.to_owned()),
                    license: Some("MIT".to_owned()),
                    license_file: None,
                },
            ],
        };

        check_package_source_policy(&metadata).unwrap();
    }

    #[test]
    fn source_policy_rejects_unknown_sources() {
        let metadata = CargoMetadata {
            packages: vec![CargoMetadataPackage {
                name: "external".to_owned(),
                version: "1.0.0".to_owned(),
                source: Some("git+https://example.invalid/repo".to_owned()),
                license: Some("MIT".to_owned()),
                license_file: None,
            }],
        };

        assert!(check_package_source_policy(&metadata).is_err());
    }
}
