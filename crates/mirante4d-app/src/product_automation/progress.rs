use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Serialize;

pub(super) const PROGRESS_PATH_ENV: &str = "MIRANTE4D_AUTOMATION_PROGRESS_PATH";
pub(super) const PROGRESS_NONCE_ENV: &str = "MIRANTE4D_AUTOMATION_PROGRESS_NONCE";

const PROGRESS_SCHEMA: &str = "mirante4d-product-automation-progress";
const PROGRESS_SCHEMA_VERSION: u32 = 1;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
const PROGRESS_NONCE_HEX_BYTES: usize = 32;

#[derive(Serialize)]
struct ProgressRecord<'a> {
    schema: &'static str,
    schema_version: u32,
    nonce: &'a str,
    heartbeat_sequence: u64,
    command_count: usize,
    state: ProgressState<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProgressState<'a> {
    Command {
        index: usize,
        command_kind: &'a str,
        elapsed_ms: u64,
    },
    Closeout {
        elapsed_ms: u64,
    },
}

pub(super) struct ProductAutomationProgressPublisher {
    path: PathBuf,
    nonce: String,
    heartbeat_sequence: u64,
    last_published_at: Option<Instant>,
    observed_command_index: usize,
    command_started_at: Instant,
    closeout_started_at: Option<Instant>,
    closeout_published: bool,
}

impl ProductAutomationProgressPublisher {
    pub(super) fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_values_at(
            env::var_os(PROGRESS_PATH_ENV),
            env::var_os(PROGRESS_NONCE_ENV),
            Instant::now(),
        )
    }

    fn from_values_at(
        path: Option<OsString>,
        nonce: Option<OsString>,
        now: Instant,
    ) -> anyhow::Result<Option<Self>> {
        let (path, nonce) = match (path, nonce) {
            (None, None) => return Ok(None),
            (Some(path), Some(nonce)) => (PathBuf::from(path), nonce),
            _ => anyhow::bail!(
                "{PROGRESS_PATH_ENV} and {PROGRESS_NONCE_ENV} must be provided together"
            ),
        };
        if !path.is_absolute() {
            anyhow::bail!("{PROGRESS_PATH_ENV} must be an absolute path");
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("{PROGRESS_PATH_ENV} has no parent directory"))?;
        let parent_metadata = fs::symlink_metadata(parent)
            .map_err(|_| anyhow::anyhow!("{PROGRESS_PATH_ENV} parent is unavailable"))?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            anyhow::bail!("{PROGRESS_PATH_ENV} parent must be a nonsymlink directory");
        }
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => anyhow::bail!("{PROGRESS_PATH_ENV} must name an absent sidecar"),
            Err(_) => anyhow::bail!("{PROGRESS_PATH_ENV} could not be inspected"),
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("{PROGRESS_PATH_ENV} requires a UTF-8 file name"))?;
        let nonce = nonce
            .into_string()
            .map_err(|_| anyhow::anyhow!("{PROGRESS_NONCE_ENV} must be UTF-8"))?;
        validate_nonce(&nonce)?;
        Ok(Some(Self {
            path,
            nonce,
            heartbeat_sequence: 0,
            last_published_at: None,
            observed_command_index: 0,
            command_started_at: now,
            closeout_started_at: None,
            closeout_published: false,
        }))
    }

    pub(super) fn observe_command(&mut self, index: usize, now: Instant) {
        if self.observed_command_index != index {
            self.observed_command_index = index;
            self.command_started_at = now;
        }
    }

    pub(super) fn publish_command_if_due(
        &mut self,
        command_count: usize,
        index: usize,
        command_kind: &'static str,
        now: Instant,
    ) -> Result<bool, String> {
        if self.closeout_published {
            return Ok(false);
        }
        self.observe_command(index, now);
        if self
            .last_published_at
            .is_some_and(|published| now.saturating_duration_since(published) < PROGRESS_INTERVAL)
        {
            return Ok(false);
        }
        let elapsed_ms = checked_duration_ms(now, self.command_started_at)?;
        self.publish(
            command_count,
            ProgressState::Command {
                index,
                command_kind,
                elapsed_ms,
            },
            now,
        )?;
        Ok(true)
    }

    pub(super) fn publish_closeout(
        &mut self,
        command_count: usize,
        now: Instant,
    ) -> Result<(), String> {
        if self.closeout_published {
            return Ok(());
        }
        let started_at = *self.closeout_started_at.get_or_insert(now);
        let elapsed_ms = checked_duration_ms(now, started_at)?;
        self.publish(command_count, ProgressState::Closeout { elapsed_ms }, now)?;
        self.closeout_published = true;
        Ok(())
    }

    pub(super) fn clamp_repaint_after(
        &self,
        requested: Option<Duration>,
        now: Instant,
    ) -> Option<Duration> {
        if self.closeout_published {
            return requested;
        }
        let until_progress = self.last_published_at.map_or(Duration::ZERO, |published| {
            PROGRESS_INTERVAL.saturating_sub(now.saturating_duration_since(published))
        });
        Some(requested.map_or(until_progress, |requested| requested.min(until_progress)))
    }

    fn publish(
        &mut self,
        command_count: usize,
        state: ProgressState<'_>,
        now: Instant,
    ) -> Result<(), String> {
        let heartbeat_sequence = self
            .heartbeat_sequence
            .checked_add(1)
            .ok_or_else(|| "product automation progress sequence overflowed".to_owned())?;
        let record = ProgressRecord {
            schema: PROGRESS_SCHEMA,
            schema_version: PROGRESS_SCHEMA_VERSION,
            nonce: &self.nonce,
            heartbeat_sequence,
            command_count,
            state,
        };
        write_progress_atomic_replace(&self.path, &record)?;
        self.heartbeat_sequence = heartbeat_sequence;
        self.last_published_at = Some(now);
        Ok(())
    }
}

fn validate_nonce(nonce: &str) -> anyhow::Result<()> {
    if nonce.len() != PROGRESS_NONCE_HEX_BYTES
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        anyhow::bail!(
            "{PROGRESS_NONCE_ENV} must contain exactly 32 lowercase hexadecimal characters"
        );
    }
    Ok(())
}

fn checked_duration_ms(now: Instant, started_at: Instant) -> Result<u64, String> {
    let elapsed = now
        .checked_duration_since(started_at)
        .ok_or_else(|| "product automation progress clock moved backwards".to_owned())?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| "product automation progress duration overflowed".to_owned())
}

fn write_progress_atomic_replace(path: &Path, record: &ProgressRecord<'_>) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "product automation progress path has no parent".to_owned())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "product automation progress path has no UTF-8 file name".to_owned())?;
    let stage_path = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut bytes = serde_json::to_vec(record)
        .map_err(|_| "failed to serialize product automation progress".to_owned())?;
    bytes.push(b'\n');
    let write_result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut stage = options
            .open(&stage_path)
            .map_err(|_| "failed to create product automation progress stage".to_owned())?;
        stage
            .write_all(&bytes)
            .map_err(|_| "failed to write product automation progress stage".to_owned())?;
        drop(stage);
        fs::rename(&stage_path, path)
            .map_err(|_| "failed to publish product automation progress".to_owned())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&stage_path);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn publisher_at(root: &Path, now: Instant) -> ProductAutomationProgressPublisher {
        ProductAutomationProgressPublisher::from_values_at(
            Some(root.join("progress.json").into_os_string()),
            Some(OsString::from(NONCE)),
            now,
        )
        .unwrap()
        .unwrap()
    }

    fn read_record(root: &Path) -> Value {
        serde_json::from_slice(&fs::read(root.join("progress.json")).unwrap()).unwrap()
    }

    #[test]
    fn progress_config_is_all_or_nothing_and_nonce_is_exact() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        assert!(
            ProductAutomationProgressPublisher::from_values_at(None, None, now)
                .unwrap()
                .is_none()
        );
        assert!(
            ProductAutomationProgressPublisher::from_values_at(
                Some(root.path().join("progress.json").into_os_string()),
                None,
                now,
            )
            .is_err()
        );
        for invalid in [
            "0123",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(
                ProductAutomationProgressPublisher::from_values_at(
                    Some(root.path().join("progress.json").into_os_string()),
                    Some(OsString::from(invalid)),
                    now,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn progress_publication_is_immediate_then_one_second_bounded() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut publisher = publisher_at(root.path(), origin);
        assert!(
            publisher
                .publish_command_if_due(3, 0, "open_dataset", origin)
                .unwrap()
        );
        assert_eq!(read_record(root.path())["heartbeat_sequence"], 1);
        assert!(
            !publisher
                .publish_command_if_due(3, 1, "wait_for", origin + Duration::from_millis(999),)
                .unwrap()
        );
        assert_eq!(read_record(root.path())["state"]["index"], 0);
        assert!(
            publisher
                .publish_command_if_due(3, 1, "wait_for", origin + Duration::from_secs(1),)
                .unwrap()
        );
        let record = read_record(root.path());
        assert_eq!(record["heartbeat_sequence"], 2);
        assert_eq!(record["state"]["index"], 1);
        assert_eq!(record["state"]["elapsed_ms"], 1);
    }

    #[test]
    fn passive_repaint_is_clamped_to_the_next_heartbeat() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut publisher = publisher_at(root.path(), origin);
        publisher
            .publish_command_if_due(1, 0, "wait_for", origin)
            .unwrap();
        assert_eq!(
            publisher.clamp_repaint_after(None, origin + Duration::from_millis(100)),
            Some(Duration::from_millis(900))
        );
        assert_eq!(
            publisher.clamp_repaint_after(
                Some(Duration::from_millis(250)),
                origin + Duration::from_millis(100),
            ),
            Some(Duration::from_millis(250))
        );
    }

    #[test]
    fn closeout_forces_an_atomic_safe_terminal_record() {
        let root = tempfile::tempdir().unwrap();
        let origin = Instant::now();
        let mut publisher = publisher_at(root.path(), origin);
        publisher
            .publish_command_if_due(2, 0, "sample_diagnostics", origin)
            .unwrap();
        publisher
            .publish_closeout(2, origin + Duration::from_millis(20))
            .unwrap();
        let record = read_record(root.path());
        assert_eq!(record.as_object().unwrap().len(), 6);
        assert_eq!(record["schema"], PROGRESS_SCHEMA);
        assert_eq!(record["schema_version"], PROGRESS_SCHEMA_VERSION);
        assert_eq!(record["nonce"], NONCE);
        assert_eq!(record["heartbeat_sequence"], 2);
        assert_eq!(record["command_count"], 2);
        assert_eq!(record["state"]["kind"], "closeout");
        assert_eq!(record["state"]["elapsed_ms"], 0);
        let encoded = serde_json::to_string(&record).unwrap();
        let private_path = root.path().to_string_lossy().into_owned();
        for private in [
            private_path.as_str(),
            "private-scenario",
            "private-diagnostic-label",
            "private-error",
        ] {
            assert!(!encoded.contains(private));
        }
        assert!(
            !root
                .path()
                .join(format!(".progress.json.tmp-{}", std::process::id()))
                .exists()
        );
    }
}
