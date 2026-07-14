use std::path::{Path, PathBuf};

pub(crate) const DIAGNOSTICS_FORMAT: &str = "mirante4d-diagnostics-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupDiagnostics {
    pub format: String,
    pub app_version: String,
    pub target_os: String,
    pub target_arch: String,
    pub target_family: String,
    pub logs_path: Option<PathBuf>,
    pub gpu_adapter: Option<String>,
}

impl StartupDiagnostics {
    pub fn target_summary(&self) -> String {
        format!(
            "{}-{} ({})",
            self.target_os, self.target_arch, self.target_family
        )
    }

    pub fn with_gpu_adapter(mut self, adapter: impl Into<String>) -> Self {
        self.gpu_adapter = Some(adapter.into());
        self
    }

    pub fn summary_text(
        &self,
        dataset_path: Option<&Path>,
        adapter_summary: Option<&str>,
    ) -> String {
        let logs_path = self
            .logs_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "stderr/stdout".to_owned());
        let adapter = adapter_summary
            .or(self.gpu_adapter.as_deref())
            .unwrap_or("not initialized");
        let dataset = dataset_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".to_owned());
        format!(
            "Mirante4D diagnostics\n\
             diagnostics_format: {}\n\
             app_version: {}\n\
             platform: {}\n\
             logs_path: {}\n\
             dataset: {}\n\
             gpu_adapter: {}\n",
            self.format,
            self.app_version,
            self.target_summary(),
            logs_path,
            dataset,
            adapter
        )
    }
}

pub fn collect_startup_diagnostics() -> StartupDiagnostics {
    StartupDiagnostics {
        format: DIAGNOSTICS_FORMAT.to_owned(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        target_os: std::env::consts::OS.to_owned(),
        target_arch: std::env::consts::ARCH.to_owned(),
        target_family: std::env::consts::FAMILY.to_owned(),
        logs_path: default_log_path(),
        gpu_adapter: None,
    }
}

pub fn default_log_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MIRANTE4D_LOG_FILE") {
        return Some(PathBuf::from(path));
    }
    default_log_dir().map(|dir| dir.join("mirante4d.log"))
}

fn default_log_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("Mirante4D").join("logs"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Library").join("Logs").join("Mirante4D"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|root| root.join(".local").join("state"))
            })
            .map(|root| root.join("mirante4d"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_diagnostics_collect_platform_and_app_version() {
        let diagnostics = collect_startup_diagnostics();

        assert_eq!(diagnostics.format, DIAGNOSTICS_FORMAT);
        assert_eq!(diagnostics.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(diagnostics.target_os, std::env::consts::OS);
        assert_eq!(diagnostics.target_arch, std::env::consts::ARCH);
        assert_eq!(diagnostics.target_family, std::env::consts::FAMILY);
        assert!(
            diagnostics
                .target_summary()
                .contains(&diagnostics.target_os)
        );
        assert!(diagnostics.gpu_adapter.is_none());
    }

    #[test]
    fn startup_diagnostics_records_gpu_backend_summary_when_available() {
        let diagnostics =
            collect_startup_diagnostics().with_gpu_adapter("Vulkan DiscreteGpu RTX driver=580");

        assert_eq!(
            diagnostics.gpu_adapter.as_deref(),
            Some("Vulkan DiscreteGpu RTX driver=580")
        );
        assert!(
            diagnostics
                .gpu_adapter
                .as_deref()
                .unwrap()
                .contains("Vulkan")
        );
    }

    #[test]
    fn startup_summary_reports_runtime_facts_without_assuming_a_dataset_format() {
        let diagnostics = collect_startup_diagnostics();

        let summary = diagnostics.summary_text(None, None);

        assert!(summary.contains("diagnostics_format: mirante4d-diagnostics-v1"));
        assert!(summary.contains("dataset: none"));
        assert!(summary.contains("gpu_adapter: not initialized"));
    }
}
