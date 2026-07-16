use std::{
    env, fs,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use mirante4d_identity::Sha256Hasher;
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const IMPORT_QUALIFICATION_PROFILE_SCHEMA: &str =
    "mirante4d-import-performance-qualification-profile-2";
pub(crate) const IMPORT_QUALIFICATION_PROFILE_MAX_BYTES: u64 = 64 * 1024;
pub(crate) const IMPORT_QUALIFICATION_HARDWARE_CLASS: &str = "HW-2";
// The owner accepted this exact opaque local-profile commitment. Raw profile
// fields stay local, and any other profile remains fail-closed.
pub(crate) const OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256: Option<&str> =
    Some("50d18c8d3f695a90ff879fc6cdea210b273cc52f97f63350931e42fdd2b38abe");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportQualificationProtocol {
    cache_condition: String,
    competing_activity: String,
}

impl ImportQualificationProtocol {
    pub(crate) fn new(
        cache_condition: impl Into<String>,
        competing_activity: impl Into<String>,
    ) -> Self {
        Self {
            cache_condition: cache_condition.into(),
            competing_activity: competing_activity.into(),
        }
    }

    pub(crate) fn cache_condition(&self) -> &str {
        &self.cache_condition
    }

    pub(crate) fn competing_activity(&self) -> &str {
        &self.competing_activity
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QualificationBuildProvenance {
    pub(crate) git_head: String,
    pub(crate) git_dirty: bool,
    pub(crate) cargo_profile: String,
    pub(crate) opt_level: String,
    pub(crate) debug: String,
    pub(crate) compiler: String,
    pub(crate) custom_rustflags: bool,
    pub(crate) rustc_wrapper: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepositoryIdentity {
    pub(crate) root: Option<PathBuf>,
    pub(crate) commit: Option<String>,
    pub(crate) dirty_worktree: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostHardwareIdentity {
    pub(crate) cpu_model: Option<String>,
    pub(crate) logical_cpu_count: Option<u64>,
    pub(crate) mem_total_kib: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportQualificationAssessment {
    pub(crate) status: &'static str,
    pub(crate) reason_codes: Vec<&'static str>,
    pub(crate) profile_sha256: Option<String>,
    pub(crate) hardware_class: Option<String>,
    pub(crate) host_fingerprint_sha256: Option<String>,
    pub(crate) storage_fingerprint_sha256: Option<String>,
    pub(crate) filesystem_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportQualificationProfile {
    schema: String,
    hardware_class: String,
    host: QualificationHost,
    scratch: QualificationScratch,
    protocol: QualificationProtocol,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct QualificationHost {
    cpu_model: String,
    logical_cpu_count: u64,
    mem_total_kib: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct QualificationScratch {
    qualified_root: PathBuf,
    filesystem_type: String,
    filesystem_source: String,
    filesystem_uuid: String,
    device_major_minor: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct QualificationProtocol {
    cache_condition: String,
    competing_activity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScratchStorageIdentity {
    filesystem_type: String,
    filesystem_source: String,
    filesystem_uuid: String,
    device_major_minor: String,
}

pub(crate) fn benchmark_host_context() -> Value {
    benchmark_host_context_from(&repository_identity(), &host_hardware_identity())
}

pub(crate) fn benchmark_host_context_from(
    repository: &RepositoryIdentity,
    hardware: &HostHardwareIdentity,
) -> Value {
    json!({
        "name": env::var("MIRANTE4D_BENCH_HARDWARE_NAME")
            .unwrap_or_else(|_| "local-dev-machine".to_owned()),
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "git_commit": repository.commit,
        "dirty_worktree": repository.dirty_worktree,
        "os": env::consts::OS,
        "arch": env::consts::ARCH,
        "cpu_model": hardware.cpu_model,
        "logical_cpu_count": hardware.logical_cpu_count,
        "mem_total_kib": hardware.mem_total_kib,
    })
}

pub(crate) fn qualification_build_provenance() -> QualificationBuildProvenance {
    QualificationBuildProvenance {
        git_head: env!("MIRANTE4D_XTASK_BUILD_GIT_HEAD").to_owned(),
        git_dirty: env!("MIRANTE4D_XTASK_BUILD_GIT_DIRTY") == "true",
        cargo_profile: env!("MIRANTE4D_XTASK_BUILD_CARGO_PROFILE").to_owned(),
        opt_level: env!("MIRANTE4D_XTASK_BUILD_OPT_LEVEL").to_owned(),
        debug: env!("MIRANTE4D_XTASK_BUILD_DEBUG").to_owned(),
        compiler: env!("MIRANTE4D_XTASK_BUILD_COMPILER").replace("\\n", "\n"),
        custom_rustflags: env!("MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS") == "true",
        rustc_wrapper: env!("MIRANTE4D_XTASK_BUILD_RUSTC_WRAPPER") == "true",
    }
}

pub(crate) fn qualification_build_provenance_evidence(
    provenance: &QualificationBuildProvenance,
) -> Value {
    json!({
        "git_head": provenance.git_head,
        "git_dirty": provenance.git_dirty,
        "cargo_profile": provenance.cargo_profile,
        "opt_level": provenance.opt_level,
        "debug": provenance.debug,
        "compiler": provenance.compiler,
        "custom_rustflags": provenance.custom_rustflags,
        "rustc_wrapper": provenance.rustc_wrapper,
    })
}

pub(crate) fn qualification_build_reason_codes(
    provenance: &QualificationBuildProvenance,
    repository: &RepositoryIdentity,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if provenance.git_head == "unavailable" {
        reasons.push("build_revision_unavailable");
    } else if repository.commit.as_deref() != Some(provenance.git_head.as_str()) {
        reasons.push("build_revision_does_not_match_repository");
    }
    if provenance.git_dirty {
        reasons.push("executable_built_from_dirty_worktree");
    }
    if provenance.cargo_profile != "release" {
        reasons.push("build_profile_not_release");
    }
    if provenance.opt_level != "3" {
        reasons.push("build_opt_level_not_standard_release");
    }
    if provenance.debug != "false" {
        reasons.push("build_debug_not_standard_release");
    }
    if provenance.compiler == "unavailable" {
        reasons.push("build_compiler_unavailable");
    }
    if provenance.custom_rustflags {
        reasons.push("custom_rustflags_not_qualified");
    }
    if provenance.rustc_wrapper {
        reasons.push("rustc_wrapper_not_qualified");
    }
    reasons
}

pub(crate) fn repository_identity() -> RepositoryIdentity {
    let root = git_text(&["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    let Some(root_path) = root.as_deref() else {
        return RepositoryIdentity {
            root,
            commit: None,
            dirty_worktree: None,
        };
    };
    let root_argument = root_path.as_os_str();
    let commit = Command::new("git")
        .arg("-C")
        .arg(root_argument)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| nonempty_stdout(&output.stdout));
    let dirty_worktree = Command::new("git")
        .arg("-C")
        .arg(root_argument)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    RepositoryIdentity {
        root,
        commit,
        dirty_worktree,
    }
}

pub(crate) fn host_hardware_identity() -> HostHardwareIdentity {
    HostHardwareIdentity {
        cpu_model: linux_cpu_model(),
        logical_cpu_count: std::thread::available_parallelism()
            .ok()
            .and_then(|count| u64::try_from(count.get()).ok()),
        mem_total_kib: linux_mem_total_kib(),
    }
}

pub(crate) fn assess_import_qualification_profile(
    profile_path: Option<&Path>,
    repository: &RepositoryIdentity,
    scratch_root: &Path,
    hardware: &HostHardwareIdentity,
    protocol: &ImportQualificationProtocol,
) -> ImportQualificationAssessment {
    let Ok(scratch_root) = fs::canonicalize(scratch_root) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["scratch_root_unavailable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256: import_host_fingerprint(hardware),
            storage_fingerprint_sha256: None,
            filesystem_type: None,
        };
    };
    let storage = scratch_storage_identity(&scratch_root).ok();
    assess_import_qualification_profile_with_storage(
        profile_path,
        repository.root.as_deref(),
        &scratch_root,
        hardware,
        storage.as_ref(),
        protocol,
    )
}

pub(crate) fn import_qualification_assessment_evidence(
    assessment: &ImportQualificationAssessment,
) -> Value {
    json!({
        "status": assessment.status,
        "reason_codes": assessment.reason_codes,
        "profile_sha256": assessment.profile_sha256,
        "declared_hardware_class": assessment.hardware_class,
        "observed_host_fingerprint_sha256": assessment.host_fingerprint_sha256,
        "observed_storage_fingerprint_sha256": assessment.storage_fingerprint_sha256,
        "observed_filesystem_type": assessment.filesystem_type,
        "raw_paths_and_storage_identifiers_redacted": true,
    })
}

fn assess_import_qualification_profile_with_storage(
    profile_path: Option<&Path>,
    repository_root: Option<&Path>,
    scratch_root: &Path,
    hardware: &HostHardwareIdentity,
    storage: Option<&ScratchStorageIdentity>,
    protocol: &ImportQualificationProtocol,
) -> ImportQualificationAssessment {
    let host_fingerprint_sha256 = import_host_fingerprint(hardware);
    let storage_fingerprint_sha256 = storage.map(import_storage_fingerprint);
    let filesystem_type = storage.map(|identity| identity.filesystem_type.clone());
    let Some(profile_path) = profile_path else {
        return ImportQualificationAssessment {
            status: "absent",
            reason_codes: vec!["qualification_profile_absent"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    let Some(repository_root) = repository_root.and_then(|path| fs::canonicalize(path).ok()) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["repository_root_unavailable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    let Ok(link_metadata) = fs::symlink_metadata(profile_path) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_unreadable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    if link_metadata.file_type().is_symlink() {
        return ImportQualificationAssessment {
            status: "rejected",
            reason_codes: vec!["qualification_profile_symlink_rejected"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    if !link_metadata.is_file() {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_not_regular_file"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    let Ok(canonical_profile_path) = fs::canonicalize(profile_path) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_unreadable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    if canonical_profile_path.starts_with(&repository_root) {
        return ImportQualificationAssessment {
            status: "rejected",
            reason_codes: vec!["qualification_profile_inside_repository"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    let Ok(metadata) = fs::metadata(&canonical_profile_path) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_unreadable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    if !metadata.is_file() {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_not_regular_file"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    if metadata.len() > IMPORT_QUALIFICATION_PROFILE_MAX_BYTES {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_too_large"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    let Ok(profile_file) = File::open(&canonical_profile_path) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_unreadable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    let mut profile_bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    if profile_file
        .take(IMPORT_QUALIFICATION_PROFILE_MAX_BYTES + 1)
        .read_to_end(&mut profile_bytes)
        .is_err()
    {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_unreadable"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    if u64::try_from(profile_bytes.len()).unwrap_or(u64::MAX)
        > IMPORT_QUALIFICATION_PROFILE_MAX_BYTES
    {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_too_large"],
            profile_sha256: None,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    }
    let profile_sha256 = Some(sha256_bytes(&profile_bytes));
    let Ok(profile) = serde_json::from_slice::<ImportQualificationProfile>(&profile_bytes) else {
        return ImportQualificationAssessment {
            status: "invalid",
            reason_codes: vec!["qualification_profile_invalid_json"],
            profile_sha256,
            hardware_class: None,
            host_fingerprint_sha256,
            storage_fingerprint_sha256,
            filesystem_type,
        };
    };
    let mut reason_codes =
        import_qualification_binding_reasons(&profile, hardware, storage, protocol);
    let qualified_root = profile
        .scratch
        .qualified_root
        .is_absolute()
        .then(|| fs::canonicalize(&profile.scratch.qualified_root).ok())
        .flatten();
    if !profile.scratch.qualified_root.is_absolute() {
        reason_codes.push("qualified_scratch_root_not_absolute");
    } else if qualified_root.is_none() {
        reason_codes.push("qualified_scratch_root_unavailable");
    } else if !scratch_root.starts_with(qualified_root.as_ref().unwrap()) {
        reason_codes.push("scratch_outside_qualified_root");
    }
    let binding_matched = reason_codes.is_empty();
    let status = if binding_matched {
        match OWNER_ACCEPTED_IMPORT_QUALIFICATION_PROFILE_SHA256 {
            Some(accepted) if profile_sha256.as_deref() == Some(accepted) => "matched",
            Some(_) => {
                reason_codes.push("qualification_profile_owner_digest_mismatch");
                "binding_matched_but_owner_digest_mismatch"
            }
            None => {
                reason_codes.push("qualification_profile_pending_owner_acceptance");
                "binding_matched_pending_owner_acceptance"
            }
        }
    } else {
        "mismatch"
    };
    reason_codes.sort_unstable();
    reason_codes.dedup();
    ImportQualificationAssessment {
        status,
        reason_codes,
        profile_sha256,
        hardware_class: Some(profile.hardware_class),
        host_fingerprint_sha256,
        storage_fingerprint_sha256,
        filesystem_type,
    }
}

fn import_qualification_binding_reasons(
    profile: &ImportQualificationProfile,
    hardware: &HostHardwareIdentity,
    storage: Option<&ScratchStorageIdentity>,
    protocol: &ImportQualificationProtocol,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if profile.schema != IMPORT_QUALIFICATION_PROFILE_SCHEMA {
        reasons.push("qualification_profile_schema_mismatch");
    }
    if profile.hardware_class != IMPORT_QUALIFICATION_HARDWARE_CLASS {
        reasons.push("qualification_hardware_class_mismatch");
    }
    if profile.host.cpu_model.trim().is_empty()
        || profile.host.logical_cpu_count == 0
        || profile.host.mem_total_kib == 0
    {
        reasons.push("qualification_host_tuple_invalid");
    }
    match (
        hardware.cpu_model.as_deref(),
        hardware.logical_cpu_count,
        hardware.mem_total_kib,
    ) {
        (Some(cpu_model), Some(logical_cpu_count), Some(mem_total_kib)) => {
            if cpu_model != profile.host.cpu_model {
                reasons.push("qualification_cpu_model_mismatch");
            }
            if logical_cpu_count != profile.host.logical_cpu_count {
                reasons.push("qualification_logical_cpu_count_mismatch");
            }
            if mem_total_kib != profile.host.mem_total_kib {
                reasons.push("qualification_memory_mismatch");
            }
        }
        _ => reasons.push("host_identity_unavailable"),
    }
    if profile.scratch.filesystem_type != "ext4" {
        reasons.push("qualification_filesystem_not_ext4");
    }
    if profile.scratch.filesystem_source.trim().is_empty()
        || profile.scratch.filesystem_uuid.trim().is_empty()
        || !valid_major_minor(&profile.scratch.device_major_minor)
    {
        reasons.push("qualification_storage_tuple_invalid");
    }
    match storage {
        Some(storage) => {
            if storage.filesystem_type != "ext4" {
                reasons.push("scratch_filesystem_not_ext4");
            }
            if storage.filesystem_type != profile.scratch.filesystem_type
                || storage.filesystem_source != profile.scratch.filesystem_source
                || storage.filesystem_uuid != profile.scratch.filesystem_uuid
                || storage.device_major_minor != profile.scratch.device_major_minor
            {
                reasons.push("qualification_storage_tuple_mismatch");
            }
        }
        None => reasons.push("scratch_storage_identity_unavailable"),
    }
    if !matches!(profile.protocol.cache_condition.as_str(), "cold" | "warm")
        || profile.protocol.competing_activity.trim().is_empty()
        || profile.protocol.competing_activity.trim() != profile.protocol.competing_activity
        || profile.protocol.competing_activity == "uncontrolled"
    {
        reasons.push("qualification_protocol_tuple_invalid");
    }
    if !matches!(protocol.cache_condition(), "cold" | "warm")
        || protocol.competing_activity().trim().is_empty()
        || protocol.competing_activity().trim() != protocol.competing_activity()
        || protocol.competing_activity() == "uncontrolled"
    {
        reasons.push("requested_protocol_tuple_uncontrolled_or_invalid");
    }
    if profile.protocol.cache_condition != protocol.cache_condition() {
        reasons.push("qualification_cache_condition_mismatch");
    }
    if profile.protocol.competing_activity != protocol.competing_activity() {
        reasons.push("qualification_competing_activity_mismatch");
    }
    reasons
}

fn valid_major_minor(value: &str) -> bool {
    let Some((major, minor)) = value.split_once(':') else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor.bytes().all(|byte| byte.is_ascii_digit())
}

fn scratch_storage_identity(path: &Path) -> std::io::Result<ScratchStorageIdentity> {
    Ok(ScratchStorageIdentity {
        filesystem_type: findmnt_value(path, "FSTYPE")?,
        filesystem_source: findmnt_value(path, "SOURCE")?,
        filesystem_uuid: findmnt_value(path, "UUID")?,
        device_major_minor: findmnt_value(path, "MAJ:MIN")?,
    })
}

fn findmnt_value(path: &Path, field: &str) -> std::io::Result<String> {
    let findmnt = [Path::new("/usr/bin/findmnt"), Path::new("/bin/findmnt")]
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "findmnt unavailable"))?;
    let output = Command::new(findmnt)
        .args(["--noheadings", "--raw", "--output", field, "--target"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            "findmnt could not identify the scratch filesystem",
        ));
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|_| std::io::Error::other("findmnt returned non-UTF-8 output"))?
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(std::io::Error::other(
            "findmnt omitted a required scratch identity field",
        ));
    }
    Ok(value)
}

fn import_host_fingerprint(hardware: &HostHardwareIdentity) -> Option<String> {
    let logical_cpu_count = hardware.logical_cpu_count?.to_string();
    let mem_total_kib = hardware.mem_total_kib?.to_string();
    Some(fingerprint(
        b"mirante4d-import-performance-host-fingerprint-1\0",
        &[
            hardware.cpu_model.as_deref()?,
            logical_cpu_count.as_str(),
            mem_total_kib.as_str(),
        ],
    ))
}

fn import_storage_fingerprint(storage: &ScratchStorageIdentity) -> String {
    fingerprint(
        b"mirante4d-import-performance-storage-fingerprint-1\0",
        &[
            storage.filesystem_type.as_str(),
            storage.filesystem_source.as_str(),
            storage.filesystem_uuid.as_str(),
            storage.device_major_minor.as_str(),
        ],
    )
}

fn fingerprint(domain: &[u8], fields: &[&str]) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(domain);
    for field in fields {
        hasher.update(
            u64::try_from(field.len())
                .expect("an in-memory fingerprint field length fits u64")
                .to_le_bytes(),
        );
        hasher.update(field.as_bytes());
    }
    hasher.finalize().to_string()
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256Hasher::digest(bytes).to_string()
}

fn git_text(arguments: &[&str]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| nonempty_stdout(&output.stdout))
        .flatten()
}

fn nonempty_stdout(stdout: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn linux_cpu_model() -> Option<String> {
    parse_linux_cpu_model(&fs::read_to_string("/proc/cpuinfo").ok()?)
}

fn parse_linux_cpu_model(contents: &str) -> Option<String> {
    contents
        .lines()
        .find_map(|line| {
            line.strip_prefix("model name")
                .or_else(|| line.strip_prefix("Hardware"))
        })
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().to_owned())
        })
        .filter(|value| !value.is_empty())
}

fn linux_mem_total_kib() -> Option<u64> {
    parse_linux_mem_total_kib(&fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_linux_mem_total_kib(contents: &str) -> Option<u64> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_host_parsers_require_concrete_values() {
        assert_eq!(
            parse_linux_cpu_model("processor: 0\nmodel name\t: Example CPU 1\n"),
            Some("Example CPU 1".to_owned())
        );
        assert_eq!(parse_linux_cpu_model("model name:   \n"), None);
        assert_eq!(
            parse_linux_mem_total_kib("MemTotal:       32768 kB\n"),
            Some(32_768)
        );
        assert_eq!(parse_linux_mem_total_kib("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn benchmark_context_does_not_publish_the_repository_root() {
        let repository = RepositoryIdentity {
            root: Some(PathBuf::from("/private/checkout")),
            commit: Some("abc123".to_owned()),
            dirty_worktree: Some(false),
        };
        let hardware = HostHardwareIdentity {
            cpu_model: Some("Example CPU".to_owned()),
            logical_cpu_count: Some(8),
            mem_total_kib: Some(1024),
        };
        let value = benchmark_host_context_from(&repository, &hardware);

        assert_eq!(value["git_commit"], "abc123");
        assert_eq!(value["logical_cpu_count"], 8);
        assert!(!value.to_string().contains("/private/checkout"));
    }

    #[test]
    fn exact_external_profile_binding_is_redacted_and_rejects_unaccepted_digest() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let qualified_root = temporary.path().join("qualified-private-scratch");
        let scratch = qualified_root.join("session-root");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&qualified_root).unwrap();
        fs::create_dir(&scratch).unwrap();
        let profile_path = temporary.path().join("private-hw-2.json");
        write_test_qualification_profile(&profile_path, &qualified_root, None);
        let hardware = test_hardware_identity();
        let storage = test_storage_identity();

        let assessment = assess_import_qualification_profile_with_storage(
            Some(&profile_path),
            Some(&repository),
            &fs::canonicalize(&scratch).unwrap(),
            &hardware,
            Some(&storage),
            &test_protocol(),
        );

        assert_eq!(
            assessment.status,
            "binding_matched_but_owner_digest_mismatch"
        );
        assert_eq!(
            assessment.reason_codes,
            vec!["qualification_profile_owner_digest_mismatch"]
        );
        assert!(assessment.profile_sha256.is_some());
        assert!(assessment.host_fingerprint_sha256.is_some());
        assert!(assessment.storage_fingerprint_sha256.is_some());
        let evidence = import_qualification_assessment_evidence(&assessment).to_string();
        let profile_path_text = profile_path.to_string_lossy().into_owned();
        let qualified_root_text = qualified_root.to_string_lossy().into_owned();
        for private_value in [
            profile_path_text.as_str(),
            qualified_root_text.as_str(),
            "Private Example CPU",
            "/dev/private-example-device",
            "private-filesystem-uuid",
            "259:42",
        ] {
            assert!(!evidence.contains(private_value));
        }
    }

    #[test]
    fn profile_mismatch_identifies_host_and_storage_failures_without_raw_values() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let qualified_root = temporary.path().join("qualified");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&qualified_root).unwrap();
        let profile_path = temporary.path().join("profile.json");
        let mut profile = test_qualification_profile(&qualified_root);
        profile["host"]["cpu_model"] = json!("Different CPU");
        profile["host"]["mem_total_kib"] = json!(999_u64);
        profile["scratch"]["filesystem_uuid"] = json!("different-uuid");
        fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();

        let assessment = assess_import_qualification_profile_with_storage(
            Some(&profile_path),
            Some(&repository),
            &fs::canonicalize(&qualified_root).unwrap(),
            &test_hardware_identity(),
            Some(&test_storage_identity()),
            &test_protocol(),
        );

        assert_eq!(assessment.status, "mismatch");
        assert!(
            assessment
                .reason_codes
                .contains(&"qualification_cpu_model_mismatch")
        );
        assert!(
            assessment
                .reason_codes
                .contains(&"qualification_memory_mismatch")
        );
        assert!(
            assessment
                .reason_codes
                .contains(&"qualification_storage_tuple_mismatch")
        );
        assert!(
            !import_qualification_assessment_evidence(&assessment)
                .to_string()
                .contains("different-uuid")
        );
    }

    #[test]
    fn profile_binds_the_exact_declared_cache_and_activity_protocol() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let qualified_root = temporary.path().join("qualified");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&qualified_root).unwrap();
        let profile_path = temporary.path().join("profile.json");
        write_test_qualification_profile(&profile_path, &qualified_root, None);

        let assessment = assess_import_qualification_profile_with_storage(
            Some(&profile_path),
            Some(&repository),
            &fs::canonicalize(&qualified_root).unwrap(),
            &test_hardware_identity(),
            Some(&test_storage_identity()),
            &ImportQualificationProtocol::new("cold", "background-indexer-paused"),
        );

        assert_eq!(assessment.status, "mismatch");
        assert!(
            assessment
                .reason_codes
                .contains(&"qualification_cache_condition_mismatch")
        );
        assert!(
            assessment
                .reason_codes
                .contains(&"qualification_competing_activity_mismatch")
        );
    }

    #[test]
    fn build_provenance_requires_the_exact_clean_release_revision() {
        let repository = RepositoryIdentity {
            root: Some(PathBuf::from("/repository")),
            commit: Some("commit-a".to_owned()),
            dirty_worktree: Some(false),
        };
        let accepted = QualificationBuildProvenance {
            git_head: "commit-a".to_owned(),
            git_dirty: false,
            cargo_profile: "release".to_owned(),
            opt_level: "3".to_owned(),
            debug: "false".to_owned(),
            compiler: "rustc example".to_owned(),
            custom_rustflags: false,
            rustc_wrapper: false,
        };
        assert!(qualification_build_reason_codes(&accepted, &repository).is_empty());

        let mut rejected = accepted;
        rejected.git_head = "commit-b".to_owned();
        rejected.git_dirty = true;
        rejected.cargo_profile = "debug".to_owned();
        rejected.opt_level = "0".to_owned();
        rejected.debug = "true".to_owned();
        rejected.custom_rustflags = true;
        rejected.rustc_wrapper = true;
        let reasons = qualification_build_reason_codes(&rejected, &repository);
        for expected in [
            "build_revision_does_not_match_repository",
            "executable_built_from_dirty_worktree",
            "build_profile_not_release",
            "build_opt_level_not_standard_release",
            "build_debug_not_standard_release",
            "custom_rustflags_not_qualified",
            "rustc_wrapper_not_qualified",
        ] {
            assert!(reasons.contains(&expected));
        }
    }

    #[test]
    fn profile_must_be_bounded_strict_regular_and_outside_the_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let qualified_root = temporary.path().join("qualified");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&qualified_root).unwrap();
        let scratch = fs::canonicalize(&qualified_root).unwrap();
        let hardware = test_hardware_identity();
        let storage = test_storage_identity();

        let inside = repository.join("profile.json");
        write_test_qualification_profile(&inside, &qualified_root, None);
        let assessment = assess_import_qualification_profile_with_storage(
            Some(&inside),
            Some(&repository),
            &scratch,
            &hardware,
            Some(&storage),
            &test_protocol(),
        );
        assert_eq!(assessment.status, "rejected");
        assert_eq!(
            assessment.reason_codes,
            vec!["qualification_profile_inside_repository"]
        );

        let oversized = temporary.path().join("oversized.json");
        fs::write(
            &oversized,
            vec![b' '; usize::try_from(IMPORT_QUALIFICATION_PROFILE_MAX_BYTES + 1).unwrap()],
        )
        .unwrap();
        let assessment = assess_import_qualification_profile_with_storage(
            Some(&oversized),
            Some(&repository),
            &scratch,
            &hardware,
            Some(&storage),
            &test_protocol(),
        );
        assert_eq!(
            assessment.reason_codes,
            vec!["qualification_profile_too_large"]
        );

        let strict = temporary.path().join("strict.json");
        write_test_qualification_profile(&strict, &qualified_root, Some(("extra", json!(true))));
        let assessment = assess_import_qualification_profile_with_storage(
            Some(&strict),
            Some(&repository),
            &scratch,
            &hardware,
            Some(&storage),
            &test_protocol(),
        );
        assert_eq!(
            assessment.reason_codes,
            vec!["qualification_profile_invalid_json"]
        );

        let symlink_target = temporary.path().join("target-profile.json");
        let symlink_path = temporary.path().join("profile-link.json");
        write_test_qualification_profile(&symlink_target, &qualified_root, None);
        std::os::unix::fs::symlink(&symlink_target, &symlink_path).unwrap();
        let assessment = assess_import_qualification_profile_with_storage(
            Some(&symlink_path),
            Some(&repository),
            &scratch,
            &hardware,
            Some(&storage),
            &test_protocol(),
        );
        assert_eq!(assessment.status, "rejected");
        assert_eq!(
            assessment.reason_codes,
            vec!["qualification_profile_symlink_rejected"]
        );
    }

    fn test_hardware_identity() -> HostHardwareIdentity {
        HostHardwareIdentity {
            cpu_model: Some("Private Example CPU".to_owned()),
            logical_cpu_count: Some(32),
            mem_total_kib: Some(65_536),
        }
    }

    fn test_storage_identity() -> ScratchStorageIdentity {
        ScratchStorageIdentity {
            filesystem_type: "ext4".to_owned(),
            filesystem_source: "/dev/private-example-device".to_owned(),
            filesystem_uuid: "private-filesystem-uuid".to_owned(),
            device_major_minor: "259:42".to_owned(),
        }
    }

    fn test_protocol() -> ImportQualificationProtocol {
        ImportQualificationProtocol::new("warm", "none")
    }

    fn test_qualification_profile(qualified_root: &Path) -> Value {
        json!({
            "schema": IMPORT_QUALIFICATION_PROFILE_SCHEMA,
            "hardware_class": IMPORT_QUALIFICATION_HARDWARE_CLASS,
            "host": {
                "cpu_model": "Private Example CPU",
                "logical_cpu_count": 32,
                "mem_total_kib": 65_536,
            },
            "scratch": {
                "qualified_root": qualified_root,
                "filesystem_type": "ext4",
                "filesystem_source": "/dev/private-example-device",
                "filesystem_uuid": "private-filesystem-uuid",
                "device_major_minor": "259:42",
            },
            "protocol": {
                "cache_condition": "warm",
                "competing_activity": "none",
            },
        })
    }

    fn write_test_qualification_profile(
        path: &Path,
        qualified_root: &Path,
        extra: Option<(&str, Value)>,
    ) {
        let mut profile = test_qualification_profile(qualified_root);
        if let Some((key, value)) = extra {
            profile
                .as_object_mut()
                .unwrap()
                .insert(key.to_owned(), value);
        }
        fs::write(path, serde_json::to_vec(&profile).unwrap()).unwrap();
    }
}
