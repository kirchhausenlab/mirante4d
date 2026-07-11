use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Stdio,
};

use anyhow::{Context, bail};
use serde::Deserialize;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const DEPENDENCY_EXCEPTIONS_DOC: &str = "docs/DEPENDENCY_EXCEPTIONS.md";
const DENY_CONFIG: &str = "deny.toml";
const PASTE_EXCEPTION_ID: &str = "EXC-PASTE-WP10A-1";
const REVIEWED_PASTE_PARENTS: &[(&str, &str)] = &[
    ("zarrs", "0.23.13"),
    ("zarrs_data_type", "0.9.0"),
    ("zarrs_plugin", "0.4.1"),
];

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CargoMetadata {
    pub(crate) packages: Vec<CargoMetadataPackage>,
    #[serde(default)]
    workspace_members: Vec<String>,
    resolve: Option<CargoMetadataResolve>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CargoMetadataPackage {
    #[serde(default)]
    id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    source: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) license_file: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct CargoMetadataResolve {
    nodes: Vec<CargoMetadataNode>,
}

#[derive(Clone, Debug, Deserialize)]
struct CargoMetadataNode {
    id: String,
    deps: Vec<CargoMetadataNodeDependency>,
}

#[derive(Clone, Debug, Deserialize)]
struct CargoMetadataNodeDependency {
    pkg: String,
}

pub(crate) fn verify_deps() -> anyhow::Result<()> {
    let metadata = cargo_metadata_all_features()?;
    check_package_source_policy(&metadata)?;
    check_package_license_policy(&metadata)?;
    ensure_dependency_exceptions_doc()?;
    check_paste_exception_inclusion_graph(&metadata)?;
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
        "paste",
        "RUSTSEC-2024-0436",
        PASTE_EXCEPTION_ID,
        "paste 1.0.15",
        "zarrs 0.23.13",
        "zarrs_data_type 0.9.0",
        "zarrs_plugin 0.4.1",
        "current schema-1",
        "WP-10C",
        "target storage to inherit the exception",
        "every Zarr dependency update",
    ];
    for needle in required {
        if !text.contains(needle) {
            bail!("{DEPENDENCY_EXCEPTIONS_DOC} must document dependency exception {needle:?}");
        }
    }
    let deny =
        fs::read_to_string(DENY_CONFIG).with_context(|| format!("failed to read {DENY_CONFIG}"))?;
    for needle in [PASTE_EXCEPTION_ID, "RUSTSEC-2024-0436"] {
        if !deny.contains(needle) {
            bail!("{DENY_CONFIG} must bind dependency exception {needle:?}");
        }
    }
    Ok(())
}

fn check_paste_exception_inclusion_graph(metadata: &CargoMetadata) -> anyhow::Result<()> {
    let resolve = metadata
        .resolve
        .as_ref()
        .context("cargo metadata omitted the resolved dependency graph")?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let graph = resolve
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                node.deps
                    .iter()
                    .map(|dependency| dependency.pkg.as_str())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let paste = reviewed_crates_io_package(metadata, "paste", "1.0.15")?;
    let paste_packages = metadata
        .packages
        .iter()
        .filter(|package| package.name == "paste")
        .count();
    if paste_packages != 1 {
        bail!("{PASTE_EXCEPTION_ID} expected exactly one paste package, found {paste_packages}");
    }

    let expected = REVIEWED_PASTE_PARENTS
        .iter()
        .map(|(name, version)| reviewed_crates_io_package(metadata, name, version))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let expected = expected
        .iter()
        .map(|package| package.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for node in &resolve.nodes {
        if node
            .deps
            .iter()
            .any(|dependency| dependency.pkg == paste.id)
        {
            actual.insert(node.id.as_str());
        }
    }
    if actual != expected {
        bail!(
            "{PASTE_EXCEPTION_ID} direct-parent graph changed; reviewed={expected:?}, actual={actual:?}"
        );
    }

    let bridges = ["mirante4d-data", "mirante4d-format"]
        .map(|name| workspace_package(metadata, &packages, name))
        .into_iter()
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let no_blocks = BTreeSet::new();
    for bridge in &bridges {
        if !dependency_reachable(&graph, bridge, &paste.id, &no_blocks) {
            bail!(
                "{PASTE_EXCEPTION_ID} bridge {} no longer reaches paste",
                package_label(packages[bridge])
            );
        }
    }

    for storage in metadata
        .packages
        .iter()
        .filter(|package| package.name == "mirante4d-storage")
    {
        if dependency_reachable(&graph, &storage.id, &paste.id, &no_blocks) {
            bail!(
                "{PASTE_EXCEPTION_ID} cannot be inherited by target storage: {} reaches paste",
                package_label(storage)
            );
        }
    }

    let mut bypasses = Vec::new();
    for member in &metadata.workspace_members {
        if bridges.contains(member.as_str()) {
            continue;
        }
        if dependency_reachable(&graph, member, &paste.id, &bridges) {
            let package = packages.get(member.as_str()).with_context(|| {
                format!("cargo metadata omitted workspace package record for {member}")
            })?;
            bypasses.push(package_label(package));
        }
    }
    if bypasses.is_empty() {
        Ok(())
    } else {
        bail!(
            "{PASTE_EXCEPTION_ID} workspace paths bypass mirante4d-data/mirante4d-format: {}",
            bypasses.join(", ")
        )
    }
}

fn reviewed_crates_io_package<'a>(
    metadata: &'a CargoMetadata,
    name: &str,
    version: &str,
) -> anyhow::Result<&'a CargoMetadataPackage> {
    let matches = metadata
        .packages
        .iter()
        .filter(|package| {
            package.name == name
                && package.version == version
                && package.source.as_deref() == Some(CRATES_IO_SOURCE)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [package] => Ok(package),
        _ => bail!(
            "{PASTE_EXCEPTION_ID} expected exactly one crates.io {name} {version} package, found {}",
            matches.len()
        ),
    }
}

fn workspace_package<'a>(
    metadata: &'a CargoMetadata,
    packages: &BTreeMap<&str, &'a CargoMetadataPackage>,
    name: &str,
) -> anyhow::Result<&'a str> {
    let matches = metadata
        .workspace_members
        .iter()
        .filter(|member| {
            packages
                .get(member.as_str())
                .is_some_and(|package| package.name == name)
        })
        .map(String::as_str)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [package] => Ok(package),
        _ => bail!(
            "{PASTE_EXCEPTION_ID} expected exactly one {name} workspace package, found {}",
            matches.len()
        ),
    }
}

fn dependency_reachable<'a>(
    graph: &BTreeMap<&'a str, Vec<&'a str>>,
    start: &'a str,
    target: &str,
    blocked: &BTreeSet<&str>,
) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(package) = pending.pop() {
        if !visited.insert(package) {
            continue;
        }
        if package == target {
            return true;
        }
        if package != start && blocked.contains(package) {
            continue;
        }
        if let Some(dependencies) = graph.get(package) {
            pending.extend(dependencies.iter().copied());
        }
    }
    false
}

fn package_label(package: &CargoMetadataPackage) -> String {
    let proc_macro = if package.name == "paste" {
        " (proc-macro)"
    } else {
        ""
    };
    format!("{} v{}{}", package.name, package.version, proc_macro)
}

fn run_cargo_deny() -> anyhow::Result<()> {
    if !Path::new("deny.toml").is_file() {
        bail!("deny.toml is required for dependency advisory/license/source checks");
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
                    id: "local".to_owned(),
                    name: "local".to_owned(),
                    version: "0.1.0".to_owned(),
                    source: None,
                    license: Some("MIT OR Apache-2.0".to_owned()),
                    license_file: None,
                },
                CargoMetadataPackage {
                    id: "external".to_owned(),
                    name: "external".to_owned(),
                    version: "1.0.0".to_owned(),
                    source: Some(CRATES_IO_SOURCE.to_owned()),
                    license: Some("MIT".to_owned()),
                    license_file: None,
                },
            ],
            workspace_members: Vec::new(),
            resolve: None,
        };

        check_package_source_policy(&metadata).unwrap();
    }

    #[test]
    fn source_policy_rejects_unknown_sources() {
        let metadata = CargoMetadata {
            packages: vec![CargoMetadataPackage {
                id: "external".to_owned(),
                name: "external".to_owned(),
                version: "1.0.0".to_owned(),
                source: Some("git+https://example.invalid/repo".to_owned()),
                license: Some("MIT".to_owned()),
                license_file: None,
            }],
            workspace_members: Vec::new(),
            resolve: None,
        };

        assert!(check_package_source_policy(&metadata).is_err());
    }

    #[test]
    fn paste_exception_requires_exact_reviewed_inclusion_graph() {
        let reviewed = reviewed_paste_metadata();
        check_paste_exception_inclusion_graph(&reviewed).unwrap();

        let mut extra_parent = reviewed.clone();
        extra_parent
            .packages
            .push(test_package("extra", "unreviewed", "1.0.0"));
        extra_parent
            .resolve
            .as_mut()
            .unwrap()
            .nodes
            .push(test_node("extra", &["paste"]));
        assert!(check_paste_exception_inclusion_graph(&extra_parent).is_err());

        let mut alternate_paste = reviewed.clone();
        alternate_paste
            .packages
            .push(test_package("paste-old", "paste", "1.0.14"));
        alternate_paste
            .resolve
            .as_mut()
            .unwrap()
            .nodes
            .push(test_node("paste-old", &[]));
        assert!(check_paste_exception_inclusion_graph(&alternate_paste).is_err());

        let mut unrelated_zarrs = reviewed.clone();
        unrelated_zarrs
            .packages
            .push(test_package("zarrs-new", "zarrs", "0.24.0"));
        unrelated_zarrs
            .resolve
            .as_mut()
            .unwrap()
            .nodes
            .push(test_node("zarrs-new", &[]));
        check_paste_exception_inclusion_graph(&unrelated_zarrs).unwrap();

        let mut path_patched_parent = reviewed.clone();
        path_patched_parent
            .packages
            .iter_mut()
            .find(|package| package.name == "zarrs")
            .unwrap()
            .source = None;
        assert!(check_paste_exception_inclusion_graph(&path_patched_parent).is_err());

        let mut bypass = reviewed.clone();
        bypass
            .resolve
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.id == "app")
            .unwrap()
            .deps
            .push(CargoMetadataNodeDependency {
                pkg: "zarrs".to_owned(),
            });
        assert!(check_paste_exception_inclusion_graph(&bypass).is_err());

        let mut target_storage = reviewed;
        target_storage.workspace_members.push("storage".to_owned());
        target_storage
            .packages
            .push(test_package("storage", "mirante4d-storage", "0.1.0"));
        target_storage
            .resolve
            .as_mut()
            .unwrap()
            .nodes
            .push(test_node("storage", &["data"]));
        assert!(check_paste_exception_inclusion_graph(&target_storage).is_err());
    }

    fn reviewed_paste_metadata() -> CargoMetadata {
        CargoMetadata {
            packages: vec![
                test_package("paste", "paste", "1.0.15"),
                test_package("zarrs", "zarrs", "0.23.13"),
                test_package("zarrs-data-type", "zarrs_data_type", "0.9.0"),
                test_package("zarrs-plugin", "zarrs_plugin", "0.4.1"),
                test_package("data", "mirante4d-data", "0.1.0"),
                test_package("format", "mirante4d-format", "0.1.0"),
                test_package("app", "mirante4d-app", "0.1.0"),
            ],
            workspace_members: vec!["data".to_owned(), "format".to_owned(), "app".to_owned()],
            resolve: Some(CargoMetadataResolve {
                nodes: vec![
                    test_node("paste", &[]),
                    test_node("zarrs", &["paste"]),
                    test_node("zarrs-data-type", &["paste"]),
                    test_node("zarrs-plugin", &["paste"]),
                    test_node("data", &["zarrs"]),
                    test_node("format", &["zarrs"]),
                    test_node("app", &["data", "format"]),
                ],
            }),
        }
    }

    fn test_package(id: &str, name: &str, version: &str) -> CargoMetadataPackage {
        CargoMetadataPackage {
            id: id.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            source: (!name.starts_with("mirante4d-")).then(|| CRATES_IO_SOURCE.to_owned()),
            license: Some("MIT".to_owned()),
            license_file: None,
        }
    }

    fn test_node(id: &str, dependencies: &[&str]) -> CargoMetadataNode {
        CargoMetadataNode {
            id: id.to_owned(),
            deps: dependencies
                .iter()
                .map(|dependency| CargoMetadataNodeDependency {
                    pkg: (*dependency).to_owned(),
                })
                .collect(),
        }
    }
}
