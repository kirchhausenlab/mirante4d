use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    private_evidence::{read_finalized_private_file, write_new_synced_private_file},
    process,
    reports::write_json_file,
};

const CONFIG_SCHEMA: &str = "mirante4d-private-static-rendering-recovery-config-1";
const FRAGMENT_SCHEMA: &str = "mirante4d-private-static-rendering-recovery-fragment-1";
const RAW_SCHEMA: &str = "mirante4d-private-static-rendering-recovery-raw-1";
const SANITIZED_SCHEMA: &str = "mirante4d-static-rendering-recovery-summary-1";
const CONFIG_BYTES_MAX: u64 = 1024 * 1024;
const OVERLAY_BYTES_MAX: u64 = 8 * 1024 * 1024;
const FRAGMENT_BYTES_MAX: u64 = 16 * 1024 * 1024;
const RAW_BYTES_MAX: u64 = 64 * 1024 * 1024;
const FIXED_WARMUPS: usize = 5;
const FIXED_SAMPLES: usize = 30;
const SESSION_COUNT: usize = 3;
const NAVIGATION_DURATION_SECONDS: u64 = 30;
const NAVIGATION_INPUT_HZ: u64 = 60;
const ONE_MILLISECOND_NS: u64 = 1_000_000;
const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const REVISION_A: &str = "24f7da1531056c950cb5479098bd723c5fd8dc91";
const REVISION_B: &str = "d5032c43525ddfa9d524d490e3391aa800c6d470";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
enum RevisionLabel {
    A,
    B,
    C,
}

impl RevisionLabel {
    const ALL: [Self; 3] = [Self::A, Self::B, Self::C];

    const fn token(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkloadKind {
    FixedLod,
    WarmNavigation,
    ColdSettlement,
}

impl WorkloadKind {
    const ALL: [Self; 3] = [Self::FixedLod, Self::WarmNavigation, Self::ColdSettlement];

    const fn token(self) -> &'static str {
        match self {
            Self::FixedLod => "fixed_lod",
            Self::WarmNavigation => "warm_navigation",
            Self::ColdSettlement => "cold_settlement",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CampaignConfig {
    schema: String,
    campaign_token: String,
    workload: PathBuf,
    workload_identity_sha256: String,
    private_output_directory: PathBuf,
    raw_report: PathBuf,
    evidence_overlay_patch: PathBuf,
    evidence_overlay_sha256: String,
    revisions: Vec<RevisionConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RevisionConfig {
    label: RevisionLabel,
    worktree: PathBuf,
    source_commit: String,
    measured_commit: String,
    measured_tree: String,
    collector: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCampaign {
    schema: String,
    campaign_token: String,
    workload_path: PathBuf,
    workload_identity_sha256: String,
    evidence_overlay_sha256: String,
    overlay_noninterference_passed: bool,
    host: HostFacts,
    revisions: Vec<MeasuredRevision>,
    fragments: Vec<SessionFragment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HostFacts {
    adapter: String,
    backend: String,
    driver: String,
    cpu_model: String,
    toolchain: String,
    power_state: String,
    gpu_timestamps_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MeasuredRevision {
    label: RevisionLabel,
    source_commit: String,
    measured_commit: String,
    measured_tree: String,
    worktree_clean: bool,
    evidence_overlay_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionFragment {
    schema: String,
    campaign_token: String,
    workload_identity_sha256: String,
    evidence_overlay_sha256: String,
    overlay_noninterference_passed: bool,
    host: HostFacts,
    process_arguments: Vec<String>,
    viewport: [u32; 2],
    renderer_settings_sha256: String,
    start_environment: EnvironmentSample,
    end_environment: EnvironmentSample,
    revision: RevisionLabel,
    source_commit: String,
    measured_commit: String,
    measured_tree: String,
    block: usize,
    order_position: usize,
    eligible_for_timing: bool,
    ineligibility_reason: Option<String>,
    #[serde(flatten)]
    data: SessionData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentSample {
    temperature_celsius_milli: Option<i64>,
    gpu_clock_mhz: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "workload", content = "evidence", rename_all = "snake_case")]
enum SessionData {
    FixedLod(FixedLodRun),
    WarmNavigation(WarmNavigationRun),
    ColdSettlement(ColdSettlementRun),
}

impl SessionData {
    const fn kind(&self) -> WorkloadKind {
        match self {
            Self::FixedLod(_) => WorkloadKind::FixedLod,
            Self::WarmNavigation(_) => WorkloadKind::WarmNavigation,
            Self::ColdSettlement(_) => WorkloadKind::ColdSettlement,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LayerScaleFact {
    layer: u32,
    scale: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScientificFacts {
    pixel_sha256: String,
    coverage_sha256: String,
    validity_sha256: String,
    requested_scales: Vec<LayerScaleFact>,
    selected_scales: Vec<LayerScaleFact>,
    navigation_scales: Vec<LayerScaleFact>,
    displayed_scales: Vec<LayerScaleFact>,
    final_current: bool,
    final_exact: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FixedLodRun {
    warmups: usize,
    measured_samples: usize,
    cases: Vec<FixedLodCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FixedLodCase {
    case_id: String,
    channels: u32,
    kernel: String,
    sampling: String,
    scientific: ScientificFacts,
    expected_scientific: ScientificFacts,
    measurements: RunMeasurements,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BoundaryCounters {
    physical_reads: u64,
    codec_decodes: u64,
    dataset_requests: u64,
    dataset_leases: u64,
    cancellation_waste_reads: u64,
    cancellation_waste_bytes: u64,
    payload_uploads: u64,
    payload_upload_bytes: u64,
    evictions: u64,
    allocator_plans: u64,
    body_rebuilds: u64,
    compactions: u64,
    residency_preflights: u64,
    renderer_queue_submissions: u64,
    hidden_jobs: u64,
    hidden_batches: u64,
    hidden_rows: u64,
    hidden_timeouts: u64,
    renderer_errors: u64,
    validation_errors: u64,
    attempt_waits: u64,
    attempt_failures: u64,
    repaint_requests: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunMeasurements {
    ui_update_ns: Vec<u64>,
    semantic_demand_ns: Vec<u64>,
    render_plan_ns: Vec<u64>,
    renderer_planning_ns: Vec<u64>,
    renderer_queue_submit_ns: Vec<u64>,
    gpu_color_ns: Vec<u64>,
    egui_texture_registrations: u64,
    egui_paint_commands: u64,
    accepted_direct_cutoffs: u64,
    color_submissions: u64,
    declared_hidden_batches: u64,
    settled_idle_renderer_submissions: u64,
    settled_idle_immediate_repaints: u64,
    presentation_revisions: Vec<u64>,
    texture_revisions: Vec<u64>,
    target_order: Vec<String>,
    repaint_reasons: BTreeMap<String, u64>,
    counters: BoundaryCounters,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WarmNavigationRun {
    driver: String,
    workflow: String,
    duration_seconds: u64,
    input_hz: u64,
    generated_wheel_count: u64,
    received_wheel_count: u64,
    accepted_camera_revisions: Vec<u64>,
    initial_camera_facts: String,
    final_camera_facts: String,
    balanced_return: bool,
    dry_run_all_bodies_resident_and_retained: bool,
    accepted_scientific_sequence: Vec<ScientificFacts>,
    expected_scientific_sequence: Vec<ScientificFacts>,
    scientific: ScientificFacts,
    expected_scientific: ScientificFacts,
    measurements: RunMeasurements,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ColdSettlementRun {
    throwaway_process_completed: bool,
    throwaway_read_decode_identity: String,
    measured_read_decode_identity: String,
    fresh_process: bool,
    fresh_wgpu_runtime: bool,
    empty_application_caches: bool,
    no_recovered_project_state: bool,
    filesystem_cache_claim: String,
    exact_settlement_wall_ns: u64,
    scientific: ScientificFacts,
    expected_scientific: ScientificFacts,
    measurements: RunMeasurements,
}

#[derive(Debug, Clone, Serialize)]
struct CaseSummary {
    case_id: String,
    baseline_revision: Option<RevisionLabel>,
    baseline_block_p95_ns: Option<Vec<u64>>,
    baseline_median_p95_ns: Option<u64>,
    candidate_block_p95_ns: Vec<u64>,
    candidate_median_p95_ns: Option<u64>,
    ratio: Option<f64>,
    evaluated: bool,
    passed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ScalarSummary {
    baseline_revision: Option<RevisionLabel>,
    baseline_block_values_ns: Option<Vec<u64>>,
    baseline_median_ns: Option<u64>,
    candidate_block_values_ns: Vec<u64>,
    candidate_median_ns: u64,
    threshold_ns: Option<u64>,
    ratio: Option<f64>,
    evaluated: bool,
    passed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Evaluation {
    overlay_noninterference_passed: bool,
    workload_identity_matched: bool,
    matching_scientific_work: bool,
    fixed_lod: Vec<CaseSummary>,
    warm_navigation_ui_p95: ScalarSummary,
    cold_exact_settlement: ScalarSummary,
    warm_zero_io_decode_request_upload_eviction: bool,
    settled_idle_zero_work: bool,
    one_color_submission_per_direct_cutoff: bool,
    no_renderer_validation_or_attempt_failure: bool,
    first_attribution_boundary: String,
    quantitative_gate_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Args {
    Run { config: PathBuf },
    Finalize { raw_report: PathBuf },
    Help,
}

pub(crate) fn run(args: Vec<String>) -> anyhow::Result<PathBuf> {
    match parse_args(args)? {
        Args::Help => {
            print_help();
            Ok(workspace_root())
        }
        Args::Finalize { raw_report } => finalize_raw_report(&raw_report),
        Args::Run { config } => run_campaign(&config),
    }
}

fn parse_args(args: Vec<String>) -> anyhow::Result<Args> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        return Ok(Args::Help);
    }
    match args.as_slice() {
        [flag, path] if flag == "--config" => Ok(Args::Run {
            config: PathBuf::from(path),
        }),
        [flag, path] if flag == "--raw-report" => Ok(Args::Finalize {
            raw_report: PathBuf::from(path),
        }),
        _ => bail!(
            "usage: cargo xtask representative-static-rendering-recovery --config /absolute/private/config.json\n       cargo xtask representative-static-rendering-recovery --raw-report /absolute/private/raw-report.json"
        ),
    }
}

fn print_help() {
    println!(
        "\
usage: cargo xtask representative-static-rendering-recovery --config /absolute/private/config.json
       cargo xtask representative-static-rendering-recovery --raw-report /absolute/private/raw-report.json

The campaign form validates three clean A/B/C worktrees and invokes the
identical EvidenceOverlay collector in the required A/B/C, B/C/A, C/A/B block
orders for fixed-LOD, 30-second warm navigation, and process-cold settlement.
The replay form validates and sanitizes an already finalized private raw
report. Private paths and scientific identities never enter the summary."
    );
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("xtask is nested under crates/")
        .to_path_buf()
}

fn run_campaign(config_path: &Path) -> anyhow::Result<PathBuf> {
    if env::var("MIRANTE4D_XTASK_ALLOW_STATIC_RECOVERY").as_deref() != Ok("1") {
        bail!(
            "the static recovery campaign requires MIRANTE4D_XTASK_ALLOW_STATIC_RECOVERY=1 on the designated workstation"
        );
    }
    let root = workspace_root();
    let config_input =
        read_finalized_private_file(config_path, &root, CONFIG_BYTES_MAX, "campaign config")?;
    let config: CampaignConfig =
        serde_json::from_slice(&config_input.bytes).context("campaign config is invalid JSON")?;
    validate_config(&config, &root)?;
    let overlay = read_finalized_private_file(
        &config.evidence_overlay_patch,
        &root,
        OVERLAY_BYTES_MAX,
        "EvidenceOverlay patch",
    )?;
    ensure!(
        overlay.sha256 == config.evidence_overlay_sha256,
        "EvidenceOverlay patch bytes differ from their configured SHA-256 identity"
    );

    let measured = config
        .revisions
        .iter()
        .map(|revision| {
            validate_revision_worktree(
                revision,
                &config.evidence_overlay_patch,
                &config.evidence_overlay_sha256,
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let revisions = config
        .revisions
        .iter()
        .map(|revision| (revision.label, revision))
        .collect::<BTreeMap<_, _>>();
    let mut fragments = Vec::with_capacity(27);
    let mut host = None;
    for workload in WorkloadKind::ALL {
        for block in 1..=SESSION_COUNT {
            let order = expected_order(block);
            for (order_index, label) in order.into_iter().enumerate() {
                let revision = revisions
                    .get(&label)
                    .copied()
                    .expect("validated config contains A, B, and C");
                let fragment_path = config.private_output_directory.join(format!(
                    "fragment-{}-block-{block}-{}.json",
                    workload.token(),
                    label.token().to_ascii_lowercase()
                ));
                if fragment_path.exists() {
                    bail!(
                        "campaign fragment already exists and will not be overwritten: {}",
                        fragment_path.display()
                    );
                }
                let mut command = Command::new(&revision.collector);
                command
                    .current_dir(&revision.worktree)
                    .arg("--schema")
                    .arg(FRAGMENT_SCHEMA)
                    .arg("--workload")
                    .arg(workload.token())
                    .arg("--revision")
                    .arg(label.token())
                    .arg("--block")
                    .arg(block.to_string())
                    .arg("--order-position")
                    .arg((order_index + 1).to_string())
                    .arg("--campaign-token")
                    .arg(&config.campaign_token)
                    .arg("--workload-identity-sha256")
                    .arg(&config.workload_identity_sha256)
                    .arg("--evidence-overlay-sha256")
                    .arg(&config.evidence_overlay_sha256)
                    .arg("--dataset")
                    .arg(&config.workload)
                    .arg("--output")
                    .arg(&fragment_path)
                    .arg("--fixed-warmups")
                    .arg(FIXED_WARMUPS.to_string())
                    .arg("--fixed-samples")
                    .arg(FIXED_SAMPLES.to_string())
                    .arg("--navigation-duration-seconds")
                    .arg(NAVIGATION_DURATION_SECONDS.to_string())
                    .arg("--navigation-input-hz")
                    .arg(NAVIGATION_INPUT_HZ.to_string())
                    .arg("--navigation-driver")
                    .arg("viewer-oblique-continuity")
                    .arg("--navigation-workflow")
                    .arg("zoom")
                    .arg("--cold-filesystem-condition")
                    .arg("throwaway_warm_then_fresh_process");
                process::run_command_with_timeout(&mut command, COLLECTOR_TIMEOUT).with_context(
                    || {
                        format!(
                            "EvidenceOverlay collector failed for {} block {block} revision {}",
                            workload.token(),
                            label.token()
                        )
                    },
                )?;
                let fragment_input = read_finalized_private_file(
                    &fragment_path,
                    &root,
                    FRAGMENT_BYTES_MAX,
                    "campaign fragment",
                )?;
                let fragment: SessionFragment = serde_json::from_slice(&fragment_input.bytes)
                    .context("campaign fragment is invalid JSON")?;
                validate_fragment_binding(
                    &fragment,
                    &config,
                    revision,
                    workload,
                    block,
                    order_index + 1,
                )?;
                if let Some(expected) = &host {
                    ensure!(
                        expected == &fragment.host,
                        "adapter, driver, CPU, toolchain, or power state changed during the campaign"
                    );
                } else {
                    host = Some(fragment.host.clone());
                }
                fragments.push(fragment);
            }
        }
    }

    let raw = RawCampaign {
        schema: RAW_SCHEMA.to_owned(),
        campaign_token: config.campaign_token.clone(),
        workload_path: config.workload.clone(),
        workload_identity_sha256: config.workload_identity_sha256.clone(),
        evidence_overlay_sha256: config.evidence_overlay_sha256.clone(),
        overlay_noninterference_passed: fragments
            .iter()
            .all(|fragment| fragment.overlay_noninterference_passed),
        host: host.context("the campaign produced no host facts")?,
        revisions: measured,
        fragments,
    };
    let mut bytes = serde_json::to_vec_pretty(&raw).context("failed to serialize raw campaign")?;
    bytes.push(b'\n');
    write_new_synced_private_file(
        &config.raw_report,
        &bytes,
        RAW_BYTES_MAX,
        "campaign raw report",
    )?;
    finalize_raw_report(&config.raw_report)
}

fn validate_config(config: &CampaignConfig, repository_root: &Path) -> anyhow::Result<()> {
    ensure!(
        config.schema == CONFIG_SCHEMA,
        "unexpected campaign config schema"
    );
    validate_safe_token(&config.campaign_token, "campaign token")?;
    validate_sha256(
        &config.workload_identity_sha256,
        "private workload identity",
    )?;
    validate_sha256(&config.evidence_overlay_sha256, "EvidenceOverlay identity")?;
    ensure!(
        config.workload.is_absolute() && config.workload.exists(),
        "private workload must be an existing absolute path"
    );
    ensure!(
        config.evidence_overlay_patch.is_absolute() && config.evidence_overlay_patch.is_file(),
        "EvidenceOverlay patch must be an existing absolute file"
    );
    ensure!(
        config.private_output_directory.is_absolute() && config.private_output_directory.is_dir(),
        "private output directory must be an existing absolute directory"
    );
    let private_output_metadata = fs::symlink_metadata(&config.private_output_directory)
        .context("private output directory is unavailable")?;
    ensure!(
        !private_output_metadata.file_type().is_symlink()
            && private_output_metadata.permissions().mode() & 0o077 == 0,
        "private output directory must be a nonsymlink directory inaccessible to group and other"
    );
    let canonical_repository = fs::canonicalize(repository_root)?;
    let canonical_private_output = fs::canonicalize(&config.private_output_directory)?;
    let canonical_workload = fs::canonicalize(&config.workload)?;
    let canonical_overlay = fs::canonicalize(&config.evidence_overlay_patch)
        .context("EvidenceOverlay patch is unavailable")?;
    ensure!(
        !canonical_private_output.starts_with(&canonical_repository)
            && !canonical_workload.starts_with(&canonical_repository)
            && !canonical_overlay.starts_with(&canonical_repository),
        "private workload, EvidenceOverlay patch, and output directory must remain outside the repository"
    );
    ensure!(
        config.raw_report.is_absolute()
            && config.raw_report.parent() == Some(config.private_output_directory.as_path()),
        "raw report must be a direct child of the configured private output directory"
    );
    ensure!(
        !config.raw_report.exists(),
        "raw report already exists and will not be overwritten"
    );
    ensure!(
        config.revisions.len() == 3
            && config
                .revisions
                .iter()
                .map(|revision| revision.label)
                .collect::<BTreeSet<_>>()
                == BTreeSet::from(RevisionLabel::ALL),
        "campaign config must contain exactly one A, B, and C revision"
    );
    for revision in &config.revisions {
        ensure!(
            revision.worktree.is_absolute() && revision.worktree.join("Cargo.toml").is_file(),
            "revision {} worktree is not an absolute repository checkout",
            revision.label.token()
        );
        ensure!(
            revision.collector.is_absolute() && revision.collector.is_file(),
            "revision {} EvidenceOverlay collector is unavailable",
            revision.label.token()
        );
        validate_git_oid(&revision.source_commit, "source commit")?;
        validate_git_oid(&revision.measured_commit, "measured commit")?;
        validate_git_oid(&revision.measured_tree, "measured tree")?;
    }
    let distinct_worktrees = config
        .revisions
        .iter()
        .map(|revision| fs::canonicalize(&revision.worktree))
        .collect::<Result<BTreeSet<_>, _>>()?;
    ensure!(
        distinct_worktrees.len() == RevisionLabel::ALL.len(),
        "A, B, and C must use three distinct clean worktrees"
    );
    let a = config
        .revisions
        .iter()
        .find(|revision| revision.label == RevisionLabel::A)
        .expect("A exists");
    let b = config
        .revisions
        .iter()
        .find(|revision| revision.label == RevisionLabel::B)
        .expect("B exists");
    ensure!(
        a.source_commit == REVISION_A,
        "revision A source commit drifted"
    );
    ensure!(
        b.source_commit == REVISION_B,
        "revision B source commit drifted"
    );
    Ok(())
}

fn validate_revision_worktree(
    revision: &RevisionConfig,
    evidence_overlay_patch: &Path,
    evidence_overlay_sha256: &str,
) -> anyhow::Result<MeasuredRevision> {
    let status = git_output(
        &revision.worktree,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    ensure!(
        status.is_empty(),
        "revision {} worktree is not clean",
        revision.label.token()
    );
    let head = git_output(&revision.worktree, &["rev-parse", "HEAD"])?;
    let tree = git_output(&revision.worktree, &["rev-parse", "HEAD^{tree}"])?;
    ensure!(
        head == revision.measured_commit,
        "revision {} measured commit differs from its clean worktree HEAD",
        revision.label.token()
    );
    ensure!(
        tree == revision.measured_tree,
        "revision {} measured tree differs from its clean worktree tree",
        revision.label.token()
    );
    let parent = git_output(&revision.worktree, &["rev-parse", "HEAD^"])?;
    ensure!(
        parent == revision.source_commit,
        "revision {} measured commit must be exactly one EvidenceOverlay commit above its source",
        revision.label.token()
    );
    let overlay_present = Command::new("git")
        .current_dir(&revision.worktree)
        .args(["apply", "--reverse", "--check", "--whitespace=nowarn"])
        .arg(evidence_overlay_patch)
        .status()
        .context("failed to verify the EvidenceOverlay patch")?;
    ensure!(
        overlay_present.success(),
        "revision {} measured tree does not contain the exact configured EvidenceOverlay patch",
        revision.label.token()
    );
    let ancestor = Command::new("git")
        .current_dir(&revision.worktree)
        .args([
            "merge-base",
            "--is-ancestor",
            &revision.source_commit,
            &revision.measured_commit,
        ])
        .status()
        .context("failed to verify source ancestry")?;
    ensure!(
        ancestor.success(),
        "revision {} measured commit does not descend from its declared source",
        revision.label.token()
    );
    Ok(MeasuredRevision {
        label: revision.label,
        source_commit: revision.source_commit.clone(),
        measured_commit: revision.measured_commit.clone(),
        measured_tree: revision.measured_tree.clone(),
        worktree_clean: true,
        evidence_overlay_sha256: evidence_overlay_sha256.to_owned(),
    })
}

fn git_output(worktree: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(worktree)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {}", worktree.display()))?;
    ensure!(
        output.status.success(),
        "git command failed in {}: {}",
        worktree.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)
        .context("git output was not UTF-8")?
        .trim()
        .to_owned())
}

fn validate_fragment_binding(
    fragment: &SessionFragment,
    config: &CampaignConfig,
    revision: &RevisionConfig,
    workload: WorkloadKind,
    block: usize,
    order_position: usize,
) -> anyhow::Result<()> {
    ensure!(
        fragment.schema == FRAGMENT_SCHEMA,
        "unexpected fragment schema"
    );
    ensure!(
        fragment.campaign_token == config.campaign_token
            && fragment.workload_identity_sha256 == config.workload_identity_sha256
            && fragment.evidence_overlay_sha256 == config.evidence_overlay_sha256,
        "fragment campaign, workload, or EvidenceOverlay binding differs"
    );
    ensure!(
        fragment.revision == revision.label
            && fragment.source_commit == revision.source_commit
            && fragment.measured_commit == revision.measured_commit
            && fragment.measured_tree == revision.measured_tree,
        "fragment revision identity differs from the clean measured worktree"
    );
    ensure!(
        fragment.data.kind() == workload
            && fragment.block == block
            && fragment.order_position == order_position,
        "fragment workload, block, or order position differs from the orchestrated invocation"
    );
    ensure!(
        fragment.eligible_for_timing == fragment.ineligibility_reason.is_none(),
        "fragment eligibility must have exactly one truthful reason state"
    );
    validate_host(&fragment.host)?;
    ensure!(
        fragment.viewport == [1920, 1080]
            && !fragment.process_arguments.is_empty()
            && fragment
                .process_arguments
                .iter()
                .all(|argument| !argument.is_empty() && !argument.contains('\0')),
        "fragment lost its fixed viewport or process argument evidence"
    );
    validate_sha256(
        &fragment.renderer_settings_sha256,
        "renderer settings identity",
    )?;
    validate_environment_pair(&fragment.start_environment, &fragment.end_environment)?;
    validate_session_data(&fragment.data, &fragment.host)?;
    Ok(())
}

fn expected_order(block: usize) -> [RevisionLabel; 3] {
    match block {
        1 => [RevisionLabel::A, RevisionLabel::B, RevisionLabel::C],
        2 => [RevisionLabel::B, RevisionLabel::C, RevisionLabel::A],
        3 => [RevisionLabel::C, RevisionLabel::A, RevisionLabel::B],
        _ => panic!("the protocol has exactly three blocks"),
    }
}

fn validate_safe_token(token: &str, label: &str) -> anyhow::Result<()> {
    ensure!(
        !token.is_empty()
            && token.len() <= 96
            && token
                .bytes()
                .all(|byte| { byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' }),
        "{label} must contain only lowercase ASCII letters, digits, and hyphens"
    );
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> anyhow::Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{label} is not a lowercase SHA-256 digest"
    );
    Ok(())
}

fn validate_git_oid(value: &str, label: &str) -> anyhow::Result<()> {
    ensure!(
        value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{label} is not a full lowercase Git object ID"
    );
    Ok(())
}

fn finalize_raw_report(raw_report_path: &Path) -> anyhow::Result<PathBuf> {
    let root = workspace_root();
    let input = read_finalized_private_file(
        raw_report_path,
        &root,
        RAW_BYTES_MAX,
        "static recovery raw report",
    )?;
    let raw: RawCampaign =
        serde_json::from_slice(&input.bytes).context("raw campaign is invalid JSON")?;
    validate_raw_campaign(&raw)?;
    let evaluation = evaluate_campaign(&raw)?;
    let sanitized = sanitized_report(&raw, &evaluation);
    let private_strings = private_strings(&raw, raw_report_path);
    validate_sanitized_report(&sanitized, &private_strings)?;

    let output_directory = root.join("target/mirante4d/static-rendering-recovery");
    fs::create_dir_all(&output_directory)?;
    let output = output_directory.join(format!("{}-summary.json", raw.campaign_token));
    ensure!(
        !output.exists(),
        "sanitized campaign summary already exists and will not be overwritten"
    );
    write_json_file(&output, &sanitized)?;
    let reread = crate::reports::read_json_file(&output)?;
    ensure!(
        reread == sanitized,
        "sanitized summary changed while reread"
    );
    validate_sanitized_report(&reread, &private_strings)?;
    if !evaluation.quantitative_gate_passed {
        bail!(
            "static rendering recovery gate did not pass; sanitized evidence was written to {}",
            output.display()
        );
    }
    Ok(output)
}

fn validate_raw_campaign(raw: &RawCampaign) -> anyhow::Result<()> {
    ensure!(raw.schema == RAW_SCHEMA, "unexpected raw campaign schema");
    validate_safe_token(&raw.campaign_token, "campaign token")?;
    ensure!(
        raw.workload_path.is_absolute(),
        "raw private workload path must be absolute"
    );
    validate_sha256(&raw.workload_identity_sha256, "private workload identity")?;
    validate_sha256(&raw.evidence_overlay_sha256, "EvidenceOverlay identity")?;
    validate_host(&raw.host)?;

    let revisions = raw
        .revisions
        .iter()
        .map(|revision| (revision.label, revision))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        raw.revisions.len() == RevisionLabel::ALL.len()
            && revisions.len() == RevisionLabel::ALL.len()
            && RevisionLabel::ALL
                .iter()
                .all(|label| revisions.contains_key(label)),
        "raw campaign must bind exactly one A, B, and C revision"
    );
    for revision in raw.revisions.as_slice() {
        validate_git_oid(&revision.source_commit, "source commit")?;
        validate_git_oid(&revision.measured_commit, "measured commit")?;
        validate_git_oid(&revision.measured_tree, "measured tree")?;
        ensure!(
            revision.worktree_clean,
            "every measured worktree must be clean"
        );
        ensure!(
            revision.evidence_overlay_sha256 == raw.evidence_overlay_sha256,
            "measured revision lost the common EvidenceOverlay binding"
        );
    }
    ensure!(
        revisions[&RevisionLabel::A].source_commit == REVISION_A
            && revisions[&RevisionLabel::B].source_commit == REVISION_B,
        "raw campaign source baselines differ from the immutable A/B revisions"
    );

    ensure!(
        raw.fragments.len() == WorkloadKind::ALL.len() * SESSION_COUNT * RevisionLabel::ALL.len(),
        "raw campaign must contain exactly 27 workload/block/revision fragments"
    );
    let mut keys = BTreeSet::new();
    let mut renderer_settings = None;
    let mut overlay_noninterference = true;
    for fragment in &raw.fragments {
        ensure!(
            fragment.schema == FRAGMENT_SCHEMA,
            "unexpected fragment schema"
        );
        ensure!(
            fragment.campaign_token == raw.campaign_token
                && fragment.workload_identity_sha256 == raw.workload_identity_sha256
                && fragment.evidence_overlay_sha256 == raw.evidence_overlay_sha256,
            "fragment lost its raw campaign binding"
        );
        ensure!(
            fragment.host == raw.host,
            "fragment host facts changed within campaign"
        );
        ensure!(
            fragment.viewport == [1920, 1080],
            "every controlled workload must use the physical 1920x1080 viewport"
        );
        validate_sha256(
            &fragment.renderer_settings_sha256,
            "renderer settings identity",
        )?;
        if let Some(expected) = &renderer_settings {
            ensure!(
                expected == &fragment.renderer_settings_sha256,
                "renderer settings changed within the campaign"
            );
        } else {
            renderer_settings = Some(fragment.renderer_settings_sha256.clone());
        }
        ensure!(
            !fragment.process_arguments.is_empty()
                && fragment
                    .process_arguments
                    .iter()
                    .all(|argument| !argument.is_empty() && !argument.contains('\0')),
            "fragment must retain its nonempty process argument vector"
        );
        validate_environment_pair(&fragment.start_environment, &fragment.end_environment)?;
        ensure!(
            fragment.eligible_for_timing == fragment.ineligibility_reason.is_none(),
            "fragment eligibility must have exactly one truthful reason state"
        );
        if let Some(reason) = &fragment.ineligibility_reason {
            ensure!(
                !reason.trim().is_empty(),
                "fragment ineligibility reason cannot be empty"
            );
        }
        let revision = revisions
            .get(&fragment.revision)
            .copied()
            .context("fragment names an unknown revision")?;
        ensure!(
            fragment.source_commit == revision.source_commit
                && fragment.measured_commit == revision.measured_commit
                && fragment.measured_tree == revision.measured_tree,
            "fragment revision facts differ from the measured revision"
        );
        ensure!(
            (1..=SESSION_COUNT).contains(&fragment.block)
                && (1..=RevisionLabel::ALL.len()).contains(&fragment.order_position)
                && expected_order(fragment.block)[fragment.order_position - 1] == fragment.revision,
            "fragment does not occupy its prescribed block order position"
        );
        ensure!(
            keys.insert((fragment.data.kind(), fragment.block, fragment.revision)),
            "raw campaign contains a duplicate workload/block/revision fragment"
        );
        overlay_noninterference &= fragment.overlay_noninterference_passed;
        validate_session_data(&fragment.data, &raw.host)?;
    }
    ensure!(
        keys.len() == raw.fragments.len(),
        "raw campaign fragment matrix is incomplete"
    );
    ensure!(
        raw.overlay_noninterference_passed == overlay_noninterference,
        "raw overlay noninterference aggregate differs from its fragments"
    );
    Ok(())
}

fn validate_host(host: &HostFacts) -> anyhow::Result<()> {
    ensure!(
        [
            &host.adapter,
            &host.backend,
            &host.driver,
            &host.cpu_model,
            &host.toolchain,
            &host.power_state,
        ]
        .into_iter()
        .all(|fact| !fact.trim().is_empty()),
        "campaign host facts must be complete"
    );
    ensure!(
        host.backend.eq_ignore_ascii_case("vulkan"),
        "the controlled campaign requires the selected Vulkan backend"
    );
    Ok(())
}

fn validate_environment_pair(
    start: &EnvironmentSample,
    end: &EnvironmentSample,
) -> anyhow::Result<()> {
    ensure!(
        start.temperature_celsius_milli.is_some() == end.temperature_celsius_milli.is_some(),
        "temperature availability changed within a fragment"
    );
    ensure!(
        start.gpu_clock_mhz.is_some() == end.gpu_clock_mhz.is_some(),
        "GPU clock availability changed within a fragment"
    );
    if let (Some(start), Some(end)) = (start.gpu_clock_mhz, end.gpu_clock_mhz) {
        ensure!(start > 0 && end > 0, "reported GPU clocks must be nonzero");
    }
    Ok(())
}

fn validate_session_data(data: &SessionData, host: &HostFacts) -> anyhow::Result<()> {
    match data {
        SessionData::FixedLod(run) => validate_fixed_lod_run(run, host),
        SessionData::WarmNavigation(run) => validate_warm_navigation_run(run, host),
        SessionData::ColdSettlement(run) => validate_cold_settlement_run(run, host),
    }
}

fn expected_fixed_cases() -> BTreeSet<String> {
    ["VoxelExact", "SmoothLinear"]
        .into_iter()
        .flat_map(|sampling| {
            ["MIP", "DVR", "ISO", "Mixed"]
                .into_iter()
                .flat_map(move |kernel| {
                    [1_u32, 2, 4, 8]
                        .into_iter()
                        .map(move |channels| format!("{kernel}-{sampling}-{channels}ch"))
                })
        })
        .collect()
}

fn validate_fixed_lod_run(run: &FixedLodRun, host: &HostFacts) -> anyhow::Result<()> {
    ensure!(
        run.warmups == FIXED_WARMUPS && run.measured_samples == FIXED_SAMPLES,
        "fixed-LOD sessions require exactly five warmups and thirty samples"
    );
    let mut actual = BTreeSet::new();
    for case in &run.cases {
        ensure!(
            matches!(case.channels, 1 | 2 | 4 | 8)
                && matches!(case.kernel.as_str(), "MIP" | "DVR" | "ISO" | "Mixed")
                && matches!(case.sampling.as_str(), "VoxelExact" | "SmoothLinear"),
            "fixed-LOD case has an unsupported topology"
        );
        let expected_id = format!("{}-{}-{}ch", case.kernel, case.sampling, case.channels);
        ensure!(
            case.case_id == expected_id,
            "fixed-LOD case ID is not canonical"
        );
        ensure!(
            actual.insert(case.case_id.clone()),
            "duplicate fixed-LOD case"
        );
        validate_scientific_facts(&case.scientific)?;
        validate_expected_scientific_facts(&case.expected_scientific, true)?;
        validate_measurements(&case.measurements, host, Some(FIXED_SAMPLES))?;
        let color_submissions = usize::try_from(case.measurements.color_submissions)
            .context("fixed-LOD color submission count does not fit usize")?;
        ensure!(
            case.measurements.accepted_direct_cutoffs == FIXED_SAMPLES as u64
                && case.measurements.color_submissions >= FIXED_SAMPLES as u64
                && case.measurements.presentation_revisions.len() == color_submissions
                && case.measurements.texture_revisions.len() == color_submissions,
            "fixed-LOD case did not retain complete evidence for its thirty direct cutoffs"
        );
    }
    ensure!(
        actual == expected_fixed_cases(),
        "fixed-LOD session differs from the required 32-case matrix"
    );
    Ok(())
}

fn validate_warm_navigation_run(run: &WarmNavigationRun, host: &HostFacts) -> anyhow::Result<()> {
    ensure!(
        run.driver == "viewer-oblique-continuity"
            && run.workflow == "zoom"
            && run.duration_seconds == NAVIGATION_DURATION_SECONDS
            && run.input_hz == NAVIGATION_INPUT_HZ,
        "warm navigation does not use the pinned real-window zoom driver"
    );
    ensure!(
        run.generated_wheel_count > 0 && run.generated_wheel_count == run.received_wheel_count,
        "warm navigation lost or invented independently clocked wheel input"
    );
    ensure!(
        !run.accepted_camera_revisions.is_empty()
            && strictly_increasing(&run.accepted_camera_revisions),
        "warm navigation camera revisions must be nonempty and strictly increasing"
    );
    ensure!(
        !run.initial_camera_facts.is_empty()
            && run.initial_camera_facts == run.final_camera_facts
            && run.balanced_return,
        "warm navigation did not return to the exact initial camera facts"
    );
    ensure!(
        run.dry_run_all_bodies_resident_and_retained,
        "warm navigation did not prove its complete accepted body sequence resident"
    );
    ensure!(
        run.accepted_scientific_sequence.len() == run.accepted_camera_revisions.len()
            && run.expected_scientific_sequence.len() == run.accepted_camera_revisions.len(),
        "warm navigation scientific sequence does not cover every accepted camera revision"
    );
    for facts in &run.accepted_scientific_sequence {
        validate_scientific_facts(facts)?;
    }
    for facts in &run.expected_scientific_sequence {
        validate_expected_scientific_facts(facts, false)?;
    }
    validate_scientific_facts(&run.scientific)?;
    validate_expected_scientific_facts(&run.expected_scientific, true)?;
    validate_measurements(&run.measurements, host, None)?;
    ensure!(
        run.measurements.ui_update_ns.len()
            == usize::try_from(run.received_wheel_count).unwrap_or(usize::MAX)
            && run.measurements.semantic_demand_ns.len() == run.accepted_camera_revisions.len()
            && run.measurements.render_plan_ns.len() == run.accepted_camera_revisions.len(),
        "warm navigation timing vectors do not cover the admitted input and camera revisions"
    );
    Ok(())
}

fn validate_cold_settlement_run(run: &ColdSettlementRun, host: &HostFacts) -> anyhow::Result<()> {
    ensure!(
        run.throwaway_process_completed
            && run.fresh_process
            && run.fresh_wgpu_runtime
            && run.empty_application_caches
            && run.no_recovered_project_state,
        "cold settlement did not preserve its fresh application/GPU boundary"
    );
    ensure!(
        !run.throwaway_read_decode_identity.is_empty()
            && run.throwaway_read_decode_identity == run.measured_read_decode_identity,
        "throwaway and measured cold launches used different read/decode work"
    );
    ensure!(
        run.filesystem_cache_claim == "filesystem_warm_application_gpu_cold",
        "cold settlement made an unsupported filesystem cache claim"
    );
    ensure!(
        run.exact_settlement_wall_ns > 0,
        "cold settlement wall time must be nonzero"
    );
    validate_scientific_facts(&run.scientific)?;
    validate_expected_scientific_facts(&run.expected_scientific, true)?;
    validate_measurements(&run.measurements, host, None)
}

fn validate_scientific_facts(facts: &ScientificFacts) -> anyhow::Result<()> {
    validate_sha256(&facts.pixel_sha256, "pixel identity")?;
    validate_sha256(&facts.coverage_sha256, "coverage identity")?;
    validate_sha256(&facts.validity_sha256, "validity identity")?;
    for (label, scales) in [
        ("requested", &facts.requested_scales),
        ("selected", &facts.selected_scales),
        ("navigation", &facts.navigation_scales),
        ("displayed", &facts.displayed_scales),
    ] {
        ensure!(
            !scales.is_empty() && scales.windows(2).all(|pair| pair[0].layer < pair[1].layer),
            "{label} scale map must be nonempty and canonically ordered by unique layer"
        );
    }
    Ok(())
}

fn validate_expected_scientific_facts(
    facts: &ScientificFacts,
    exact_map: bool,
) -> anyhow::Result<()> {
    validate_scientific_facts(facts)?;
    if exact_map {
        ensure!(
            facts.requested_scales == facts.selected_scales
                && facts.selected_scales == facts.navigation_scales
                && facts.navigation_scales == facts.displayed_scales,
            "fixed/cold independent facts must name one exact per-layer scale map"
        );
    }
    ensure!(
        !exact_map || (facts.final_current && facts.final_exact),
        "independent final facts must require Exact/current publication"
    );
    Ok(())
}

fn validate_measurements(
    measurements: &RunMeasurements,
    host: &HostFacts,
    exact_samples: Option<usize>,
) -> anyhow::Result<()> {
    for (label, samples) in [
        ("UI update", &measurements.ui_update_ns),
        ("semantic demand", &measurements.semantic_demand_ns),
        ("render plan", &measurements.render_plan_ns),
    ] {
        ensure!(
            !samples.is_empty() && samples.iter().all(|sample| *sample > 0),
            "{label} timing samples must be nonempty and nonzero"
        );
        if let Some(expected) = exact_samples {
            ensure!(
                samples.len() == expected,
                "fixed-LOD {label} timing vector must contain exactly thirty samples"
            );
        }
    }
    for (label, samples) in [
        ("renderer planning", &measurements.renderer_planning_ns),
        (
            "renderer queue submit",
            &measurements.renderer_queue_submit_ns,
        ),
    ] {
        ensure!(
            !samples.is_empty() && samples.iter().all(|sample| *sample > 0),
            "{label} timing samples must be nonempty and nonzero"
        );
    }
    ensure!(
        measurements.semantic_demand_ns.len() == measurements.render_plan_ns.len(),
        "semantic-demand and render-plan samples must be paired"
    );
    let color_submissions = usize::try_from(measurements.color_submissions)
        .context("color submission count does not fit usize")?;
    ensure!(
        measurements.accepted_direct_cutoffs > 0,
        "measurements must cover at least one accepted direct cutoff"
    );
    ensure!(
        measurements.renderer_planning_ns.len() == color_submissions
            && measurements.renderer_queue_submit_ns.len() == color_submissions
            && measurements.target_order.len() == color_submissions
            && measurements
                .target_order
                .iter()
                .all(|target| target == "ThreeD"),
        "renderer timing and target order must cover every static ThreeD color submission"
    );
    if host.gpu_timestamps_supported {
        ensure!(
            measurements.gpu_color_ns.len() == color_submissions
                && measurements.gpu_color_ns.iter().all(|sample| *sample > 0),
            "supported GPU timestamps must cover every color submission"
        );
    } else {
        ensure!(
            measurements.gpu_color_ns.is_empty(),
            "unsupported GPU timestamps cannot publish substitute samples"
        );
    }
    ensure!(
        measurements.counters.renderer_queue_submissions >= measurements.color_submissions,
        "renderer queue submission counter is below its color submissions"
    );
    ensure!(
        strictly_increasing(&measurements.presentation_revisions)
            && strictly_increasing(&measurements.texture_revisions),
        "presentation and texture revisions must be strictly increasing"
    );
    ensure!(
        measurements
            .repaint_reasons
            .iter()
            .all(|(reason, count)| !reason.trim().is_empty() && *count > 0),
        "repaint reason counts must use nonempty keys and nonzero values"
    );
    let repaint_total = measurements
        .repaint_reasons
        .values()
        .try_fold(0_u64, |total, count| total.checked_add(*count))
        .context("repaint reason count overflow")?;
    ensure!(
        repaint_total == measurements.counters.repaint_requests,
        "repaint reason counts must reconcile with the repaint-request counter"
    );
    let counters = &measurements.counters;
    ensure!(
        counters.payload_uploads > 0 || counters.payload_upload_bytes == 0,
        "payload upload bytes require at least one upload"
    );
    ensure!(
        counters.cancellation_waste_reads > 0 || counters.cancellation_waste_bytes == 0,
        "cancellation-waste bytes require at least one wasted read"
    );
    ensure!(
        counters.hidden_timeouts <= counters.hidden_jobs,
        "hidden timeouts cannot exceed hidden jobs"
    );
    Ok(())
}

fn strictly_increasing(values: &[u64]) -> bool {
    !values.is_empty() && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn evaluate_campaign(raw: &RawCampaign) -> anyhow::Result<Evaluation> {
    let matching_scientific_work = RevisionLabel::ALL
        .into_iter()
        .filter(|label| *label != RevisionLabel::A)
        .all(|label| {
            (1..=SESSION_COUNT).all(|block| {
                WorkloadKind::ALL.into_iter().all(|workload| {
                    let candidate = campaign_fragment(raw, RevisionLabel::C, workload, block);
                    let comparison = campaign_fragment(raw, label, workload, block);
                    session_is_correct(&candidate.data)
                        && session_is_correct(&comparison.data)
                        && sessions_match(&candidate.data, &comparison.data)
                })
            })
        });

    let mut fixed_lod = Vec::with_capacity(expected_fixed_cases().len());
    for case_id in expected_fixed_cases() {
        let candidate_blocks = fixed_case_block_p95(raw, RevisionLabel::C, &case_id);
        let candidate_median = median_three_option(&candidate_blocks);
        let mut baselines = Vec::new();
        for label in [RevisionLabel::A, RevisionLabel::B] {
            let blocks = fixed_case_block_p95(raw, label, &case_id);
            if fixed_case_revision_is_admissible(raw, label, &case_id)
                && let Some(median) = median_three_option(&blocks)
            {
                baselines.push((label, blocks, median));
            }
        }
        let baseline = baselines.into_iter().min_by_key(|(label, _, median)| {
            (*median, if *label == RevisionLabel::A { 0 } else { 1 })
        });
        let candidate_admissible =
            fixed_case_revision_is_admissible(raw, RevisionLabel::C, &case_id);
        let evaluated = candidate_admissible && candidate_median.is_some() && baseline.is_some();
        let passed = evaluated
            && candidate_median
                .zip(baseline.as_ref().map(|(_, _, median)| *median))
                .is_some_and(|(candidate, baseline)| {
                    u128::from(candidate) * 100 <= u128::from(baseline) * 105
                });
        let baseline_median = baseline.as_ref().map(|(_, _, median)| *median);
        fixed_lod.push(CaseSummary {
            case_id,
            baseline_revision: baseline.as_ref().map(|(label, _, _)| *label),
            baseline_block_p95_ns: baseline.as_ref().map(|(_, blocks, _)| blocks.clone()),
            baseline_median_p95_ns: baseline_median,
            candidate_block_p95_ns: candidate_blocks,
            candidate_median_p95_ns: candidate_median,
            ratio: candidate_median
                .zip(baseline_median)
                .map(|(candidate, baseline)| candidate as f64 / baseline as f64),
            evaluated,
            passed,
        });
    }

    let warm_navigation_ui_p95 = scalar_summary(
        raw,
        WorkloadKind::WarmNavigation,
        |fragment| match &fragment.data {
            SessionData::WarmNavigation(run) => nearest_rank(&run.measurements.ui_update_ns, 95),
            _ => unreachable!("fragment lookup preserves workload kind"),
        },
        warm_revision_is_admissible,
    )?;
    let cold_exact_settlement = scalar_summary(
        raw,
        WorkloadKind::ColdSettlement,
        |fragment| match &fragment.data {
            SessionData::ColdSettlement(run) => Some(run.exact_settlement_wall_ns),
            _ => unreachable!("fragment lookup preserves workload kind"),
        },
        cold_revision_is_admissible,
    )?;

    let candidate_fragments = raw
        .fragments
        .iter()
        .filter(|fragment| fragment.revision == RevisionLabel::C)
        .collect::<Vec<_>>();
    let candidate_measurements = candidate_fragments
        .iter()
        .flat_map(|fragment| session_measurements(&fragment.data))
        .collect::<Vec<_>>();
    let warm_zero_io_decode_request_upload_eviction = candidate_fragments
        .iter()
        .filter(|fragment| fragment.data.kind() == WorkloadKind::WarmNavigation)
        .all(|fragment| warm_boundary_is_zero(measurements(&fragment.data)));
    let settled_idle_zero_work = candidate_measurements.iter().all(|measurements| {
        measurements.settled_idle_renderer_submissions == 0
            && measurements.settled_idle_immediate_repaints == 0
    });
    let one_color_submission_per_direct_cutoff =
        candidate_measurements.iter().all(|measurements| {
            measurements.color_submissions <= measurements.accepted_direct_cutoffs
                && measurements.counters.hidden_batches <= measurements.declared_hidden_batches
        });
    let no_renderer_validation_or_attempt_failure = candidate_measurements
        .iter()
        .all(|measurements| execution_is_clean(measurements));

    let all_fixed_passed = fixed_lod
        .iter()
        .all(|summary| summary.evaluated && summary.passed);
    let quantitative_gate_passed = raw.overlay_noninterference_passed
        && matching_scientific_work
        && all_fixed_passed
        && warm_navigation_ui_p95.evaluated
        && warm_navigation_ui_p95.passed
        && cold_exact_settlement.evaluated
        && cold_exact_settlement.passed
        && warm_zero_io_decode_request_upload_eviction
        && settled_idle_zero_work
        && one_color_submission_per_direct_cutoff
        && no_renderer_validation_or_attempt_failure;
    let first_attribution_boundary = first_attribution_boundary(
        raw,
        &fixed_lod,
        &warm_navigation_ui_p95,
        &cold_exact_settlement,
        matching_scientific_work,
        warm_zero_io_decode_request_upload_eviction,
        settled_idle_zero_work,
        one_color_submission_per_direct_cutoff,
        no_renderer_validation_or_attempt_failure,
        quantitative_gate_passed,
    );

    Ok(Evaluation {
        overlay_noninterference_passed: raw.overlay_noninterference_passed,
        workload_identity_matched: true,
        matching_scientific_work,
        fixed_lod,
        warm_navigation_ui_p95,
        cold_exact_settlement,
        warm_zero_io_decode_request_upload_eviction,
        settled_idle_zero_work,
        one_color_submission_per_direct_cutoff,
        no_renderer_validation_or_attempt_failure,
        first_attribution_boundary,
        quantitative_gate_passed,
    })
}

fn campaign_fragment(
    raw: &RawCampaign,
    revision: RevisionLabel,
    workload: WorkloadKind,
    block: usize,
) -> &SessionFragment {
    raw.fragments
        .iter()
        .find(|fragment| {
            fragment.revision == revision
                && fragment.data.kind() == workload
                && fragment.block == block
        })
        .expect("validated campaigns contain the complete fragment matrix")
}

fn fixed_case<'a>(fragment: &'a SessionFragment, case_id: &str) -> &'a FixedLodCase {
    match &fragment.data {
        SessionData::FixedLod(run) => run
            .cases
            .iter()
            .find(|case| case.case_id == case_id)
            .expect("validated fixed-LOD sessions contain the complete case matrix"),
        _ => unreachable!("fixed-case lookup uses a fixed-LOD fragment"),
    }
}

fn measurements(data: &SessionData) -> &RunMeasurements {
    match data {
        SessionData::FixedLod(_) => {
            unreachable!("fixed-LOD measurements are case-specific")
        }
        SessionData::WarmNavigation(run) => &run.measurements,
        SessionData::ColdSettlement(run) => &run.measurements,
    }
}

fn session_measurements(data: &SessionData) -> Vec<&RunMeasurements> {
    match data {
        SessionData::FixedLod(run) => run.cases.iter().map(|case| &case.measurements).collect(),
        SessionData::WarmNavigation(run) => vec![&run.measurements],
        SessionData::ColdSettlement(run) => vec![&run.measurements],
    }
}

fn session_is_correct(data: &SessionData) -> bool {
    match data {
        SessionData::FixedLod(run) => run.cases.iter().all(|case| {
            case.scientific == case.expected_scientific
                && case.scientific.final_current
                && case.scientific.final_exact
        }),
        SessionData::WarmNavigation(run) => {
            run.accepted_scientific_sequence == run.expected_scientific_sequence
                && run.scientific == run.expected_scientific
                && run.scientific.final_current
                && run.scientific.final_exact
        }
        SessionData::ColdSettlement(run) => {
            run.scientific == run.expected_scientific
                && run.scientific.final_current
                && run.scientific.final_exact
        }
    }
}

fn sessions_match(left: &SessionData, right: &SessionData) -> bool {
    match (left, right) {
        (SessionData::FixedLod(left), SessionData::FixedLod(right)) => {
            left.cases.iter().all(|left_case| {
                right
                    .cases
                    .iter()
                    .find(|right_case| right_case.case_id == left_case.case_id)
                    .is_some_and(|right_case| {
                        left_case.scientific == right_case.scientific
                            && left_case.expected_scientific == right_case.expected_scientific
                    })
            })
        }
        (SessionData::WarmNavigation(left), SessionData::WarmNavigation(right)) => {
            left.generated_wheel_count == right.generated_wheel_count
                && left.received_wheel_count == right.received_wheel_count
                && left.accepted_camera_revisions == right.accepted_camera_revisions
                && left.initial_camera_facts == right.initial_camera_facts
                && left.final_camera_facts == right.final_camera_facts
                && left.accepted_scientific_sequence == right.accepted_scientific_sequence
                && left.expected_scientific_sequence == right.expected_scientific_sequence
                && left.scientific == right.scientific
                && left.expected_scientific == right.expected_scientific
        }
        (SessionData::ColdSettlement(left), SessionData::ColdSettlement(right)) => {
            left.throwaway_read_decode_identity == right.throwaway_read_decode_identity
                && left.measured_read_decode_identity == right.measured_read_decode_identity
                && left.scientific == right.scientific
                && left.expected_scientific == right.expected_scientific
        }
        _ => false,
    }
}

fn fixed_case_block_p95(raw: &RawCampaign, revision: RevisionLabel, case_id: &str) -> Vec<u64> {
    if !raw.host.gpu_timestamps_supported {
        return Vec::new();
    }
    (1..=SESSION_COUNT)
        .map(|block| {
            let fragment = campaign_fragment(raw, revision, WorkloadKind::FixedLod, block);
            nearest_rank(&fixed_case(fragment, case_id).measurements.gpu_color_ns, 95)
                .expect("validated supported timestamps are nonempty")
        })
        .collect()
}

fn fixed_case_revision_is_admissible(
    raw: &RawCampaign,
    revision: RevisionLabel,
    case_id: &str,
) -> bool {
    raw.host.gpu_timestamps_supported
        && (1..=SESSION_COUNT).all(|block| {
            let fragment = campaign_fragment(raw, revision, WorkloadKind::FixedLod, block);
            let candidate = campaign_fragment(raw, RevisionLabel::C, WorkloadKind::FixedLod, block);
            let case = fixed_case(fragment, case_id);
            let candidate_case = fixed_case(candidate, case_id);
            fragment.eligible_for_timing
                && case.scientific == case.expected_scientific
                && case.scientific.final_current
                && case.scientific.final_exact
                && (revision == RevisionLabel::C
                    || (case.scientific == candidate_case.scientific
                        && case.expected_scientific == candidate_case.expected_scientific))
                && fixed_measurement_is_admissible(&case.measurements)
        })
}

fn fixed_measurement_is_admissible(measurements: &RunMeasurements) -> bool {
    let counters = &measurements.counters;
    measurements.color_submissions == FIXED_SAMPLES as u64
        && measurements.accepted_direct_cutoffs == FIXED_SAMPLES as u64
        && counters.physical_reads == 0
        && counters.codec_decodes == 0
        && counters.dataset_requests == 0
        && counters.payload_uploads == 0
        && counters.payload_upload_bytes == 0
        && counters.evictions == 0
        && execution_is_clean(measurements)
        && scheduling_is_clean(measurements)
}

fn warm_revision_is_admissible(raw: &RawCampaign, revision: RevisionLabel) -> bool {
    (1..=SESSION_COUNT).all(|block| {
        let fragment = campaign_fragment(raw, revision, WorkloadKind::WarmNavigation, block);
        let candidate =
            campaign_fragment(raw, RevisionLabel::C, WorkloadKind::WarmNavigation, block);
        fragment.eligible_for_timing
            && session_is_correct(&fragment.data)
            && (revision == RevisionLabel::C || sessions_match(&fragment.data, &candidate.data))
            && warm_boundary_is_zero(measurements(&fragment.data))
            && execution_is_clean(measurements(&fragment.data))
            && scheduling_is_clean(measurements(&fragment.data))
    })
}

fn cold_revision_is_admissible(raw: &RawCampaign, revision: RevisionLabel) -> bool {
    (1..=SESSION_COUNT).all(|block| {
        let fragment = campaign_fragment(raw, revision, WorkloadKind::ColdSettlement, block);
        let candidate =
            campaign_fragment(raw, RevisionLabel::C, WorkloadKind::ColdSettlement, block);
        fragment.eligible_for_timing
            && session_is_correct(&fragment.data)
            && (revision == RevisionLabel::C || sessions_match(&fragment.data, &candidate.data))
            && execution_is_clean(measurements(&fragment.data))
            && scheduling_is_clean(measurements(&fragment.data))
    })
}

fn warm_boundary_is_zero(measurements: &RunMeasurements) -> bool {
    let counters = &measurements.counters;
    counters.physical_reads == 0
        && counters.codec_decodes == 0
        && counters.dataset_requests == 0
        && counters.cancellation_waste_reads == 0
        && counters.cancellation_waste_bytes == 0
        && counters.payload_uploads == 0
        && counters.payload_upload_bytes == 0
        && counters.evictions == 0
        && counters.allocator_plans == 0
        && counters.body_rebuilds == 0
}

fn execution_is_clean(measurements: &RunMeasurements) -> bool {
    let counters = &measurements.counters;
    counters.renderer_errors == 0
        && counters.validation_errors == 0
        && counters.attempt_failures == 0
        && counters.hidden_timeouts == 0
}

fn scheduling_is_clean(measurements: &RunMeasurements) -> bool {
    measurements.color_submissions <= measurements.accepted_direct_cutoffs
        && measurements.counters.hidden_batches <= measurements.declared_hidden_batches
        && measurements.settled_idle_renderer_submissions == 0
        && measurements.settled_idle_immediate_repaints == 0
}

fn scalar_summary<F, V>(
    raw: &RawCampaign,
    workload: WorkloadKind,
    value: F,
    revision_is_admissible: V,
) -> anyhow::Result<ScalarSummary>
where
    F: Fn(&SessionFragment) -> Option<u64>,
    V: Fn(&RawCampaign, RevisionLabel) -> bool,
{
    let values_for = |revision| {
        (1..=SESSION_COUNT)
            .map(|block| value(campaign_fragment(raw, revision, workload, block)))
            .collect::<Option<Vec<_>>>()
    };
    let candidate_blocks = values_for(RevisionLabel::C)
        .context("candidate scalar workload lacks a complete timing vector")?;
    let candidate_median = median_three(&candidate_blocks)?;
    let mut baselines = Vec::new();
    for label in [RevisionLabel::A, RevisionLabel::B] {
        if revision_is_admissible(raw, label)
            && let Some(blocks) = values_for(label)
        {
            baselines.push((label, median_three(&blocks)?, blocks));
        }
    }
    let baseline = baselines
        .into_iter()
        .min_by_key(|(label, median, _)| (*median, if *label == RevisionLabel::A { 0 } else { 1 }));
    let candidate_admissible = revision_is_admissible(raw, RevisionLabel::C);
    let baseline_median = baseline.as_ref().map(|(_, median, _)| *median);
    let threshold = baseline_median.map(scalar_threshold);
    let evaluated = candidate_admissible && baseline.is_some();
    let passed = evaluated && threshold.is_some_and(|threshold| candidate_median <= threshold);
    Ok(ScalarSummary {
        baseline_revision: baseline.as_ref().map(|(label, _, _)| *label),
        baseline_block_values_ns: baseline.as_ref().map(|(_, _, blocks)| blocks.clone()),
        baseline_median_ns: baseline_median,
        candidate_block_values_ns: candidate_blocks,
        candidate_median_ns: candidate_median,
        threshold_ns: threshold,
        ratio: baseline_median.map(|baseline| candidate_median as f64 / baseline as f64),
        evaluated,
        passed,
    })
}

fn scalar_threshold(baseline: u64) -> u64 {
    let ratio = (u128::from(baseline) * 110 / 100).min(u128::from(u64::MAX)) as u64;
    ratio.max(baseline.saturating_add(ONE_MILLISECOND_NS))
}

fn nearest_rank(values: &[u64], percentile: usize) -> Option<u64> {
    if values.is_empty() || !(1..=100).contains(&percentile) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

fn median_three(values: &[u64]) -> anyhow::Result<u64> {
    ensure!(
        values.len() == SESSION_COUNT,
        "protocol median requires three runs"
    );
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Ok(sorted[1])
}

fn median_three_option(values: &[u64]) -> Option<u64> {
    (values.len() == SESSION_COUNT).then(|| {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        sorted[1]
    })
}

#[allow(clippy::too_many_arguments)]
fn first_attribution_boundary(
    raw: &RawCampaign,
    fixed_lod: &[CaseSummary],
    warm: &ScalarSummary,
    cold: &ScalarSummary,
    matching_scientific_work: bool,
    warm_zero: bool,
    idle_zero: bool,
    scheduling_clean: bool,
    execution_clean: bool,
    gate_passed: bool,
) -> String {
    if gate_passed {
        return "none".to_owned();
    }
    if !raw.overlay_noninterference_passed {
        return "evidence_overlay_noninterference_failure".to_owned();
    }
    if !matching_scientific_work {
        return "matching_work_or_independent_correctness_failure".to_owned();
    }
    if let Some(failed) = fixed_lod
        .iter()
        .find(|summary| summary.evaluated && !summary.passed)
    {
        return format!("shared_or_kernel_renderer:{}", failed.case_id);
    }
    if fixed_lod.iter().any(|summary| !summary.evaluated) {
        return "unevaluated_fixed_lod_gpu_timestamps_or_baseline".to_owned();
    }
    if !warm_zero && candidate_exceeds_b_reuse_work(raw) {
        return "reuse_residency_currentness".to_owned();
    }
    if !idle_zero || !scheduling_clean {
        return "attempt_presentation_scheduling".to_owned();
    }
    if !execution_clean {
        return "renderer_or_validation_failure_requires_correction".to_owned();
    }
    if let Some(stage) = planner_regression_boundary(raw) {
        return stage.to_owned();
    }
    if (!warm.passed || !cold.passed) && ui_handoff_exceeds_baseline(raw, warm, cold) {
        return "native_presentation_ui_handoff".to_owned();
    }
    "unlocalized_no_optimization_authorized".to_owned()
}

fn candidate_exceeds_b_reuse_work(raw: &RawCampaign) -> bool {
    (1..=SESSION_COUNT).any(|block| {
        let candidate = measurements(
            &campaign_fragment(raw, RevisionLabel::C, WorkloadKind::WarmNavigation, block).data,
        );
        let baseline = measurements(
            &campaign_fragment(raw, RevisionLabel::B, WorkloadKind::WarmNavigation, block).data,
        );
        let candidate = &candidate.counters;
        let baseline = &baseline.counters;
        candidate.physical_reads > baseline.physical_reads
            || candidate.codec_decodes > baseline.codec_decodes
            || candidate.dataset_requests > baseline.dataset_requests
            || candidate.payload_uploads > baseline.payload_uploads
            || candidate.evictions > baseline.evictions
            || candidate.allocator_plans > baseline.allocator_plans
            || candidate.body_rebuilds > baseline.body_rebuilds
    })
}

fn planner_regression_boundary(raw: &RawCampaign) -> Option<&'static str> {
    let values = |revision: RevisionLabel, field: fn(&RunMeasurements) -> Vec<u64>| {
        (1..=SESSION_COUNT)
            .map(|block| {
                let measurements = measurements(
                    &campaign_fragment(raw, revision, WorkloadKind::WarmNavigation, block).data,
                );
                nearest_rank(&field(measurements), 95).expect("validated planner samples exist")
            })
            .collect::<Vec<_>>()
    };
    let combined = |measurements: &RunMeasurements| {
        measurements
            .semantic_demand_ns
            .iter()
            .zip(&measurements.render_plan_ns)
            .map(|(semantic, render)| semantic.saturating_add(*render))
            .collect::<Vec<_>>()
    };
    let semantic = |measurements: &RunMeasurements| measurements.semantic_demand_ns.clone();
    let render = |measurements: &RunMeasurements| measurements.render_plan_ns.clone();
    let candidate_combined = median_three_option(&values(RevisionLabel::C, combined))?;
    let mut baselines = [RevisionLabel::A, RevisionLabel::B]
        .into_iter()
        .filter(|label| warm_revision_is_admissible(raw, *label))
        .map(|label| {
            (
                label,
                median_three_option(&values(label, combined)).expect("three planner blocks exist"),
            )
        })
        .collect::<Vec<_>>();
    baselines
        .sort_by_key(|(label, value)| (*value, if *label == RevisionLabel::A { 0 } else { 1 }));
    let (baseline_label, baseline_combined) = *baselines.first()?;
    if !exceeds_planner_threshold(candidate_combined, baseline_combined) {
        return None;
    }
    let candidate_semantic = median_three_option(&values(RevisionLabel::C, semantic))?;
    let baseline_semantic = median_three_option(&values(baseline_label, semantic))?;
    if exceeds_planner_threshold(candidate_semantic, baseline_semantic) {
        return Some("semantic_demand_planner");
    }
    let candidate_render = median_three_option(&values(RevisionLabel::C, render))?;
    let baseline_render = median_three_option(&values(baseline_label, render))?;
    if exceeds_planner_threshold(candidate_render, baseline_render) {
        return Some("render_plan_planner");
    }
    Some("semantic_demand_plus_render_plan_planner")
}

fn exceeds_planner_threshold(candidate: u64, baseline: u64) -> bool {
    u128::from(candidate) * 100 > u128::from(baseline) * 110
        && candidate > baseline.saturating_add(500_000)
}

fn ui_handoff_exceeds_baseline(
    raw: &RawCampaign,
    warm: &ScalarSummary,
    cold: &ScalarSummary,
) -> bool {
    let labels = [warm.baseline_revision, cold.baseline_revision]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    labels.into_iter().any(|baseline| {
        [WorkloadKind::WarmNavigation, WorkloadKind::ColdSettlement]
            .into_iter()
            .any(|workload| {
                let totals = |revision| {
                    (1..=SESSION_COUNT)
                        .map(|block| {
                            let measurements = measurements(
                                &campaign_fragment(raw, revision, workload, block).data,
                            );
                            measurements
                                .egui_texture_registrations
                                .saturating_add(measurements.egui_paint_commands)
                        })
                        .sum::<u64>()
                };
                totals(RevisionLabel::C) > totals(baseline)
            })
    })
}

fn sanitized_report(raw: &RawCampaign, evaluation: &Evaluation) -> Value {
    let revisions = raw
        .revisions
        .iter()
        .map(|revision| {
            json!({
                "label": revision.label,
                "source_commit": revision.source_commit,
                "measured_commit": revision.measured_commit,
                "measured_tree": revision.measured_tree,
                "worktree_clean": revision.worktree_clean,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema": SANITIZED_SCHEMA,
        "campaign_token": raw.campaign_token,
        "bindings": {
            "workload_identity_matched": evaluation.workload_identity_matched,
            "evidence_overlay_sha256": raw.evidence_overlay_sha256,
            "overlay_noninterference_passed": raw.overlay_noninterference_passed,
        },
        "protocol": {
            "workloads": ["fixed_lod", "warm_navigation", "cold_settlement"],
            "blocks": 3,
            "orders": [["A", "B", "C"], ["B", "C", "A"], ["C", "A", "B"]],
            "fixed_warmups": FIXED_WARMUPS,
            "fixed_samples": FIXED_SAMPLES,
            "fixed_viewport": [1920, 1080],
            "warm_navigation_seconds": NAVIGATION_DURATION_SECONDS,
            "warm_navigation_input_hz": NAVIGATION_INPUT_HZ,
            "nearest_rank_no_interpolation": true,
            "revision_aggregate": "median_of_three_run_level_values",
        },
        "revisions": revisions,
        "evaluation": evaluation,
    })
}

fn private_strings(raw: &RawCampaign, raw_report_path: &Path) -> Vec<String> {
    fn push_scientific(strings: &mut Vec<String>, facts: &ScientificFacts) {
        strings.push(facts.pixel_sha256.clone());
        strings.push(facts.coverage_sha256.clone());
        strings.push(facts.validity_sha256.clone());
    }
    let mut strings = vec![
        raw.workload_path.display().to_string(),
        raw.workload_identity_sha256.clone(),
        raw_report_path.display().to_string(),
        raw.host.adapter.clone(),
        raw.host.driver.clone(),
        raw.host.cpu_model.clone(),
        raw.host.toolchain.clone(),
        raw.host.power_state.clone(),
    ];
    for fragment in &raw.fragments {
        strings.push(fragment.renderer_settings_sha256.clone());
        strings.extend(fragment.process_arguments.iter().cloned());
        match &fragment.data {
            SessionData::FixedLod(run) => {
                for case in &run.cases {
                    push_scientific(&mut strings, &case.scientific);
                    push_scientific(&mut strings, &case.expected_scientific);
                }
            }
            SessionData::WarmNavigation(run) => {
                strings.push(run.initial_camera_facts.clone());
                strings.push(run.final_camera_facts.clone());
                for facts in run
                    .accepted_scientific_sequence
                    .iter()
                    .chain(&run.expected_scientific_sequence)
                {
                    push_scientific(&mut strings, facts);
                }
                push_scientific(&mut strings, &run.scientific);
                push_scientific(&mut strings, &run.expected_scientific);
            }
            SessionData::ColdSettlement(run) => {
                strings.push(run.throwaway_read_decode_identity.clone());
                strings.push(run.measured_read_decode_identity.clone());
                push_scientific(&mut strings, &run.scientific);
                push_scientific(&mut strings, &run.expected_scientific);
            }
        }
    }
    strings
}

fn validate_sanitized_report(report: &Value, private_strings: &[String]) -> anyhow::Result<()> {
    ensure!(
        report.pointer("/schema").and_then(Value::as_str) == Some(SANITIZED_SCHEMA)
            && report
                .pointer("/bindings/workload_identity_matched")
                .and_then(Value::as_bool)
                == Some(true),
        "sanitized summary lost its schema or workload-equivalence binding"
    );
    fn visit(value: &Value, private_strings: &[String]) -> anyhow::Result<()> {
        match value {
            Value::String(string) => {
                let contains_private = private_strings
                    .iter()
                    .filter(|private| !private.is_empty())
                    .any(|private| string.contains(private));
                ensure!(
                    !Path::new(string).is_absolute()
                        && !string.starts_with("m4d-sc-v1-sha256:")
                        && !string.starts_with("m4d-package-v1-sha256:")
                        && !contains_private,
                    "sanitized summary retained a private path, identity, or scientific fact"
                );
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, private_strings)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    ensure!(
                        !matches!(
                            key.as_str(),
                            "workload_path"
                                | "workload_identity_sha256"
                                | "renderer_settings_sha256"
                                | "pixel_sha256"
                                | "coverage_sha256"
                                | "validity_sha256"
                                | "requested_scales"
                                | "selected_scales"
                                | "navigation_scales"
                                | "displayed_scales"
                                | "camera_facts"
                                | "read_decode_identity"
                                | "process_arguments"
                        ),
                        "sanitized summary retained a private field"
                    );
                    visit(value, private_strings)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }
    visit(report, private_strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scientific() -> ScientificFacts {
        let scales = vec![LayerScaleFact { layer: 7, scale: 0 }];
        ScientificFacts {
            pixel_sha256: "1".repeat(64),
            coverage_sha256: "2".repeat(64),
            validity_sha256: "3".repeat(64),
            requested_scales: scales.clone(),
            selected_scales: scales.clone(),
            navigation_scales: scales.clone(),
            displayed_scales: scales,
            final_current: true,
            final_exact: true,
        }
    }

    fn measurements(samples: usize, base_ns: u64) -> RunMeasurements {
        let values = (0..samples)
            .map(|index| base_ns + u64::try_from(index).unwrap())
            .collect::<Vec<_>>();
        let revisions = (1..=u64::try_from(samples).unwrap()).collect::<Vec<_>>();
        RunMeasurements {
            ui_update_ns: values.clone(),
            semantic_demand_ns: values.clone(),
            render_plan_ns: values.clone(),
            renderer_planning_ns: values.clone(),
            renderer_queue_submit_ns: values.clone(),
            gpu_color_ns: values,
            egui_texture_registrations: u64::try_from(samples).unwrap(),
            egui_paint_commands: u64::try_from(samples).unwrap(),
            accepted_direct_cutoffs: u64::try_from(samples).unwrap(),
            color_submissions: u64::try_from(samples).unwrap(),
            declared_hidden_batches: 0,
            settled_idle_renderer_submissions: 0,
            settled_idle_immediate_repaints: 0,
            presentation_revisions: revisions.clone(),
            texture_revisions: revisions,
            target_order: vec!["ThreeD".to_owned(); samples],
            repaint_reasons: BTreeMap::new(),
            counters: BoundaryCounters {
                renderer_queue_submissions: u64::try_from(samples).unwrap(),
                ..BoundaryCounters::default()
            },
        }
    }

    fn fixed_run(base_ns: u64) -> FixedLodRun {
        let cases = expected_fixed_cases()
            .into_iter()
            .map(|case_id| {
                let mut parts = case_id.split('-');
                let kernel = parts.next().unwrap().to_owned();
                let sampling = parts.next().unwrap().to_owned();
                let channels = parts
                    .next()
                    .unwrap()
                    .trim_end_matches("ch")
                    .parse()
                    .unwrap();
                FixedLodCase {
                    case_id,
                    channels,
                    kernel,
                    sampling,
                    scientific: scientific(),
                    expected_scientific: scientific(),
                    measurements: measurements(FIXED_SAMPLES, base_ns),
                }
            })
            .collect();
        FixedLodRun {
            warmups: FIXED_WARMUPS,
            measured_samples: FIXED_SAMPLES,
            cases,
        }
    }

    fn warm_run(base_ns: u64) -> WarmNavigationRun {
        let sequence = vec![scientific(), scientific(), scientific()];
        WarmNavigationRun {
            driver: "viewer-oblique-continuity".to_owned(),
            workflow: "zoom".to_owned(),
            duration_seconds: NAVIGATION_DURATION_SECONDS,
            input_hz: NAVIGATION_INPUT_HZ,
            generated_wheel_count: 3,
            received_wheel_count: 3,
            accepted_camera_revisions: vec![1, 2, 3],
            initial_camera_facts: "private-camera-facts".to_owned(),
            final_camera_facts: "private-camera-facts".to_owned(),
            balanced_return: true,
            dry_run_all_bodies_resident_and_retained: true,
            accepted_scientific_sequence: sequence.clone(),
            expected_scientific_sequence: sequence,
            scientific: scientific(),
            expected_scientific: scientific(),
            measurements: measurements(3, base_ns),
        }
    }

    fn cold_run(base_ns: u64) -> ColdSettlementRun {
        ColdSettlementRun {
            throwaway_process_completed: true,
            throwaway_read_decode_identity: "private-read-decode-identity".to_owned(),
            measured_read_decode_identity: "private-read-decode-identity".to_owned(),
            fresh_process: true,
            fresh_wgpu_runtime: true,
            empty_application_caches: true,
            no_recovered_project_state: true,
            filesystem_cache_claim: "filesystem_warm_application_gpu_cold".to_owned(),
            exact_settlement_wall_ns: base_ns,
            scientific: scientific(),
            expected_scientific: scientific(),
            measurements: measurements(1, base_ns),
        }
    }

    fn revision(label: RevisionLabel) -> MeasuredRevision {
        let source_commit = match label {
            RevisionLabel::A => REVISION_A.to_owned(),
            RevisionLabel::B => REVISION_B.to_owned(),
            RevisionLabel::C => "c".repeat(40),
        };
        MeasuredRevision {
            label,
            source_commit: source_commit.clone(),
            measured_commit: source_commit,
            measured_tree: match label {
                RevisionLabel::A => "a".repeat(40),
                RevisionLabel::B => "b".repeat(40),
                RevisionLabel::C => "d".repeat(40),
            },
            worktree_clean: true,
            evidence_overlay_sha256: "e".repeat(64),
        }
    }

    fn raw_campaign() -> RawCampaign {
        let host = HostFacts {
            adapter: "private adapter".to_owned(),
            backend: "Vulkan".to_owned(),
            driver: "private driver".to_owned(),
            cpu_model: "private CPU".to_owned(),
            toolchain: "private toolchain".to_owned(),
            power_state: "private fixed power".to_owned(),
            gpu_timestamps_supported: true,
        };
        let mut fragments = Vec::new();
        for workload in WorkloadKind::ALL {
            for block in 1..=SESSION_COUNT {
                for (position, label) in expected_order(block).into_iter().enumerate() {
                    let base_ns = match (workload, label) {
                        (WorkloadKind::FixedLod, RevisionLabel::A) => 100,
                        (WorkloadKind::FixedLod, RevisionLabel::B) => 110,
                        (WorkloadKind::FixedLod, RevisionLabel::C) => 104,
                        (WorkloadKind::WarmNavigation, RevisionLabel::A) => 1_000_000,
                        (WorkloadKind::WarmNavigation, RevisionLabel::B) => 1_100_000,
                        (WorkloadKind::WarmNavigation, RevisionLabel::C) => 1_050_000,
                        (WorkloadKind::ColdSettlement, RevisionLabel::A) => 10_000_000,
                        (WorkloadKind::ColdSettlement, RevisionLabel::B) => 11_000_000,
                        (WorkloadKind::ColdSettlement, RevisionLabel::C) => 10_500_000,
                    };
                    let revision = revision(label);
                    let data = match workload {
                        WorkloadKind::FixedLod => SessionData::FixedLod(fixed_run(base_ns)),
                        WorkloadKind::WarmNavigation => {
                            SessionData::WarmNavigation(warm_run(base_ns))
                        }
                        WorkloadKind::ColdSettlement => {
                            SessionData::ColdSettlement(cold_run(base_ns))
                        }
                    };
                    fragments.push(SessionFragment {
                        schema: FRAGMENT_SCHEMA.to_owned(),
                        campaign_token: "campaign-token".to_owned(),
                        workload_identity_sha256: "f".repeat(64),
                        evidence_overlay_sha256: "e".repeat(64),
                        overlay_noninterference_passed: true,
                        host: host.clone(),
                        process_arguments: vec!["private-collector-argument".to_owned()],
                        viewport: [1920, 1080],
                        renderer_settings_sha256: "4".repeat(64),
                        start_environment: EnvironmentSample {
                            temperature_celsius_milli: Some(50_000),
                            gpu_clock_mhz: Some(1_800),
                        },
                        end_environment: EnvironmentSample {
                            temperature_celsius_milli: Some(51_000),
                            gpu_clock_mhz: Some(1_800),
                        },
                        revision: label,
                        source_commit: revision.source_commit,
                        measured_commit: revision.measured_commit,
                        measured_tree: revision.measured_tree,
                        block,
                        order_position: position + 1,
                        eligible_for_timing: true,
                        ineligibility_reason: None,
                        data,
                    });
                }
            }
        }
        RawCampaign {
            schema: RAW_SCHEMA.to_owned(),
            campaign_token: "campaign-token".to_owned(),
            workload_path: PathBuf::from("/private/dataset.m4d"),
            workload_identity_sha256: "f".repeat(64),
            evidence_overlay_sha256: "e".repeat(64),
            overlay_noninterference_passed: true,
            host,
            revisions: RevisionLabel::ALL.into_iter().map(revision).collect(),
            fragments,
        }
    }

    fn candidate_warm_mut(raw: &mut RawCampaign) -> impl Iterator<Item = &mut WarmNavigationRun> {
        raw.fragments
            .iter_mut()
            .filter_map(|fragment| {
                (fragment.revision == RevisionLabel::C).then_some(&mut fragment.data)
            })
            .filter_map(|data| match data {
                SessionData::WarmNavigation(run) => Some(run),
                _ => None,
            })
    }

    #[test]
    fn representative_static_rendering_recovery() {
        let raw = raw_campaign();
        validate_raw_campaign(&raw).unwrap();
        let evaluation = evaluate_campaign(&raw).unwrap();
        assert!(evaluation.quantitative_gate_passed);
        assert!(evaluation.fixed_lod.iter().all(|case| case.passed));
        assert_eq!(nearest_rank(&[4, 1, 3, 2, 5], 95), Some(5));
        let sanitized = sanitized_report(&raw, &evaluation);
        let private = private_strings(&raw, Path::new("/private/raw.json"));
        validate_sanitized_report(&sanitized, &private).unwrap();
        let serialized = serde_json::to_string(&sanitized).unwrap();
        for secret in [
            "/private/dataset.m4d",
            &"f".repeat(64),
            "private-camera-facts",
            "private-read-decode-identity",
            "private adapter",
        ] {
            assert!(!serialized.contains(secret));
        }

        let mut bad_order = raw.clone();
        bad_order.fragments[0].order_position = 2;
        assert!(validate_raw_campaign(&bad_order).is_err());

        let mut mismatched_work = raw.clone();
        let candidate = mismatched_work
            .fragments
            .iter_mut()
            .find(|fragment| {
                fragment.revision == RevisionLabel::C
                    && fragment.data.kind() == WorkloadKind::FixedLod
            })
            .unwrap();
        if let SessionData::FixedLod(run) = &mut candidate.data {
            run.cases[0].scientific.pixel_sha256 = "9".repeat(64);
        }
        validate_raw_campaign(&mismatched_work).unwrap();
        let evaluation = evaluate_campaign(&mismatched_work).unwrap();
        assert!(!evaluation.matching_scientific_work);
        assert!(!evaluation.quantitative_gate_passed);

        let mut reuse_failure = raw.clone();
        for run in candidate_warm_mut(&mut reuse_failure) {
            run.measurements.counters.payload_uploads = 1;
            run.measurements.counters.payload_upload_bytes = 4096;
        }
        let evaluation = evaluate_campaign(&reuse_failure).unwrap();
        assert!(!evaluation.warm_zero_io_decode_request_upload_eviction);
        assert_eq!(
            evaluation.first_attribution_boundary,
            "reuse_residency_currentness"
        );

        let mut fixed_regression = raw.clone();
        for fragment in &mut fixed_regression.fragments {
            if fragment.revision == RevisionLabel::C
                && let SessionData::FixedLod(run) = &mut fragment.data
            {
                for case in &mut run.cases {
                    case.measurements.gpu_color_ns.fill(200);
                }
            }
        }
        let evaluation = evaluate_campaign(&fixed_regression).unwrap();
        assert!(evaluation.fixed_lod.iter().all(|case| !case.passed));
        assert!(
            evaluation
                .first_attribution_boundary
                .starts_with("shared_or_kernel_renderer:")
        );

        let mut navigation_regression = raw.clone();
        for run in candidate_warm_mut(&mut navigation_regression) {
            run.measurements.ui_update_ns.fill(2_000_003);
        }
        let evaluation = evaluate_campaign(&navigation_regression).unwrap();
        assert!(!evaluation.warm_navigation_ui_p95.passed);
        assert!(!evaluation.quantitative_gate_passed);

        let mut cold_regression = raw;
        for fragment in &mut cold_regression.fragments {
            if fragment.revision == RevisionLabel::C
                && let SessionData::ColdSettlement(run) = &mut fragment.data
            {
                run.exact_settlement_wall_ns = 11_000_001;
            }
        }
        let evaluation = evaluate_campaign(&cold_regression).unwrap();
        assert!(!evaluation.cold_exact_settlement.passed);
        assert!(!evaluation.quantitative_gate_passed);
    }
}
