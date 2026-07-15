use crate::retained_leases::RetainedLeases;
use crate::viewer_layout::PanelId;
use mirante4d_dataset::{
    DatasetResourceIdentity, DatasetResourceKey, DatasetSourceId, ResourceLease,
    ResourcePayloadDescriptor, ResourcePayloadView, ResourceRegion, ResourceValidity,
};
use mirante4d_domain::{LogicalLayerKey, ScaleLevel};

struct HistogramTestLease {
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    bytes: Vec<u8>,
}

impl ResourceLease for HistogramTestLease {
    fn key(&self) -> DatasetResourceKey {
        self.key
    }

    fn payload(&self) -> ResourcePayloadView<'_> {
        let value_len = usize::try_from(self.descriptor.value_byte_len()).unwrap();
        let (values, validity) = self.bytes.split_at(value_len);
        self.descriptor
            .view(
                values,
                (self.descriptor.validity_byte_len() != 0).then_some(validity),
            )
            .unwrap()
    }
}

fn histogram_key(
    layer: u32,
    timepoint: u64,
    scale: u32,
    origin_x: u64,
    samples: u64,
) -> DatasetResourceKey {
    DatasetResourceKey::new(
        DatasetResourceIdentity::Unverified(DatasetSourceId::new(77)),
        LogicalLayerKey::new(layer),
        TimeIndex::new(timepoint),
        ScaleLevel::new(scale),
        ResourceRegion::new([0, 0, origin_x], Shape3D::new(1, 1, samples).unwrap()).unwrap(),
    )
}

fn u16_histogram_lease(
    key: DatasetResourceKey,
    values: &[u16],
    validity: Option<u8>,
) -> Arc<dyn ResourceLease> {
    let descriptor = ResourcePayloadDescriptor::new(
        IntensityDType::Uint16,
        key.region().shape(),
        if validity.is_some() {
            ResourceValidity::BitMask
        } else {
            ResourceValidity::AllValid
        },
    )
    .unwrap();
    let mut bytes = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    if let Some(validity) = validity {
        bytes.push(validity);
    }
    Arc::new(HistogramTestLease {
        key,
        descriptor,
        bytes,
    })
}

fn histogram_for_test(
    bridge: &RetainedLeases,
    layer: u32,
    timepoint: u64,
    scale: u32,
) -> LayerHistogramSummary {
    let requirements = bridge.required_keys().collect::<Vec<_>>();
    active_layer_histogram_summary(
        bridge,
        histogram::ActiveLayerHistogramInput {
            requirements: &requirements,
            identity: DatasetResourceIdentity::Unverified(DatasetSourceId::new(77)),
            layer: LogicalLayerKey::new(layer),
            layer_name: "intensity",
            dtype: IntensityDType::Uint16,
            timepoint: TimeIndex::new(timepoint),
            scale: ScaleLevel::new(scale),
        },
    )
}

#[test]
fn workbench_shell_exposes_channel_display_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

    harness.get_by_label("Layers");
    harness.get_by_label("channel visible");
    harness.get_by_label("channel opacity");
    harness.get_by_label("display window");
    harness.get_by_label("channel color");
    harness.get_by_label("transfer gamma");
    harness.get_by_label("invert LUT");
    harness.get_by_label("transfer preset");
    harness.get_by_label("Channel Presets");
}

#[test]
fn active_layer_histogram_reads_only_valid_lease_samples_and_keeps_valid_zero() {
    let key = histogram_key(0, 0, 0, 0, 4);
    let mut bridge = RetainedLeases::new();
    bridge.replace_requirements([key]).unwrap();
    bridge
        .install(u16_histogram_lease(key, &[0, 4, 10, 20], Some(0b0000_1101)))
        .unwrap();

    let histogram = histogram_for_test(&bridge, 0, 0, 0);
    let window = auto_dense_window_from_histogram(&histogram).unwrap();
    assert!(matches!(histogram.status, HistogramStatus::Sampled { .. }));
    assert_eq!(histogram.sample_count, 3);
    assert_eq!(histogram.min_value, 0.0);
    assert_eq!(histogram.max_value, 20.0);
    assert_eq!(histogram.bins.iter().sum::<u64>(), 3);
    assert!(window.low() >= 0.0);
    assert!(window.high() <= 20.0);
}

#[test]
fn active_layer_histogram_is_pending_when_its_own_cohort_lease_is_missing() {
    let retained = histogram_key(0, 0, 0, 0, 2);
    let missing = histogram_key(0, 0, 0, 2, 2);
    let mut bridge = RetainedLeases::new();
    bridge.replace_requirements([retained, missing]).unwrap();
    bridge
        .install(u16_histogram_lease(retained, &[1, 2], None))
        .unwrap();

    let histogram = histogram_for_test(&bridge, 0, 0, 0);
    assert!(matches!(histogram.status, HistogramStatus::Pending { .. }));
    assert_eq!(histogram.sample_count, 2);
    assert!(!histogram_can_auto_window(&histogram));
    assert!(
        auto_dense_window_from_histogram(&histogram)
            .unwrap_err()
            .to_string()
            .contains("cannot auto-window")
    );
}

#[test]
fn unrelated_missing_lease_does_not_keep_active_histogram_pending() {
    let active = histogram_key(0, 0, 0, 0, 2);
    let unrelated = histogram_key(1, 0, 0, 0, 2);
    let mut bridge = RetainedLeases::new();
    bridge.replace_requirements([active, unrelated]).unwrap();
    bridge
        .install(u16_histogram_lease(active, &[3, 9], None))
        .unwrap();

    let histogram = histogram_for_test(&bridge, 0, 0, 0);
    assert!(matches!(histogram.status, HistogramStatus::Sampled { .. }));
    assert_eq!(histogram.sample_count, 2);
    assert_eq!(histogram.min_value, 3.0);
    assert_eq!(histogram.max_value, 9.0);
}

#[test]
fn linked_view_missing_lease_in_same_cohort_does_not_block_histogram() {
    let active = histogram_key(0, 0, 0, 0, 2);
    let linked = histogram_key(0, 0, 0, 2, 2);
    let mut bridge = RetainedLeases::new();
    bridge.replace_requirements([active, linked]).unwrap();
    bridge
        .install(u16_histogram_lease(active, &[3, 9], None))
        .unwrap();
    let requirements = [active];

    let histogram = active_layer_histogram_summary(
        &bridge,
        histogram::ActiveLayerHistogramInput {
            requirements: &requirements,
            identity: active.identity(),
            layer: active.layer(),
            layer_name: "intensity",
            dtype: IntensityDType::Uint16,
            timepoint: active.timepoint(),
            scale: active.scale(),
        },
    );

    assert!(matches!(histogram.status, HistogramStatus::Sampled { .. }));
    assert_eq!(histogram.sample_count, 2);
}

#[test]
fn histogram_without_a_requested_lease_reports_loading_without_io() {
    let histogram = histogram_for_test(&RetainedLeases::new(), 0, 0, 0);
    match histogram.status {
        HistogramStatus::Pending { reason } => assert!(reason.contains("leases loading")),
        other => panic!("expected pending lease histogram, got {other:?}"),
    }
    assert_eq!(histogram.sample_count, 0);
    assert!(histogram.bins.is_empty());
}

#[test]
fn workbench_shell_exposes_histogram_and_auto_window_controls() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

    harness.get_by_label("histogram");
    harness.get_by_label("histogram bins");
    harness.get_by_label("Auto Dense");
    harness.get_by_label("Auto Signal");
}

#[test]
fn auto_signal_window_ignores_dominant_low_background_bin() {
    let histogram = LayerHistogramSummary {
        status: HistogramStatus::Exact,
        bin_count: 5,
        sample_count: 100,
        min_value: 0.0,
        max_value: 100.0,
        bins: vec![80, 0, 5, 10, 5],
    };

    let dense = auto_dense_window_from_histogram(&histogram).unwrap();
    let signal = auto_signal_window_from_histogram(&histogram).unwrap();

    assert!(dense.low() < 20.0);
    assert!(signal.low() >= 40.0);
    assert!(signal.high() > signal.low());
    assert!(signal.high() <= histogram.max_value);
}

#[test]
fn histogram_bins_label_is_plain_product_text_not_ascii_art() {
    let histogram = LayerHistogramSummary {
        status: HistogramStatus::Exact,
        bin_count: 4,
        sample_count: 10,
        min_value: 0.0,
        max_value: 3.0,
        bins: vec![0, 2, 8, 0],
    };

    let label = histogram_bins_label(&histogram);

    assert_eq!(label, "4 bins, peak count 8");
    assert!(!label.contains('@'));
    assert!(!label.contains('#'));
}

#[test]
fn application_playback_commands_reconcile_transient_state_and_timepoint() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let ctx = egui::Context::default();

    assert_eq!(
        app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &ctx),
        Ok(CommandEffect::Changed)
    );
    let snapshot = app.application.snapshot();
    assert!(snapshot.transient().playback_active());
    assert_eq!(snapshot.transient().last_playback_tick(), None);
    assert!(
        !app
            .dataset
            .scope_requirements(dataset_requests::SCOPE_PLAYBACK)
            .is_empty()
    );
    assert_eq!(
        playback_status_label(
            snapshot.transient().playback_active(),
            application_view(&snapshot).timepoint(),
            workbench_playback_runtime::catalog_timepoint_count(&snapshot),
        ),
        "playback playing | t 1/3"
    );

    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(10), &ctx)
        .unwrap();
    assert_eq!(
        application_view(&app.application.snapshot()).timepoint(),
        TimeIndex::new(0)
    );
    for (tick, expected) in [(11, 1), (12, 2), (13, 0)] {
        app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(tick), &ctx)
            .unwrap();
        assert_eq!(
            application_view(&app.application.snapshot()).timepoint(),
            TimeIndex::new(expected)
        );
    }
    let snapshot = app.application.snapshot();
    let previous = stepped_timepoint(
        application_view(&snapshot).timepoint(),
        workbench_playback_runtime::catalog_timepoint_count(&snapshot),
        -1,
    );
    app.apply_application_command(ApplicationCommand::SetTimepoint(previous), &ctx)
        .unwrap();

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(false), &ctx)
        .unwrap();
    let snapshot = app.application.snapshot();
    assert!(!snapshot.transient().playback_active());
    assert_eq!(snapshot.transient().last_playback_tick(), None);
    assert!(
        app.dataset
            .scope_requirements(dataset_requests::SCOPE_PLAYBACK)
            .is_empty()
    );
    assert_eq!(
        playback_status_label(
            snapshot.transient().playback_active(),
            application_view(&snapshot).timepoint(),
            workbench_playback_runtime::catalog_timepoint_count(&snapshot),
        ),
        "playback stopped | t 3/3"
    );
}

#[test]
fn timepoint_command_dirties_cross_section_panels_without_dirtying_3d_panel() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = mirante4d_render_api::RenderExtent::new(480, 360).unwrap();

    let initial_snapshot = app.application.snapshot();
    let cross_section = *application_view(&initial_snapshot).cross_section();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section,
        },
        &ctx,
    )
    .unwrap();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        assert!(
            app.render_coordination
                .record_viewports(panel_id.presentation_slot(), presentation, render)
        );
        let generation = app
            .render_coordination
            .surface(panel_id.presentation_slot())
            .generation();
        assert!(
            app.render_coordination
                .record_cross_section_presentation(
                    panel_id.presentation_slot(),
                    generation,
                    CrossSectionPanelScheduleState::missing_viewport(generation),
                )
        );
    }
    let generations_before =
        [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz].map(|panel_id| {
            app.render_coordination
                .surface(panel_id.presentation_slot())
                .generation()
        });

    app.apply_application_command(ApplicationCommand::SetTimepoint(TimeIndex::new(1)), &ctx)
        .unwrap();

    assert_eq!(
        application_view(&app.application.snapshot()).timepoint(),
        TimeIndex::new(1)
    );
    let runtime = &app.render_coordination;
    for (panel_id, generation_before) in [PanelId::Xy, PanelId::Xz, PanelId::Yz].into_iter().zip([
        generations_before[0],
        generations_before[1],
        generations_before[3],
    ]) {
        let panel = runtime.surface(panel_id.presentation_slot());
        assert!(panel.generation() > generation_before);
        assert!(
            !panel.display_current(),
            "{} should be dirty after a timepoint change",
            panel_id.label()
        );
    }
    let three_d = runtime.surface(PanelId::ThreeD.presentation_slot());
    assert_eq!(three_d.generation(), generations_before[2]);
}

#[test]
fn workbench_shell_exposes_playback_controls_for_time_series() {
    use egui_kittest::{Harness, kittest::Queryable};

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_target_fixture(tempdir.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(root).unwrap();

    let harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| test_workbench_app_for_ui_harness(cc, opened));

    harness.get_by_label("First");
    harness.get_by_label("Prev");
    harness.get_by_label("Play");
    harness.get_by_label("Next");
    harness.get_by_label("Last");
    harness.get_by_label("playback stopped | t 1/3");
}
