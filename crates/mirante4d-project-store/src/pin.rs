//! Durable checkpoint-pin transactions for the private project-store actor.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use crate::{
    ProjectGenerationId, ProjectStoreFault, ProjectStoreLimits,
    inspection::inspect_store_graph,
    lease::{GcTransition, GcTransitionOccurrence, LeaseError, ProjectStoreLeases},
    local::{LocalPublicationError, LocalStoreRoot, PinTransition, PinTransitionObserver},
    wire::{RefKind, RefRecord},
};

struct LeasePinTransitionObserver<'a>(&'a ProjectStoreLeases);

impl PinTransitionObserver for LeasePinTransitionObserver<'_> {
    type Occurrence = GcTransitionOccurrence;

    fn before(&self, transition: PinTransition) -> Result<Self::Occurrence, ()> {
        self.0
            .gc_transition_before(gc_transition(transition))
            .map_err(|_| ())
    }

    fn after(&self, occurrence: Self::Occurrence) -> Result<(), ()> {
        self.0.gc_transition_after(occurrence).map_err(|_| ())
    }
}

const fn gc_transition(transition: PinTransition) -> GcTransition {
    match transition {
        PinTransition::StageCreate => GcTransition::PinStageCreate,
        PinTransition::Write => GcTransition::PinWrite,
        PinTransition::FileSync => GcTransition::PinFileSync,
        PinTransition::Replace => GcTransition::PinReplace,
        PinTransition::DirectorySync => GcTransition::PinDirectorySync,
        PinTransition::UnpinRemove => GcTransition::UnpinRemove,
        PinTransition::UnpinDirectorySync => GcTransition::UnpinDirectorySync,
    }
}

pub(crate) fn publish_pin<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    checkpoint_id: &str,
    generation_id: ProjectGenerationId,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    require_writer(root, leases)?;
    let graph = inspect_store_graph(root, limits, &mut is_cancelled)?;
    if !graph.generation_ids().contains(&generation_id) {
        return Err(ProjectStoreFault::Corruption {
            stage: "pin_generation",
        });
    }
    let next = RefRecord::new(
        RefKind::Pin,
        graph.state().project_id(),
        generation_id,
        None,
        None,
    )
    .map_err(|_| ProjectStoreFault::Corruption { stage: "pin_ref" })?;
    let expected = root
        .read_pin_ref(checkpoint_id, limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "pin_ref"))?;
    let observer = LeasePinTransitionObserver(leases);
    if expected == Some(next) {
        return map_pin_publication_result(
            leases,
            root.sync_pin_recovery(
                checkpoint_id,
                Some(next),
                PinTransition::DirectorySync,
                limits,
                &observer,
                &mut is_cancelled,
            ),
            "pin_ref",
        );
    }
    if expected.is_none() && graph.pin_count() >= limits.pin_refs_max {
        return Err(ProjectStoreFault::Capacity { stage: "pin_refs" });
    }
    if graph.prospective_orphan_count_after_pin_change(
        expected.map(RefRecord::current),
        Some(generation_id),
    ) > limits.recovery_candidates_max
    {
        return Err(ProjectStoreFault::Capacity {
            stage: "recovery_candidates",
        });
    }
    root.validate_pin_inventory(limits, expected.is_none(), &mut is_cancelled)
        .map_err(|error| map_local_error(error, "pin_inventory"))?;
    require_writer(root, leases)?;
    map_pin_publication_result(
        leases,
        root.replace_pin(
            checkpoint_id,
            expected,
            next,
            limits,
            &observer,
            &mut is_cancelled,
        ),
        "pin_ref",
    )
}

pub(crate) fn remove_pin<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    checkpoint_id: &str,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    require_writer(root, leases)?;
    let graph = inspect_store_graph(root, limits, &mut is_cancelled)?;
    let Some(expected) = root
        .read_pin_ref(checkpoint_id, limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "pin_ref"))?
    else {
        let observer = LeasePinTransitionObserver(leases);
        return map_pin_publication_result(
            leases,
            root.sync_pin_recovery(
                checkpoint_id,
                None,
                PinTransition::UnpinDirectorySync,
                limits,
                &observer,
                &mut is_cancelled,
            ),
            "pin_ref",
        );
    };
    if graph.prospective_orphan_count_after_pin_change(Some(expected.current()), None)
        > limits.recovery_candidates_max
    {
        return Err(ProjectStoreFault::Capacity {
            stage: "recovery_candidates",
        });
    }
    root.validate_store_inventory(limits, 0, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "pin_inventory"))?;
    require_writer(root, leases)?;
    let observer = LeasePinTransitionObserver(leases);
    map_pin_publication_result(
        leases,
        root.remove_pin(
            checkpoint_id,
            expected,
            limits,
            &observer,
            &mut is_cancelled,
        ),
        "pin_ref",
    )
}

fn map_pin_publication_result(
    leases: &ProjectStoreLeases,
    result: Result<(), LocalPublicationError>,
    stage: &'static str,
) -> Result<(), ProjectStoreFault> {
    match result {
        Err(LocalPublicationError::RefCommitIndeterminate) => {
            leases.suspend_writes();
            Err(ProjectStoreFault::CommitIndeterminate)
        }
        result => result.map_err(|error| map_local_error(error, stage)),
    }
}

fn require_writer(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
) -> Result<(), ProjectStoreFault> {
    match leases.confirm_writer(root) {
        Ok(true) => Ok(()),
        Ok(false) => Err(ProjectStoreFault::ReadOnly),
        Err(LeaseError::Indeterminate) => Err(ProjectStoreFault::CommitIndeterminate),
        Err(LeaseError::InvalidAnchor | LeaseError::Io { .. }) => {
            Err(ProjectStoreFault::Corruption { stage: "pin_lease" })
        }
    }
}

fn map_local_error(error: LocalPublicationError, stage: &'static str) -> ProjectStoreFault {
    match error {
        LocalPublicationError::Cancelled => ProjectStoreFault::Cancelled,
        LocalPublicationError::Capacity { .. } | LocalPublicationError::StorageFull { .. } => {
            ProjectStoreFault::Capacity { stage }
        }
        LocalPublicationError::ReadOnly { .. } => ProjectStoreFault::ReadOnly,
        LocalPublicationError::RefChanged | LocalPublicationError::RefAlreadyPresent => {
            ProjectStoreFault::Corruption { stage }
        }
        LocalPublicationError::RefCommitIndeterminate
        | LocalPublicationError::PackageCommitIndeterminate => {
            ProjectStoreFault::CommitIndeterminate
        }
        LocalPublicationError::AtomicPublishUnsupported => ProjectStoreFault::UnsupportedFilesystem,
        LocalPublicationError::SourceLength { .. } => ProjectStoreFault::SourceChanged,
        LocalPublicationError::SourceIo { .. } => ProjectStoreFault::SourceChanged,
        LocalPublicationError::SourceDigest => ProjectStoreFault::DigestMismatch,
        LocalPublicationError::InvalidPath
        | LocalPublicationError::ExistingMismatch
        | LocalPublicationError::InvalidGeneration
        | LocalPublicationError::InvalidControl
        | LocalPublicationError::DestinationExists
        | LocalPublicationError::Io { .. } => ProjectStoreFault::Corruption { stage },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        ProjectOpenMode,
        generation::GenerationDocument,
        lease::{GcTransitionInjector, GcTransitionTarget, TransitionEdge},
    };

    const MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
    );
    const ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "cfd67414728bb345edb7d5eabffac2530f04ed3b768d720782efe88e2d7ca370"
    );
    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestProject(PathBuf);

    impl TestProject {
        fn extracted(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "mirante4d-project-pin-{label}-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let status = Command::new("tar")
                .args(["-xzf"])
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../fixtures/project/project-store-v1.tar.gz"),
                )
                .args(["-C"])
                .arg(&root)
                .arg("recoverable.m4dproj")
                .status()
                .unwrap();
            assert!(status.success());
            Self(root.join("recoverable.m4dproj"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn pin_create_replace_and_unpin_change_only_the_named_root() {
        let project = TestProject::extracted("lifecycle");
        fs::remove_file(project.path().join("refs/pins/checkpoint-a")).unwrap();
        fs::remove_dir(project.path().join("refs/pins")).unwrap();
        let head = fs::read(project.path().join("refs/head")).unwrap();
        let unchanged = file_bytes_except_pins(project.path());
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let limits = ProjectStoreLimits::default();
        let orphan = ProjectGenerationId::parse(ORPHAN).unwrap();
        let manual = ProjectGenerationId::parse(MANUAL).unwrap();

        publish_pin(&root, &leases, "review_1", orphan, limits, || false).unwrap();
        let created = root
            .read_pin_ref("review_1", limits, || false)
            .unwrap()
            .unwrap();
        assert_eq!(created.current(), orphan);
        assert!(
            inspect_store_graph(&root, limits, || false)
                .unwrap()
                .orphan_generation_ids()
                .is_empty()
        );
        publish_pin(&root, &leases, "review_2", orphan, limits, || false).unwrap();
        remove_pin(&root, &leases, "review_1", limits, || false).unwrap();
        assert!(
            inspect_store_graph(&root, limits, || false)
                .unwrap()
                .orphan_generation_ids()
                .is_empty()
        );
        publish_pin(&root, &leases, "review_1", orphan, limits, || false).unwrap();
        remove_pin(&root, &leases, "review_2", limits, || false).unwrap();

        publish_pin(&root, &leases, "review_1", manual, limits, || false).unwrap();
        assert_eq!(
            root.read_pin_ref("review_1", limits, || false)
                .unwrap()
                .unwrap()
                .current(),
            manual
        );
        assert_eq!(
            inspect_store_graph(&root, limits, || false)
                .unwrap()
                .orphan_generation_ids(),
            [orphan]
        );

        remove_pin(&root, &leases, "review_1", limits, || false).unwrap();
        remove_pin(&root, &leases, "review_1", limits, || false).unwrap();
        assert!(
            root.read_pin_ref("review_1", limits, || false)
                .unwrap()
                .is_none()
        );
        assert!(project.path().join("refs/pins").is_dir());
        assert_eq!(fs::read(project.path().join("refs/head")).unwrap(), head);
        assert_eq!(file_bytes_except_pins(project.path()), unchanged);
    }

    #[test]
    fn pin_rejects_invalid_capacity_read_only_and_cancelled_work_without_mutation() {
        let project = TestProject::extracted("rejections");
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let orphan = ProjectGenerationId::parse(ORPHAN).unwrap();
        let limits = ProjectStoreLimits::default();

        for checkpoint_id in ["", "Upper", ".hidden", "bad/name", &"a".repeat(65)] {
            assert!(matches!(
                publish_pin(&root, &leases, checkpoint_id, orphan, limits, || false,),
                Err(ProjectStoreFault::Corruption { stage: "pin_ref" })
            ));
        }
        assert!(matches!(
            publish_pin(
                &root,
                &leases,
                "missing",
                ProjectGenerationId::from_digest(mirante4d_identity::Sha256Digest::from_bytes(
                    [0x77; 32]
                )),
                limits,
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "pin_generation"
            })
        ));
        assert!(matches!(
            publish_pin(
                &root,
                &leases,
                "capacity",
                orphan,
                ProjectStoreLimits {
                    pin_refs_max: 1,
                    ..limits
                },
                || false,
            ),
            Err(ProjectStoreFault::Capacity { stage: "pin_refs" })
        ));
        assert!(matches!(
            publish_pin(&root, &leases, "cancelled", orphan, limits, || true),
            Err(ProjectStoreFault::Cancelled)
        ));

        let second_root = LocalStoreRoot::open(project.path()).unwrap();
        let read_only =
            ProjectStoreLeases::acquire(&second_root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(read_only.effective_mode(), ProjectOpenMode::ReadOnly);
        assert!(matches!(
            publish_pin(
                &second_root,
                &read_only,
                "read-only",
                orphan,
                limits,
                || false,
            ),
            Err(ProjectStoreFault::ReadOnly)
        ));
        assert!(!project.path().join("refs/pins/cancelled").exists());
        assert!(!project.path().join("refs/pins/read-only").exists());
    }

    #[test]
    fn pin_and_unpin_sync_failure_is_indeterminate_and_suspends_writes() {
        let project = TestProject::extracted("pin-indeterminate");
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let orphan = ProjectGenerationId::parse(ORPHAN).unwrap();
        let limits = ProjectStoreLimits::default();
        root.fail_ref_commit_directory_sync_at(0);
        assert!(matches!(
            publish_pin(&root, &leases, "uncertain", orphan, limits, || false),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));
        assert_eq!(root.ref_commit_directory_sync_attempts(), 2);
        assert!(
            root.read_pin_ref("uncertain", limits, || false)
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            remove_pin(&root, &leases, "uncertain", limits, || false),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));

        let unpin_project = TestProject::extracted("unpin-indeterminate");
        let unpin_root = LocalStoreRoot::open(unpin_project.path()).unwrap();
        let unpin_leases =
            ProjectStoreLeases::acquire(&unpin_root, ProjectOpenMode::PreferWritable).unwrap();
        unpin_root.fail_ref_commit_directory_sync_at(0);
        assert!(matches!(
            remove_pin(&unpin_root, &unpin_leases, "checkpoint-a", limits, || false,),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));
        assert!(
            unpin_root
                .read_pin_ref("checkpoint-a", limits, || false)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            publish_pin(&unpin_root, &unpin_leases, "after", orphan, limits, || {
                false
            },),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));
    }

    #[test]
    fn pin_and_unpin_transition_failures_and_retries_are_exact() {
        let limits = ProjectStoreLimits::default();
        let target = ProjectGenerationId::parse(ORPHAN).unwrap();
        assert_eq!(
            GcTransition::PIN.map(GcTransition::name),
            [
                "pin_stage_create",
                "pin_write",
                "pin_file_sync",
                "pin_replace",
                "pin_directory_sync",
            ]
        );
        assert_eq!(
            GcTransition::UNPIN.map(GcTransition::name),
            ["unpin_remove", "unpin_directory_sync"]
        );
        for transition in GcTransition::PIN.into_iter().chain(GcTransition::UNPIN) {
            assert_eq!(GcTransition::parse(transition.name()), Some(transition));
        }

        let traced = TestProject::extracted("transition-trace");
        let traced_root = LocalStoreRoot::open(traced.path()).unwrap();
        let mut traced_leases =
            ProjectStoreLeases::acquire(&traced_root, ProjectOpenMode::PreferWritable).unwrap();
        let trace = GcTransitionInjector::recorder();
        traced_leases.set_gc_transition_injector(trace.clone());
        publish_pin(
            &traced_root,
            &traced_leases,
            "checkpoint-a",
            target,
            limits,
            || false,
        )
        .unwrap();
        remove_pin(&traced_root, &traced_leases, "checkpoint-a", limits, || {
            false
        })
        .unwrap();
        for (transition, attempts) in [
            (GcTransition::PinStageCreate, 1),
            (GcTransition::PinWrite, 1),
            (GcTransition::PinFileSync, 1),
            (GcTransition::PinReplace, 1),
            (GcTransition::PinDirectorySync, 2),
            (GcTransition::UnpinRemove, 1),
            (GcTransition::UnpinDirectorySync, 1),
        ] {
            assert_eq!(
                trace.attempts(transition),
                attempts,
                "{}",
                transition.name()
            );
        }

        let mut pin_cases = Vec::new();
        for transition in GcTransition::PIN {
            let occurrences = if transition == GcTransition::PinDirectorySync {
                2
            } else {
                1
            };
            for edge in [TransitionEdge::Before, TransitionEdge::After] {
                for occurrence in 0..occurrences {
                    pin_cases.push((transition, edge, occurrence));
                }
            }
        }
        assert_eq!(pin_cases.len(), 12);
        for (case, (transition, edge, occurrence)) in pin_cases.into_iter().enumerate() {
            let project = TestProject::extracted(&format!("pin-transition-{case}"));
            let root = LocalStoreRoot::open(project.path()).unwrap();
            let mut leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let previous = root
                .read_pin_ref("checkpoint-a", limits, || false)
                .unwrap()
                .unwrap();
            assert_ne!(previous.current(), target);
            let injector = GcTransitionInjector::failing(GcTransitionTarget {
                transition,
                edge,
                occurrence,
            });
            leases.set_gc_transition_injector(injector.clone());
            let result = publish_pin(&root, &leases, "checkpoint-a", target, limits, || false);
            assert_eq!(injector.fired(), 1);
            let mutation_started = transition == GcTransition::PinDirectorySync
                || (transition == GcTransition::PinReplace && edge == TransitionEdge::After);
            if mutation_started {
                assert!(matches!(
                    result,
                    Err(ProjectStoreFault::CommitIndeterminate)
                ));
                assert_eq!(
                    root.read_pin_ref("checkpoint-a", limits, || false)
                        .unwrap()
                        .unwrap()
                        .current(),
                    target
                );
                assert!(matches!(
                    publish_pin(&root, &leases, "same-session", target, limits, || false,),
                    Err(ProjectStoreFault::CommitIndeterminate)
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(ProjectStoreFault::Corruption { stage: "pin_ref" })
                ));
                assert_eq!(
                    root.read_pin_ref("checkpoint-a", limits, || false).unwrap(),
                    Some(previous)
                );
            }

            drop(leases);
            let mut retry =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let retry_trace = GcTransitionInjector::recorder();
            retry.set_gc_transition_injector(retry_trace.clone());
            publish_pin(&root, &retry, "checkpoint-a", target, limits, || false).unwrap();
            if mutation_started {
                assert_eq!(retry_trace.attempts(GcTransition::PinDirectorySync), 1);
                for transition in [
                    GcTransition::PinStageCreate,
                    GcTransition::PinWrite,
                    GcTransition::PinFileSync,
                    GcTransition::PinReplace,
                ] {
                    assert_eq!(retry_trace.attempts(transition), 0);
                }
            }
            publish_pin(&root, &retry, "checkpoint-a", target, limits, || false).unwrap();
            assert_eq!(
                root.read_pin_ref("checkpoint-a", limits, || false)
                    .unwrap()
                    .unwrap()
                    .current(),
                target
            );
        }

        let mut unpin_cases = Vec::new();
        for transition in GcTransition::UNPIN {
            for edge in [TransitionEdge::Before, TransitionEdge::After] {
                unpin_cases.push((transition, edge, 0));
            }
        }
        assert_eq!(unpin_cases.len(), 4);
        for (case, (transition, edge, occurrence)) in unpin_cases.into_iter().enumerate() {
            let project = TestProject::extracted(&format!("unpin-transition-{case}"));
            let root = LocalStoreRoot::open(project.path()).unwrap();
            let mut leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let previous = root
                .read_pin_ref("checkpoint-a", limits, || false)
                .unwrap()
                .unwrap();
            let injector = GcTransitionInjector::failing(GcTransitionTarget {
                transition,
                edge,
                occurrence,
            });
            leases.set_gc_transition_injector(injector.clone());
            let result = remove_pin(&root, &leases, "checkpoint-a", limits, || false);
            assert_eq!(injector.fired(), 1);
            let mutation_started = transition == GcTransition::UnpinDirectorySync
                || (transition == GcTransition::UnpinRemove && edge == TransitionEdge::After);
            if mutation_started {
                assert!(matches!(
                    result,
                    Err(ProjectStoreFault::CommitIndeterminate)
                ));
                assert!(
                    root.read_pin_ref("checkpoint-a", limits, || false)
                        .unwrap()
                        .is_none()
                );
                assert!(matches!(
                    publish_pin(&root, &leases, "same-session", target, limits, || false),
                    Err(ProjectStoreFault::CommitIndeterminate)
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(ProjectStoreFault::Corruption { stage: "pin_ref" })
                ));
                assert_eq!(
                    root.read_pin_ref("checkpoint-a", limits, || false).unwrap(),
                    Some(previous)
                );
            }

            drop(leases);
            let mut retry =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let retry_trace = GcTransitionInjector::recorder();
            retry.set_gc_transition_injector(retry_trace.clone());
            remove_pin(&root, &retry, "checkpoint-a", limits, || false).unwrap();
            if mutation_started {
                assert_eq!(retry_trace.attempts(GcTransition::UnpinDirectorySync), 1);
                assert_eq!(retry_trace.attempts(GcTransition::UnpinRemove), 0);
            }
            remove_pin(&root, &retry, "checkpoint-a", limits, || false).unwrap();
            assert!(
                root.read_pin_ref("checkpoint-a", limits, || false)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn unpin_rejects_a_post_state_above_the_recovery_candidate_cap() {
        let project = TestProject::extracted("orphan-cap");
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let orphan_id = ProjectGenerationId::parse(ORPHAN).unwrap();
        let limits = ProjectStoreLimits::default();
        publish_pin(&root, &leases, "only-root", orphan_id, limits, || false).unwrap();

        let orphan_path = generation_path(project.path(), orphan_id);
        let graph = inspect_store_graph(&root, limits, || false).unwrap();
        let orphan = GenerationDocument::decode(
            orphan_id,
            graph.state().project_id(),
            &fs::read(orphan_path).unwrap(),
            limits,
        )
        .unwrap();
        let second = GenerationDocument::build_from_projection(
            orphan.projection().clone(),
            orphan.parent_generation_id(),
            orphan.base_manual_generation_id(),
            orphan.forked_from(),
            orphan.kind(),
            orphan.generation_sequence().checked_add(100).unwrap(),
            orphan.bindings().clone(),
            orphan.reachable_objects().to_vec(),
            limits,
        )
        .unwrap()
        .encode(limits)
        .unwrap();
        let second_path = generation_path(project.path(), second.id());
        fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        fs::write(second_path, second.bytes()).unwrap();

        let capped = ProjectStoreLimits {
            recovery_candidates_max: 1,
            ..limits
        };
        assert_eq!(
            inspect_store_graph(&root, capped, || false)
                .unwrap()
                .orphan_generation_ids(),
            [second.id()]
        );
        assert!(matches!(
            publish_pin(
                &root,
                &leases,
                "only-root",
                ProjectGenerationId::parse(MANUAL).unwrap(),
                capped,
                || false,
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "recovery_candidates"
            })
        ));
        assert!(matches!(
            remove_pin(&root, &leases, "only-root", capped, || false),
            Err(ProjectStoreFault::Capacity {
                stage: "recovery_candidates"
            })
        ));
        assert_eq!(
            root.read_pin_ref("only-root", limits, || false)
                .unwrap()
                .unwrap()
                .current(),
            orphan_id
        );
    }

    #[test]
    fn pin_rejects_linked_checkpoint_files_without_touching_outside_data() {
        let project = TestProject::extracted("linked-pin");
        let pin = project.path().join("refs/pins/checkpoint-a");
        let original = fs::read(&pin).unwrap();
        let outside = project.path().parent().unwrap().join("outside-pin");
        fs::write(&outside, b"outside sentinel").unwrap();
        fs::remove_file(&pin).unwrap();
        symlink(&outside, &pin).unwrap();
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_pin(
                &root,
                &leases,
                "checkpoint-a",
                ProjectGenerationId::parse(ORPHAN).unwrap(),
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert_eq!(fs::read(&outside).unwrap(), b"outside sentinel");

        fs::remove_file(&pin).unwrap();
        fs::remove_file(&outside).unwrap();
        fs::write(&pin, &original).unwrap();
        fs::hard_link(&pin, &outside).unwrap();
        assert!(matches!(
            remove_pin(
                &root,
                &leases,
                "checkpoint-a",
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert_eq!(fs::read(&outside).unwrap(), original);
    }

    fn generation_path(root: &Path, id: ProjectGenerationId) -> PathBuf {
        let digest = id.digest().to_string();
        root.join("generations")
            .join("sha256")
            .join(&digest[..2])
            .join(format!("{}.json", &digest[2..]))
    }

    fn file_bytes_except_pins(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    let relative = path.strip_prefix(root).unwrap().to_path_buf();
                    if !relative.starts_with("refs/pins") {
                        files.insert(relative, fs::read(path).unwrap());
                    }
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }
}
