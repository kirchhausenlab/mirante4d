use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::process::run_command_with_timeout;

const INDEX_PATH: &str = "docs/documentation-index.json";
const INDEX_SCHEMA: &str = "mirante4d-documentation-index";
const INDEX_SCHEMA_VERSION: u32 = 1;
const ROOT_INDEX: &str = "docs/README.md";
const MAX_INDEX_BYTES: u64 = 128 * 1024;
const MAX_DOCUMENTS: usize = 64;
const MAX_WALKED_ENTRIES: usize = 512;
const MAX_DIRECTORY_DEPTH: usize = 16;
pub(crate) const DOCS_CHECK_TIMEOUT: Duration = Duration::from_secs(90);
const REQUIRED_RUMDL_RULES: &str = "MD051,MD052,MD053,MD057,MD062";
const REQUIRED_READ_ORDER: &[&str] = &[
    "docs/PRODUCT.md",
    "docs/CURRENT_STATE.md",
    "docs/planning/NOW.md",
];
const REQUIRED_AUTHORITIES: &[(&str, &str)] = &[
    ("agent-policy", "docs/AGENTS.md"),
    ("documentation-index", "docs/README.md"),
    ("product-charter", "docs/PRODUCT.md"),
    ("current-state", "docs/CURRENT_STATE.md"),
    ("current-work", "docs/planning/NOW.md"),
    ("unresolved-backlog", "docs/BACKLOG.md"),
    ("architecture", "docs/ARCHITECTURE.md"),
    ("data-format", "docs/DATA_FORMAT.md"),
    ("development-commands", "docs/DEVELOPMENT.md"),
    ("verification", "docs/TESTING.md"),
    ("release", "docs/RELEASE.md"),
    ("decisions", "docs/decisions/README.md"),
    ("dependency-exceptions", "docs/DEPENDENCY_EXCEPTIONS.md"),
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentationIndex {
    schema: String,
    schema_version: u32,
    read_order: Vec<String>,
    documents: Vec<DocumentRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentRecord {
    path: String,
    truth_scope: TruthScope,
    authorities: Vec<String>,
    listed_in: NullablePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TruthScope {
    Current,
    Target,
    Deferred,
    Reference,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct NullablePath(Option<String>);

pub(crate) fn docs_check() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    check_index_at(&repo_root)?;

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

    println!("docs-check passed: exact inventory, authority graph, read order, links, and anchors");
    Ok(())
}

fn check_index_at(repo_root: &Path) -> anyhow::Result<()> {
    let index_path = repo_root.join(INDEX_PATH);
    let metadata = fs::metadata(&index_path)
        .with_context(|| format!("failed to inspect {}", index_path.display()))?;
    if !metadata.is_file() {
        bail!("{INDEX_PATH} must be a regular file");
    }
    if metadata.len() > MAX_INDEX_BYTES {
        bail!(
            "{INDEX_PATH} is {} bytes; maximum is {MAX_INDEX_BYTES}",
            metadata.len()
        );
    }

    let source = fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;
    let index: DocumentationIndex =
        serde_json::from_str(&source).with_context(|| format!("failed to parse {INDEX_PATH}"))?;
    validate_index(repo_root, &index)
}

fn validate_index(repo_root: &Path, index: &DocumentationIndex) -> anyhow::Result<()> {
    if index.schema != INDEX_SCHEMA {
        bail!(
            "{INDEX_PATH} schema must be {INDEX_SCHEMA:?}, found {:?}",
            index.schema
        );
    }
    if index.schema_version != INDEX_SCHEMA_VERSION {
        bail!(
            "{INDEX_PATH} schema_version must be {INDEX_SCHEMA_VERSION}, found {}",
            index.schema_version
        );
    }
    if index.documents.is_empty() || index.documents.len() > MAX_DOCUMENTS {
        bail!(
            "{INDEX_PATH} must list 1..={MAX_DOCUMENTS} documents, found {}",
            index.documents.len()
        );
    }

    let expected_read_order = REQUIRED_READ_ORDER
        .iter()
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    if index.read_order != expected_read_order {
        bail!(
            "{INDEX_PATH} read_order must be {:?}, found {:?}",
            REQUIRED_READ_ORDER,
            index.read_order
        );
    }

    let mut records = BTreeMap::new();
    let mut authority_owners = BTreeMap::new();
    for record in &index.documents {
        validate_document_path(&record.path)?;
        if records.insert(record.path.as_str(), record).is_some() {
            bail!("{INDEX_PATH} lists duplicate document {:?}", record.path);
        }
        for authority in &record.authorities {
            validate_authority_token(authority)?;
            if let Some(previous) =
                authority_owners.insert(authority.as_str(), record.path.as_str())
            {
                bail!(
                    "authority {authority:?} is owned by both {previous:?} and {:?}",
                    record.path
                );
            }
        }
    }

    for &(authority, required_owner) in REQUIRED_AUTHORITIES {
        match authority_owners.get(authority) {
            Some(actual_owner) if *actual_owner == required_owner => {}
            Some(actual_owner) => bail!(
                "authority {authority:?} must be owned by {required_owner:?}, found {actual_owner:?}"
            ),
            None => bail!("required authority {authority:?} is missing"),
        }
        let expected_scope = if authority == "decisions" {
            TruthScope::Target
        } else {
            TruthScope::Current
        };
        let record = records
            .get(required_owner)
            .with_context(|| format!("required authority owner {required_owner:?} is missing"))?;
        if record.truth_scope != expected_scope {
            bail!(
                "authority {authority:?} owner {required_owner:?} must have truth_scope {expected_scope:?}, found {:?}",
                record.truth_scope
            );
        }
    }

    for path in &index.read_order {
        if !records.contains_key(path.as_str()) {
            bail!("read_order path {path:?} is not registered");
        }
    }

    validate_listing_graph(&records)?;

    let registered = records
        .keys()
        .map(|path| (*path).to_owned())
        .collect::<BTreeSet<_>>();
    let discovered = collect_markdown_inventory(repo_root)?;
    if registered != discovered {
        let missing = discovered
            .difference(&registered)
            .cloned()
            .collect::<Vec<_>>();
        let stale = registered
            .difference(&discovered)
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "{INDEX_PATH} does not match docs/**/*.md; unregistered={missing:?}, missing_files={stale:?}"
        );
    }

    validate_declared_links(repo_root, &records)?;
    validate_read_order_links(repo_root, &index.read_order)?;
    require_markdown_link(repo_root, "AGENTS.md", "docs/AGENTS.md")?;
    require_markdown_link(repo_root, "docs/AGENTS.md", ROOT_INDEX)?;

    Ok(())
}

fn validate_listing_graph(records: &BTreeMap<&str, &DocumentRecord>) -> anyhow::Result<()> {
    for (path, record) in records {
        match (*path, record.listed_in.0.as_deref()) {
            (ROOT_INDEX, None) => {}
            (ROOT_INDEX, Some(parent)) => {
                bail!("{ROOT_INDEX} must be the only listing root, not listed in {parent:?}")
            }
            (_, None) => bail!("document {path:?} must declare listed_in"),
            (_, Some(parent)) => {
                validate_document_path(parent)?;
                if !records.contains_key(parent) {
                    bail!("document {path:?} is listed in unregistered document {parent:?}");
                }
            }
        }

        let mut current = *path;
        let mut visited = BTreeSet::new();
        while current != ROOT_INDEX {
            if !visited.insert(current) {
                bail!("listed_in cycle prevents {path:?} from reaching {ROOT_INDEX}");
            }
            let current_record = records
                .get(current)
                .with_context(|| format!("listed_in path {current:?} is not registered"))?;
            current = current_record.listed_in.0.as_deref().with_context(|| {
                format!("listed_in chain for {path:?} stops before {ROOT_INDEX}")
            })?;
            if visited.len() > records.len() {
                bail!("listed_in chain for {path:?} exceeds the document inventory");
            }
        }
    }
    Ok(())
}

fn validate_declared_links(
    repo_root: &Path,
    records: &BTreeMap<&str, &DocumentRecord>,
) -> anyhow::Result<()> {
    for (path, record) in records {
        if let Some(parent) = record.listed_in.0.as_deref() {
            require_markdown_link(repo_root, parent, path)?;
        }
    }
    Ok(())
}

fn validate_read_order_links(repo_root: &Path, read_order: &[String]) -> anyhow::Result<()> {
    let source = read_markdown(repo_root, ROOT_INDEX)?;
    let read_order_section = markdown_section(&source, "Read Order")
        .context("docs/README.md must contain a ## Read Order section")?;
    let mut previous_offset = None;

    for path in read_order {
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
            bail!("read-order links in {ROOT_INDEX} do not follow the registered order");
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

fn validate_authority_token(authority: &str) -> anyhow::Result<()> {
    let valid = !authority.is_empty()
        && !authority.starts_with('-')
        && !authority.ends_with('-')
        && authority
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        bail!("authority token must use lowercase words separated by hyphens: {authority:?}");
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
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn documentation_index_accepts_exact_inventory_and_rooted_listing_graph() {
        let fixture = DocumentationFixture::new();
        check_index_at(fixture.root()).unwrap();
    }

    #[test]
    fn documentation_index_rejects_unregistered_markdown() {
        let fixture = DocumentationFixture::new();
        fs::write(fixture.root().join("docs/EXTRA.md"), "# Extra\n").unwrap();

        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(
            error.contains("unregistered=[\"docs/EXTRA.md\"]"),
            "{error}"
        );
    }

    #[test]
    fn documentation_index_rejects_unsafe_paths_and_unknown_fields() {
        let fixture = DocumentationFixture::new();
        fixture.mutate_index(|index| {
            index["documents"][0]["path"] = json!("docs/../README.md");
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("not normalized"), "{error}");

        let fixture = DocumentationFixture::new();
        fixture.mutate_index(|index| index["unexpected"] = json!(true));
        let error = format!("{:#}", check_index_at(fixture.root()).unwrap_err());
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn documentation_index_rejects_duplicate_authority_and_listing_cycle() {
        let fixture = DocumentationFixture::new();
        fixture.mutate_index(|index| {
            index["documents"][1]["authorities"] = json!(["documentation-index"]);
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("owned by both"), "{error}");

        let fixture = DocumentationFixture::new();
        fixture.mutate_index(|index| {
            index["documents"][1]["listed_in"] = json!("docs/CURRENT_STATE.md");
            index["documents"][3]["listed_in"] = json!("docs/PRODUCT.md");
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("listed_in cycle"), "{error}");
    }

    #[test]
    fn documentation_index_rejects_a_declared_listing_without_a_real_link() {
        let fixture = DocumentationFixture::new();
        fixture.mutate_markdown(ROOT_INDEX, |source| {
            source.replace("[docs/BACKLOG.md](BACKLOG.md)\n", "")
        });

        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("relative target \"BACKLOG.md\""), "{error}");
    }

    #[test]
    fn documentation_index_rejects_duplicate_or_misordered_read_order_links() {
        let fixture = DocumentationFixture::new();
        fixture.mutate_markdown(ROOT_INDEX, |source| {
            source.replace("\n## Other", "\n[duplicate](PRODUCT.md)\n\n## Other")
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("must appear exactly once"), "{error}");

        let fixture = DocumentationFixture::new();
        fixture.mutate_markdown(ROOT_INDEX, |source| {
            source
                .replace("[docs/PRODUCT.md](PRODUCT.md)", "[read-order-swap-marker]")
                .replace(
                    "[docs/CURRENT_STATE.md](CURRENT_STATE.md)",
                    "[docs/PRODUCT.md](PRODUCT.md)",
                )
                .replace(
                    "[read-order-swap-marker]",
                    "[docs/CURRENT_STATE.md](CURRENT_STATE.md)",
                )
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(
            error.contains("do not follow the registered order"),
            "{error}"
        );
    }

    #[test]
    fn documentation_index_rejects_broken_agent_entry_links() {
        let fixture = DocumentationFixture::new();
        fixture.mutate_markdown("AGENTS.md", |source| {
            source.replace("](docs/AGENTS.md)", "](missing.md)")
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("\"AGENTS.md\" must contain"), "{error}");

        let fixture = DocumentationFixture::new();
        fixture.mutate_markdown("docs/AGENTS.md", |source| {
            source.replace("](README.md)", "](missing.md)")
        });
        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("\"docs/AGENTS.md\" must contain"), "{error}");
    }

    #[test]
    fn documentation_index_rejects_required_authority_truth_scope_drift() {
        let fixture = DocumentationFixture::new();
        fixture.mutate_index(|index| {
            let product = index["documents"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|record| record["path"] == "docs/PRODUCT.md")
                .unwrap();
            product["truth_scope"] = json!("target");
        });

        let error = check_index_at(fixture.root()).unwrap_err().to_string();
        assert!(error.contains("must have truth_scope Current"), "{error}");
    }

    struct DocumentationFixture {
        directory: TempDir,
    }

    impl DocumentationFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let documents = vec![
                document("docs/README.md", "current", &["documentation-index"], None),
                document(
                    "docs/PRODUCT.md",
                    "current",
                    &["product-charter"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/AGENTS.md",
                    "current",
                    &["agent-policy"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/CURRENT_STATE.md",
                    "current",
                    &["current-state"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/planning/NOW.md",
                    "current",
                    &["current-work"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/BACKLOG.md",
                    "current",
                    &["unresolved-backlog"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/ARCHITECTURE.md",
                    "current",
                    &["architecture"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/DATA_FORMAT.md",
                    "current",
                    &["data-format"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/DEVELOPMENT.md",
                    "current",
                    &["development-commands"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/TESTING.md",
                    "current",
                    &["verification"],
                    Some(ROOT_INDEX),
                ),
                document("docs/RELEASE.md", "current", &["release"], Some(ROOT_INDEX)),
                document(
                    "docs/decisions/README.md",
                    "target",
                    &["decisions"],
                    Some(ROOT_INDEX),
                ),
                document(
                    "docs/DEPENDENCY_EXCEPTIONS.md",
                    "current",
                    &["dependency-exceptions"],
                    Some(ROOT_INDEX),
                ),
            ];
            let index = json!({
                "schema": INDEX_SCHEMA,
                "schema_version": INDEX_SCHEMA_VERSION,
                "read_order": REQUIRED_READ_ORDER,
                "documents": documents,
            });
            for record in index["documents"].as_array().unwrap() {
                let path = record["path"].as_str().unwrap();
                let destination = directory.path().join(path);
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                let source = if path == ROOT_INDEX {
                    format!("# {path}\n\n## Read Order\n\n")
                } else {
                    format!("# {path}\n")
                };
                fs::write(destination, source).unwrap();
            }
            for record in index["documents"].as_array().unwrap() {
                let Some(parent) = record["listed_in"].as_str() else {
                    continue;
                };
                let path = record["path"].as_str().unwrap();
                let parent_path = directory.path().join(parent);
                let mut source = fs::read_to_string(&parent_path).unwrap();
                source.push_str(&format!(
                    "[{path}]({})\n",
                    relative_link_target(parent, path)
                ));
                fs::write(parent_path, source).unwrap();
            }
            let readme = directory.path().join(ROOT_INDEX);
            let mut readme_source = fs::read_to_string(&readme).unwrap();
            readme_source.push_str("\n## Other\n");
            fs::write(readme, readme_source).unwrap();

            let agents = directory.path().join("docs/AGENTS.md");
            let mut agents_source = fs::read_to_string(&agents).unwrap();
            agents_source.push_str("[Documentation index](README.md)\n");
            fs::write(agents, agents_source).unwrap();
            fs::write(
                directory.path().join("AGENTS.md"),
                "# Agents\n\n[Agent guide](docs/AGENTS.md)\n",
            )
            .unwrap();
            fs::write(
                directory.path().join(INDEX_PATH),
                serde_json::to_vec_pretty(&index).unwrap(),
            )
            .unwrap();
            Self { directory }
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn mutate_index(&self, mutate: impl FnOnce(&mut Value)) {
            let path = self.root().join(INDEX_PATH);
            let mut index: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            mutate(&mut index);
            fs::write(path, serde_json::to_vec_pretty(&index).unwrap()).unwrap();
        }

        fn mutate_markdown(&self, path: &str, mutate: impl FnOnce(String) -> String) {
            let path = self.root().join(path);
            let source = fs::read_to_string(&path).unwrap();
            fs::write(path, mutate(source)).unwrap();
        }
    }

    fn document(
        path: &str,
        truth_scope: &str,
        authorities: &[&str],
        listed_in: Option<&str>,
    ) -> Value {
        json!({
            "path": path,
            "truth_scope": truth_scope,
            "authorities": authorities,
            "listed_in": listed_in,
        })
    }
}
