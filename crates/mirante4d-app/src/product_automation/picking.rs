use mirante4d_renderer::{
    GridPosition, PickCompleteness, PickHit, PickHitKind, PickPolicy, PickValue, ScreenPosition,
};
use serde_json::{Value, json};

use crate::{ViewportHover, ViewportIntensity};

pub(crate) fn viewport_hover_json(hover: ViewportHover) -> Value {
    json!({
        "x": hover.x,
        "y": hover.y,
        "intensity": viewport_intensity_json(hover.intensity),
        "intensity_display": hover.intensity.to_string(),
    })
}

fn viewport_intensity_json(intensity: ViewportIntensity) -> Value {
    match intensity {
        ViewportIntensity::U8(value) => json!({
            "dtype": "uint8",
            "value": value,
        }),
        ViewportIntensity::U16(value) => json!({
            "dtype": "uint16",
            "value": value,
        }),
        ViewportIntensity::F32(value) => json!({
            "dtype": "float32",
            "value": finite_f32_json(value),
        }),
    }
}

pub(crate) fn pick_hit_json(hit: &PickHit) -> Value {
    json!({
        "kind": pick_hit_kind_label(hit.kind),
        "layer_id": hit.layer_id.as_ref().map(|id| id.as_str()),
        "object_id": hit.object_id.as_ref().map(|id| id.as_str()),
        "source_layer_id": hit.source_layer_id.as_ref().map(|id| id.as_str()),
        "timepoint": hit.timepoint.get(),
        "world_position": hit.world_position.map(world_position_json).unwrap_or(Value::Null),
        "grid_position": hit.grid_position.map(grid_position_json).unwrap_or(Value::Null),
        "screen_position": hit.screen_position.map(screen_position_json).unwrap_or(Value::Null),
        "value": hit.value.as_ref().map(pick_value_json).unwrap_or(Value::Null),
        "policy": pick_policy_label(hit.policy),
        "completeness": pick_completeness_label(hit.completeness),
    })
}

fn pick_value_json(value: &PickValue) -> Value {
    match value {
        PickValue::IntensityU8(value) => json!({
            "kind": "intensity_u8",
            "value": value,
        }),
        PickValue::IntensityU16(value) => json!({
            "kind": "intensity_u16",
            "value": value,
        }),
        PickValue::IntensityF32(value) => json!({
            "kind": "intensity_f32",
            "value": finite_f32_json(*value),
        }),
        PickValue::ObjectMetadata(value) => json!({
            "kind": "object_metadata",
            "value": value,
        }),
    }
}

fn world_position_json(position: glam::DVec3) -> Value {
    json!({
        "x": finite_f64_json(position.x),
        "y": finite_f64_json(position.y),
        "z": finite_f64_json(position.z),
    })
}

fn grid_position_json(position: GridPosition) -> Value {
    json!({
        "x": finite_f64_json(position.x),
        "y": finite_f64_json(position.y),
        "z": finite_f64_json(position.z),
    })
}

fn screen_position_json(position: ScreenPosition) -> Value {
    json!({
        "x": finite_f32_json(position.x),
        "y": finite_f32_json(position.y),
    })
}

fn finite_f32_json(value: f32) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}

fn finite_f64_json(value: f64) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}

fn pick_hit_kind_label(kind: PickHitKind) -> &'static str {
    match kind {
        PickHitKind::Voxel => "voxel",
        PickHitKind::Track => "track",
        PickHitKind::Roi => "roi",
        PickHitKind::Annotation => "annotation",
        PickHitKind::AnnotationHandle => "annotation_handle",
        PickHitKind::Measurement => "measurement",
        PickHitKind::Plane => "plane",
        PickHitKind::Ui => "ui",
        PickHitKind::Empty => "empty",
    }
}

fn pick_policy_label(policy: PickPolicy) -> &'static str {
    match policy {
        PickPolicy::SceneObject => "scene_object",
        PickPolicy::FirstThresholdHit => "first_threshold_hit",
        PickPolicy::MipArgmax => "mip_argmax",
        PickPolicy::ProbeRay => "probe_ray",
    }
}

fn pick_completeness_label(completeness: PickCompleteness) -> &'static str {
    match completeness {
        PickCompleteness::Exact => "exact",
        PickCompleteness::Approximate => "approximate",
        PickCompleteness::Incomplete => "incomplete",
        PickCompleteness::Loading => "loading",
    }
}
