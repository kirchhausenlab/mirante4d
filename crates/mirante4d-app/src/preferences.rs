use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Context;
use mirante4d_data::DataRuntimeConfig;
use serde::{Deserialize, Serialize};

pub(crate) const PREFERENCES_FORMAT: &str = "mirante4d-preferences-v1";
pub(crate) const APP_MIB: u64 = 1024 * 1024;
pub(crate) const APP_GIB: u64 = 1024 * APP_MIB;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPreferences {
    pub format: String,
    pub runtime: AppRuntimePreferences,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRuntimePreferences {
    pub volume_cache_budget_bytes: u64,
    pub brick_cache_budget_bytes: u64,
    pub gpu_volume_cache_budget_bytes: u64,
    pub gpu_brick_cache_budget_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigPlatform {
    Linux,
    Macos,
    Windows,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            format: PREFERENCES_FORMAT.to_owned(),
            runtime: AppRuntimePreferences::default(),
        }
    }
}

impl Default for AppRuntimePreferences {
    fn default() -> Self {
        let config = DataRuntimeConfig::default();
        Self {
            volume_cache_budget_bytes: config.volume_cache_budget_bytes,
            brick_cache_budget_bytes: config.brick_cache_budget_bytes,
            gpu_volume_cache_budget_bytes: APP_GIB,
            gpu_brick_cache_budget_bytes: 2 * APP_GIB,
        }
    }
}

impl AppRuntimePreferences {
    pub fn from_system_memory_bytes(system_memory_bytes: Option<u64>) -> Self {
        let mut preferences = Self::default();
        if let Some(memory_bytes) = system_memory_bytes {
            preferences.brick_cache_budget_bytes =
                (memory_bytes / 5).clamp(512 * APP_MIB, 32 * APP_GIB);
            preferences.gpu_brick_cache_budget_bytes =
                (memory_bytes / 10).clamp(APP_GIB, 8 * APP_GIB);
        }
        preferences
    }
}

impl AppPreferences {
    pub fn runtime_config(&self) -> DataRuntimeConfig {
        DataRuntimeConfig::from_cache_budgets(
            self.runtime.volume_cache_budget_bytes,
            self.runtime.brick_cache_budget_bytes,
        )
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.format != PREFERENCES_FORMAT {
            anyhow::bail!("unsupported preferences format {:?}", self.format);
        }
        validate_runtime_preferences(self.runtime)
    }
}

pub fn default_app_preferences_for_system() -> AppPreferences {
    AppPreferences {
        format: PREFERENCES_FORMAT.to_owned(),
        runtime: AppRuntimePreferences::from_system_memory_bytes(detected_system_memory_bytes()),
    }
}

fn validate_runtime_preferences(runtime: AppRuntimePreferences) -> anyhow::Result<()> {
    if runtime.volume_cache_budget_bytes < APP_MIB {
        anyhow::bail!("volume cache budget must be at least 1 MiB");
    }
    if runtime.brick_cache_budget_bytes < APP_MIB {
        anyhow::bail!("brick cache budget must be at least 1 MiB");
    }
    if runtime.gpu_volume_cache_budget_bytes < APP_MIB {
        anyhow::bail!("GPU volume cache budget must be at least 1 MiB");
    }
    if runtime.gpu_brick_cache_budget_bytes < APP_MIB {
        anyhow::bail!("GPU brick cache budget must be at least 1 MiB");
    }
    Ok(())
}

pub(crate) fn bytes_to_mib_rounded(bytes: u64) -> u64 {
    ((bytes + APP_MIB / 2) / APP_MIB).max(1)
}

pub(crate) fn mib_to_bytes(mib: u64) -> u64 {
    mib.saturating_mul(APP_MIB)
}

fn detected_system_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|contents| parse_linux_meminfo_total_bytes(&contents))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_meminfo_total_bytes(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        let mut parts = rest.split_whitespace();
        let value = parts.next()?.parse::<u64>().ok()?;
        let unit = parts.next()?;
        if unit == "kB" {
            value.checked_mul(1024)
        } else {
            None
        }
    })
}

pub fn default_preferences_path() -> Option<PathBuf> {
    let platform = if cfg!(target_os = "windows") {
        ConfigPlatform::Windows
    } else if cfg!(target_os = "macos") {
        ConfigPlatform::Macos
    } else {
        ConfigPlatform::Linux
    };
    preferences_path_for_platform(
        platform,
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("APPDATA").map(PathBuf::from),
    )
}

fn preferences_path_for_platform(
    platform: ConfigPlatform,
    home: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    appdata: Option<PathBuf>,
) -> Option<PathBuf> {
    let config_dir = match platform {
        ConfigPlatform::Linux => xdg_config_home
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| home.map(|path| path.join(".config")))?
            .join("mirante4d"),
        ConfigPlatform::Macos => home?
            .join("Library")
            .join("Application Support")
            .join("Mirante4D"),
        ConfigPlatform::Windows => appdata?.join("Mirante4D"),
    };
    Some(config_dir.join("preferences.json"))
}

pub fn load_app_preferences(path: &Path) -> anyhow::Result<AppPreferences> {
    match fs::read_to_string(path) {
        Ok(encoded) => {
            let preferences: AppPreferences = serde_json::from_str(&encoded)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            preferences.validate()?;
            Ok(preferences)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AppPreferences::default()),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub fn write_app_preferences(path: &Path, preferences: &AppPreferences) -> anyhow::Result<()> {
    preferences.validate()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let temporary = path.with_extension("json.tmp");
    let encoded = serde_json::to_string_pretty(preferences)?;
    fs::write(&temporary, format!("{encoded}\n"))
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("failed to commit {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    #[test]
    fn preferences_path_for_platform_uses_os_conventions() {
        assert_eq!(
            preferences_path_for_platform(
                ConfigPlatform::Linux,
                Some(PathBuf::from("/home/user")),
                Some(PathBuf::from("/xdg")),
                None,
            )
            .unwrap(),
            PathBuf::from("/xdg/mirante4d/preferences.json"),
        );
        assert_eq!(
            preferences_path_for_platform(
                ConfigPlatform::Linux,
                Some(PathBuf::from("/home/user")),
                None,
                None,
            )
            .unwrap(),
            PathBuf::from("/home/user/.config/mirante4d/preferences.json"),
        );
        assert_eq!(
            preferences_path_for_platform(
                ConfigPlatform::Macos,
                Some(PathBuf::from("/Users/user")),
                None,
                None,
            )
            .unwrap(),
            PathBuf::from("/Users/user/Library/Application Support/Mirante4D/preferences.json"),
        );
        assert_eq!(
            preferences_path_for_platform(
                ConfigPlatform::Windows,
                None,
                None,
                Some(PathBuf::from("/appdata")),
            )
            .unwrap(),
            PathBuf::from("/appdata/Mirante4D/preferences.json"),
        );
    }

    #[test]
    fn runtime_preferences_use_bounded_system_ram_policy() {
        assert_eq!(
            AppRuntimePreferences::from_system_memory_bytes(None),
            AppRuntimePreferences::default(),
        );
        assert_eq!(
            AppRuntimePreferences::from_system_memory_bytes(Some(APP_GIB)).brick_cache_budget_bytes,
            512 * APP_MIB,
        );
        assert_eq!(
            AppRuntimePreferences::from_system_memory_bytes(Some(20 * APP_GIB))
                .brick_cache_budget_bytes,
            4 * APP_GIB,
        );
        assert_eq!(
            AppRuntimePreferences::from_system_memory_bytes(Some(256 * APP_GIB))
                .brick_cache_budget_bytes,
            32 * APP_GIB,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_meminfo_parser_reads_total_memory_bytes() {
        assert_eq!(
            parse_linux_meminfo_total_bytes(
                "MemTotal:       16384256 kB\nMemFree:         123456 kB\n",
            ),
            Some(16_777_478_144),
        );
        assert_eq!(parse_linux_meminfo_total_bytes("MemTotal: 123 MB\n"), None);
    }

    #[test]
    fn app_preferences_reject_invalid_format_and_tiny_budget() {
        let tempdir = tempfile::tempdir().unwrap();
        let preferences_path = tempdir.path().join("preferences.json");
        let invalid_format = AppPreferences {
            format: "mirante4d-preferences-v0".to_owned(),
            runtime: AppRuntimePreferences::default(),
        };

        assert!(write_app_preferences(&preferences_path, &invalid_format).is_err());

        fs::write(
            &preferences_path,
            r#"{
  "format": "mirante4d-preferences-v1",
  "runtime": {
    "volume_cache_budget_bytes": 0,
    "brick_cache_budget_bytes": 1048576
  }
}
"#,
        )
        .unwrap();

        assert!(load_app_preferences(&preferences_path).is_err());
    }
}
