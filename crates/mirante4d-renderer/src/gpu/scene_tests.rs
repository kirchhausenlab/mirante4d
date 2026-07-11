use glam::DVec3;
use mirante4d_domain::TimeIndex;

use crate::scene_render::build_scene_render_commands;
use crate::{
    CoordinateSpace, OcclusionPolicy, PickCompleteness, PickHitKind, PickPolicy, PickQuery,
    RenderViewport, SceneColorRgba, SceneFrameContext, SceneGeometry, SceneLayer, SceneLayerId,
    SceneLayerKind, SceneObject, SceneObjectId, SceneRgbaImage, SceneStyle, SceneTime,
    extract_scene_draw_list,
};

use super::{GpuRenderTimings, GpuRenderer, test_support::*};

fn assert_gpu_timings_if_enabled(
    renderer: &GpuRenderer,
    timings: Option<GpuRenderTimings>,
    label: &str,
) {
    if renderer.adapter_diagnostics().timestamp_queries_enabled {
        assert!(
            timings.and_then(|timings| timings.gpu_compute_ns).is_some(),
            "{label} GPU timestamp-enabled render must report GPU compute time"
        );
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_scene_renderer_draws_world_interaction_and_screen_primitives() {
    let renderer = GpuRenderer::new_blocking().unwrap();
    let viewport = RenderViewport::new(64, 64).unwrap();
    let camera = scene_test_camera();
    let base = SceneRgbaImage::solid(64, 64, SceneColorRgba::new(0, 0, 0, 255)).unwrap();
    let track_color = SceneColorRgba::new(255, 0, 0, 255);
    let annotation_color = SceneColorRgba::new(0, 255, 0, 255);
    let measurement_color = SceneColorRgba::new(0, 0, 255, 255);
    let interaction_color = SceneColorRgba::new(255, 0, 255, 255);
    let reference_color = SceneColorRgba::new(255, 255, 255, 255);
    let layers = vec![
        line_layer(
            "tracks",
            SceneLayerKind::Track,
            track_color,
            "track-a",
            DVec3::new(-2.0, -2.0, 0.0),
            DVec3::new(2.0, -2.0, 0.0),
        ),
        line_layer(
            "annotations",
            SceneLayerKind::Annotation,
            annotation_color,
            "roi-a",
            DVec3::new(-2.0, 0.0, 0.0),
            DVec3::new(2.0, 0.0, 0.0),
        ),
        line_layer(
            "measurements",
            SceneLayerKind::Measurement,
            measurement_color,
            "measurement-a",
            DVec3::new(-2.0, 2.0, 0.0),
            DVec3::new(2.0, 2.0, 0.0),
        ),
        SceneLayer::new(
            SceneLayerId::new("interaction").unwrap(),
            SceneLayerKind::Interaction,
        )
        .with_style(SceneStyle::new(interaction_color))
        .with_object(SceneObject::new(
            SceneObjectId::new("hover").unwrap(),
            CoordinateSpace::Screen,
            SceneTime::Static,
            OcclusionPolicy::AlwaysOnTop,
            SceneGeometry::Point {
                position: DVec3::new(55.0, 55.0, 0.0),
                radius_px: 3.0,
            },
        )),
        SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_style(SceneStyle::new(reference_color))
        .with_object(SceneObject::new(
            SceneObjectId::new("timestamp").unwrap(),
            CoordinateSpace::Screen,
            SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(2.0, 2.0),
                text: "t0".to_owned(),
            },
        )),
    ];
    let draw_list = extract_scene_draw_list(&layers, SceneFrameContext::new(TimeIndex::new(0)));
    let expected_render_commands = build_scene_render_commands(&draw_list, camera, viewport)
        .commands()
        .len() as u64;

    let gpu_output = renderer
        .render_scene_layers_rgba_with_timings(&base, &draw_list, camera, viewport)
        .unwrap();
    assert_gpu_timings_if_enabled(&renderer, gpu_output.timings, "scene RGBA readback");
    let output = gpu_output.output;

    assert_eq!(output.diagnostics.input_draw_items, 5);
    assert_eq!(output.diagnostics.render_commands, expected_render_commands);
    assert!(output.diagnostics.render_commands >= output.diagnostics.input_draw_items);
    assert_eq!(output.diagnostics.unsupported_draw_items, 0);
    assert!(output.diagnostics.changed_pixels > 0);
    assert_eq!(output.image.pixel(32, 19).unwrap(), measurement_color);
    assert_eq!(output.image.pixel(32, 32).unwrap(), annotation_color);
    assert_eq!(output.image.pixel(32, 45).unwrap(), track_color);
    assert_eq!(output.image.pixel(55, 55).unwrap(), interaction_color);
    assert!(
        output
            .image
            .pixels()
            .contains(&reference_color.packed_rgba_u32())
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_scene_pick_returns_topmost_selectable_command_id() {
    let renderer = GpuRenderer::new_blocking().unwrap();
    let viewport = RenderViewport::new(64, 64).unwrap();
    let camera = scene_test_camera();
    let layer = SceneLayer::new(
        SceneLayerId::new("annotations").unwrap(),
        SceneLayerKind::Annotation,
    )
    .with_object(SceneObject::new(
        SceneObjectId::new("back").unwrap(),
        CoordinateSpace::Screen,
        SceneTime::Static,
        OcclusionPolicy::ScreenSpace,
        SceneGeometry::Point {
            position: DVec3::new(32.0, 32.0, 0.0),
            radius_px: 10.0,
        },
    ))
    .with_object(SceneObject::new(
        SceneObjectId::new("front").unwrap(),
        CoordinateSpace::Screen,
        SceneTime::Static,
        OcclusionPolicy::ScreenSpace,
        SceneGeometry::Point {
            position: DVec3::new(32.0, 32.0, 0.0),
            radius_px: 6.0,
        },
    ));
    let draw_list = extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0)));

    let pick = renderer
        .pick_scene_object_id_with_timings(
            &draw_list,
            camera,
            viewport,
            PickQuery {
                timepoint: TimeIndex::new(0),
                screen_position: crate::ScreenPosition::new(32.0, 32.0),
            },
        )
        .unwrap();
    assert_gpu_timings_if_enabled(&renderer, pick.timings, "scene object pick");
    let hit = pick.hit;

    assert_eq!(hit.kind, PickHitKind::Annotation);
    assert_eq!(hit.object_id.as_ref().map(|id| id.as_str()), Some("front"));
    assert_eq!(hit.policy, PickPolicy::SceneObject);
    assert_eq!(hit.completeness, PickCompleteness::Exact);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_scene_renderer_respects_pass_order() {
    let renderer = GpuRenderer::new_blocking().unwrap();
    let viewport = RenderViewport::new(64, 64).unwrap();
    let camera = scene_test_camera();
    let base = SceneRgbaImage::solid(64, 64, SceneColorRgba::new(0, 0, 0, 255)).unwrap();
    let world_color = SceneColorRgba::new(255, 0, 0, 255);
    let interaction_color = SceneColorRgba::new(255, 0, 255, 255);
    let screen_color = SceneColorRgba::new(0, 255, 0, 255);
    let layers = vec![
        SceneLayer::new(
            SceneLayerId::new("world").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_style(SceneStyle::new(world_color))
        .with_object(SceneObject::new(
            SceneObjectId::new("world-point").unwrap(),
            CoordinateSpace::World,
            SceneTime::Static,
            OcclusionPolicy::DepthTestGeometry,
            SceneGeometry::Point {
                position: DVec3::ZERO,
                radius_px: 8.0,
            },
        )),
        SceneLayer::new(
            SceneLayerId::new("interaction").unwrap(),
            SceneLayerKind::Interaction,
        )
        .with_style(SceneStyle::new(interaction_color))
        .with_object(SceneObject::new(
            SceneObjectId::new("hover-point").unwrap(),
            CoordinateSpace::Screen,
            SceneTime::Static,
            OcclusionPolicy::AlwaysOnTop,
            SceneGeometry::Point {
                position: DVec3::new(32.0, 32.0, 0.0),
                radius_px: 8.0,
            },
        )),
        SceneLayer::new(
            SceneLayerId::new("screen").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_style(SceneStyle::new(screen_color))
        .with_object(SceneObject::new(
            SceneObjectId::new("screen-label").unwrap(),
            CoordinateSpace::Screen,
            SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(28.0, 28.0),
                text: "top".to_owned(),
            },
        )),
    ];
    let draw_list = extract_scene_draw_list(&layers, SceneFrameContext::new(TimeIndex::new(0)));
    let expected_commands_by_pass =
        build_scene_render_commands(&draw_list, camera, viewport).commands_by_pass;

    let output = renderer
        .render_scene_layers_rgba(&base, &draw_list, camera, viewport)
        .unwrap();

    assert_eq!(
        output.diagnostics.commands_by_pass,
        expected_commands_by_pass
    );
    assert_eq!(output.image.pixel(32, 32).unwrap(), screen_color);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_scene_renderer_preserves_base_image_for_empty_draw_list() {
    let renderer = GpuRenderer::new_blocking().unwrap();
    let viewport = RenderViewport::new(8, 8).unwrap();
    let camera = scene_test_camera();
    let base_color = SceneColorRgba::new(11, 22, 33, 255);
    let base = SceneRgbaImage::solid(8, 8, base_color).unwrap();
    let draw_list = extract_scene_draw_list(&[], SceneFrameContext::new(TimeIndex::new(0)));

    let output = renderer
        .render_scene_layers_rgba(&base, &draw_list, camera, viewport)
        .unwrap();

    assert_eq!(output.image.pixels(), base.pixels());
    assert_eq!(output.diagnostics.render_commands, 0);
    assert_eq!(output.diagnostics.changed_pixels, 0);
}
