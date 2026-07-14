use std::sync::Arc;

use mirante4d_dataset::{
    DatasetCatalog, DatasetLayer, DatasetResourceIdentity, DatasetResourceKey, DatasetSourceId,
    ResourceRegion, ScientificIdentityStatus,
};
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, GridToWorld, IntensityDType, IsoLightState,
    LayerTransfer, Opacity, Projection, RenderState, RgbColor, SamplingPolicy, ScaleLevel, Shape3D,
    Shape4D, TimeIndex, ToolKind, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
};
use mirante4d_identity::{
    ArtifactContentId, DerivationRecordId, ExactBytesDigest, MediaType, ObjectRole,
    RawObjectDescriptor, RecipeId,
};
use mirante4d_project_model::{
    ArtifactCompleteness, ArtifactRecoverability, ArtifactSchema, ChannelPresetEntry,
    DatasetLocatorHint,
};
use mirante4d_render_api::{
    FrameCompleteness, FrameCoverage, FrameIdentity, FrameProgress, LayerRenderIntent,
    PresentationToken, PresentationViewport, PresentedFrame, RenderExtent, RenderIntent,
    RenderRequirement, RenderRequirementRole, RenderRequirements, RenderViewIntent,
};
use mirante4d_settings::{GIB, RejectedFileDisposition};

use super::*;

#[test]
fn snapshot_carries_only_an_optional_backend_neutral_presentation_projection() {
    let snapshot = application().snapshot();
    assert_eq!(snapshot.presentation(), None);

    let resource_identity = DatasetResourceIdentity::Verified(
        ScientificContentId::parse(&format!(
            "{}{}",
            ScientificContentId::PREFIX,
            "0".repeat(64)
        ))
        .unwrap(),
    );
    let key = DatasetResourceKey::new(
        resource_identity,
        LogicalLayerKey::new(0),
        TimeIndex::new(0),
        ScaleLevel::BASE,
        ResourceRegion::new([0, 0, 0], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
    );
    let extent = RenderExtent::new(32, 24).unwrap();
    let render_intent = RenderIntent::new(
        FrameIdentity::new(7),
        resource_identity,
        TimeIndex::new(0),
        RenderViewIntent::volume(camera(), IsoLightState::attached_camera()),
        PresentationViewport::new(32.0, 24.0).unwrap(),
        extent,
        vec![LayerRenderIntent::new(
            LogicalLayerKey::new(0),
            transfer(),
            RenderState::mip(SamplingPolicy::SmoothLinear),
        )],
    )
    .unwrap();
    let requirements = RenderRequirements::new(
        &render_intent,
        vec![RenderRequirement::new(
            key,
            RenderRequirementRole::FirstUsefulFrame,
        )],
    )
    .unwrap();
    let presented = PresentedFrame::new(
        PresentationToken::new(1).unwrap(),
        extent,
        FrameProgress::new(
            FrameCoverage::from_available(&requirements, &[key]).unwrap(),
            FrameCompleteness::Exact,
            None,
        )
        .unwrap(),
    );
    let projected = snapshot.with_presentation(Some(presented.clone()));
    assert_eq!(projected.presentation(), Some(&presented));
}

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
        ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
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
        ApplicationCommand::RequestProjectSaveAs {
            new_project_id: project_id(5),
        },
        ApplicationCommand::RequestProjectRecovery,
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
    let token = request_source_verification(&mut application);
    application
        .dispatch(ApplicationCommand::UpdateSourceVerificationProgress {
            token: token.clone(),
            completed_work: 1,
            total_work: 1,
        })
        .unwrap();
    let before = application.fork_for_dispatch();
    let dataset = dataset_reference('1');
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    let fault = application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: token.clone(),
            completion: OperationCompletion::SourceVerified {
                source_generation: SourceSessionGeneration::new(2),
                catalog: Arc::clone(&catalog),
                dataset: dataset.clone(),
            },
        })
        .unwrap_err();
    assert_eq!(fault.code(), ApplicationFaultCode::SourceSessionMismatch);
    assert_eq!(application, before);

    let identity = *dataset.scientific_content_id();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::SourceVerified {
                source_generation: SourceSessionGeneration::new(1),
                catalog,
                dataset,
            },
        })
        .unwrap();
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
fn source_verification_progress_is_one_bounded_monotonic_observation() {
    let mut application = application();
    let token = request_source_verification(&mut application);
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Verifying {
            operation_id,
            completed_work: 0,
            total_work: 0,
        } if *operation_id == token.operation_id()
    ));

    application
        .dispatch(ApplicationCommand::UpdateSourceVerificationProgress {
            token: token.clone(),
            completed_work: 4,
            total_work: 10,
        })
        .unwrap();
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Verifying {
            completed_work: 4,
            total_work: 10,
            ..
        }
    ));

    for (completed_work, total_work) in [(3, 10), (4, 11), (11, 10), (0, 0)] {
        let before = application.fork_for_dispatch();
        assert_eq!(
            application
                .dispatch(ApplicationCommand::UpdateSourceVerificationProgress {
                    token: token.clone(),
                    completed_work,
                    total_work,
                })
                .unwrap_err()
                .code(),
            ApplicationFaultCode::InvalidOperationProgress
        );
        assert_eq!(application, before);
    }

    let dataset = dataset_reference('1');
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::SourceVerified {
                source_generation: SourceSessionGeneration::new(1),
                catalog,
                dataset,
            },
        })
        .unwrap();
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Verified(_)
    ));
}

#[test]
fn source_verification_completion_ignores_view_currentness_but_not_source_generation() {
    let mut application = application();
    let token = request_source_verification(&mut application);
    application
        .dispatch(ApplicationCommand::UpdateSourceVerificationProgress {
            token: token.clone(),
            completed_work: 7,
            total_work: 7,
        })
        .unwrap();
    let request_currentness = token.currentness_generation();
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    assert_ne!(application.snapshot().currentness(), request_currentness);

    let dataset = dataset_reference('1');
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::SourceVerified {
                source_generation: SourceSessionGeneration::new(1),
                catalog,
                dataset,
            },
        })
        .unwrap();
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Verified(_)
    ));
    assert_eq!(unbound_view(&application.snapshot()).timepoint().get(), 1);
}

#[test]
fn source_verification_rejects_catalog_or_reference_drift_atomically() {
    let mut application = application();
    let token = request_source_verification(&mut application);
    application
        .dispatch(ApplicationCommand::UpdateSourceVerificationProgress {
            token: token.clone(),
            completed_work: 1,
            total_work: 1,
        })
        .unwrap();
    let dataset = dataset_reference('1');
    let wrong_catalog = Arc::new(
        DatasetCatalog::new(
            "changed-label",
            ScientificIdentityStatus::Verified(*dataset.scientific_content_id()),
            application.snapshot().catalog().layers().cloned().collect(),
        )
        .unwrap(),
    );
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: token.clone(),
                completion: OperationCompletion::SourceVerified {
                    source_generation: SourceSessionGeneration::new(1),
                    catalog: wrong_catalog,
                    dataset: dataset.clone(),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::DatasetIdentityMismatch
    );
    assert_eq!(application, before);

    let mismatched_catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::SourceVerified {
                    source_generation: SourceSessionGeneration::new(1),
                    catalog: mismatched_catalog,
                    dataset: dataset_reference('2'),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::DatasetIdentityMismatch
    );
    assert_eq!(application, before);
}

#[test]
fn cancelled_source_verification_suppresses_its_late_prepared_completion() {
    let mut application = application();
    let token = request_source_verification(&mut application);
    application
        .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
        .unwrap();
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Required
    ));

    let dataset = dataset_reference('1');
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::SourceVerified {
                    source_generation: SourceSessionGeneration::new(1),
                    catalog,
                    dataset,
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationNotFound
    );
    assert_eq!(application, before);
}

#[test]
fn invalidated_source_verification_cannot_displace_its_successor() {
    let mut application = application();
    let superseded = request_source_verification(&mut application);
    let dataset = dataset_reference('1');
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);

    application
        .dispatch(ApplicationCommand::InvalidateSourceVerification {
            source_generation: SourceSessionGeneration::new(1),
        })
        .unwrap();
    let events = application.drain_events(MAX_PENDING_EVENTS);
    assert!(events.iter().any(|event| matches!(
        event,
        ApplicationEvent::OperationCancellationRequested { token }
            if token == &superseded
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ApplicationEvent::SourceVerificationInvalidated {
            source_generation
        } if *source_generation == SourceSessionGeneration::new(1)
    )));

    let successor = request_source_verification(&mut application);
    let before_late = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: superseded,
                completion: OperationCompletion::SourceVerified {
                    source_generation: SourceSessionGeneration::new(1),
                    catalog: Arc::clone(&catalog),
                    dataset: dataset.clone(),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationNotFound
    );
    assert_eq!(application, before_late);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: successor,
            completion: OperationCompletion::SourceVerified {
                source_generation: SourceSessionGeneration::new(1),
                catalog,
                dataset,
            },
        })
        .unwrap();
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Verified(_)
    ));
}

#[test]
fn failed_source_verification_never_admits_a_late_prepared_binding() {
    let mut application = application();
    let token = request_source_verification(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: token.clone(),
            completion: OperationCompletion::Failed(OperationFailureCode::SourceChanged),
        })
        .unwrap();
    assert!(matches!(
        application.snapshot().source(),
        SourceVerificationSnapshot::Required
    ));
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::SourceVerificationInvalidated {
                    source_generation
                } if *source_generation == SourceSessionGeneration::new(1)
            ))
    );

    let dataset = dataset_reference('1');
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::SourceVerified {
                    source_generation: SourceSessionGeneration::new(1),
                    catalog,
                    dataset,
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationNotFound
    );
    assert_eq!(application, before);
}

#[test]
fn observed_source_drift_invalidates_identity_and_cancels_project_persistence() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectSave)
        .unwrap();
    let (save_token, captured_revision) = save_request(&mut application);

    application
        .dispatch(ApplicationCommand::InvalidateSourceVerification {
            source_generation: SourceSessionGeneration::new(1),
        })
        .unwrap();
    let snapshot = application.snapshot();
    assert!(matches!(
        snapshot.source(),
        SourceVerificationSnapshot::Required
    ));
    assert_eq!(
        snapshot.catalog().scientific_identity(),
        &ScientificIdentityStatus::Unverified(DatasetSourceId::new(1))
    );
    assert_eq!(
        application
            .dispatch(ApplicationCommand::RequestProjectSave)
            .unwrap_err()
            .code(),
        ApplicationFaultCode::IdentityVerificationRequired
    );

    let before_late = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: save_token,
                completion: OperationCompletion::ProjectSaved(captured_revision),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationNotFound
    );
    assert_eq!(application, before_late);

    let before_wrong_generation = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::InvalidateSourceVerification {
                source_generation: SourceSessionGeneration::new(2),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::SourceSessionMismatch
    );
    assert_eq!(application, before_wrong_generation);
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
fn initial_unsaved_save_is_requested_but_a_clean_save_is_a_noop() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);

    assert_eq!(
        application
            .dispatch(ApplicationCommand::RequestProjectSave)
            .unwrap(),
        CommandEffect::Changed
    );
    let (token, revision) = save_request(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectSaved(revision),
        })
        .unwrap();
    application.drain_events(MAX_PENDING_EVENTS);

    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::RequestProjectSave)
            .unwrap(),
        CommandEffect::NoChange
    );
    assert_eq!(application, before);
    assert!(application.snapshot().active_operations().is_empty());
    assert_eq!(application.snapshot().pending_event_count(), 0);
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
fn save_as_retains_an_initial_new_id_projection_and_switches_only_on_exact_completion() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(3)))
        .unwrap();
    application.drain_events(MAX_PENDING_EVENTS);

    let old_project_id = project_id(4);
    let new_project_id = project_id(5);
    application
        .dispatch(ApplicationCommand::RequestProjectSaveAs { new_project_id })
        .unwrap();
    let (token, retained) = save_as_request(&mut application);
    assert_eq!(token.kind(), OperationKind::ProjectSaveAs);
    assert_eq!(token.project_id(), Some(old_project_id));
    assert_eq!(
        token.project_revision().map(ProjectRevisionId::sequence),
        Some(1)
    );
    assert_eq!(retained.state().project_id(), new_project_id);
    assert_eq!(
        retained.revision(),
        ProjectRevisionId::initial(new_project_id)
    );
    assert_eq!(
        retained.revision_high_water(),
        &ProjectRevisionHighWater::initial(new_project_id)
    );
    assert_eq!(retained.state().view().timepoint(), TimeIndex::new(3));

    let before_edit = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(4)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationConflict
    );
    assert_eq!(application, before_edit);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectSavedAs(Box::new(retained.as_ref().clone())),
        })
        .unwrap();
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water,
        saved_revision,
        dirty,
        retained_history_entries,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.project_id(), new_project_id);
    assert_eq!(revision, ProjectRevisionId::initial(new_project_id));
    assert_eq!(
        revision_high_water,
        ProjectRevisionHighWater::initial(new_project_id)
    );
    assert_eq!(saved_revision, Some(revision));
    assert!(!dirty);
    assert_eq!(retained_history_entries, 1);
}

#[test]
fn save_as_rejects_a_nonexact_completion_and_failure_retains_the_old_project() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let new_project_id = project_id(5);
    application
        .dispatch(ApplicationCommand::RequestProjectSaveAs { new_project_id })
        .unwrap();
    let (token, _retained) = save_as_request(&mut application);
    let mismatched = projection(new_project_id, dataset_reference('1'), 1, 1);

    let before_mismatch = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: token.clone(),
                completion: OperationCompletion::ProjectSavedAs(Box::new(mismatched)),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidOperationCompletion
    );
    assert_eq!(application, before_mismatch);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::Failed(OperationFailureCode::ProjectWriteFailed),
        })
        .unwrap();
    let WorkspaceSnapshot::Bound { project, .. } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.project_id(), project_id(4));
    assert!(application.snapshot().active_operations().is_empty());
}

#[test]
fn recovery_blocks_mutation_and_is_dirty_even_with_the_exact_verified_locator() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectRecovery)
        .unwrap();
    let token = recovery_request(&mut application);
    assert_eq!(token.kind(), OperationKind::ProjectRecovery);

    let before_mutation = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(2)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationConflict
    );
    assert_eq!(application, before_mutation);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectRecovered(Box::new(projection(
                project_id(4),
                dataset_reference('1'),
                5,
                7,
            ))),
        })
        .unwrap();
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water,
        saved_revision,
        dirty,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.dataset(), &dataset_reference('1'));
    assert_eq!(revision.sequence(), 5);
    assert_eq!(revision_high_water.sequence(), 7);
    assert_eq!(saved_revision, None);
    assert!(dirty);
}

#[test]
fn recovery_rebinds_a_same_science_locator_and_rejects_other_science_atomically() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectRecovery)
        .unwrap();
    let token = recovery_request(&mut application);

    let before_mismatch = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: token.clone(),
                completion: OperationCompletion::ProjectRecovered(Box::new(projection(
                    project_id(4),
                    dataset_reference('2'),
                    5,
                    7,
                ))),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::DatasetIdentityMismatch
    );
    assert_eq!(application, before_mismatch);

    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token: token.clone(),
                completion: OperationCompletion::ProjectRecovered(Box::new(projection(
                    project_id(8),
                    dataset_reference('1'),
                    5,
                    7,
                ))),
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidProjectTransition
    );
    assert_eq!(application, before_mismatch);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectRecovered(Box::new(projection(
                project_id(4),
                dataset_reference_at('1', "recovered-location.m4d"),
                5,
                7,
            ))),
        })
        .unwrap();
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        revision_high_water,
        saved_revision,
        dirty,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.dataset(), &dataset_reference('1'));
    assert_eq!(revision.sequence(), 8);
    assert_eq!(revision_high_water.sequence(), 8);
    assert_eq!(saved_revision, None);
    assert!(dirty);
}

#[test]
fn verified_unbound_workspace_can_request_and_admit_a_dirty_recovery() {
    let mut application = application();
    verify(&mut application, dataset_reference('1'));
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestProjectRecovery)
        .unwrap();
    let token = recovery_request(&mut application);
    assert_eq!(token.project_id(), None);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectRecovered(Box::new(projection(
                project_id(9),
                dataset_reference('1'),
                4,
                6,
            ))),
        })
        .unwrap();
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        saved_revision,
        dirty,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.project_id(), project_id(9));
    assert_eq!(revision.sequence(), 4);
    assert_eq!(saved_revision, None);
    assert!(dirty);
}

#[test]
fn project_open_can_complete_directly_with_a_dirty_recovery_projection() {
    let mut application = application_for_project_open('1');
    let token = project_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectRecovered(Box::new(projection(
                project_id(9),
                dataset_reference('1'),
                3,
                5,
            ))),
        })
        .unwrap();

    let WorkspaceSnapshot::Bound {
        project,
        revision,
        saved_revision,
        dirty,
        ..
    } = application.snapshot().workspace().clone()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.project_id(), project_id(9));
    assert_eq!(revision.sequence(), 3);
    assert_eq!(saved_revision, None);
    assert!(dirty);
}

#[test]
fn source_invalidation_cancels_save_as_and_recovery_operations() {
    for save_as in [true, false] {
        let mut application = bound_application();
        application.drain_events(MAX_PENDING_EVENTS);
        let expected_kind = if save_as {
            application
                .dispatch(ApplicationCommand::RequestProjectSaveAs {
                    new_project_id: project_id(5),
                })
                .unwrap();
            OperationKind::ProjectSaveAs
        } else {
            application
                .dispatch(ApplicationCommand::RequestProjectRecovery)
                .unwrap();
            OperationKind::ProjectRecovery
        };
        application.drain_events(MAX_PENDING_EVENTS);

        application
            .dispatch(ApplicationCommand::InvalidateSourceVerification {
                source_generation: SourceSessionGeneration::new(1),
            })
            .unwrap();
        assert!(application.snapshot().active_operations().is_empty());
        assert!(
            application
                .drain_events(MAX_PENDING_EVENTS)
                .iter()
                .any(|event| matches!(
                    event,
                    ApplicationEvent::OperationCancellationRequested { token }
                        if token.kind() == expected_kind
                ))
        );
    }
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
fn unverified_analysis_can_start_but_cannot_stage_artifacts() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    assert_eq!(token.source_identity(), None);
    let before = application.fork_for_dispatch();
    let fault = application
        .dispatch(ApplicationCommand::StageAnalysisBundle {
            token: token.clone(),
            artifacts: vec![analysis_artifact(7, ArtifactSchema::AnalysisTableV1)],
        })
        .unwrap_err();
    assert_eq!(
        fault.code(),
        ApplicationFaultCode::IdentityVerificationRequired
    );
    assert_eq!(application, before);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::Cancelled,
        })
        .unwrap();
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
fn dataset_open_failure_remains_exact_after_a_rejected_durable_edit() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);
    let before_edit = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationConflict
    );
    assert_eq!(application, before_edit);

    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::Failed(OperationFailureCode::DatasetReadFailed),
            })
            .unwrap(),
        CommandEffect::Changed
    );
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
            OperationKind::SourceVerification,
            OperationFailureCode::SourceChanged,
            OperationFailureCode::DatasetReadFailed,
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
            OperationKind::ProjectSaveAs,
            OperationFailureCode::ProjectCommitIndeterminate,
            OperationFailureCode::ProjectNotFound,
        ),
        (
            OperationKind::ProjectRecovery,
            OperationFailureCode::ProjectReadFailed,
            OperationFailureCode::ProjectWriteFailed,
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
fn typed_project_store_faults_are_admitted_only_by_read_or_mutation_operations() {
    let mutation_codes = [
        OperationFailureCode::ProjectReadOnly,
        OperationFailureCode::ProjectWriterContended,
        OperationFailureCode::ProjectStaleParent,
        OperationFailureCode::ProjectDestinationExists,
        OperationFailureCode::ProjectUnsupportedFilesystem,
        OperationFailureCode::ProjectCapacityExceeded,
        OperationFailureCode::ProjectSourceChanged,
        OperationFailureCode::ProjectDigestMismatch,
        OperationFailureCode::ProjectCorrupt,
        OperationFailureCode::ProjectBusy,
    ];
    for kind in [OperationKind::ProjectSave, OperationKind::ProjectSaveAs] {
        for code in mutation_codes {
            assert!(failure_code_matches_kind(kind, code));
        }
    }

    let read_codes = [
        OperationFailureCode::ProjectCapacityExceeded,
        OperationFailureCode::ProjectDigestMismatch,
        OperationFailureCode::ProjectCorrupt,
        OperationFailureCode::ProjectBusy,
    ];
    for kind in [OperationKind::ProjectOpen, OperationKind::ProjectRecovery] {
        for code in read_codes {
            assert!(failure_code_matches_kind(kind, code));
        }
        for code in [
            OperationFailureCode::ProjectReadOnly,
            OperationFailureCode::ProjectWriterContended,
            OperationFailureCode::ProjectStaleParent,
            OperationFailureCode::ProjectDestinationExists,
            OperationFailureCode::ProjectUnsupportedFilesystem,
            OperationFailureCode::ProjectSourceChanged,
        ] {
            assert!(!failure_code_matches_kind(kind, code));
        }
    }
}

#[test]
fn analysis_bundle_is_invisible_until_commit_then_undo_removes_both_artifacts() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    let table_artifact = analysis_artifact(7, ArtifactSchema::AnalysisTableV1);
    let plot_artifact = analysis_artifact(8, ArtifactSchema::AnalysisPlotV1);
    let table = AnalysisTableDescriptor::new(
        AnalysisTableId::from_artifact_handle(table_artifact.handle_id()),
        12,
    );
    let plot = AnalysisPlotDescriptor::new(
        AnalysisPlotId::from_artifact_handle(plot_artifact.handle_id()),
        vec![2, 3],
    )
    .unwrap();
    let table_id = table.id();
    let plot_id = plot.id();

    let projection = stage_analysis_bundle(
        &mut application,
        token.clone(),
        vec![table_artifact.clone(), plot_artifact.clone()],
    );
    let staged = application.snapshot();
    assert_eq!(bound_project_arc(&staged).artifacts(), &[]);
    assert!(staged.transient().analysis_tables().is_empty());
    assert!(staged.transient().analysis_plots().is_empty());
    assert_eq!(projection.state().artifacts().len(), 2);
    assert_eq!(projection.revision().sequence(), 1);

    assert_eq!(
        application
            .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
            .unwrap(),
        CommandEffect::Changed
    );
    assert_eq!(
        application.snapshot().active_operations(),
        std::slice::from_ref(&token)
    );
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::OperationCancellationRequested { token: event_token }
                    if event_token == &token
            ))
    );
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
            .unwrap(),
        CommandEffect::NoChange
    );

    let before_frozen_edit = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationConflict
    );
    assert_eq!(application, before_frozen_edit);

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::AnalysisCommitted {
                projection: Box::new(projection.as_ref().clone()),
                table: table.clone(),
                plot: Some(plot.clone()),
            },
        })
        .unwrap();
    let committed = application.snapshot();
    let WorkspaceSnapshot::Bound {
        project,
        revision,
        saved_revision,
        ..
    } = committed.workspace()
    else {
        panic!("workspace was not bound");
    };
    assert_eq!(project.artifacts().len(), 2);
    assert_eq!(revision.sequence(), 1);
    assert_eq!(saved_revision.map(ProjectRevisionId::sequence), Some(1));
    assert_eq!(committed.dirty(), Some(false));
    assert_eq!(committed.transient().analysis_tables(), &[table]);
    assert_eq!(committed.transient().analysis_plots(), &[plot]);
    assert_eq!(
        committed.transient().selected_analysis_table(),
        Some(table_id)
    );
    assert_eq!(
        committed.transient().selected_analysis_plot(),
        Some(plot_id)
    );
    assert!(committed.active_operations().is_empty());

    let before_direct_remove = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::RemoveArtifact(
                table_id.artifact_handle_id(),
            ))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidProjectTransition
    );
    assert_eq!(application, before_direct_remove);

    let point = AnalysisPlotPointSelection::new(plot_id, 1, 2);
    application
        .dispatch(ApplicationCommand::SelectAnalysisPlotPoint(Some(point)))
        .unwrap();
    let before_invalid_point = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SelectAnalysisPlotPoint(Some(
                AnalysisPlotPointSelection::new(plot_id, 1, 3),
            )))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::AnalysisPointOutOfBounds
    );
    assert_eq!(application, before_invalid_point);

    application.dispatch(ApplicationCommand::Undo).unwrap();
    let undone = application.snapshot();
    assert!(bound_project_arc(&undone).artifacts().is_empty());
    assert!(undone.transient().analysis_tables().is_empty());
    assert!(undone.transient().analysis_plots().is_empty());
    assert_eq!(undone.transient().selected_analysis_table(), None);
    assert_eq!(undone.transient().selected_analysis_plot(), None);
    assert_eq!(undone.transient().selected_analysis_plot_point(), None);
}

#[test]
fn generic_artifact_commands_cannot_admit_analysis_schemas() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let before = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::UpsertArtifact(analysis_artifact(
                7,
                ArtifactSchema::AnalysisTableV1,
            )))
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidProjectTransition
    );
    assert_eq!(application, before);
}

#[test]
fn analysis_stage_rejects_full_descriptor_registries_atomically() {
    let mut full_tables = bound_application();
    for index in 0..MAX_ANALYSIS_TABLES {
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
        bytes[15] = 1;
        let id = AnalysisTableId::from_artifact_handle(&ArtifactHandleId::from_bytes(bytes));
        full_tables
            .analysis_table_catalog
            .insert(id, AnalysisTableDescriptor::new(id, 0));
    }
    full_tables.drain_events(MAX_PENDING_EVENTS);
    full_tables
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut full_tables, OperationKind::Analysis);
    let before = full_tables.fork_for_dispatch();
    assert_eq!(
        full_tables
            .dispatch(ApplicationCommand::StageAnalysisBundle {
                token,
                artifacts: vec![analysis_artifact(250, ArtifactSchema::AnalysisTableV1)],
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::AnalysisRegistryFull
    );
    assert_eq!(full_tables, before);

    let mut full_plots = bound_application();
    for index in 0..MAX_ANALYSIS_PLOTS {
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
        bytes[15] = 2;
        let id = AnalysisPlotId::from_artifact_handle(&ArtifactHandleId::from_bytes(bytes));
        full_plots
            .analysis_plot_catalog
            .insert(id, AnalysisPlotDescriptor::new(id, Vec::new()).unwrap());
    }
    full_plots.drain_events(MAX_PENDING_EVENTS);
    full_plots
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut full_plots, OperationKind::Analysis);
    let before = full_plots.fork_for_dispatch();
    assert_eq!(
        full_plots
            .dispatch(ApplicationCommand::StageAnalysisBundle {
                token,
                artifacts: vec![
                    analysis_artifact(250, ArtifactSchema::AnalysisTableV1),
                    analysis_artifact(251, ArtifactSchema::AnalysisPlotV1),
                ],
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::AnalysisRegistryFull
    );
    assert_eq!(full_plots, before);
}

#[test]
fn authenticated_reopen_descriptors_become_visible_together() {
    let table_artifact = analysis_artifact(7, ArtifactSchema::AnalysisTableV1);
    let plot_artifact = analysis_artifact(8, ArtifactSchema::AnalysisPlotV1);
    let second_table_artifact = analysis_artifact(9, ArtifactSchema::AnalysisTableV1);
    let table = AnalysisTableDescriptor::new(
        AnalysisTableId::from_artifact_handle(table_artifact.handle_id()),
        4,
    );
    let plot = AnalysisPlotDescriptor::new(
        AnalysisPlotId::from_artifact_handle(plot_artifact.handle_id()),
        vec![4],
    )
    .unwrap();
    let second_table = AnalysisTableDescriptor::new(
        AnalysisTableId::from_artifact_handle(second_table_artifact.handle_id()),
        2,
    );
    let project_id = project_id(9);
    let state = ProjectState::new(
        project_id,
        dataset_reference('1'),
        view(),
        vec![preset()],
        vec![
            table_artifact.clone(),
            plot_artifact.clone(),
            second_table_artifact.clone(),
        ],
    )
    .unwrap();
    let projection = ProjectGenerationProjection::new(
        ProjectRevisionId::new(project_id, 3),
        ProjectRevisionHighWater::new(project_id, 3),
        state,
    )
    .unwrap();
    let mut application = application_for_project_open('1');
    let token = project_open_token(&mut application);
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::ProjectOpened(Box::new(projection)),
        })
        .unwrap();
    assert!(
        application
            .snapshot()
            .transient()
            .analysis_tables()
            .is_empty()
    );
    assert!(
        application
            .snapshot()
            .transient()
            .analysis_plots()
            .is_empty()
    );

    let currentness = application.snapshot().currentness();
    application
        .dispatch(ApplicationCommand::InstallLoadedAnalysisDescriptors {
            project_id,
            revision: ProjectRevisionId::new(project_id, 3),
            currentness,
            bundles: vec![
                LoadedAnalysisDescriptorBundle::new(
                    vec![table_artifact.clone(), plot_artifact.clone()],
                    table.clone(),
                    Some(plot.clone()),
                ),
                LoadedAnalysisDescriptorBundle::new(
                    vec![second_table_artifact.clone()],
                    second_table.clone(),
                    None,
                ),
            ],
        })
        .unwrap();
    let loaded = application.snapshot();
    assert_eq!(
        loaded.transient().analysis_tables(),
        &[table.clone(), second_table.clone()]
    );
    assert_eq!(
        loaded.transient().analysis_plots(),
        std::slice::from_ref(&plot)
    );

    let before_stale = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::InstallLoadedAnalysisDescriptors {
                project_id,
                revision: ProjectRevisionId::new(project_id, 2),
                currentness,
                bundles: vec![LoadedAnalysisDescriptorBundle::new(
                    vec![table_artifact.clone(), plot_artifact.clone()],
                    table.clone(),
                    Some(plot.clone()),
                )],
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::StaleOperationCompletion
    );
    assert_eq!(application, before_stale);

    let before = application.fork_for_dispatch();
    let missing = AnalysisTableDescriptor::new(
        AnalysisTableId::from_artifact_handle(
            analysis_artifact(10, ArtifactSchema::AnalysisTableV1).handle_id(),
        ),
        1,
    );
    assert_eq!(
        application
            .dispatch(ApplicationCommand::InstallLoadedAnalysisDescriptors {
                project_id,
                revision: ProjectRevisionId::new(project_id, 3),
                currentness,
                bundles: vec![LoadedAnalysisDescriptorBundle::new(
                    vec![second_table_artifact],
                    missing,
                    None,
                )],
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidOperationCompletion
    );
    assert_eq!(application, before);
}

#[test]
fn analysis_stage_and_commit_reject_stale_or_wrong_input_atomically() {
    let mut stale = bound_application();
    stale.drain_events(MAX_PENDING_EVENTS);
    stale
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let stale_token = started_token(&mut stale, OperationKind::Analysis);
    stale
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    let before_stale = stale.fork_for_dispatch();
    assert_eq!(
        stale
            .dispatch(ApplicationCommand::StageAnalysisBundle {
                token: stale_token.clone(),
                artifacts: vec![analysis_artifact(7, ArtifactSchema::AnalysisTableV1)],
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::StaleOperationCompletion
    );
    assert_eq!(stale, before_stale);
    stale
        .dispatch(ApplicationCommand::CompleteOperation {
            token: stale_token,
            completion: OperationCompletion::Cancelled,
        })
        .unwrap();
    assert!(stale.snapshot().active_operations().is_empty());

    let mut wrong = bound_application();
    wrong.drain_events(MAX_PENDING_EVENTS);
    wrong
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut wrong, OperationKind::Analysis);
    let artifact = analysis_artifact(7, ArtifactSchema::AnalysisTableV1);
    let projection = stage_analysis_bundle(&mut wrong, token.clone(), vec![artifact.clone()]);
    let before_wrong = wrong.fork_for_dispatch();
    let wrong_table = AnalysisTableDescriptor::new(
        AnalysisTableId::from_artifact_handle(
            analysis_artifact(9, ArtifactSchema::AnalysisTableV1).handle_id(),
        ),
        12,
    );
    assert_eq!(
        wrong
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::AnalysisCommitted {
                    projection: Box::new(projection.as_ref().clone()),
                    table: wrong_table,
                    plot: None,
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::InvalidOperationCompletion
    );
    assert_eq!(wrong, before_wrong);
    assert!(bound_project_arc(&wrong.snapshot()).artifacts().is_empty());
}

#[test]
fn failed_staged_analysis_leaves_project_and_descriptors_unchanged() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    let before = application.snapshot();
    let before_project = bound_project_arc(&before);
    let WorkspaceSnapshot::Bound {
        revision: before_revision,
        ..
    } = before.workspace()
    else {
        panic!("workspace was not bound");
    };
    let before_revision = *before_revision;
    let before_transient = application.snapshot().transient().clone();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    stage_analysis_bundle(
        &mut application,
        token.clone(),
        vec![analysis_artifact(7, ArtifactSchema::AnalysisTableV1)],
    );

    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::Failed(OperationFailureCode::AnalysisExecutionFailed),
        })
        .unwrap();
    let after = application.snapshot();
    assert_eq!(bound_project_arc(&after), before_project);
    let WorkspaceSnapshot::Bound { revision, .. } = after.workspace() else {
        panic!("workspace was not bound");
    };
    assert_eq!(*revision, before_revision);
    assert_eq!(after.transient(), &before_transient);
    assert!(after.active_operations().is_empty());
}

#[test]
fn source_invalidation_cancels_staged_analysis_without_exposing_results() {
    let mut application = bound_application();
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Analysis))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Analysis);
    stage_analysis_bundle(
        &mut application,
        token.clone(),
        vec![analysis_artifact(7, ArtifactSchema::AnalysisTableV1)],
    );

    application
        .dispatch(ApplicationCommand::InvalidateSourceVerification {
            source_generation: SourceSessionGeneration::new(1),
        })
        .unwrap();
    let snapshot = application.snapshot();
    assert_eq!(snapshot.active_operations(), std::slice::from_ref(&token));
    assert!(bound_project_arc(&snapshot).artifacts().is_empty());
    assert!(snapshot.transient().analysis_tables().is_empty());
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::OperationCancellationRequested { token: event_token }
                    if event_token == &token
            ))
    );
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::Cancelled,
        })
        .unwrap();
    assert!(application.snapshot().active_operations().is_empty());
}

#[test]
fn analysis_plot_descriptors_remain_bounded() {
    let id = AnalysisPlotId::from_artifact_handle(
        analysis_artifact(8, ArtifactSchema::AnalysisPlotV1).handle_id(),
    );
    assert_eq!(
        AnalysisPlotDescriptor::new(id, vec![0; MAX_ANALYSIS_PLOT_SERIES + 1]),
        Err(AnalysisDescriptorError::TooManySeries)
    );
    assert_eq!(
        AnalysisPlotDescriptor::new(id, vec![MAX_ANALYSIS_PLOT_POINTS + 1]),
        Err(AnalysisDescriptorError::TooManyPoints)
    );
    assert_eq!(
        AnalysisPlotDescriptor::new(id, vec![u64::MAX, 1]),
        Err(AnalysisDescriptorError::PointCountOverflow)
    );
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
                catalog: Arc::new(catalog_for_source(4, 2)),
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
fn dataset_open_freezes_durable_edits_but_allows_transient_state_and_exact_completion() {
    let mut application = application();
    verify(&mut application, dataset_reference('1'));
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);

    for command in [
        ApplicationCommand::AttachVerifiedDataset,
        ApplicationCommand::SetTimepoint(TimeIndex::new(1)),
    ] {
        let before = application.fork_for_dispatch();
        assert_eq!(
            application.dispatch(command).unwrap_err().code(),
            ApplicationFaultCode::OperationConflict
        );
        assert_eq!(application, before);
    }

    let currentness = application.snapshot().currentness();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::SetActiveTool(ToolKind::Inspect))
            .unwrap(),
        CommandEffect::Changed
    );
    assert_eq!(application.snapshot().currentness(), currentness);
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::DatasetOpened {
                    catalog: Arc::new(catalog_for_source(4, 2)),
                    workspace: Box::new(unbound_workspace(project_id(9))),
                    source_generation: SourceSessionGeneration::new(2),
                },
            })
            .unwrap(),
        CommandEffect::Changed
    );
    let snapshot = application.snapshot();
    assert_eq!(
        snapshot.source_generation(),
        SourceSessionGeneration::new(2)
    );
    assert!(!snapshot.is_bound());
    assert!(snapshot.active_operations().is_empty());
}

#[test]
fn dataset_open_can_still_be_cancelled_while_the_durable_freeze_is_active() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);

    assert_eq!(
        application
            .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
            .unwrap(),
        CommandEffect::Changed
    );
    assert!(application.snapshot().active_operations().is_empty());
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::OperationCancellationRequested { token: cancelled }
                    if cancelled == &token
            ))
    );
}

#[test]
fn source_invalidation_cancels_dataset_open_before_advancing_currentness() {
    let mut application = application();
    verify(&mut application, dataset_reference('1'));
    application.drain_events(MAX_PENDING_EVENTS);
    application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let token = dataset_open_token(&mut application);

    application
        .dispatch(ApplicationCommand::InvalidateSourceVerification {
            source_generation: SourceSessionGeneration::new(1),
        })
        .unwrap();
    assert!(application.snapshot().active_operations().is_empty());
    assert_ne!(
        application.snapshot().currentness(),
        token.currentness_generation()
    );
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::OperationCancellationRequested { token: cancelled }
                    if cancelled == &token
            ))
    );

    let before_late = application.fork_for_dispatch();
    assert_eq!(
        application
            .dispatch(ApplicationCommand::CompleteOperation {
                token,
                completion: OperationCompletion::DatasetOpened {
                    catalog: Arc::new(catalog_for_source(4, 2)),
                    workspace: Box::new(unbound_workspace(project_id(9))),
                    source_generation: SourceSessionGeneration::new(2),
                },
            })
            .unwrap_err()
            .code(),
        ApplicationFaultCode::OperationNotFound
    );
    assert_eq!(application, before_late);
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
fn import_completion_survives_unrelated_view_currentness_changes() {
    let mut application = application();
    application
        .dispatch(ApplicationCommand::BeginOperation(OperationKind::Import))
        .unwrap();
    let token = started_token(&mut application, OperationKind::Import);

    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(3)))
        .unwrap();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: token.clone(),
            completion: OperationCompletion::Succeeded,
        })
        .unwrap();

    assert!(application.snapshot().active_operations().is_empty());
    assert!(
        application
            .drain_events(MAX_PENDING_EVENTS)
            .iter()
            .any(|event| {
                matches!(
                    event,
                    ApplicationEvent::OperationCompleted {
                        token: completed,
                        outcome: OperationOutcome::Succeeded,
                    } if completed == &token
                )
            })
    );
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
        ScientificIdentityStatus::Unverified(DatasetSourceId::new(2)),
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
    let token = request_source_verification(application);
    application
        .dispatch(ApplicationCommand::UpdateSourceVerificationProgress {
            token: token.clone(),
            completed_work: 1,
            total_work: 1,
        })
        .unwrap();
    let catalog = verified_catalog(application.snapshot().catalog(), &dataset);
    let source_generation = application.snapshot().source_generation();
    application
        .dispatch(ApplicationCommand::CompleteOperation {
            token,
            completion: OperationCompletion::SourceVerified {
                source_generation,
                catalog,
                dataset,
            },
        })
        .unwrap();
}

fn request_source_verification(application: &mut ApplicationState) -> OperationToken {
    application
        .dispatch(ApplicationCommand::RequestSourceVerification)
        .unwrap();
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::SourceVerificationRequested { token } => Some(token),
            _ => None,
        })
        .expect("source-verification request event")
}

fn verified_catalog(catalog: &DatasetCatalog, dataset: &DatasetReference) -> Arc<DatasetCatalog> {
    Arc::new(
        catalog_with_identity(
            catalog,
            ScientificIdentityStatus::Verified(*dataset.scientific_content_id()),
        )
        .unwrap(),
    )
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
    catalog_for_source(label_digit, 1)
}

fn catalog_for_source(label_digit: u8, source_id: u64) -> DatasetCatalog {
    catalog_named_with_timepoints_and_source(format!("catalog-{label_digit}"), 10_000, source_id)
}

fn catalog_with_timepoints(timepoints: u64) -> DatasetCatalog {
    catalog_named_with_timepoints(format!("catalog-t{timepoints}"), timepoints)
}

fn catalog_named_with_timepoints(label: String, timepoints: u64) -> DatasetCatalog {
    catalog_named_with_timepoints_and_source(label, timepoints, 1)
}

fn catalog_named_with_timepoints_and_source(
    label: String,
    timepoints: u64,
    source_id: u64,
) -> DatasetCatalog {
    DatasetCatalog::new(
        label,
        ScientificIdentityStatus::Unverified(DatasetSourceId::new(source_id)),
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
        mirante4d_dataset::ResourceValidity::AllValid,
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
    let schema = ArtifactSchema::AnnotationV1;
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

fn analysis_artifact(byte: u8, schema: ArtifactSchema) -> ArtifactReference {
    let content_digit = format!("{:x}", byte % 16);
    ArtifactReference::new(
        ArtifactHandleId::from_bytes([byte; 16]),
        schema,
        ArtifactContentId::parse(&format!(
            "{}{}",
            ArtifactContentId::PREFIX,
            content_digit.repeat(64)
        ))
        .unwrap(),
        RawObjectDescriptor::new(
            ExactBytesDigest::parse(&format!(
                "{}{}",
                ExactBytesDigest::PREFIX,
                content_digit.repeat(64)
            ))
            .unwrap(),
            12,
            MediaType::parse(schema.media_type()).unwrap(),
            ObjectRole::parse(schema.object_role()).unwrap(),
        ),
        Some(DerivationRecordId::from_canonical_body_bytes(br#"{"operation":"test"}"#).unwrap()),
        Some(RecipeId::from_canonical_body_bytes(br#"{"recipe":"test"}"#).unwrap()),
        vec![LogicalLayerKey::new(0)],
        "Analysis result",
        true,
        ArtifactCompleteness::Complete,
        ArtifactRecoverability::Regenerable,
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

fn stage_analysis_bundle(
    application: &mut ApplicationState,
    token: OperationToken,
    artifacts: Vec<ArtifactReference>,
) -> Arc<ProjectGenerationProjection> {
    application
        .dispatch(ApplicationCommand::StageAnalysisBundle {
            token: token.clone(),
            artifacts,
        })
        .unwrap();
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::AnalysisCommitRequested {
                token: event_token,
                projection,
            } if event_token == token => Some(projection),
            _ => None,
        })
        .expect("analysis-commit request event")
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

fn save_as_request(
    application: &mut ApplicationState,
) -> (OperationToken, Arc<ProjectGenerationProjection>) {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectSaveAsRequested { token, projection } => {
                Some((token, projection))
            }
            _ => None,
        })
        .expect("project-save-as event")
}

fn recovery_request(application: &mut ApplicationState) -> OperationToken {
    application
        .drain_events(MAX_PENDING_EVENTS)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectRecoveryRequested { token } => Some(token),
            _ => None,
        })
        .expect("project-recovery event")
}

fn bound_project_arc(snapshot: &ApplicationSnapshot) -> Arc<ProjectState> {
    let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() else {
        panic!("workspace was not bound");
    };
    Arc::clone(project)
}
