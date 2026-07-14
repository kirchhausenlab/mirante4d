use std::{fs, path::Path};

use anyhow::Context;
use serde_json::Value;

pub(crate) fn read_json_file(path: &Path) -> anyhow::Result<Value> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

pub(crate) fn write_json_file(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(value).context("failed to serialize JSON report")?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}
