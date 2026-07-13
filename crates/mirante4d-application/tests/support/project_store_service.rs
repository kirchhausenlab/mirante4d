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
fn real_actor_autosaves_exact_captures_without_advancing_manual_saved_revision() {
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
    let first_receipt = wait_for_autosave(&mut service, &first_snapshot);
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
    let second_receipt = wait_for_autosave(&mut service, &second_snapshot);
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
    assert!(matches!(
        service
            .handle_completion(ProjectStoreCompletion::Autosaved {
                request_id: request_id(1),
                result: Err(ProjectStoreFault::Cancelled),
            })
            .unwrap(),
        ProjectStoreServiceEvent::AutosaveFinished {
            result: Err(ProjectStoreFault::Cancelled),
            ..
        }
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
        acknowledged,
        ProjectStoreServiceEvent::CancellationAcknowledged { request_id: actual }
            if actual == request_id(2)
    ));
    let event = service
        .handle_completion(ProjectStoreCompletion::Autosaved {
            request_id: request_id(1),
            result: Err(ProjectStoreFault::CommitIndeterminate),
        })
        .unwrap();
    assert!(matches!(
        &event,
        ProjectStoreServiceEvent::AutosaveFinished {
            request_id: actual,
            revision: completed_revision,
            result: Err(ProjectStoreFault::CommitIndeterminate),
        } if *actual == request_id(1) && *completed_revision == revision
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
) -> ProjectStoreReceipt {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for event in service.drive(snapshot, |_| Ok(Vec::new())).unwrap() {
            if let ProjectStoreServiceEvent::AutosaveFinished { result, .. } = event {
                return result.unwrap();
            }
        }
        thread::yield_now();
    }
    panic!("project-store actor did not complete autosave");
}

fn verified_bound_application() -> ApplicationState {
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
        .dispatch(ApplicationCommand::AttachVerifiedDataset)
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
