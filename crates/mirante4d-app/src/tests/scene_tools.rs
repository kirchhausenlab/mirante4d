#[test]
fn app_color_image_composites_scene_layers_into_viewport_texture() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let base = color_image_for_state(&state);
    state.scene_artifacts = sample_scene_artifacts();
    let composited = color_image_for_state(&state);

    assert_eq!(composited.size, base.size);
    assert_ne!(composited.pixels, base.pixels);
}
#[test]
fn app_scene_artifact_editor_updates_roi_through_command_store() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi-a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::ZERO,
            max: DVec3::splat(1.0),
        },
        SceneArtifactTime::Timepoint(TimeIndex(0)),
    )
    .unwrap();
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi {
            artifact: roi.clone(),
        })
        .unwrap();
    let mut updated = roi.clone();
    updated.name = "nucleus".to_owned();
    updated.visible = false;
    updated.style = AnalysisSceneStyleRgba::new([0.25, 0.5, 0.75, 1.0]).unwrap();

    assert!(update_scene_roi_artifact(&mut state, updated).unwrap());

    let stored = state.scene_artifacts.roi(&roi.id).unwrap();
    assert_eq!(stored.name, "nucleus");
    assert!(!stored.visible);
    assert_eq!(stored.style.color_rgba, [0.25, 0.5, 0.75, 1.0]);
    assert!(state.scene_artifacts.can_undo());

    state.scene_artifacts.undo().unwrap();
    assert_eq!(state.scene_artifacts.roi(&roi.id).unwrap(), &roi);
    assert!(state.scene_artifacts.can_redo());
}

#[test]
fn app_scene_artifact_editor_removes_roi_and_clears_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi-a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::ZERO,
            max: DVec3::splat(1.0),
        },
        SceneArtifactTime::Timepoint(TimeIndex(0)),
    )
    .unwrap();
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi {
            artifact: roi.clone(),
        })
        .unwrap();
    select_scene_artifact(&mut state, EditableSceneArtifactKind::Roi, &roi.id);
    assert!(selected_scene_artifact_matches(
        &state,
        EditableSceneArtifactKind::Roi,
        &roi.id
    ));

    assert!(remove_scene_artifact(&mut state, EditableSceneArtifactKind::Roi, &roi.id).unwrap());

    assert!(state.scene_artifacts.roi(&roi.id).is_none());
    assert!(state.viewer_tools.selection.is_none());
    assert!(state.scene_artifacts.can_undo());
    state.scene_artifacts.undo().unwrap();
    assert!(state.scene_artifacts.roi(&roi.id).is_some());
}

#[test]
fn app_scene_artifact_editor_normalizes_roi_box_geometry() {
    let mut geometry = AnalysisWorldGeometry::Box3D {
        min: DVec3::new(4.0, 1.0, 9.0),
        max: DVec3::new(2.0, 3.0, 5.0),
    };

    normalize_world_geometry(&mut geometry).unwrap();

    assert_eq!(
        geometry,
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(2.0, 1.0, 5.0),
            max: DVec3::new(4.0, 3.0, 9.0),
        }
    );
}

#[test]
fn app_scene_artifact_editor_refreshes_measurement_result_after_geometry_edit() {
    let mut measurement = MeasurementArtifact::distance(
        SceneArtifactId::new("measurement", "distance-a").unwrap(),
        "distance-a",
        DVec3::ZERO,
        DVec3::new(3.0, 4.0, 0.0),
        MeasurementProvenance {
            source: "test".to_owned(),
            scope: "test".to_owned(),
        },
        SceneArtifactTime::Timepoint(TimeIndex(0)),
    )
    .unwrap();
    measurement.geometry =
        AnalysisMeasurementGeometry::distance(DVec3::ZERO, DVec3::new(0.0, 0.0, 12.0)).unwrap();

    refresh_measurement_result(&mut measurement);

    let result = measurement.result.unwrap();
    assert_eq!(result.value, 12.0);
    assert_eq!(result.unit, "world_unit");
    assert_eq!(result.description, "distance");
}

#[test]
fn app_scene_artifact_editor_updates_track_and_annotation_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();

    let track_id = SceneArtifactId::new("track", "track-a").unwrap();
    let mut track = state.scene_artifacts.track(&track_id).unwrap().clone();
    track.name = "edited track".to_owned();
    track.visible = false;
    track.style = AnalysisSceneStyleRgba::new([0.1, 0.2, 0.3, 1.0]).unwrap();

    assert!(update_scene_track_artifact(&mut state, track).unwrap());

    let stored_track = state.scene_artifacts.track(&track_id).unwrap();
    assert_eq!(stored_track.name, "edited track");
    assert!(!stored_track.visible);
    assert_eq!(stored_track.style.color_rgba, [0.1, 0.2, 0.3, 1.0]);

    let annotation_id = SceneArtifactId::new("annotation", "note-a").unwrap();
    let mut annotation = state
        .scene_artifacts
        .annotations()
        .find(|annotation| annotation.id == annotation_id)
        .unwrap()
        .clone();
    annotation.name = "edited note".to_owned();
    annotation.text = Some("reviewed".to_owned());
    annotation.visible = false;

    assert!(update_scene_annotation_artifact(&mut state, annotation).unwrap());

    let stored_annotation = state
        .scene_artifacts
        .annotations()
        .find(|annotation| annotation.id == annotation_id)
        .unwrap();
    assert_eq!(stored_annotation.name, "edited note");
    assert_eq!(stored_annotation.text.as_deref(), Some("reviewed"));
    assert!(!stored_annotation.visible);
}

#[test]
fn app_scene_artifact_editor_removes_annotation_and_clears_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let annotation_id = SceneArtifactId::new("annotation", "note-a").unwrap();

    select_scene_artifact(
        &mut state,
        EditableSceneArtifactKind::Annotation,
        &annotation_id,
    );
    assert!(selected_scene_artifact_matches(
        &state,
        EditableSceneArtifactKind::Annotation,
        &annotation_id
    ));

    assert!(
        remove_scene_artifact(
            &mut state,
            EditableSceneArtifactKind::Annotation,
            &annotation_id,
        )
        .unwrap()
    );

    assert!(
        state
            .scene_artifacts
            .annotations()
            .all(|annotation| annotation.id != annotation_id)
    );
    assert!(state.viewer_tools.selection.is_none());
    state.scene_artifacts.undo().unwrap();
    assert!(
        state
            .scene_artifacts
            .annotations()
            .any(|annotation| annotation.id == annotation_id)
    );
}

#[test]
fn app_scene_artifact_editor_normalizes_annotation_geometry() {
    let mut geometry = AnalysisWorldGeometry::Ellipsoid {
        center: DVec3::ZERO,
        radii: DVec3::new(-1.0, 0.0, 2.0),
    };

    normalize_world_geometry(&mut geometry).unwrap();

    assert_eq!(
        geometry,
        AnalysisWorldGeometry::Ellipsoid {
            center: DVec3::ZERO,
            radii: DVec3::new(1.0e-6, 1.0e-6, 2.0),
        }
    );
}

#[test]
fn app_scene_artifact_editor_updates_track_point_positions() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    state.scene_artifacts = sample_scene_artifacts();
    let track_id = SceneArtifactId::new("track", "track-a").unwrap();
    let mut track = state.scene_artifacts.track(&track_id).unwrap().clone();
    track.points[1].position_world = DVec3::new(5.0, 6.0, 7.0);

    assert!(update_scene_track_artifact(&mut state, track).unwrap());

    assert_eq!(
        state.scene_artifacts.track(&track_id).unwrap().points[1].position_world,
        DVec3::new(5.0, 6.0, 7.0)
    );
}

#[test]
fn app_hover_readout_uses_named_volume_pick_policies() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let hover = ViewportHover {
        x: state.frame.width / 2,
        y: state.frame.height / 2,
        intensity: ViewportIntensity::U16(0),
    };

    state.active_render_mode = RenderMode::Mip;
    rerender_state_with_backend(&mut state, None).unwrap();
    let mip = pick_hit_from_viewport_hover(&state, hover).unwrap();

    state.active_render_mode = RenderMode::Isosurface;
    state.iso_display_level = iso_level_for_u16_threshold(3_000);
    rerender_state_with_backend(&mut state, None).unwrap();
    let iso = pick_hit_from_viewport_hover(&state, hover).unwrap();

    state.active_render_mode = RenderMode::Dvr;
    state.dvr_density_scale = 12.0;
    rerender_state_with_backend(&mut state, None).unwrap();
    let dvr = pick_hit_from_viewport_hover(&state, hover).unwrap();

    assert_eq!(mip.policy, PickPolicy::MipArgmax);
    assert_eq!(iso.policy, PickPolicy::FirstThresholdHit);
    assert_eq!(dvr.policy, PickPolicy::ProbeRay);
    assert!(mip.grid_position.is_some());
    assert!(iso.grid_position.is_some());
    assert!(dvr.grid_position.is_some());
    assert_ne!(mip.value, Some(PickValue::IntensityU16(0)));
}

#[test]
fn app_tool_commands_commit_roi_and_measurement_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let anchor = world_tool_hit(DVec3::new(1.0, 2.0, 3.0), 4.0, 5.0);
    let current = world_tool_hit(DVec3::new(4.0, 6.0, 8.0), 9.0, 10.0);

    let outcome = apply_viewer_tool_commands(
        &mut state,
        vec![
            ViewerToolCommand::CommitRoi {
                anchor: anchor.clone(),
                current: current.clone(),
            },
            ViewerToolCommand::CommitMeasurement { anchor, current },
        ],
    )
    .unwrap();

    assert!(outcome.rerender_requested);
    assert_eq!(state.scene_artifacts.rois().count(), 1);
    assert_eq!(state.scene_artifacts.measurements().count(), 1);
    let roi = state.scene_artifacts.rois().next().unwrap();
    assert_eq!(roi.world_bounds().unwrap().min, DVec3::new(1.0, 2.0, 3.0));
    assert_eq!(roi.world_bounds().unwrap().max, DVec3::new(4.0, 6.0, 8.0));
    let measurement = state.scene_artifacts.measurements().next().unwrap();
    assert_eq!(
        measurement.result.as_ref().unwrap().value,
        (50.0_f64).sqrt()
    );
}

#[test]
fn project_json_atomic_write_restores_existing_file_after_commit_failure() {
    let tempdir = tempfile::tempdir().unwrap();
    let project_path = tempdir.path().join("atomic-session.m4dproj");
    fs::create_dir_all(&project_path).unwrap();
    let project_json = project_json_path(&project_path);
    let old_json = "{\n  \"project\": \"old\"\n}\n";
    fs::write(&project_json, old_json).unwrap();

    let err = write_project_json_atomically_with_forced_commit_failure(
        &project_path,
        "{\n  \"project\": \"new\"\n}",
    )
    .unwrap_err();

    assert!(err.to_string().contains("failed to commit"));
    assert_eq!(fs::read_to_string(&project_json).unwrap(), old_json);
    assert!(!project_path.join(".project.json.tmp").exists());
    assert!(!project_path.join(".project.json.replace-backup").exists());
}

#[test]
fn project_artifact_atomic_write_restores_existing_file_after_commit_failure() {
    let tempdir = tempfile::tempdir().unwrap();
    let artifact_path = tempdir
        .path()
        .join("atomic-session.m4dproj/artifacts/tables/table.json");
    fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    let old_json = "{\n  \"artifact\": \"old\"\n}\n";
    fs::write(&artifact_path, old_json).unwrap();

    let err = write_json_artifact_atomically_with_forced_commit_failure(
        &artifact_path,
        "{\n  \"artifact\": \"new\"\n}",
    )
    .unwrap_err();

    assert!(err.to_string().contains("failed to commit"));
    assert_eq!(fs::read_to_string(&artifact_path).unwrap(), old_json);
    assert!(!artifact_path.with_file_name(".table.json.tmp").exists());
    assert!(
        !artifact_path
            .with_file_name(".table.json.replace-backup")
            .exists()
    );
}

#[test]
fn rejects_file_based_m4dproj_sessions() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("legacy-file.m4dproj");
    fs::write(&path, "{}").unwrap();

    let err = read_session_file(&path).unwrap_err();

    assert!(err.to_string().contains(".m4dproj directory"));
}
