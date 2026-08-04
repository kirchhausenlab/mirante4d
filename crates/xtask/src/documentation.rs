use std::{
    collections::BTreeSet,
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, bail};

use crate::process::run_command_with_timeout;

const ROOT_INDEX: &str = "docs/README.md";
const DECISION_INDEX: &str = "docs/decisions/README.md";
const ACTIVE_PLAN_DIRECTORY: &str = "docs/plans/active";
const DECISION_DIRECTORY: &str = "docs/decisions";
const MAX_WALKED_ENTRIES: usize = 512;
const MAX_DIRECTORY_DEPTH: usize = 16;
pub(crate) const DOCS_CHECK_TIMEOUT: Duration = Duration::from_secs(90);
const REQUIRED_RUMDL_RULES: &str = "MD051,MD052,MD053,MD057,MD062";
const REQUIRED_READ_ORDER: &[&str] = &[
    "docs/PRODUCT.md",
    "docs/CURRENT_STATE.md",
    "docs/planning/NOW.md",
];
const REQUIRED_INDEX_LINKS: &[&str] = &[
    "docs/AGENTS.md",
    "docs/PRODUCT.md",
    "docs/CURRENT_STATE.md",
    "docs/planning/NOW.md",
    "docs/ARCHITECTURE.md",
    "docs/DATA_FORMAT.md",
    "docs/TESTING.md",
    "docs/DEVELOPMENT.md",
    "docs/RELEASE.md",
    "docs/decisions/README.md",
    "docs/BACKLOG.md",
    "docs/DEPENDENCY_EXCEPTIONS.md",
];

pub(crate) fn docs_check() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    check_docs_at(&repo_root)?;

    let mut rumdl = Command::new(env::var_os("MIRANTE4D_RUMDL").unwrap_or_else(|| "rumdl".into()));
    rumdl.current_dir(&repo_root).args([
        "check",
        "--no-cache",
        "--config",
        ".rumdl.toml",
        "--extend-enable",
        REQUIRED_RUMDL_RULES,
        ".",
    ]);
    run_command_with_timeout(&mut rumdl, DOCS_CHECK_TIMEOUT)?;

    println!(
        "docs-check passed: authorities, read order, active plans, ADRs, links, anchors, and formatting"
    );
    Ok(())
}

fn check_docs_at(repo_root: &Path) -> anyhow::Result<()> {
    require_regular_file(repo_root, "AGENTS.md")?;
    require_regular_file(repo_root, ROOT_INDEX)?;

    for path in REQUIRED_INDEX_LINKS {
        require_regular_file(repo_root, path)?;
        require_markdown_link(repo_root, ROOT_INDEX, path)?;
    }

    validate_read_order_links(repo_root)?;
    require_markdown_link(repo_root, "AGENTS.md", "docs/AGENTS.md")?;
    require_markdown_link(repo_root, "docs/AGENTS.md", ROOT_INDEX)?;

    let inventory = collect_markdown_inventory(repo_root)?;
    for path in inventory
        .iter()
        .filter(|path| is_direct_child(path, ACTIVE_PLAN_DIRECTORY))
    {
        require_markdown_link(repo_root, ROOT_INDEX, path)?;
    }
    for path in inventory.iter().filter(|path| is_adr(path)) {
        require_markdown_link(repo_root, DECISION_INDEX, path)?;
    }

    Ok(())
}

fn require_regular_file(repo_root: &Path, path: &str) -> anyhow::Result<()> {
    let absolute = repo_root.join(path);
    let metadata = fs::symlink_metadata(&absolute)
        .with_context(|| format!("required documentation file is missing: {path}"))?;
    if !metadata.file_type().is_file() {
        bail!("required documentation path must be a regular file: {path}");
    }
    Ok(())
}

fn validate_read_order_links(repo_root: &Path) -> anyhow::Result<()> {
    let source = read_markdown(repo_root, ROOT_INDEX)?;
    let read_order_section = markdown_section(&source, "Read Order")
        .context("docs/README.md must contain a ## Read Order section")?;
    let mut previous_offset = None;

    for path in REQUIRED_READ_ORDER {
        let target = relative_link_target(ROOT_INDEX, path);
        let needle = format!("]({target})");
        let offsets = read_order_section
            .match_indices(&needle)
            .map(|(offset, _)| offset)
            .collect::<Vec<_>>();
        if offsets.len() != 1 {
            bail!(
                "read-order link from {ROOT_INDEX} to {path:?} must appear exactly once in the Read Order section, found {}",
                offsets.len()
            );
        }
        if previous_offset.is_some_and(|previous| offsets[0] <= previous) {
            bail!("read-order links in {ROOT_INDEX} do not follow the required order");
        }
        previous_offset = Some(offsets[0]);
    }
    Ok(())
}

fn require_markdown_link(repo_root: &Path, parent: &str, child: &str) -> anyhow::Result<()> {
    let source = read_markdown(repo_root, parent)?;
    let target = relative_link_target(parent, child);
    let needle = format!("]({target})");
    if !source.contains(&needle) {
        bail!(
            "{parent:?} must contain a Markdown link to {child:?} using relative target {target:?}"
        );
    }
    Ok(())
}

fn read_markdown(repo_root: &Path, path: &str) -> anyhow::Result<String> {
    fs::read_to_string(repo_root.join(path)).with_context(|| format!("failed to read {path}"))
}

fn markdown_section<'a>(source: &'a str, heading: &str) -> Option<&'a str> {
    let marker = format!("## {heading}");
    let heading_start = source
        .strip_prefix(&marker)
        .map(|_| 0)
        .or_else(|| source.find(&format!("\n{marker}")))?;
    let body_start = source[heading_start..].find('\n')? + heading_start + 1;
    let body = &source[body_start..];
    let body_end = body.find("\n## ").unwrap_or(body.len());
    Some(&body[..body_end])
}

fn relative_link_target(parent: &str, child: &str) -> String {
    let parent_directory = parent
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    let parent_components = parent_directory
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let child_components = child.split('/').collect::<Vec<_>>();
    let shared = parent_components
        .iter()
        .zip(&child_components)
        .take_while(|(parent, child)| parent == child)
        .count();

    std::iter::repeat_n("..", parent_components.len() - shared)
        .chain(child_components[shared..].iter().copied())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_direct_child(path: &str, directory: &str) -> bool {
    Path::new(path).parent() == Some(Path::new(directory))
}

fn is_adr(path: &str) -> bool {
    is_direct_child(path, DECISION_DIRECTORY)
        && Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("ADR-") && name.ends_with(".md"))
}

fn validate_document_path(path: &str) -> anyhow::Result<()> {
    if !path.starts_with("docs/") || !path.ends_with(".md") {
        bail!("documentation path must match docs/**/*.md, found {path:?}");
    }
    if path.contains('\\') || path.chars().any(char::is_control) {
        bail!("documentation path is not portable and safe: {path:?}");
    }

    let parsed = Path::new(path);
    if parsed.is_absolute() {
        bail!("documentation path must be repository-relative: {path:?}");
    }
    let mut normalized = PathBuf::new();
    for component in parsed.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            _ => bail!("documentation path is not normalized: {path:?}"),
        }
    }
    if normalized.to_str() != Some(path) {
        bail!("documentation path is not normalized UTF-8: {path:?}");
    }
    Ok(())
}

fn collect_markdown_inventory(repo_root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let docs_root = repo_root.join("docs");
    let mut pending = vec![(docs_root.clone(), 0_usize)];
    let mut walked_entries = 0_usize;
    let mut markdown = BTreeSet::new();

    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_DIRECTORY_DEPTH {
            bail!(
                "documentation tree exceeds maximum directory depth {MAX_DIRECTORY_DEPTH} at {}",
                directory.display()
            );
        }
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to enumerate {}", directory.display()))?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            walked_entries += 1;
            if walked_entries > MAX_WALKED_ENTRIES {
                bail!("documentation tree exceeds {MAX_WALKED_ENTRIES} filesystem entries");
            }
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
            if file_type.is_symlink() {
                bail!(
                    "documentation tree must not contain symlink {}",
                    entry.path().display()
                );
            }
            if file_type.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if file_type.is_file() && entry.path().extension().is_some_and(|ext| ext == "md")
            {
                let entry_path = entry.path();
                let relative = entry_path.strip_prefix(repo_root).with_context(|| {
                    format!("{} is outside the repository", entry_path.display())
                })?;
                let path = relative
                    .to_str()
                    .with_context(|| format!("non-UTF-8 documentation path {relative:?}"))?
                    .to_owned();
                validate_document_path(&path)?;
                markdown.insert(path);
            }
        }
    }

    Ok(markdown)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const ACTIVE_PLAN: &str = "docs/plans/active/CURRENT_PLAN.md";
    const ADR: &str = "docs/decisions/ADR-0001-test-decision.md";

    #[test]
    fn documentation_tree_accepts_authorities_and_discovered_lifecycle_docs() {
        let fixture = DocumentationFixture::new();
        check_docs_at(fixture.root()).unwrap();
    }

    #[test]
    fn documentation_tree_accepts_unlisted_reference_markdown() {
        let fixture = DocumentationFixture::new();
        fixture.write("docs/reference/NOTE.md", "# Note\n");
        check_docs_at(fixture.root()).unwrap();
    }

    #[test]
    fn documentation_tree_rejects_missing_required_authority() {
        let fixture = DocumentationFixture::new();
        fs::remove_file(fixture.root().join("docs/PRODUCT.md")).unwrap();

        let error = check_docs_at(fixture.root()).unwrap_err().to_string();
        assert!(
            error.contains("required documentation file is missing"),
            "{error}"
        );
        assert!(error.contains("docs/PRODUCT.md"), "{error}");
    }

    #[test]
    fn documentation_tree_rejects_wrong_read_order() {
        let fixture = DocumentationFixture::new();
        fixture.mutate(ROOT_INDEX, |source| {
            source
                .replace("[Product](PRODUCT.md)", "[read-order-marker]")
                .replace("[Current state](CURRENT_STATE.md)", "[Product](PRODUCT.md)")
                .replace("[read-order-marker]", "[Current state](CURRENT_STATE.md)")
        });

        let error = check_docs_at(fixture.root()).unwrap_err().to_string();
        assert!(
            error.contains("do not follow the required order"),
            "{error}"
        );
    }

    #[test]
    fn documentation_tree_rejects_missing_active_plan_link() {
        let fixture = DocumentationFixture::new();
        fixture.mutate(ROOT_INDEX, |source| {
            source.replace("[Active plan](plans/active/CURRENT_PLAN.md)\n", "")
        });

        let error = check_docs_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains(ACTIVE_PLAN), "{error}");
    }

    #[test]
    fn documentation_tree_rejects_missing_adr_link() {
        let fixture = DocumentationFixture::new();
        fixture.mutate(DECISION_INDEX, |source| {
            source.replace("[Decision](ADR-0001-test-decision.md)\n", "")
        });

        let error = check_docs_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains(ADR), "{error}");
    }

    #[test]
    fn documentation_tree_rejects_broken_agent_entry_links() {
        let fixture = DocumentationFixture::new();
        fixture.mutate("AGENTS.md", |source| {
            source.replace("](docs/AGENTS.md)", "](missing.md)")
        });
        let error = check_docs_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("\"AGENTS.md\" must contain"), "{error}");

        let fixture = DocumentationFixture::new();
        fixture.mutate("docs/AGENTS.md", |source| {
            source.replace("](README.md)", "](missing.md)")
        });
        let error = check_docs_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("\"docs/AGENTS.md\" must contain"), "{error}");
    }

    struct DocumentationFixture {
        directory: TempDir,
    }

    impl DocumentationFixture {
        fn new() -> Self {
            let fixture = Self {
                directory: tempfile::tempdir().unwrap(),
            };

            fixture.write("AGENTS.md", "# Agents\n\n[Agent guide](docs/AGENTS.md)\n");
            fixture.write(
                "docs/AGENTS.md",
                "# Agent Guide\n\n[Documentation](README.md)\n",
            );
            for path in REQUIRED_INDEX_LINKS {
                if *path != "docs/AGENTS.md" && *path != ROOT_INDEX {
                    fixture.write(path, &format!("# {path}\n"));
                }
            }
            fixture.write(ACTIVE_PLAN, "# Current Plan\n");
            fixture.write(ADR, "# Test Decision\n");
            fixture.write(
                DECISION_INDEX,
                "# Decisions\n\n[Decision](ADR-0001-test-decision.md)\n",
            );

            let mut index = String::from(
                "# Documentation\n\n\
                 ## Read Order\n\n\
                 1. [Product](PRODUCT.md)\n\
                 2. [Current state](CURRENT_STATE.md)\n\
                 3. [Current work](planning/NOW.md)\n\n\
                 ## Index\n\n",
            );
            for path in REQUIRED_INDEX_LINKS {
                index.push_str(&format!(
                    "[{path}]({})\n",
                    relative_link_target(ROOT_INDEX, path)
                ));
            }
            index.push_str("[Active plan](plans/active/CURRENT_PLAN.md)\n");
            fixture.write(ROOT_INDEX, &index);
            fixture
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn write(&self, path: &str, source: &str) {
            let destination = self.root().join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, source).unwrap();
        }

        fn mutate(&self, path: &str, mutate: impl FnOnce(String) -> String) {
            let destination = self.root().join(path);
            let source = fs::read_to_string(&destination).unwrap();
            fs::write(destination, mutate(source)).unwrap();
        }
    }
}
