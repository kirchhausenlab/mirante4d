//! Current project-package location retained only until WP-10B.

use std::path::PathBuf;

/// The path is a reopening/publication location and is never project identity.
pub(crate) struct CurrentProjectRuntime {
    pub(crate) current_project_path: Option<PathBuf>,
}

impl CurrentProjectRuntime {
    pub(crate) const fn unbound() -> Self {
        Self {
            current_project_path: None,
        }
    }
}
