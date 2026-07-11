use std::{fs, path::PathBuf};

use anyhow::Context;
use mirante4d_format::{FixtureKind, write_fixture};

pub(crate) fn generate_fixture(name: &str) -> anyhow::Result<PathBuf> {
    let kind = FixtureKind::from_name(name).with_context(|| {
        format!(
            "unknown fixture {name:?}; expected one of basic-u16-16cube, anisotropic-u16-16cube, time-u16-8cube-3t, time-multichannel-u16-8cube-3t-2c, multichannel-u16-8cube-4c, basic-f32-8cube"
        )
    })?;
    let output_root = PathBuf::from("target/mirante4d/fixtures");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    write_fixture(kind, &output_root).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_fixture_rejects_unknown_fixture_name() {
        let error = generate_fixture("unknown-fixture").unwrap_err().to_string();

        assert!(error.contains("unknown fixture"));
        assert!(error.contains("basic-u16-16cube"));
    }
}
