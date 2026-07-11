use std::sync::Arc;

use mirante4d_dataset::{DatasetCatalog, DatasetLayer, ScientificIdentityStatus};
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, GridToWorld, IntensityDType, IsoLightState,
    LayerTransfer, Opacity, Projection, RenderState, RgbColor, SamplingPolicy, Shape4D, TimeIndex,
    ToolKind, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
};
use mirante4d_identity::{
    ArtifactContentId, ExactBytesDigest, MediaType, ObjectRole, RawObjectDescriptor,
};
use mirante4d_project_model::{
    ArtifactCompleteness, ArtifactRecoverability, ArtifactSchema, ChannelPresetEntry,
    DatasetLocatorHint,
};
use mirante4d_settings::{GIB, RejectedFileDisposition};

use super::*;

#[test]
fn unbound_view_changes_are_transient_and_invalid_or_noop_commands_are_atomic() {
    let mut application = application();

    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(3)))
            .unwrap(),
        CommandEffect::Changed
    );
    let changed = application.snapshot();
    assert_eq!(changed.currentness().get(), 1);
    assert_eq!(changed.dirty(), None);
    assert_eq!(unbound_view(&changed).timepoint(), TimeIndex::new(3));

    let before_noop = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(3)))
            .unwrap(),
        CommandEffect::NoChange
    );
    assert_eq!(application, before_noop);

    let before_invalid = application.fork_for_dispatch();
    let fault = application
        .dispatch(ApplicationCommand::SetActiveLayer(LogicalLayerKey::new(99)))
        .unwrap_err();
    assert_eq!(fault.code(), ApplicationFaultCode::LayerNotFound);
    assert_eq!(application, before_invalid);
}

#[test]
fn catalog_is_owned_by_snapshot_and_closes_every_view_transition() {
    let application = application();
    let first = application.snapshot();
    let second = application.snapshot();
    assert!(Arc::ptr_eq(first.catalog(), second.catalog()));
    assert_eq!(first.catalog().layers().count(), first.catalog().len());

    let missing_layer_catalog = DatasetCatalog::new(
        "incomplete",
        ScientificIdentityStatus::Unverified,
        vec![dataset_layer(0, 4)],
    )
    .unwrap();
    assert_eq!(
        ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            missing_layer_catalog,
            unbound_workspace(project_id(1)),
            ResourcePolicy::default(),
        )
        .unwrap_err(),
        ApplicationFaultCode::DatasetLayerClosureMismatch
    );

    let mut bounded = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        catalog_with_timepoints(3),
        unbound_workspace(project_id(2)),
        ResourcePolicy::default(),
    )
    .unwrap();
    let before = bounded.fork_for_dispatch();
    assert_eq!(
        bounded
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(3)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::TimepointOutOfBounds
    );
    assert_eq!(bounded, before);
}

#[test]
fn replace_view_and_apply_preset_are_each_one_atomic_view_transition() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);

    let replaced = ViewState::new(
        view().layers().to_vec(),
        LogicalLayerKey::new(1),
        TimeIndex::new(7),
        camera(),
        ViewerLayout::Single3d,
        cross_section(),
        IsoLightState::attached_camera(),
    )
    .unwrap();
    application
        .dispatch(ApplicationCommand::ReplaceView(replaced))
        .unwrap();
    assert_bound_revision(&application.snapshot(), 1, 1, true);
    let WorkspaceSnapshot::Bound { project, .. } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.view().active_layer(), LogicalLayerKey::new(1));
    assert_eq!(project.view().timepoint(), TimeIndex::new(7));

    let vivid = vivid_preset();
    let vivid_id = vivid.id().clone();
    application
        .dispatch(ApplicationCommand::UpsertChannelPreset(vivid))
        .unwrap();
    assert_bound_revision(&application.snapshot(), 2, 2, true);
    assert_eq!(
        application.snapshot().transient().selected_channel_preset(),
        Some(&vivid_id)
    );

    application
        .dispatch(ApplicationCommand::ApplyChannelPreset(vivid_id.clone()))
        .unwrap();
    let snapshot = application.snapshot();
    assert_bound_revision(&snapshot, 3, 3, true);
    let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() else {
        panic!("workspace was not bound");
    };
    assert!(project.view().layers().iter().all(|layer| !layer.visible()));
    assert_eq!(
        snapshot.transient().selected_channel_preset(),
        Some(&vivid_id)
    );

    let before_noop = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::ApplyChannelPreset(vivid_id))
            .unwrap(),
        CommandEffect::NoChange
    );
    assert_eq!(application, before_noop);
}

#[test]
fn logical_playback_ticks_replace_wall_clock_state_and_obey_catalog_bounds() {
    let mut application = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        catalog_with_timepoints(3),
        unbound_workspace(project_id(2)),
        ResourcePolicy::default(),
    )
    .unwrap();
    application
        .dispatch(ApplicationCommand::SetPlaybackActive(true))
        .unwrap();
    application.drain_events(MAX_PENDING_EVENTS);

    application
        .dispatch(ApplicationCommand::AdvancePlaybackTick(100))
        .unwrap();
    assert_eq!(
        application.snapshot().transient().last_playback_tick(),
        Some(100)
    );
    assert_eq!(
        unbound_view(&application.snapshot()).timepoint(),
        TimeIndex::new(0)
    );
    assert_eq!(
        application.snapshot().currentness(),
        CurrentnessGeneration::initial()
    );

    assert_eq!(
        application
            .dispatch(ApplicationCommand::AdvancePlaybackTick(100))
            .unwrap(),
        CommandEffect::NoChange
    );
    application
        .dispatch(ApplicationCommand::AdvancePlaybackTick(101))
        .unwrap();
    application
        .dispatch(ApplicationCommand::AdvancePlaybackTick(102))
        .unwrap();
    application
        .dispatch(ApplicationCommand::AdvancePlaybackTick(103))
        .unwrap();
    assert_eq!(
        unbound_view(&application.snapshot()).timepoint(),
        TimeIndex::new(0)
    );
    assert_eq!(application.snapshot().currentness().get(), 3);
    assert_eq!(
        application.snapshot().transient().last_playback_tick(),
        Some(103)
    );

    application
        .dispatch(ApplicationCommand::SetPlaybackActive(false))
        .unwrap();
    assert_eq!(
        application.snapshot().transient().last_playback_tick(),
        None
    );

    let mut single = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        catalog_with_timepoints(1),
        unbound_workspace(project_id(3)),
        ResourcePolicy::default(),
    )
    .unwrap();
    assert_eq!(
        single
            .dispatch(ApplicationCommand::SetPlaybackActive(true))
            .unwrap(),
        CommandEffect::NoChange
    );
}

#[test]
fn all_project_io_and_attachment_are_identity_gated_before_verification() {
    let mut application = application();
    for command in [
        ApplicationCommand::AttachVerifiedDataset,
        ApplicationCommand::RequestProjectOpen,
        ApplicationCommand::RequestProjectSave,
    ] {
        let before = application.fork_for_dispatch();
        assert_eq!(
            application.dispatch(command).unwrap_err().code(),
            ApplicationFaultCode::IdentityVerificationRequired
        );
        assert_eq!(application, before);
    }
}

#[test]
fn source_verification_requires_the_exact_current_session_generation() {
    let mut application = application();
    let before = application.fork_for_dispatch();
    let fault = application
        .admit_verified_source_for_test(SourceSessionGeneration::new(2), dataset_reference('1'))
        .unwrap_err();
    assert_eq!(fault, ApplicationFaultCode::SourceSessionMismatch);
    assert_eq!(application, before);

    let reference = dataset_reference('1');
    let identity = *reference.scientific_content_id();
    verify(&mut application, reference);
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Verified(_)
    ));
    assert_eq!(
        application
            .snapshot()
            .catalog()
            .scientific_identity()
            .verified_id(),
        Some(&identity)
    );

    let verified_catalog = DatasetCatalog::new(
        "preverified",
        ScientificIdentityStatus::Verified(identity),
        vec![dataset_layer(0, 4), dataset_layer(1, 4)],
    )
    .unwrap();
    assert_eq!(
        ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            verified_catalog,
            unbound_workspace(project_id(3)),
            ResourcePolicy::default(),
        )
        .unwrap_err(),
        ApplicationFaultCode::DatasetIdentityMismatch
    );
}

#[test]
fn verified_attachment_creates_the_only_project_state_and_revision_authority() {
    let mut application = application();
    verify(&mut application, dataset_reference('1'));

    application
        .dispatch(ApplicationCommand::AttachVerifiedDataset)
        .unwrap();
    let snapshot = application.snapshot();
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water,
        saved_revision,
        dirty,
        ..
    } = snapshot.workspace()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.project_id(), project_id(4));
    assert_eq!(revision.sequence(), 0);
    assert_eq!(revision_high_water.sequence(), 0);
    assert_eq!(*saved_revision, None);
    assert!(*dirty);

    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    assert_bound_revision(&application.snapshot(), 1, 1, true);
}

#[test]
fn bound_noops_and_invalid_commands_do_not_advance_revision_or_high_water() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);

    let before_noop = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(0)))
            .unwrap(),
        CommandEffect::NoChange
    );
    assert_eq!(application, before_noop);

    let before_invalid = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetActiveLayer(LogicalLayerKey::new(77)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::LayerNotFound
    );
    assert_eq!(application, before_invalid);
    assert_bound_revision(&application.snapshot(), 0, 0, true);
}

#[test]
fn undo_redo_and_branching_never_reuse_a_revision_sequence() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    for timepoint in [1, 2] {
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(timepoint)))
            .unwrap();
    }
    assert_bound_revision(&application.snapshot(), 2, 2, true);

    application.dispatch(ApplicationCommand::Undo).unwrap();
    assert_bound_revision(&application.snapshot(), 1, 2, true);
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(3)))
        .unwrap();
    assert_bound_revision(&application.snapshot(), 3, 3, true);

    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::Redo)
            .unwrap_err()
            .code(),
        ApplicationFaultCode::RedoUnavailable
    );
    assert_eq!(application, before);
}

#[test]
fn retained_history_is_bounded_without_rolling_back_high_water() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    for sequence in 1..=(MAX_HISTORY_ENTRIES as u64 + 12) {
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(sequence)))
            .unwrap();
        application.drain_events(MAX_PENDING_EVENTS);
    }
    let WorkspaceSnapshot::Bound {
        revision,
        revision_high_water,
        retained_history_entries,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(retained_history_entries, MAX_HISTORY_ENTRIES);
    assert_eq!(revision.sequence(), MAX_HISTORY_ENTRIES as u64 + 12);
    assert_eq!(revision_high_water.sequence(), revision.sequence());
}

#[test]
fn save_completion_for_captured_revision_survives_a_later_edit_and_remains_dirty() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectSave)
        .unwrap();
    let (token, captured_revision) = save_request(&mut application);
    assert_eq!(captured_revision.sequence(), 0);

    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectSaved(captured_revision),
        })
        .unwrap();

    let WorkspaceSnapshot::Bound {
        revision,
        saved_revision,
        dirty,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(revision.sequence(), 1);
    assert_eq!(saved_revision, Some(captured_revision));
    assert!(dirty);
}

#[test]
fn exact_current_save_completion_marks_the_project_clean() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectSave)
        .unwrap();
    let (token, revision) = save_request(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectSaved(revision),
        })
        .unwrap();
    assert_eq!(application.snapshot().dirty(), Some(false));
}

#[test]
fn a_second_save_is_rejected_until_the_active_save_finishes() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectSave)
        .unwrap();
    let before = application.fork_for_dispatch();

    assert_eq!(
        application
            .dispatch(ApplicationCommand::RequestProjectSave)
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationConflict
    );
    assert_eq!(application, before);
}

#[test]
fn verified_project_open_restores_revision_and_rejects_another_scientific_identity() {
    let mut application = application();
    let verified = dataset_reference('1');
    verify(&mut application, verified.clone());
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectOpen)
        .unwrap();
    let token = project_open_token(&mut application);
    let opened_projection = projection(project_id(8), verified, 5, 7);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectOpened(Box::new(opened_projection)),
        })
        .unwrap();
    assert_bound_revision(&application.snapshot(), 5, 7, false);

    let mut mismatched = application_for_project_open('1');
    let token = project_open_token(&mut mismatched);
    let before = mismatched.fork_for_dispatch();
    let fault = mismatched
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectOpened(Box::new(projection(
                project_id(9),
                dataset_reference('2'),
                0,
                0,
            ))),
        })
        .unwrap_err();
    assert_eq!(fault.code(), ApplicationFaultCode::DatasetIdentityMismatch);
    assert_eq!(mismatched, before);
}

#[test]
fn project_open_rebinds_same_content_to_the_verified_reference_as_dirty() {
    let verified = dataset_reference('1');
    let relocated = dataset_reference_at('1', "relocated-dataset.m4d");
    assert!(verified.has_same_scientific_content(&relocated));
    assert_ne!(verified, relocated);

    let mut application = application_for_project_open('1');
    let token = project_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectOpened(Box::new(projection(
                project_id(9),
                relocated,
                0,
                0,
            ))),
        })
        .unwrap();

    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water,
        saved_revision,
        dirty,
        can_undo,
        retained_history_entries,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.dataset(), &verified);
    assert_eq!(revision.sequence(), 1);
    assert_eq!(revision_high_water.sequence(), 1);
    assert_eq!(saved_revision.map(ProjectRevisionId::sequence), Some(0));
    assert!(dirty);
    assert!(!can_undo);
    assert_eq!(retained_history_entries, 1);
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::ProjectRevisionChanged {
                    revision,
                    dirty: true,
                    ..
                } if revision.sequence() == 1
            ))
    );
}

#[test]
fn unverified_analysis_and_import_operations_are_allowed_but_cannot_admit_artifacts() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    assert_eq!(token.source_identity(), None);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::Succeeded,
        })
        .unwrap();

    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Import))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Import);
    let before = application.fork_for_dispatch();
    let fault = application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ArtifactReady(Box::new(artifact(7))),
        })
        .unwrap_err();
    assert_eq!(
        fault.code(),
        ApplicationFaultCode::InvalidOperationCompletion
    );
    assert_eq!(application, before);
}

#[test]
fn stale_and_mismatched_completions_are_rejected_with_operation_context() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    let before_stale = application.fork_for_dispatch();
    let fault = application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: token.clone(),
            completion: OperationCompletion::Succeeded,
        })
        .unwrap_err();
    assert_eq!(fault.code(), ApplicationFaultCode::StaleOperationCompletion);
    assert_eq!(fault.operation_id(), Some(token.operation_id()));
    assert_eq!(fault.task_id(), Some(token.task_id()));
    assert_eq!(application, before_stale);

    let mut mismatched = token.clone();
    mismatched.task_id = TaskId(999);
    let before_mismatch = application.fork_for_dispatch();
    let fault = application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: mismatched.clone(),
            completion: OperationCompletion::Succeeded,
        })
        .unwrap_err();
    assert_eq!(fault.code(), ApplicationFaultCode::OperationTokenMismatch);
    assert_eq!(fault.task_id(), Some(mismatched.task_id()));
    assert_eq!(application, before_mismatch);
}

#[test]
fn stale_dataset_open_completion_can_be_explicitly_retired_after_atomic_rejection() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();

    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: token.clone(),
                completion: OperationCompletion::Failed(OperationFailureCode::DatasetReadFailed,),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::StaleOperationCompletion
    );
    assert_eq!(
        application.snapshot().active_operations(),
        std::slice::from_ref(&token)
    );

    application
        .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
        .unwrap();
    assert!(application.snapshot().active_operations().is_empty());
}

#[test]
fn operation_failure_codes_are_closed_over_their_operation_kind() {
    let cases = [
        (
            OperationKind::DatasetOpen,
            OperationFailureCode::DatasetReadFailed,
            OperationFailureCode::AnalysisExecutionFailed,
        ),
        (
            OperationKind::ProjectOpen,
            OperationFailureCode::ProjectReadFailed,
            OperationFailureCode::ProjectWriteFailed,
        ),
        (
            OperationKind::ProjectSave,
            OperationFailureCode::ProjectCommitIndeterminate,
            OperationFailureCode::ProjectNotFound,
        ),
        (
            OperationKind::Analysis,
            OperationFailureCode::AnalysisCapacityExceeded,
            OperationFailureCode::ImportCapacityExceeded,
        ),
        (
            OperationKind::Import,
            OperationFailureCode::ImportExecutionFailed,
            OperationFailureCode::DatasetInvalid,
        ),
    ];

    for (kind, accepted, rejected) in cases {
        assert!(completion_matches_kind(
            kind,
            &OperationCompletion::Failed(accepted)
        ));
        assert!(!completion_matches_kind(
            kind,
            &OperationCompletion::Failed(rejected)
        ));
    }
}

#[test]
fn verified_analysis_artifact_admission_is_one_durable_revision() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ArtifactReady(Box::new(artifact(7))),
        })
        .unwrap();
    let WorkspaceSnapshot::Bound {
        project, revision, ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(revision.sequence(), 1);
    assert_eq!(project.artifacts().len(), 1);
    assert!(application.snapshot().active_operations().is_empty());
}

#[test]
fn analysis_ready_appends_bounded_descriptors_with_deterministic_ids_and_selections() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    let table_0 =
        AnalysisTableDescriptor::new(AnalysisTableId::from_operation(token.operation_id(), 0), 12);
    let table_1 =
        AnalysisTableDescriptor::new(AnalysisTableId::from_operation(token.operation_id(), 1), 5);
    let plot_0 = AnalysisPlotDescriptor::new(
        AnalysisPlotId::from_operation(token.operation_id(), 0),
        vec![2, 3],
    )
    .unwrap();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: token.clone(),
            completion: OperationCompletion::AnalysisReady {
                tables: vec![table_0.clone(), table_1.clone()],
                plots: vec![plot_0.clone()],
            },
        })
        .unwrap();

    let snapshot = application.snapshot();
    let transient = snapshot.transient();
    assert_eq!(transient.analysis_tables(), &[table_0, table_1.clone()]);
    assert_eq!(transient.analysis_plots(), std::slice::from_ref(&plot_0));
    assert_eq!(transient.selected_analysis_table(), Some(table_1.id()));
    assert_eq!(transient.selected_analysis_plot(), Some(plot_0.id()));
    assert_eq!(transient.selected_analysis_plot_point(), None);
    assert!(snapshot.active_operations().is_empty());
    assert_eq!(snapshot.dirty(), None);
    assert_eq!(snapshot.currentness(), CurrentnessGeneration::initial());

    let before_missing = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SelectAnalysisTable(Some(
                AnalysisTableId::from_operation(OperationId(999), 0),
            )))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::AnalysisTableNotFound
    );
    assert_eq!(application, before_missing);

    let point = AnalysisPlotPointSelection::new(plot_0.id(), 1, 2);
    application
        .dispatch(ApplicationCommand::SelectAnalysisPlotPoint(Some(point)))
        .unwrap();
    assert_eq!(
        application
            .snapshot()
            .transient()
            .selected_analysis_plot_point(),
        Some(point)
    );

    let before_invalid = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SelectAnalysisPlotPoint(Some(
                AnalysisPlotPointSelection::new(plot_0.id(), 1, 3),
            )))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::AnalysisPointOutOfBounds
    );
    assert_eq!(application, before_invalid);

    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let next = started_token(&mut application, OperationKind::Analysis);
    let next_table =
        AnalysisTableDescriptor::new(AnalysisTableId::from_operation(next.operation_id(), 0), 7);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: next,
            completion: OperationCompletion::AnalysisReady {
                tables: vec![next_table.clone()],
                plots: Vec::new(),
            },
        })
        .unwrap();
    let transient = application.snapshot().transient().clone();
    assert_eq!(transient.analysis_tables().len(), 3);
    assert_eq!(transient.analysis_plots(), std::slice::from_ref(&plot_0));
    assert_eq!(transient.selected_analysis_table(), Some(next_table.id()));
    assert_eq!(transient.selected_analysis_plot(), Some(plot_0.id()));
    assert_eq!(transient.selected_analysis_plot_point(), Some(point));
}

#[test]
fn analysis_descriptors_reject_wrong_operation_slots_and_all_bounds_atomically() {
    assert_eq!(
        AnalysisPlotDescriptor::new(
            AnalysisPlotId::from_operation(OperationId(1), 0),
            vec![0; MAX_ANALYSIS_PLOT_SERIES + 1],
        ),
        Err(AnalysisDescriptorError::TooManySeries)
    );
    assert_eq!(
        AnalysisPlotDescriptor::new(
            AnalysisPlotId::from_operation(OperationId(1), 0),
            vec![MAX_ANALYSIS_PLOT_POINTS + 1],
        ),
        Err(AnalysisDescriptorError::TooManyPoints)
    );
    assert_eq!(
        AnalysisPlotDescriptor::new(
            AnalysisPlotId::from_operation(OperationId(1), 0),
            vec![u64::MAX, 1],
        ),
        Err(AnalysisDescriptorError::PointCountOverflow)
    );

    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: token.clone(),
                completion: OperationCompletion::AnalysisReady {
                    tables: vec![AnalysisTableDescriptor::new(
                        AnalysisTableId::from_operation(token.operation_id(), 1),
                        1,
                    )],
                    plots: Vec::new(),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidOperationCompletion
    );
    assert_eq!(application, before);

    let too_many = (0..=MAX_ANALYSIS_TABLES)
        .map(|slot| {
            AnalysisTableDescriptor::new(
                AnalysisTableId::from_operation(token.operation_id(), u16::try_from(slot).unwrap()),
                0,
            )
        })
        .collect();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::AnalysisReady {
                    tables: too_many,
                    plots: Vec::new(),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::AnalysisRegistryFull
    );
    assert_eq!(application, before);
}

#[test]
fn operation_registry_and_event_queue_are_hard_bounded() {
    let mut operations = application();
    for _ in 0..MAX_ACTIVE_OPERATIONS {
        operations
            .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
            .unwrap();
        operations.drain_events(MAX_PENDING_EVENTS);
    }
    let before = operations.fork_for_dispatch();
    assert_eq!(
        operations
            .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationRegistryFull
    );
    assert_eq!(operations, before);
    assert_eq!(
        operations.snapshot().active_operations().len(),
        MAX_ACTIVE_OPERATIONS
    );

    let mut events = application();
    for timepoint in 1..=MAX_PENDING_EVENTS as u64 {
        events
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(timepoint)))
            .unwrap();
    }
    let before = events.fork_for_dispatch();
    assert_eq!(
        events
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(999)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::EventQueueFull
    );
    assert_eq!(events, before);
    assert_eq!(events.snapshot().pending_event_count(), MAX_PENDING_EVENTS);
}

#[test]
fn resource_policy_changes_are_pending_then_persisted_or_rejected_with_restart_intent() {
    let mut application = application();
    let original = application.snapshot().resource_policy();
    let next = ResourcePolicy::new(8 * GIB, 4 * GIB).unwrap();
    let other = ResourcePolicy::new(12 * GIB, 5 * GIB).unwrap();

    application
        .dispatch(ApplicationCommand::RequestResourcePolicyChange {
            policy: next,
            rejected_file_disposition: RejectedFileDisposition::Preserve,
        })
        .unwrap();
    let next_token = application.snapshot().pending_settings_change().unwrap();
    assert_eq!(application.snapshot().resource_policy(), original);
    assert_eq!(application.snapshot().pending_resource_policy(), Some(next));
    assert_eq!(
        application
            .dispatch(ApplicationCommand::RequestResourcePolicyChange {
                policy: next,
                rejected_file_disposition: RejectedFileDisposition::Preserve,
            })
            .unwrap(),
        CommandEffect::NoChange
    );
    let before_mismatch = application.fork_for_dispatch();
    let mismatched_token = SettingsChangeToken {
        id: next_token.id(),
        policy: other,
        rejected_file_disposition: RejectedFileDisposition::Preserve,
    };
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
                token: mismatched_token,
                outcome: ResourcePolicyPersistenceOutcome::Persisted,
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::ResourcePolicyCompletionMismatch
    );
    assert_eq!(application, before_mismatch);

    application
        .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
            token: next_token,
            outcome: ResourcePolicyPersistenceOutcome::Persisted,
        })
        .unwrap();
    assert_eq!(application.snapshot().resource_policy(), next);
    assert_eq!(application.snapshot().pending_resource_policy(), None);
    assert!(application.snapshot().latest_problem().is_none());
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| {
                matches!(
                    event,
                    ApplicationEvent::ResourcePolicyPersisted {
                        token,
                        restart_required: true
                    } if token.policy() == next
                )
            })
    );

    application
        .dispatch(ApplicationCommand::RequestResourcePolicyChange {
            policy: other,
            rejected_file_disposition: RejectedFileDisposition::Preserve,
        })
        .unwrap();
    let other_token = application.snapshot().pending_settings_change().unwrap();
    assert_ne!(other_token.id(), next_token.id());
    let before_stale = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
                token: next_token,
                outcome: ResourcePolicyPersistenceOutcome::Persisted,
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::ResourcePolicyCompletionMismatch
    );
    assert_eq!(application, before_stale);
    application
        .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
            token: other_token,
            outcome: ResourcePolicyPersistenceOutcome::Rejected(
                ResourcePolicyRejection::CommitIndeterminate,
            ),
        })
        .unwrap();
    assert_eq!(application.snapshot().resource_policy(), next);
    assert_eq!(application.snapshot().pending_resource_policy(), None);
    assert!(matches!(
        application.snapshot().latest_problem(),
        Some(ApplicationEvent::ResourcePolicyRejected {
            reason: ResourcePolicyRejection::CommitIndeterminate,
            ..
        })
    ));
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| {
                matches!(
                    event,
                    ApplicationEvent::ResourcePolicyRejected {
                        token,
                        reason: ResourcePolicyRejection::CommitIndeterminate,
                    } if *token == other_token
                )
            })
    );
}

#[test]
fn settings_replacement_disposition_is_part_of_the_atomic_change_token() {
    let mut application = application();
    let active = application.snapshot().resource_policy();
    application
        .dispatch(ApplicationCommand::RequestResourcePolicyChange {
            policy: active,
            rejected_file_disposition: RejectedFileDisposition::Preserve,
        })
        .unwrap();
    let preserve_token = application.snapshot().pending_settings_change().unwrap();
    assert_eq!(preserve_token.policy(), active);
    application
        .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
            token: preserve_token,
            outcome: ResourcePolicyPersistenceOutcome::Persisted,
        })
        .unwrap();

    application
        .dispatch(ApplicationCommand::RequestResourcePolicyChange {
            policy: active,
            rejected_file_disposition: RejectedFileDisposition::ReplaceExplicitly,
        })
        .unwrap();
    let token = application.snapshot().pending_settings_change().unwrap();
    assert_eq!(token.policy(), active);
    assert_eq!(
        token.rejected_file_disposition(),
        RejectedFileDisposition::ReplaceExplicitly
    );

    let before = application.fork_for_dispatch();
    let mismatched = SettingsChangeToken {
        id: token.id(),
        policy: active,
        rejected_file_disposition: RejectedFileDisposition::Preserve,
    };
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
                token: mismatched,
                outcome: ResourcePolicyPersistenceOutcome::Rejected(
                    ResourcePolicyRejection::ExplicitReplacementRequired,
                ),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::ResourcePolicyCompletionMismatch
    );
    assert_eq!(application, before);
}

#[test]
fn transient_commands_never_dirty_or_advance_the_project_revision() {
    let mut application = bound_application();
    let before = application.snapshot();
    for command in [
        ApplicationCommand::SetPlaybackActive(true),
        ApplicationCommand::SetActiveTool(ToolKind::Inspect),
        ApplicationCommand::SelectChannelPreset(Some(ChannelPresetId::new("all").unwrap())),
        ApplicationCommand::SetActiveCrossSectionPanel(Some(CrossSectionPanelId::Xy)),
    ] {
        application.dispatch(command).unwrap();
    }
    let after = application.snapshot();
    assert_bound_revision(&after, 0, 0, true);
    assert_eq!(after.currentness(), before.currentness());
    assert!(after.transient().playback_active());
    assert_eq!(after.transient().active_tool(), ToolKind::Inspect);

    let before_invalid = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SelectArtifact(Some(
                ArtifactHandleId::from_bytes([99; 16]),
            )))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::ArtifactNotFound
    );
    assert_eq!(application, before_invalid);
}

#[test]
fn selected_ids_are_normalized_after_removal_and_history_movement() {
    let mut application = bound_application();
    application
        .dispatch(ApplicationCommand::UpsertArtifact(artifact(7)))
        .unwrap();
    let artifact_id = ArtifactHandleId::from_bytes([7; 16]);
    application
        .dispatch(ApplicationCommand::SelectArtifact(Some(
            artifact_id.clone(),
        )))
        .unwrap();
    application
        .dispatch(ApplicationCommand::RemoveArtifact(artifact_id.clone()))
        .unwrap();
    assert_eq!(application.snapshot().transient().selected_artifact(), None);

    application.dispatch(ApplicationCommand::Undo).unwrap();
    application
        .dispatch(ApplicationCommand::SelectArtifact(Some(artifact_id)))
        .unwrap();
    application.dispatch(ApplicationCommand::Redo).unwrap();
    assert_eq!(application.snapshot().transient().selected_artifact(), None);

    let preset_id = ChannelPresetId::new("all").unwrap();
    application
        .dispatch(ApplicationCommand::SelectChannelPreset(Some(
            preset_id.clone(),
        )))
        .unwrap();
    application
        .dispatch(ApplicationCommand::RemoveChannelPreset(preset_id))
        .unwrap();
    assert_eq!(
        application.snapshot().transient().selected_channel_preset(),
        None
    );
}

#[test]
fn source_replacement_is_central_generation_guarded_and_resets_bound_transients() {
    let mut application = bound_application();
    application
        .dispatch(ApplicationCommand::SetPlaybackActive(true))
        .unwrap();
    application
        .dispatch(ApplicationCommand::SetActiveTool(ToolKind::MeasureDistance))
        .unwrap();
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::DatasetOpened {
                catalog: Arc::new(catalog(4)),
                workspace: Box::new(unbound_workspace(project_id(9))),
                source_generation: SourceSessionGeneration::new(2),
            },
        })
        .unwrap();
    let snapshot = application.snapshot();
    assert!(!snapshot.is_bound());
    assert!(matches!(
        snapshot.source(),
        SourceVerificationSnapshot::Required
    ));
    assert_eq!(snapshot.source_generation().get(), 2);
    assert_eq!(snapshot.catalog().label(), "catalog-4");
    assert_eq!(snapshot.transient(), &TransientApplicationState::default());

    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::DatasetOpened {
                    catalog: Arc::new(catalog(5)),
                    workspace: Box::new(unbound_workspace(project_id(10))),
                    source_generation: SourceSessionGeneration::new(2),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::SourceGenerationNotAdvanced
    );
    assert_eq!(application, before);
}

#[test]
fn dataset_open_request_rejects_while_an_operation_is_active() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Import))
        .unwrap();
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::RequestDatasetOpen)
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationConflict
    );
    assert_eq!(application, before);
}

#[test]
fn dataset_open_failure_is_typed_and_does_not_replace_the_source() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::Failed(OperationFailureCode::DatasetPermissionDenied),
        })
        .unwrap();
    let snapshot = application.snapshot();
    assert_eq!(
        snapshot.source_generation(),
        SourceSessionGeneration::new(1)
    );
    assert_eq!(snapshot.catalog().label(), "catalog-3");
    assert!(snapshot.active_operations().is_empty());
    assert!(matches!(
        snapshot.latest_problem(),
        Some(ApplicationEvent::OperationCompleted {
            outcome: OperationOutcome::Failed(OperationFailureCode::DatasetPermissionDenied),
            ..
        })
    ));
    application.drain_events(MAX_PENDING_EVENTS);
    assert!(application.snapshot().latest_problem().is_some());

    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    assert!(application.snapshot().latest_problem().is_none());
}

#[test]
fn project_execution_failure_is_distinct_from_reducer_rejection() {
    let mut application = application_for_project_open('1');
    let token = project_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: token.clone(),
            completion: OperationCompletion::Failed(OperationFailureCode::ProjectInvalidDocument),
        })
        .unwrap();
    assert!(!application.snapshot().is_bound());
    assert!(application.snapshot().active_operations().is_empty());
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::OperationCompleted {
                    token: completed,
                    outcome: OperationOutcome::Failed(OperationFailureCode::ProjectInvalidDocument),
                } if completed == &token
            ))
    );
}

#[test]
fn dataset_open_completion_rejects_verified_or_view_incompatible_catalogs_atomically() {
    let mut incompatible = application();
    incompatible
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut incompatible);
    let before = incompatible.fork_for_dispatch();
    let incomplete = DatasetCatalog::new(
        "incomplete",
        ScientificIdentityStatus::Unverified,
        vec![dataset_layer(0, 4)],
    )
    .unwrap();
    assert_eq!(
        incompatible
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::DatasetOpened {
                    source_generation: SourceSessionGeneration::new(2),
                    catalog: Arc::new(incomplete),
                    workspace: Box::new(unbound_workspace(project_id(7))),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::DatasetLayerClosureMismatch
    );
    assert_eq!(incompatible, before);

    let mut preverified = application();
    preverified
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut preverified);
    let reference = dataset_reference('7');
    let catalog = DatasetCatalog::new(
        "preverified",
        ScientificIdentityStatus::Verified(*reference.scientific_content_id()),
        vec![dataset_layer(0, 4), dataset_layer(1, 4)],
    )
    .unwrap();
    let before = preverified.fork_for_dispatch();
    assert_eq!(
        preverified
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::DatasetOpened {
                    source_generation: SourceSessionGeneration::new(2),
                    catalog: Arc::new(catalog),
                    workspace: Box::new(unbound_workspace(project_id(8))),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::DatasetIdentityMismatch
    );
    assert_eq!(preverified, before);
}

#[test]
fn snapshots_and_candidate_clones_share_project_history_storage() {
    let mut application = bound_application();
    let first = bound_project_arc(&application.snapshot());
    let second = bound_project_arc(&application.snapshot());
    assert!(Arc::ptr_eq(&first, &second));

    application
        .dispatch(ApplicationCommand::SetPlaybackActive(true))
        .unwrap();
    let after_transient = bound_project_arc(&application.snapshot());
    assert!(Arc::ptr_eq(&first, &after_transient));
}

#[test]
fn unbound_workspace_rejects_a_preset_that_does_not_close_over_view_layers() {
    let incomplete = ChannelPreset::new(
        ChannelPresetId::new("incomplete").unwrap(),
        "Incomplete",
        vec![preset_entry(0)],
    )
    .unwrap();
    assert_eq!(
        UnboundWorkspace::new(project_id(1), view(), vec![incomplete]).unwrap_err(),
        ApplicationFaultCode::InvalidProjectTransition
    );
}

fn application() -> ApplicationState {
    ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        catalog(3),
        unbound_workspace(project_id(4)),
        ResourcePolicy::new(4 * GIB, GIB).unwrap(),
    )
    .unwrap()
}

fn bound_application() -> ApplicationState {
    let mut application = application();
    verify(&mut application, dataset_reference('1'));
    application
        .dispatch(ApplicationCommand::AttachVerifiedDataset)
        .unwrap();
    application
}

fn application_for_project_open(identity_digit: char) -> ApplicationState {
    let mut application = application();
    verify(&mut application, dataset_reference(identity_digit));
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectOpen)
        .unwrap();
    application
}

fn verify(application: &mut ApplicationState, dataset: DatasetReference) {
    application
        .admit_verified_source_for_test(SourceSessionGeneration::new(1), dataset)
        .unwrap();
}

fn unbound_workspace(id: ProjectId) -> UnboundWorkspace {
    UnboundWorkspace::new(id, view(), vec![preset()]).unwrap()
}

fn project_id(byte: u8) -> ProjectId {
    ProjectId::from_bytes([byte; 16])
}

fn dataset_reference(digit: char) -> DatasetReference {
    dataset_reference_at(digit, "dataset.m4d")
}

fn dataset_reference_at(digit: char, locator: &str) -> DatasetReference {
    let digest = digit.to_string().repeat(64);
    DatasetReference::new(
        ScientificContentId::parse(&format!("{}{}", ScientificContentId::PREFIX, digest)).unwrap(),
        None,
        None,
        Some(DatasetLocatorHint::new(locator).unwrap()),
    )
}

fn transfer() -> LayerTransfer {
    LayerTransfer::new(
        DisplayWindow::new(0.0, 1.0).unwrap(),
        RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
        Opacity::new(1.0).unwrap(),
        TransferCurve::linear(),
        false,
    )
}

fn layer(ordinal: u32) -> LayerViewState {
    LayerViewState::new(
        LogicalLayerKey::new(ordinal),
        true,
        transfer(),
        RenderState::mip(SamplingPolicy::SmoothLinear),
    )
}

fn preset_entry(ordinal: u32) -> ChannelPresetEntry {
    ChannelPresetEntry::new(
        LogicalLayerKey::new(ordinal),
        true,
        transfer(),
        RenderState::mip(SamplingPolicy::SmoothLinear),
    )
}

fn vivid_preset() -> ChannelPreset {
    ChannelPreset::new(
        ChannelPresetId::new("vivid").unwrap(),
        "Vivid",
        [0, 1]
            .into_iter()
            .map(|ordinal| {
                ChannelPresetEntry::new(
                    LogicalLayerKey::new(ordinal),
                    false,
                    transfer(),
                    RenderState::mip(SamplingPolicy::VoxelExact),
                )
            })
            .collect(),
    )
    .unwrap()
}

fn camera() -> CameraView {
    CameraView::new(
        Projection::Orthographic,
        WorldPoint3::origin(),
        UnitQuaternion::identity(),
        1.0,
        320.0,
        10.0,
    )
    .unwrap()
}

fn cross_section() -> CrossSectionView {
    CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0).unwrap()
}

fn view() -> ViewState {
    ViewState::new(
        vec![layer(0), layer(1)],
        LogicalLayerKey::new(0),
        TimeIndex::new(0),
        camera(),
        ViewerLayout::Single3d,
        cross_section(),
        IsoLightState::attached_camera(),
    )
    .unwrap()
}

fn catalog(label_digit: u8) -> DatasetCatalog {
    catalog_named_with_timepoints(format!("catalog-{label_digit}"), 10_000)
}

fn catalog_with_timepoints(timepoints: u64) -> DatasetCatalog {
    catalog_named_with_timepoints(format!("catalog-t{timepoints}"), timepoints)
}

fn catalog_named_with_timepoints(label: String, timepoints: u64) -> DatasetCatalog {
    DatasetCatalog::new(
        label,
        ScientificIdentityStatus::Unverified,
        [0, 1]
            .into_iter()
            .map(|ordinal| dataset_layer(ordinal, timepoints))
            .collect(),
    )
    .unwrap()
}

fn dataset_layer(ordinal: u32, timepoints: u64) -> DatasetLayer {
    DatasetLayer::new(
        LogicalLayerKey::new(ordinal),
        format!("layer-{ordinal}"),
        Shape4D::new(timepoints, 5, 7, 11).unwrap(),
        IntensityDType::Uint16,
        GridToWorld::identity(),
    )
    .unwrap()
}

fn preset() -> ChannelPreset {
    ChannelPreset::new(
        ChannelPresetId::new("all").unwrap(),
        "All",
        vec![preset_entry(0), preset_entry(1)],
    )
    .unwrap()
}

fn artifact(byte: u8) -> ArtifactReference {
    let zero = "0".repeat(64);
    let schema = ArtifactSchema::AnalysisTableV1;
    ArtifactReference::new(
        ArtifactHandleId::from_bytes([byte; 16]),
        schema,
        ArtifactContentId::parse(&format!("{}{}", ArtifactContentId::PREFIX, zero)).unwrap(),
        RawObjectDescriptor::new(
            ExactBytesDigest::parse(&format!("{}{}", ExactBytesDigest::PREFIX, "0".repeat(64)))
                .unwrap(),
            12,
            MediaType::parse(schema.media_type()).unwrap(),
            ObjectRole::parse(schema.object_role()).unwrap(),
        ),
        None,
        None,
        vec![LogicalLayerKey::new(0)],
        "Table",
        true,
        ArtifactCompleteness::Complete,
        ArtifactRecoverability::NonRegenerable,
    )
    .unwrap()
}

fn projection(
    id: ProjectId,
    dataset: DatasetReference,
    revision: u64,
    high_water: u64,
) -> ProjectGenerationProjection {
    let project = ProjectState::new(id, dataset, view(), vec![preset()], Vec::new()).unwrap();
    ProjectGenerationProjection::new(
        ProjectRevisionId::new(id, revision),
        ProjectRevisionHighWater::new(id, high_water),
        project,
    )
    .unwrap()
}

fn unbound_view(snapshot: &ApplicationSnapshot) -> &ViewState {
    let WorkspaceSnapshot::Unbound { workspace } = snapshot.workspace() else {
        panic!("workspace was bound");
    };
    workspace.view()
}

fn assert_bound_revision(
    snapshot: &ApplicationSnapshot,
    revision: u64,
    high_water: u64,
    dirty: bool,
) {
    let WorkspaceSnapshot::Bound {
        revision: actual_revision,
        revision_high_water,
        dirty: actual_dirty,
        ..
    } = snapshot.workspace()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(actual_revision.sequence(), revision);
    assert_eq!(revision_high_water.sequence(), high_water);
    assert_eq!(*actual_dirty, dirty);
}

fn started_token(application: &mut ApplicationState, kind: OperationKind) -> OperationToken {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::OperationStarted { token } if token.kind() == kind => Some(token),
            _ => None,
        })
        .expect("operation-start event")
}

fn project_open_token(application: &mut ApplicationState) -> OperationToken {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectOpenRequested { token } => Some(token),
            _ => None,
        })
        .expect("project-open event")
}

fn dataset_open_token(application: &mut ApplicationState) -> OperationToken {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::DatasetOpenRequested { token } => Some(token),
            _ => None,
        })
        .expect("dataset-open event")
}

fn save_request(application: &mut ApplicationState) -> (OperationToken, ProjectRevisionId) {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectSaveRequested { token, projection } => {
                Some((token, projection.revision()))
            }
            _ => None,
        })
        .expect("project-save event")
}

fn bound_project_arc(snapshot: &ApplicationSnapshot) -> Arc<ProjectState> {
    let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() else {
        panic!("workspace was not bound");
    };
    Arc::clone(project)
}
