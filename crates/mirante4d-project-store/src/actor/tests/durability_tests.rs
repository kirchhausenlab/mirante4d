use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use super::*;

const RESULT_PREFIX: &str = "mirante4d-project-store-vm-result:";
const SOURCE_STORE: &str = "source.m4dproj";
const DESTINATION_STORE: &str = "destination.m4dproj";
const SELECTED_FILE: &str = ".mirante4d-vm-selected-generation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VmScenario {
    SaveAs,
    ManualSave,
    Autosave,
    Pin,
    Unpin,
    StagingCleanup,
    Trash,
    Purge,
    Performance,
}

#[test]
#[ignore = "runs only inside the trusted rootless WP-10B VM harness"]
fn project_store_vm_guest_driver() {
    let role = required_env("MIRANTE4D_PROJECT_STORE_VM_ROLE");
    let case = required_env("MIRANTE4D_PROJECT_STORE_VM_CASE");
    let transition = env::var("MIRANTE4D_PROJECT_STORE_VM_TRANSITION").unwrap_or_default();
    let lane = env::var("MIRANTE4D_PROJECT_STORE_VM_LANE").unwrap_or_else(|_| "none".to_owned());
    let root_a = PathBuf::from(
        env::var_os("MIRANTE4D_PROJECT_STORE_VM_ROOT_A").unwrap_or_else(|| "/mnt/project-a".into()),
    );
    let root_b = PathBuf::from(
        env::var_os("MIRANTE4D_PROJECT_STORE_VM_ROOT_B").unwrap_or_else(|| "/mnt/project-b".into()),
    );
    let scenario = scenario(&case, &transition, &lane);

    match role.as_str() {
        "exercise" | "trace" => {
            let result = run_operation(scenario, &root_a, &root_b);
            assert!(
                result.is_ok(),
                "VM exercise failed before its target: {result:?}"
            );
            assert_eq!(
                role, "trace",
                "exercise returned without reaching its target"
            );
            emit_result(&case, "trace", serde_json::json!({}));
        }
        "validate" => {
            let counters = if scenario == VmScenario::Performance {
                performance_evidence(&root_a)
            } else {
                validate_and_retry(scenario, &root_a, &root_b);
                serde_json::json!({
                    "exact_retry_attempts": 1,
                    "power_loss_simulated": true
                })
            };
            emit_result(&case, "validate", counters);
        }
        other => panic!("unknown project-store VM role {other:?}"),
    }
}

fn scenario(case: &str, transition: &str, lane: &str) -> VmScenario {
    if case == "performance" {
        return VmScenario::Performance;
    }
    if transition.starts_with("recovery_") {
        return match lane {
            "manual" => VmScenario::ManualSave,
            "autosave" => VmScenario::Autosave,
            _ => panic!("a recovery transition requires a lane"),
        };
    }
    if transition.starts_with("head_") {
        return match lane {
            "manual" => VmScenario::ManualSave,
            "autosave" => VmScenario::Autosave,
            _ => panic!("a head transition requires a lane"),
        };
    }
    if transition.starts_with("pin_") {
        return VmScenario::Pin;
    }
    if transition.starts_with("unpin_") {
        return VmScenario::Unpin;
    }
    if transition.starts_with("staging_cleanup_") {
        return VmScenario::StagingCleanup;
    }
    if transition.starts_with("gc_") {
        return VmScenario::Trash;
    }
    if transition.starts_with("purge_") {
        return VmScenario::Purge;
    }
    VmScenario::SaveAs
}

fn run_operation(
    scenario: VmScenario,
    root_a: &Path,
    root_b: &Path,
) -> Result<(), ProjectStoreFault> {
    match scenario {
        VmScenario::SaveAs => run_save_as(root_a, root_b, false),
        VmScenario::ManualSave => run_manual_save(root_a, false),
        VmScenario::Autosave => run_autosave(root_a, false),
        VmScenario::Pin => run_pin(root_a, false),
        VmScenario::Unpin => run_unpin(root_a, false),
        VmScenario::StagingCleanup => run_staging_cleanup(root_a, false),
        VmScenario::Trash => run_trash(root_a, false),
        VmScenario::Purge => run_purge(root_a, false),
        VmScenario::Performance => panic!("performance has no transition exercise"),
    }
}

fn validate_and_retry(scenario: VmScenario, root_a: &Path, root_b: &Path) {
    let result = match scenario {
        VmScenario::SaveAs => run_save_as(root_a, root_b, true),
        VmScenario::ManualSave => run_manual_save(root_a, true),
        VmScenario::Autosave => run_autosave(root_a, true),
        VmScenario::Pin => run_pin(root_a, true),
        VmScenario::Unpin => run_unpin(root_a, true),
        VmScenario::StagingCleanup => run_staging_cleanup(root_a, true),
        VmScenario::Trash => run_trash(root_a, true),
        VmScenario::Purge => run_purge(root_a, true),
        VmScenario::Performance => unreachable!(),
    };
    result.unwrap_or_else(|fault| panic!("fresh VM validation/retry failed: {fault:?}"));
}

struct StoreObjectSource {
    descriptor: RawObjectDescriptor,
    path: PathBuf,
}

impl ProjectObjectSource for StoreObjectSource {
    fn descriptor(&self) -> &RawObjectDescriptor {
        &self.descriptor
    }

    fn open(&self) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        fs::File::open(&self.path).map(|file| Box::new(file) as Box<dyn std::io::Read + Send>)
    }
}

fn store_backed_save_as_capture(
    source: &Path,
    target: &GenerationDocument,
) -> ProjectCommitCapture {
    let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
    for artifact in target.projection().state().artifacts() {
        let storage = target.bindings().get(&artifact.object().digest()).unwrap();
        let ArtifactStorage::Direct { object } = storage else {
            panic!("the cross-device Save As fixture must remain directly stored");
        };
        assert_eq!(object.digest(), artifact.object().digest());
        assert_eq!(object.byte_length(), artifact.object().byte_length());
        let digest = object.digest().digest().to_string();
        let path = source
            .join("objects")
            .join("sha256")
            .join(&digest[..2])
            .join(&digest[2..]);
        assert!(
            path.is_file(),
            "Save As source object is absent from root A"
        );
        sources.push(Box::new(StoreObjectSource {
            descriptor: artifact.object().clone(),
            path,
        }));
    }
    ProjectCommitCapture::new(
        target.projection().clone(),
        None,
        None,
        target.forked_from(),
        sources,
    )
    .unwrap()
}

fn run_save_as(root_a: &Path, root_b: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    let destination = root_b.join(DESTINATION_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    let source_before = file_tree(&source);
    let destination_was_visible = destination.exists();

    let (actor, _) = open_actor(&source)?;
    let target = frozen_generation_in("divergent.m4dproj", DIVERGENT_INITIAL);
    let source_generation = generation_id(RECOVERABLE_G2);
    let capture = store_backed_save_as_capture(&source, &target);
    actor.try_submit(ProjectStoreCommand::SaveAs {
        request_id: request_id(2),
        destination: ProjectStorePath::new(destination.clone()).unwrap(),
        source_generation,
        capture,
    })?;
    let result = match public_recv_timeout(&actor) {
        ProjectStoreCompletion::SavedAs { result, .. } => result,
        other => panic!("unexpected VM Save As completion: {other:?}"),
    };
    close_actor(actor, 3)?;
    match result {
        Ok(_) => {}
        Err(ProjectStoreFault::DestinationExists) if validating && destination_was_visible => {}
        Err(fault) => return Err(fault),
    }
    verify_expected_authority(
        &destination,
        Some(generation_id(DIVERGENT_INITIAL)),
        None,
        target.projection(),
    )?;
    assert_eq!(file_tree(&source), source_before);
    Ok(())
}

fn run_manual_save(root_a: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    let orphan = frozen_generation_in("recoverable.m4dproj", RECOVERABLE_ORPHAN);
    let (actor, session, opened) = open_actor_with_projection(&source)?;
    let already_committed = validating
        && session.current_manual_generation() == Some(generation_id(RECOVERABLE_ORPHAN))
        && &opened == orphan.projection();
    let (capture, _) = controlled_fixture_capture(
        "recoverable.m4dproj",
        &orphan,
        orphan.projection().clone(),
        orphan.parent_generation_id(),
        orphan.base_manual_generation_id(),
        orphan.forked_from(),
        ControlledRead::Normal,
    );
    actor.try_submit(ProjectStoreCommand::ManualSave {
        request_id: request_id(2),
        capture,
    })?;
    let result = match public_recv_timeout(&actor) {
        ProjectStoreCompletion::ManualSaved { result, .. } => result,
        other => panic!("unexpected VM Manual Save completion: {other:?}"),
    };
    match result {
        Ok(_) => {}
        Err(ProjectStoreFault::StaleParent) if already_committed => {}
        Err(fault) => return Err(fault),
    }
    verify_and_close(actor, 3)
}

fn run_autosave(root_a: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "stale.m4dproj");
    let frozen = frozen_generation_in("stale.m4dproj", STALE_AUTOSAVE);
    let target_projection = next_revision_projection(&frozen);
    let (actor, session, _opened) = open_actor_with_projection(&source)?;
    let committed_autosave_matches = session
        .current_autosave_generation()
        .filter(|current| *current != generation_id(STALE_AUTOSAVE))
        .map(|current| {
            let root = LocalStoreRoot::open(&source).unwrap();
            let bytes = root
                .read_generation_bytes(
                    current,
                    ProjectStoreLimits::default().generation_bytes_max,
                    || false,
                )
                .unwrap();
            GenerationDocument::decode(
                current,
                target_projection.state().project_id(),
                &bytes,
                ProjectStoreLimits::default(),
            )
            .unwrap()
            .projection()
                == &target_projection
        });
    if validating && committed_autosave_matches.is_some() {
        assert_eq!(
            committed_autosave_matches,
            Some(true),
            "the committed autosave retry target must match its stored projection"
        );
    }
    let already_committed = validating && committed_autosave_matches == Some(true);
    let (capture, _) = controlled_fixture_capture(
        "stale.m4dproj",
        &frozen,
        target_projection,
        Some(generation_id(STALE_AUTOSAVE)),
        Some(generation_id(STALE_MANUAL)),
        frozen.forked_from(),
        ControlledRead::Normal,
    );
    actor.try_submit(ProjectStoreCommand::Autosave {
        request_id: request_id(2),
        destination: None,
        capture,
    })?;
    let result = match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Autosaved { result, .. } => result,
        other => panic!("unexpected VM Autosave completion: {other:?}"),
    };
    match result {
        Ok(_) => {}
        Err(ProjectStoreFault::StaleParent) if already_committed => {}
        Err(fault) => return Err(fault),
    }
    verify_and_close(actor, 3)
}

fn run_pin(root_a: &Path, _validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    let (actor, _) = open_actor(&source)?;
    actor.try_submit(ProjectStoreCommand::Pin {
        request_id: request_id(2),
        checkpoint_id: "vm-checkpoint".to_owned(),
        generation_id: generation_id(RECOVERABLE_ORPHAN),
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Pinned { result, .. } => result?,
        other => panic!("unexpected VM Pin completion: {other:?}"),
    }
    verify_and_close(actor, 3)
}

fn run_unpin(root_a: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    let (actor, _) = open_actor(&source)?;
    if !validating {
        actor.try_submit(ProjectStoreCommand::Pin {
            request_id: request_id(2),
            checkpoint_id: "vm-checkpoint".to_owned(),
            generation_id: generation_id(RECOVERABLE_ORPHAN),
        })?;
        match public_recv_timeout(&actor) {
            ProjectStoreCompletion::Pinned { result, .. } => result?,
            other => panic!("unexpected VM setup Pin completion: {other:?}"),
        }
        sync_host();
    }
    actor.try_submit(ProjectStoreCommand::Unpin {
        request_id: request_id(3),
        checkpoint_id: "vm-checkpoint".to_owned(),
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Unpinned { result, .. } => result?,
        other => panic!("unexpected VM Unpin completion: {other:?}"),
    }
    verify_and_close(actor, 4)
}

fn run_staging_cleanup(root_a: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    if !validating {
        let transaction = source
            .join("staging")
            .join("tx-1-00000000000000000000000000000000-0000000000000000-00");
        fs::create_dir_all(&transaction).unwrap();
        fs::write(transaction.join("payload"), b"").unwrap();
        sync_host();
    }
    let (actor, _) = open_actor(&source)?;
    verify_and_close(actor, 2)
}

fn run_trash(root_a: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    let selected = if validating {
        let selected = fs::read_to_string(root_a.join(SELECTED_FILE)).unwrap();
        let selected = selected.lines().map(generation_id).collect::<Vec<_>>();
        assert_eq!(
            selected.len(),
            2,
            "Trash retry requires both selected orphans"
        );
        selected
    } else {
        let selected = crate::trash::tests::install_two_safe_orphans(&source);
        let collision = selected[1];
        let active_generation = crate::trash::tests::active_generation_file(&source, collision);
        let trash_generation = crate::trash::tests::trash_generation_file(&source, collision);
        fs::create_dir_all(trash_generation.parent().unwrap()).unwrap();
        fs::copy(active_generation, trash_generation).unwrap();
        fs::write(
            root_a.join(SELECTED_FILE),
            format!("{}\n{}\n", selected[0], selected[1]),
        )
        .unwrap();
        sync_host();
        selected.to_vec()
    };
    let (actor, _) = open_actor(&source)?;
    actor.try_submit(ProjectStoreCommand::Trash {
        request_id: request_id(2),
        generations: selected,
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Trashed { result, .. } => {
            result?;
        }
        other => panic!("unexpected VM Trash completion: {other:?}"),
    }
    verify_and_close(actor, 3)
}

fn run_purge(root_a: &Path, validating: bool) -> Result<(), ProjectStoreFault> {
    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "recoverable.m4dproj");
    if !validating {
        let (selected, _) = crate::trash::tests::install_unique_regenerable_orphan(&source);
        let root = LocalStoreRoot::open(&source).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        crate::trash::trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )?;
        drop(leases);
        sync_host();
    }
    let (actor, _) = open_actor(&source)?;
    actor.try_submit(ProjectStoreCommand::Purge {
        request_id: request_id(2),
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Purged { result, .. } => {
            result?;
        }
        other => panic!("unexpected VM Purge completion: {other:?}"),
    }
    verify_and_close(actor, 3)
}

fn performance_evidence(root_a: &Path) -> serde_json::Value {
    let actor = ProjectStoreActor::start(Default::default()).unwrap();
    let mut enqueue = Vec::with_capacity(1_000);
    let mut poll = Vec::with_capacity(1_000);
    for id in 1..=1_000_u64 {
        let started = Instant::now();
        actor
            .try_submit(ProjectStoreCommand::PlanCompaction {
                request_id: request_id(id),
            })
            .unwrap();
        enqueue.push(nanos(started.elapsed()));
        loop {
            let started = Instant::now();
            let completion = actor.try_recv();
            poll.push(nanos(started.elapsed()));
            if completion.is_some() {
                break;
            }
            thread::yield_now();
        }
    }
    close_actor(actor, 1_001).unwrap();

    let source = root_a.join(SOURCE_STORE);
    ensure_fixture_store(&source, "stale.m4dproj");
    let (actor, _) = open_actor(&source).unwrap();
    let frozen = frozen_generation_in("stale.m4dproj", STALE_MANUAL);
    let (capture, _) = controlled_fixture_capture(
        "stale.m4dproj",
        &frozen,
        next_revision_projection(&frozen),
        Some(generation_id(STALE_MANUAL)),
        None,
        frozen.forked_from(),
        ControlledRead::Normal,
    );
    actor
        .try_submit(ProjectStoreCommand::ManualSave {
            request_id: request_id(2),
            capture,
        })
        .unwrap();
    let unchanged_bytes = match public_recv_timeout(&actor) {
        ProjectStoreCompletion::ManualSaved {
            result: Ok(receipt),
            ..
        } => receipt.published_bytes(),
        other => panic!("unexpected performance Manual Save completion: {other:?}"),
    };
    let rss = process_rss_bytes();
    verify_and_close(actor, 3).unwrap();

    serde_json::json!({
        "enqueue_samples": enqueue.len(),
        "enqueue_p99_nanoseconds": p99(&mut enqueue),
        "poll_samples": poll.len(),
        "poll_p99_nanoseconds": p99(&mut poll),
        "unchanged_artifact_bytes_rewritten": unchanged_bytes,
        "post_open_or_save_metadata_rss_bytes": rss,
        "exact_retry_attempts": 0,
        "power_loss_simulated": false
    })
}

fn open_actor(path: &Path) -> Result<(ProjectStoreActor, ProjectStoreSession), ProjectStoreFault> {
    open_actor_with_projection(path).map(|(actor, session, _)| (actor, session))
}

fn open_actor_with_projection(
    path: &Path,
) -> Result<
    (
        ProjectStoreActor,
        ProjectStoreSession,
        ProjectGenerationProjection,
    ),
    ProjectStoreFault,
> {
    let actor = ProjectStoreActor::start(Default::default())?;
    actor.try_submit(ProjectStoreCommand::Open {
        request_id: request_id(1),
        path: ProjectStorePath::new(path.to_path_buf()).unwrap(),
        mode: ProjectOpenMode::PreferWritable,
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Opened {
            result: Ok((session, projection)),
            ..
        } => Ok((actor, session, projection)),
        ProjectStoreCompletion::Opened {
            result: Err(fault), ..
        } => Err(fault),
        other => panic!("unexpected VM Open completion: {other:?}"),
    }
}

fn verify_expected_authority(
    path: &Path,
    manual: Option<ProjectGenerationId>,
    autosave: Option<ProjectGenerationId>,
    projection: &ProjectGenerationProjection,
) -> Result<(), ProjectStoreFault> {
    let (actor, session, opened) = open_actor_with_projection(path)?;
    assert_eq!(session.current_manual_generation(), manual);
    assert_eq!(session.current_autosave_generation(), autosave);
    assert_eq!(&opened, projection);
    verify_and_close(actor, 9_100)
}

fn verify_and_close(actor: ProjectStoreActor, id: u64) -> Result<(), ProjectStoreFault> {
    actor.try_submit(ProjectStoreCommand::FullVerify {
        request_id: request_id(id),
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Verified { result, .. } => {
            result?;
        }
        other => panic!("unexpected VM Full Verify completion: {other:?}"),
    }
    close_actor(actor, id + 1)
}

fn close_actor(actor: ProjectStoreActor, id: u64) -> Result<(), ProjectStoreFault> {
    actor.try_submit(ProjectStoreCommand::Close {
        request_id: request_id(id),
    })?;
    match public_recv_timeout(&actor) {
        ProjectStoreCompletion::Closed { result, .. } => result?,
        other => panic!("unexpected VM Close completion: {other:?}"),
    }
    actor.join().expect("project-store VM actor joins cleanly");
    Ok(())
}

fn ensure_fixture_store(destination: &Path, store: &str) {
    if destination.exists() {
        return;
    }
    fs::create_dir_all(destination).unwrap();
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(fixture_archive())
        .arg("-C")
        .arg(destination)
        .arg("--strip-components=1")
        .arg(store)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to install VM fixture {store}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    sync_host();
}

fn fixture_archive() -> PathBuf {
    env::var_os("MIRANTE4D_PROJECT_STORE_VM_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(super::fixture_archive)
}

fn sync_host() {
    let status = Command::new("sync").status().unwrap();
    assert!(status.success(), "guest sync failed");
}

fn process_rss_bytes() -> u64 {
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("VmRSS is available");
    kib.checked_mul(1_024).unwrap()
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn p99(samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    let index = samples
        .len()
        .saturating_mul(99)
        .div_ceil(100)
        .saturating_sub(1);
    samples[index]
}

fn emit_result(case: &str, role: &str, counters: serde_json::Value) {
    println!(
        "{RESULT_PREFIX}{}",
        serde_json::json!({
            "schema": "mirante4d-wp10b-vm-guest-result",
            "schema_version": 1,
            "role": role,
            "case": case,
            "status": "passed",
            "counters": counters
        })
    );
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("required VM environment {name} is missing"))
}
