use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use mirante4d_dataset::{
    DatasetCatalog, DatasetLayer, DatasetSourceId, ResourceValidity, ScientificIdentityStatus,
};
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, GridToWorld, IntensityDType, IsoLightState,
    LayerTransfer, LogicalLayerKey, Opacity, Projection, RenderState, RgbColor, SamplingPolicy,
    Shape4D, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
};
use mirante4d_identity::ScientificContentId;
use mirante4d_project_model::{DatasetReference, LayerViewState, ProjectId, ViewState};
use mirante4d_settings::{GIB, ResourcePolicy};

use super::*;
use crate::{
    ApplicationCommand, ApplicationEvent, ApplicationState, MAX_PENDING_EVENTS,
    OperationCompletion, SourceSessionGeneration, UnboundWorkspace,
};

#[derive(Clone, Default)]
struct ManualClock(Arc<Mutex<u64>>);

impl ManualClock {
    fn set(&self, tick: u64) {
        *self.0.lock().unwrap_or_else(|poison| poison.into_inner()) = tick;
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> u64 {
        *self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

#[test]
fn idle_and_maximum_deadlines_are_exact() {
    let project = ProjectId::from_bytes([1; 16]);
    let mut idle = AutosaveScheduler::default();
    assert_eq!(idle.observe(seconds(0), eligible(project, 0)), Ok(None));
    assert_eq!(idle.observe(seconds(20), eligible(project, 1)), Ok(None));
    assert_eq!(idle.observe(seconds(49), eligible(project, 1)), Ok(None));
    assert_eq!(
        idle.observe(seconds(50), eligible(project, 1)),
        Ok(Some(due(project, 1)))
    );

    let mut maximum = AutosaveScheduler::default();
    for (tick, revision) in [(0, 0), (25, 1), (50, 2), (75, 3), (100, 4), (119, 5)] {
        assert_eq!(
            maximum.observe(seconds(tick), eligible(project, revision)),
            Ok(None)
        );
    }
    assert_eq!(
        maximum.observe(seconds(120), eligible(project, 5)),
        Ok(Some(due(project, 5)))
    );
}

#[test]
fn every_eligibility_fact_blocks_capture_without_consuming_the_deadline() {
    let project = ProjectId::from_bytes([2; 16]);
    for blocked in 0..4 {
        let mut scheduler = AutosaveScheduler::default();
        scheduler.observe(seconds(0), eligible(project, 0)).unwrap();
        let mut observation = eligible(project, 0);
        match blocked {
            0 => observation.verified = false,
            1 => observation.writable = false,
            2 => observation.commit_active = true,
            3 => observation.writes_suspended = true,
            _ => unreachable!(),
        }
        assert_eq!(scheduler.observe(seconds(30), observation), Ok(None));
        assert_eq!(
            scheduler.observe(seconds(30), eligible(project, 0)),
            Ok(Some(due(project, 0)))
        );
    }

    let mut unbound = AutosaveScheduler::default();
    unbound.observe(seconds(0), eligible(project, 0)).unwrap();
    assert_eq!(unbound.observe(seconds(30), ineligible_unbound()), Ok(None));
    let mut clean = AutosaveScheduler::default();
    clean.observe(seconds(0), eligible(project, 0)).unwrap();
    assert_eq!(
        clean.observe(seconds(30), clean_project(project, 0)),
        Ok(None)
    );
}

#[test]
fn edits_during_capture_keep_their_own_deadlines_and_failures_do_not_retry() {
    let project = ProjectId::from_bytes([3; 16]);
    let mut scheduler = AutosaveScheduler::default();
    scheduler.observe(seconds(0), eligible(project, 0)).unwrap();
    assert_eq!(
        scheduler.observe(seconds(30), eligible(project, 0)),
        Ok(Some(due(project, 0)))
    );

    let mut while_active = eligible(project, 1);
    while_active.commit_active = true;
    assert_eq!(scheduler.observe(seconds(35), while_active), Ok(None));
    assert_eq!(
        scheduler.observe(seconds(64), eligible(project, 1)),
        Ok(None)
    );
    assert_eq!(
        scheduler.observe(seconds(65), eligible(project, 1)),
        Ok(Some(due(project, 1)))
    );

    assert_eq!(
        scheduler.observe(seconds(90), eligible(project, 1)),
        Ok(None)
    );
    assert_eq!(
        scheduler.observe(seconds(200), eligible(project, 1)),
        Ok(None)
    );
    assert_eq!(
        scheduler.observe(seconds(201), eligible(project, 2)),
        Ok(None)
    );
    assert_eq!(
        scheduler.observe(seconds(231), eligible(project, 2)),
        Ok(Some(due(project, 2)))
    );
}

#[test]
fn decreasing_tick_is_rejected_atomically() {
    let project = ProjectId::from_bytes([4; 16]);
    let mut scheduler = AutosaveScheduler::default();
    scheduler
        .observe(seconds(10), eligible(project, 0))
        .unwrap();
    let before = scheduler.clone();
    assert_eq!(
        scheduler.observe(seconds(9), eligible(project, 1)),
        Err(ProjectStoreServiceError::ClockRegressed {
            previous: seconds(10),
            observed: seconds(9),
        })
    );
    assert_eq!(scheduler, before);
}

#[test]
fn pending_repaint_deadline_and_busy_status_are_exact_without_polling() {
    let application = verified_bound_application();
    let snapshot = application.snapshot();
    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let clock = ManualClock::default();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock.clone(),
        Some(destination),
    )
    .unwrap();

    let initial = service.status();
    assert_eq!(initial.lifecycle(), ProjectStoreLifecycle::Unbound);
    assert!(initial.writable());
    assert!(!initial.foreground_active());
    assert!(!initial.autosave_active());
    assert!(service.can_open());
    assert!(service.can_save());
    assert!(!service.can_save_as());
    assert!(!service.has_pending_work());
    assert_eq!(service.repaint_after(), None);

    assert!(
        service
            .drive(&snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(service.repaint_after(), Some(Duration::from_secs(30)));
    assert!(!service.has_pending_work());

    clock.set(seconds(10));
    assert!(
        service
            .drive(&snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(service.repaint_after(), Some(Duration::from_secs(20)));

    clock.set(seconds(30));
    let submitted = service.drive(&snapshot, |_| Ok(Vec::new())).unwrap();
    assert!(matches!(
        submitted.as_slice(),
        [ProjectStoreServiceEvent::AutosaveSubmitted { revision, .. }]
            if *revision == bound_revision(&snapshot)
    ));
    assert!(service.has_pending_work());
    assert_eq!(service.repaint_after(), None);
    let active = service.status();
    assert!(active.autosave_active());
    assert!(!active.foreground_active());
    assert!(!service.can_open());
    assert!(!service.can_save());
    assert!(!service.can_save_as());

    service.join().unwrap();
}

#[test]
fn missing_recovery_destination_disables_only_provisional_autosave() {
    let application = verified_bound_application();
    let snapshot = application.snapshot();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();

    assert!(
        service
            .drive(&snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    let status = service.status();
    assert_eq!(status.lifecycle(), ProjectStoreLifecycle::Unbound);
    assert!(!status.writable());
    assert!(service.can_open());
    assert!(service.can_save());
    assert!(!service.can_save_as());
    assert!(!service.has_pending_work());
    assert_eq!(service.repaint_after(), None);

    service.join().unwrap();
}

#[test]
fn status_and_action_availability_follow_the_private_lifecycle() {
    let directory = TestDirectory::new();
    let path = ProjectStorePath::new(directory.path().join("project.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        Some(path.clone()),
    )
    .unwrap();
    let project_id = ProjectId::from_bytes([6; 16]);
    let autosave = generation_id('6');
    let manual = generation_id('7');

    service.binding = StoreBinding::Provisional(SessionFacts {
        path: path.clone(),
        project_id,
        mode: ProjectOpenMode::PreferWritable,
        current_manual: None,
        current_autosave: Some(autosave),
    });
    let provisional = service.status();
    assert_eq!(provisional.lifecycle(), ProjectStoreLifecycle::Provisional);
    assert_eq!(provisional.project_id(), Some(project_id));
    assert_eq!(provisional.current_manual(), None);
    assert_eq!(provisional.current_autosave(), Some(autosave));
    assert!(service.can_save());
    assert!(!service.can_save_as());
    assert!(!service.can_open());

    service.binding = StoreBinding::Established(SessionFacts {
        path: path.clone(),
        project_id,
        mode: ProjectOpenMode::PreferWritable,
        current_manual: Some(manual),
        current_autosave: Some(autosave),
    });
    let established = service.status();
    assert_eq!(established.lifecycle(), ProjectStoreLifecycle::Established);
    assert_eq!(established.mode(), Some(ProjectOpenMode::PreferWritable));
    assert_eq!(established.current_manual(), Some(manual));
    assert!(established.writable());
    assert!(service.can_save());
    assert!(service.can_save_as());

    service.binding = StoreBinding::RecoverySelected {
        facts: SessionFacts {
            path: path.clone(),
            project_id,
            mode: ProjectOpenMode::PreferWritable,
            current_manual: Some(manual),
            current_autosave: Some(autosave),
        },
        selected_generation: autosave,
    };
    let selected = service.status();
    assert_eq!(
        selected.lifecycle(),
        ProjectStoreLifecycle::RecoverySelected
    );
    assert!(!selected.writable());
    assert!(!service.can_save());
    assert!(service.can_save_as());

    service.binding = StoreBinding::RecoveryOnly;
    let recovery_only = service.status();
    assert_eq!(
        recovery_only.lifecycle(),
        ProjectStoreLifecycle::RecoveryOnly
    );
    assert!(!service.can_open());
    assert!(!service.can_save());
    assert!(!service.can_save_as());

    service.binding = StoreBinding::Closed;
    assert_eq!(service.status().lifecycle(), ProjectStoreLifecycle::Closed);
    assert!(!service.can_open());
    assert!(!service.can_save());
    assert!(!service.can_save_as());
    service.join().unwrap();
}

#[test]
fn opening_a_provisional_store_is_dirty_until_recovery_is_selected() {
    let directory = TestDirectory::new();
    let path = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let project_id = ProjectId::from_bytes([9; 16]);
    let autosave = generation_id('9');
    let (binding, opens_dirty) = StoreBinding::from_opened(SessionFacts {
        path: path.clone(),
        project_id,
        mode: ProjectOpenMode::PreferWritable,
        current_manual: None,
        current_autosave: Some(autosave),
    })
    .unwrap();
    assert!(opens_dirty);
    assert!(matches!(
        binding,
        StoreBinding::Provisional(SessionFacts {
            current_autosave: Some(current_autosave),
            ..
        }) if current_autosave == autosave
    ));

    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    service.binding = binding;
    assert_eq!(
        service.status().lifecycle(),
        ProjectStoreLifecycle::Provisional
    );
    assert!(service.can_save());
    assert!(!service.can_save_as());
    service.join().unwrap();
}

#[test]
fn save_as_rejects_a_projection_not_retained_by_its_exact_token() {
    let mut application = verified_bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let requested_id = ProjectId::from_bytes([10; 16]);
    application
        .dispatch(ApplicationCommand::RequestProjectSaveAs {
            new_project_id: requested_id,
        })
        .unwrap();
    let (token, projection) = application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectSaveAsRequested { token, projection } => {
                Some((token, projection.as_ref().clone()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(token.target_project_id(), Some(requested_id));
    let snapshot = application.snapshot();
    let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() else {
        panic!("test application is bound");
    };
    let wrong_id = ProjectId::from_bytes([11; 16]);
    let wrong_state = ProjectState::new(
        wrong_id,
        project.dataset().clone(),
        project.view().clone(),
        project.channel_presets().to_vec(),
        project.artifacts().to_vec(),
    )
    .unwrap();
    let wrong_projection = ProjectGenerationProjection::new(
        ProjectRevisionId::initial(wrong_id),
        ProjectRevisionHighWater::initial(wrong_id),
        wrong_state,
    )
    .unwrap();

    let directory = TestDirectory::new();
    let source_path = ProjectStorePath::new(directory.path().join("source.m4dproj")).unwrap();
    let destination = ProjectStorePath::new(directory.path().join("fork.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    service.binding = StoreBinding::Established(SessionFacts {
        path: source_path,
        project_id: token.project_id().unwrap(),
        mode: ProjectOpenMode::PreferWritable,
        current_manual: Some(generation_id('a')),
        current_autosave: None,
    });
    assert_eq!(
        service.submit_save_as(
            &snapshot,
            token.clone(),
            destination.clone(),
            wrong_projection,
            Vec::new(),
        ),
        Err(ProjectStoreServiceError::InvalidOperationToken)
    );

    let original_view = projection.state().view();
    let tampered_view = ViewState::new(
        original_view.layers().to_vec(),
        original_view.active_layer(),
        TimeIndex::new(1),
        *original_view.camera(),
        original_view.layout(),
        *original_view.cross_section(),
        *original_view.iso_light(),
    )
    .unwrap();
    let tampered_state = ProjectState::new(
        requested_id,
        projection.state().dataset().clone(),
        tampered_view,
        projection.state().channel_presets().to_vec(),
        projection.state().artifacts().to_vec(),
    )
    .unwrap();
    let tampered_projection = ProjectGenerationProjection::new(
        projection.revision(),
        projection.revision_high_water().clone(),
        tampered_state,
    )
    .unwrap();
    assert_eq!(
        service.submit_save_as(
            &snapshot,
            token,
            destination,
            tampered_projection,
            Vec::new(),
        ),
        Err(ProjectStoreServiceError::InvalidProjection)
    );
    service.join().unwrap();
}

#[test]
fn foreground_completion_at_autosave_deadline_does_not_capture_the_stale_snapshot() {
    let mut application = verified_bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let snapshot = application.snapshot();
    let (token, projection) = project_save_request(&mut application);
    let directory = TestDirectory::new();
    let recovery = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let destination = ProjectStorePath::new(directory.path().join("project.m4dproj")).unwrap();
    let clock = ManualClock::default();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock.clone(),
        Some(recovery),
    )
    .unwrap();

    assert!(
        service
            .drive(&snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    service
        .submit_save(token.clone(), projection, Some(destination), Vec::new())
        .unwrap();
    clock.set(seconds(30));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stale_capture_calls = 0;
    let completion = loop {
        let events = service
            .drive(&snapshot, |_| {
                stale_capture_calls += 1;
                Ok(Vec::new())
            })
            .unwrap();
        if !events.is_empty() {
            break events;
        }
        assert!(
            Instant::now() < deadline,
            "initial Save completion timed out"
        );
        thread::yield_now();
    };
    let saved_revision = match completion.as_slice() {
        [
            ProjectStoreServiceEvent::Created {
                token: completed,
                saved_revision,
            },
        ] if completed == &token => *saved_revision,
        unexpected => panic!("unexpected completion at autosave deadline: {unexpected:?}"),
    };
    assert_eq!(stale_capture_calls, 0);
    assert_eq!(service.repaint_after(), Some(Duration::ZERO));

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectSaved(saved_revision),
        })
        .unwrap();
    let clean_snapshot = application.snapshot();
    assert_eq!(bound_saved_revision(&clean_snapshot), Some(saved_revision));
    assert!(
        service
            .drive(&clean_snapshot, |_| panic!("clean revision was autosaved"))
            .unwrap()
            .is_empty()
    );

    close_service(&mut service, &clean_snapshot);
    service.join().unwrap();
}

#[test]
fn real_open_recovery_inspection_failure_enters_recovery_only() {
    let directory = TestDirectory::new();
    let path = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    create_established_store(&path);

    let mut opener = verified_unbound_application();
    opener.drain_events(MAX_PENDING_EVENTS);
    let token = project_open_request(&mut opener);
    let opener_snapshot = opener.snapshot();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    service
        .submit_open(token.clone(), path.clone(), ProjectOpenMode::PreferWritable)
        .unwrap();

    let opened = wait_for_raw_completion(&service);
    assert!(matches!(
        &opened,
        ProjectStoreCompletion::Opened { result: Ok(_), .. }
    ));
    let envelope_path = path.as_path().join("project.json");
    let mut envelope = fs::read(&envelope_path).unwrap();
    envelope[0] ^= 1;
    fs::write(envelope_path, envelope).unwrap();
    assert!(service.handle_completion(opened).unwrap().is_empty());

    let events = wait_for_service_events(&mut service, &opener_snapshot);
    assert!(
        matches!(
            events.as_slice(),
            [ProjectStoreServiceEvent::OpenFailed {
                token: completed,
                fault: ProjectStoreFault::Corruption { .. } | ProjectStoreFault::SourceChanged,
                candidates,
            }] if completed == &token && candidates.is_empty()
        ),
        "unexpected Open/InspectRecovery failure events: {events:?}"
    );
    assert_eq!(
        service.status().lifecycle(),
        ProjectStoreLifecycle::RecoveryOnly
    );
    assert!(service.recovery_candidates().is_empty());
    assert!(!service.can_open());
    assert!(!service.can_save());
    assert!(!service.can_save_as());

    close_service(&mut service, &opener_snapshot);
    service.join().unwrap();
}

#[test]
fn real_recovery_selected_save_as_establishes_the_new_project() {
    let application = verified_bound_application();
    let source_snapshot = application.snapshot();
    let directory = TestDirectory::new();
    let source = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let destination = ProjectStorePath::new(directory.path().join("recovered.m4dproj")).unwrap();
    create_provisional_store(&source, &source_snapshot);

    let mut opener = verified_unbound_application();
    opener.drain_events(MAX_PENDING_EVENTS);
    let open_token = project_open_request(&mut opener);
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    service
        .submit_open(open_token.clone(), source, ProjectOpenMode::PreferWritable)
        .unwrap();
    let opened = wait_for_service_events(&mut service, &opener.snapshot());
    let recovered = match opened.as_slice() {
        [
            ProjectStoreServiceEvent::Opened {
                token,
                projection,
                opens_dirty: true,
                ..
            },
        ] if token == &open_token => projection.as_ref().clone(),
        unexpected => panic!("unexpected provisional Open result: {unexpected:?}"),
    };
    assert_eq!(
        service.status().lifecycle(),
        ProjectStoreLifecycle::RecoverySelected
    );
    assert!(!service.can_save());
    assert!(service.can_save_as());

    opener
        .dispatch(ApplicationCommand::CompleteOperation {
            token: open_token,
            completion: OperationCompletion::ProjectRecovered(Box::new(recovered)),
        })
        .unwrap();
    opener.drain_events(MAX_PENDING_EVENTS);
    let new_project_id = ProjectId::from_bytes([12; 16]);
    opener
        .dispatch(ApplicationCommand::RequestProjectSaveAs { new_project_id })
        .unwrap();
    let (save_as_token, fork) = project_save_as_request(&mut opener);
    let save_as_snapshot = opener.snapshot();
    service
        .submit_save_as(
            &save_as_snapshot,
            save_as_token.clone(),
            destination.clone(),
            fork.clone(),
            Vec::new(),
        )
        .unwrap();

    let saved_as = wait_for_service_events(&mut service, &save_as_snapshot);
    let completed_fork = match saved_as.as_slice() {
        [
            ProjectStoreServiceEvent::SavedAs {
                token,
                projection,
                receipt,
            },
        ] if token == &save_as_token => {
            assert_eq!(receipt.captured_revision(), fork.revision());
            projection.as_ref().clone()
        }
        unexpected => panic!("unexpected recovered Save As result: {unexpected:?}"),
    };
    assert_eq!(completed_fork, fork);
    assert!(destination.as_path().is_dir());
    let established = service.status();
    assert_eq!(established.lifecycle(), ProjectStoreLifecycle::Established);
    assert_eq!(established.project_id(), Some(new_project_id));
    assert!(established.current_manual().is_some());
    assert_eq!(established.current_autosave(), None);
    assert!(service.can_save());
    assert!(service.can_save_as());

    opener
        .dispatch(ApplicationCommand::CompleteOperation {
            token: save_as_token,
            completion: OperationCompletion::ProjectSavedAs(Box::new(completed_fork)),
        })
        .unwrap();
    let established_snapshot = opener.snapshot();
    let WorkspaceSnapshot::Bound {
        project,
        dirty,
        saved_revision,
        revision,
        ..
    } = established_snapshot.workspace()
    else {
        panic!("Save As establishes a bound project");
    };
    assert_eq!(project.project_id(), new_project_id);
    assert!(!*dirty);
    assert_eq!(saved_revision, &Some(*revision));

    close_service(&mut service, &established_snapshot);
    service.join().unwrap();
}

#[test]
fn inactive_cancellation_uses_the_service_path() {
    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        Some(destination),
    )
    .unwrap();
    assert_eq!(
        service.cancel_active_autosave(),
        Err(ProjectStoreServiceError::OperationConflict)
    );
    service.join().unwrap();
}

#[test]
fn pending_recovery_review_cancellation_is_terminal_and_closes_the_session() {
    let mut application = verified_unbound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectOpen)
        .unwrap();
    let token = application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectOpenRequested { token } => Some(token),
            _ => None,
        })
        .unwrap();
    let snapshot = application.snapshot();
    let mut projection_application = verified_bound_application();
    projection_application.drain_events(MAX_PENDING_EVENTS);
    let (_, projection) = project_save_request(&mut projection_application);
    let directory = TestDirectory::new();
    let path = ProjectStorePath::new(directory.path().join("project.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    service.binding = StoreBinding::Established(SessionFacts {
        path,
        project_id: projection.state().project_id(),
        mode: ProjectOpenMode::PreferWritable,
        current_manual: Some(generation_id('b')),
        current_autosave: Some(generation_id('c')),
    });
    service.pending_normal_open = Some(PendingNormalOpen {
        token: token.clone(),
        projection,
        candidates: Vec::new(),
        opens_dirty: false,
    });

    assert!(matches!(
        service.cancel_pending_open(token.operation_id()),
        Ok(ProjectStoreServiceEvent::OperationFailed {
            token: cancelled,
            fault: ProjectStoreFault::Cancelled,
        }) if cancelled == token
    ));
    assert_eq!(service.status().lifecycle(), ProjectStoreLifecycle::Closing);
    assert!(service.pending_normal_open.is_none());

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = service.drive(&snapshot, |_| Ok(Vec::new())).unwrap();
        if events.iter().any(|event| {
            matches!(
                event,
                ProjectStoreServiceEvent::Closed { result: Ok(()), .. }
            )
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "close completion timed out");
        thread::yield_now();
    }
    assert_eq!(service.status().lifecycle(), ProjectStoreLifecycle::Closed);
    service.join().unwrap();
}

#[test]
fn initial_save_preserves_the_reducer_token_on_qualified_or_unsupported_filesystems() {
    let mut application = verified_bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let (token, projection) = project_save_request(&mut application);

    let mut wrong_application = verified_bound_application();
    wrong_application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Import))
        .unwrap();
    let wrong_token = wrong_application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::OperationStarted { token } => Some(token),
            _ => None,
        })
        .expect("import operation token");

    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("project.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    assert_eq!(
        service.submit_save(
            wrong_token,
            projection.clone(),
            Some(destination.clone()),
            Vec::new(),
        ),
        Err(ProjectStoreServiceError::InvalidOperationToken)
    );

    let request = service
        .submit_save(
            token.clone(),
            projection.clone(),
            Some(destination),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(request, request_id(1));
    assert!(service.has_pending_work());
    assert!(service.status().foreground_active());
    assert!(!service.can_open());
    assert!(!service.can_save());
    assert!(!service.can_save_as());

    match wait_for_foreground_completion(&mut service, &application.snapshot()) {
        ProjectStoreServiceEvent::Created {
            token: completed,
            saved_revision,
        } => {
            assert_eq!(completed, token);
            assert_eq!(saved_revision, projection.revision());
            assert_eq!(
                service.status().lifecycle(),
                ProjectStoreLifecycle::Established
            );
            assert!(service.can_save());
            assert!(service.can_save_as());
        }
        ProjectStoreServiceEvent::OperationFailed {
            token: completed,
            fault: ProjectStoreFault::UnsupportedFilesystem,
        } => {
            assert_eq!(completed, token);
            assert_eq!(service.status().lifecycle(), ProjectStoreLifecycle::Unbound);
            assert!(service.can_save());
            assert!(!service.can_save_as());
        }
        unexpected => panic!("unexpected initial-save completion: {unexpected:?}"),
    }
    assert!(!service.has_pending_work());
    service.join().unwrap();
}

#[test]
fn real_actor_autosave_obeys_filesystem_policy_without_advancing_manual_saved_revision() {
    let mut application = verified_bound_application();
    let first_snapshot = application.snapshot();
    let first_revision = bound_revision(&first_snapshot);
    assert_eq!(bound_saved_revision(&first_snapshot), None);

    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let clock = ManualClock::default();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock.clone(),
        Some(destination),
    )
    .unwrap();

    assert!(
        service
            .drive(&first_snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    clock.set(seconds(30));
    let captures = Arc::new(AtomicU64::new(0));
    let submitted_captures = Arc::clone(&captures);
    let submitted = service
        .drive(&first_snapshot, move |_| {
            submitted_captures.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        })
        .unwrap();
    assert!(matches!(
        submitted.as_slice(),
        [ProjectStoreServiceEvent::AutosaveSubmitted {
            request_id,
            revision,
        }] if request_id.get() == 1 && *revision == first_revision
    ));
    let first_receipt = match wait_for_autosave(&mut service, &first_snapshot) {
        Ok(receipt) => receipt,
        Err(ProjectStoreFault::UnsupportedFilesystem) => {
            assert_eq!(captures.load(Ordering::Relaxed), 1);
            assert_eq!(bound_saved_revision(&application.snapshot()), None);
            assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
            service.join().unwrap();
            return;
        }
        Err(fault) => panic!("unexpected first autosave fault: {fault:?}"),
    };
    assert_eq!(captures.load(Ordering::Relaxed), 1);
    assert_eq!(first_receipt.captured_revision(), first_revision);
    assert_eq!(first_receipt.previous_generation_id(), None);
    assert_eq!(bound_saved_revision(&application.snapshot()), None);

    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    let second_snapshot = application.snapshot();
    let second_revision = bound_revision(&second_snapshot);
    clock.set(seconds(40));
    assert!(
        service
            .drive(&second_snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    clock.set(seconds(70));
    assert!(
        service
            .drive(&second_snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .iter()
            .any(|event| matches!(
                event,
                ProjectStoreServiceEvent::AutosaveSubmitted { revision, .. }
                    if *revision == second_revision
            ))
    );
    let second_receipt = wait_for_autosave(&mut service, &second_snapshot)
        .expect("a filesystem qualified for the first autosave remains writable");
    assert_eq!(second_receipt.captured_revision(), second_revision);
    assert_eq!(
        second_receipt.previous_generation_id(),
        Some(first_receipt.current_generation_id())
    );
    assert_eq!(second_receipt.autosave_base_generation_id(), None);
    assert_eq!(bound_saved_revision(&application.snapshot()), None);
    service.join().unwrap();
}

#[test]
fn failed_capture_is_not_retried_until_a_later_durable_edit() {
    let mut application = verified_bound_application();
    let first_snapshot = application.snapshot();
    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let clock = ManualClock::default();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock.clone(),
        Some(destination),
    )
    .unwrap();

    service.drive(&first_snapshot, |_| Ok(Vec::new())).unwrap();
    clock.set(seconds(30));
    assert!(matches!(
        service.drive(&first_snapshot, |_| Err(ProjectStoreFault::SourceChanged)),
        Err(ProjectStoreServiceError::Store(
            ProjectStoreFault::SourceChanged
        ))
    ));
    clock.set(seconds(300));
    assert!(
        service
            .drive(&first_snapshot, |_| panic!("captured revision retried"))
            .unwrap()
            .is_empty()
    );

    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    let later_snapshot = application.snapshot();
    clock.set(seconds(301));
    service.drive(&later_snapshot, |_| Ok(Vec::new())).unwrap();
    clock.set(seconds(331));
    assert!(matches!(
        service.drive(&later_snapshot, |_| Err(ProjectStoreFault::Cancelled)),
        Err(ProjectStoreServiceError::Store(
            ProjectStoreFault::Cancelled
        ))
    ));
    service.join().unwrap();
}

#[test]
fn cancelled_completion_does_not_rearm_the_captured_revision() {
    let clock = ManualClock::default();
    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock,
        Some(destination),
    )
    .unwrap();
    let project = ProjectId::from_bytes([8; 16]);
    let revision = ProjectRevisionId::initial(project);
    service
        .scheduler
        .observe(seconds(0), eligible(project, 0))
        .unwrap();
    assert_eq!(
        service.scheduler.observe(seconds(30), eligible(project, 0)),
        Ok(Some(due(project, 0)))
    );
    service.active_autosave = Some(ActiveAutosave {
        request_id: request_id(1),
        project_id: project,
        revision,
        revision_high_water: ProjectRevisionHighWater::initial(project),
        expected_parent: None,
        autosave_base: None,
        cancellation_request: None,
    });
    let events = service
        .handle_completion(ProjectStoreCompletion::Autosaved {
            request_id: request_id(1),
            result: Err(ProjectStoreFault::Cancelled),
        })
        .unwrap();
    assert!(matches!(
        events.as_slice(),
        [ProjectStoreServiceEvent::AutosaveFinished {
            result: Err(ProjectStoreFault::Cancelled),
            ..
        }]
    ));
    assert_eq!(
        service
            .scheduler
            .observe(seconds(300), eligible(project, 0)),
        Ok(None)
    );
    assert_eq!(
        service
            .scheduler
            .observe(seconds(301), eligible(project, 1)),
        Ok(None)
    );
    assert_eq!(
        service
            .scheduler
            .observe(seconds(331), eligible(project, 1)),
        Ok(Some(due(project, 1)))
    );
    service.join().unwrap();
}

#[test]
fn commit_indeterminate_suspends_writes_until_service_reopen() {
    let clock = ManualClock::default();
    let directory = TestDirectory::new();
    let destination = ProjectStorePath::new(directory.path().join("recovery.m4dproj")).unwrap();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock,
        Some(destination),
    )
    .unwrap();
    let project = ProjectId::from_bytes([9; 16]);
    let revision = ProjectRevisionId::initial(project);
    service.active_autosave = Some(ActiveAutosave {
        request_id: request_id(1),
        project_id: project,
        revision,
        revision_high_water: ProjectRevisionHighWater::initial(project),
        expected_parent: None,
        autosave_base: None,
        cancellation_request: Some(request_id(2)),
    });
    let acknowledged = service
        .handle_completion(ProjectStoreCompletion::Cancelled {
            request_id: request_id(2),
            result: Ok(()),
        })
        .unwrap();
    assert!(matches!(
        acknowledged.as_slice(),
        [ProjectStoreServiceEvent::CancellationAcknowledged {
            request_id: actual,
            target_request_id: target,
        }] if *actual == request_id(2) && *target == request_id(1)
    ));
    let events = service
        .handle_completion(ProjectStoreCompletion::Autosaved {
            request_id: request_id(1),
            result: Err(ProjectStoreFault::CommitIndeterminate),
        })
        .unwrap();
    assert!(matches!(
        events.as_slice(),
        [ProjectStoreServiceEvent::AutosaveFinished {
            request_id: actual,
            revision: completed_revision,
            result: Err(ProjectStoreFault::CommitIndeterminate),
        }] if *actual == request_id(1) && *completed_revision == revision
    ));
    assert!(service.writes_suspended());
    service.join().unwrap();
}

fn eligible(project_id: ProjectId, revision: u64) -> AutosaveObservation {
    AutosaveObservation {
        project_id: Some(project_id),
        revision: Some(ProjectRevisionId::new(project_id, revision)),
        bound: true,
        dirty: true,
        verified: true,
        writable: true,
        commit_active: false,
        writes_suspended: false,
    }
}

fn clean_project(project_id: ProjectId, revision: u64) -> AutosaveObservation {
    AutosaveObservation {
        dirty: false,
        ..eligible(project_id, revision)
    }
}

fn ineligible_unbound() -> AutosaveObservation {
    AutosaveObservation {
        project_id: None,
        revision: None,
        bound: false,
        dirty: false,
        verified: true,
        writable: true,
        commit_active: false,
        writes_suspended: false,
    }
}

fn due(project_id: ProjectId, revision: u64) -> AutosaveDue {
    AutosaveDue {
        project_id,
        revision: ProjectRevisionId::new(project_id, revision),
    }
}

fn seconds(value: u64) -> u64 {
    value * NANOS_PER_SECOND
}

fn request_id(value: u64) -> ProjectStoreRequestId {
    ProjectStoreRequestId::new(value).unwrap()
}

fn generation_id(digit: char) -> ProjectGenerationId {
    ProjectGenerationId::parse(&format!(
        "{}{}",
        ProjectGenerationId::PREFIX,
        digit.to_string().repeat(64)
    ))
    .unwrap()
}

fn bound_revision(snapshot: &ApplicationSnapshot) -> ProjectRevisionId {
    let WorkspaceSnapshot::Bound { revision, .. } = snapshot.workspace() else {
        panic!("test application is bound");
    };
    *revision
}

fn bound_saved_revision(snapshot: &ApplicationSnapshot) -> Option<ProjectRevisionId> {
    let WorkspaceSnapshot::Bound { saved_revision, .. } = snapshot.workspace() else {
        panic!("test application is bound");
    };
    *saved_revision
}

fn wait_for_autosave(
    service: &mut ProjectStoreApplicationService<ManualClock>,
    snapshot: &ApplicationSnapshot,
) -> Result<ProjectStoreReceipt, ProjectStoreFault> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for event in service.drive(snapshot, |_| Ok(Vec::new())).unwrap() {
            if let ProjectStoreServiceEvent::AutosaveFinished { result, .. } = event {
                return result;
            }
        }
        thread::yield_now();
    }
    panic!("project-store actor did not complete autosave");
}

fn wait_for_foreground_completion(
    service: &mut ProjectStoreApplicationService<ManualClock>,
    snapshot: &ApplicationSnapshot,
) -> ProjectStoreServiceEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for event in service.drive(snapshot, |_| Ok(Vec::new())).unwrap() {
            if matches!(
                event,
                ProjectStoreServiceEvent::Created { .. }
                    | ProjectStoreServiceEvent::OperationFailed { .. }
            ) {
                return event;
            }
        }
        thread::yield_now();
    }
    panic!("project-store actor did not complete the foreground save");
}

fn wait_for_raw_completion(
    service: &ProjectStoreApplicationService<ManualClock>,
) -> ProjectStoreCompletion {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(completion) = service.actor().unwrap().try_recv() {
            return completion;
        }
        thread::yield_now();
    }
    panic!("project-store actor did not emit a completion");
}

fn wait_for_service_events(
    service: &mut ProjectStoreApplicationService<ManualClock>,
    snapshot: &ApplicationSnapshot,
) -> Vec<ProjectStoreServiceEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let events = service.drive(snapshot, |_| Ok(Vec::new())).unwrap();
        if !events.is_empty() {
            return events;
        }
        thread::yield_now();
    }
    panic!("project-store service did not emit an event");
}

fn close_service(
    service: &mut ProjectStoreApplicationService<ManualClock>,
    snapshot: &ApplicationSnapshot,
) {
    let request_id = service.close().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let events = service.drive(snapshot, |_| Ok(Vec::new())).unwrap();
        if events.iter().any(|event| {
            matches!(
                event,
                ProjectStoreServiceEvent::Closed {
                    request_id: completed,
                    result: Ok(()),
                } if *completed == request_id
            )
        }) {
            return;
        }
        thread::yield_now();
    }
    panic!("project-store service did not close");
}

fn create_provisional_store(path: &ProjectStorePath, snapshot: &ApplicationSnapshot) {
    let clock = ManualClock::default();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        clock.clone(),
        Some(path.clone()),
    )
    .unwrap();
    assert!(
        service
            .drive(snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .is_empty()
    );
    clock.set(seconds(30));
    assert!(matches!(
        service
            .drive(snapshot, |_| Ok(Vec::new()))
            .unwrap()
            .as_slice(),
        [ProjectStoreServiceEvent::AutosaveSubmitted { .. }]
    ));
    wait_for_autosave(&mut service, snapshot).expect("provisional autosave must succeed");
    close_service(&mut service, snapshot);
    service.join().unwrap();
}

fn create_established_store(path: &ProjectStorePath) {
    let mut application = verified_bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let (token, projection) = project_save_request(&mut application);
    let snapshot = application.snapshot();
    let mut service = ProjectStoreApplicationService::start(
        ProjectStoreConfig::default(),
        ManualClock::default(),
        None,
    )
    .unwrap();
    service
        .submit_save(token.clone(), projection, Some(path.clone()), Vec::new())
        .unwrap();
    assert!(matches!(
        wait_for_foreground_completion(&mut service, &snapshot),
        ProjectStoreServiceEvent::Created {
            token: completed,
            ..
        } if completed == token
    ));
    close_service(&mut service, &snapshot);
    service.join().unwrap();
}

fn project_open_request(application: &mut ApplicationState) -> OperationToken {
    application
        .dispatch(ApplicationCommand::RequestProjectOpen)
        .unwrap();
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectOpenRequested { token } => Some(token),
            _ => None,
        })
        .expect("project open request")
}

fn project_save_as_request(
    application: &mut ApplicationState,
) -> (OperationToken, ProjectGenerationProjection) {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectSaveAsRequested { token, projection } => {
                Some((token, projection.as_ref().clone()))
            }
            _ => None,
        })
        .expect("project Save As request")
}

fn project_save_request(
    application: &mut ApplicationState,
) -> (OperationToken, ProjectGenerationProjection) {
    application
        .dispatch(ApplicationCommand::RequestProjectSave)
        .unwrap();
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectSaveRequested { token, projection } => {
                Some((token, projection.as_ref().clone()))
            }
            _ => None,
        })
        .expect("project save request")
}

fn verified_bound_application() -> ApplicationState {
    let mut application = verified_unbound_application();
    application
        .dispatch(ApplicationCommand::AttachVerifiedDataset)
        .unwrap();
    application
}

fn verified_unbound_application() -> ApplicationState {
    let project_id = ProjectId::from_bytes([7; 16]);
    let layer = LogicalLayerKey::new(0);
    let transfer = LayerTransfer::new(
        DisplayWindow::new(0.0, 1.0).unwrap(),
        RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
        Opacity::new(1.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let view = ViewState::new(
        vec![LayerViewState::new(
            layer,
            true,
            transfer,
            RenderState::mip(SamplingPolicy::SmoothLinear),
        )],
        layer,
        TimeIndex::new(0),
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            320.0,
            10.0,
        )
        .unwrap(),
        ViewerLayout::Single3d,
        CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0).unwrap(),
        IsoLightState::attached_camera(),
    )
    .unwrap();
    let catalog = DatasetCatalog::new(
        "service-test",
        ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
        vec![
            DatasetLayer::new(
                layer,
                "layer",
                Shape4D::new(2, 2, 2, 2).unwrap(),
                IntensityDType::Uint16,
                GridToWorld::identity(),
                ResourceValidity::AllValid,
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let mut application = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        catalog,
        UnboundWorkspace::new(project_id, view, Vec::new()).unwrap(),
        ResourcePolicy::new(4 * GIB, GIB).unwrap(),
    )
    .unwrap();
    application
        .dispatch(ApplicationCommand::RequestSourceVerification)
        .unwrap();
    let token = application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::SourceVerificationRequested { token } => Some(token),
            _ => None,
        })
        .unwrap();
    let identity = ScientificContentId::parse(&format!(
        "{}{}",
        ScientificContentId::PREFIX,
        "1".repeat(64)
    ))
    .unwrap();
    let dataset = DatasetReference::new(identity, None, None, None);
    let verified_catalog = Arc::new(
        DatasetCatalog::new(
            application.snapshot().catalog().label(),
            ScientificIdentityStatus::Verified(identity),
            application.snapshot().catalog().layers().cloned().collect(),
        )
        .unwrap(),
    );
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::SourceVerified {
                source_generation: SourceSessionGeneration::new(1),
                catalog: verified_catalog,
                dataset,
            },
        })
        .unwrap();
    application
}

struct TestDirectory {
    path: std::path::PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "mirante4d-project-store-service-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
