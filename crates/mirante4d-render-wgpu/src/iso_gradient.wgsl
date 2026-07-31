// Six-tap grid gradient transformed into world space by the inverse transpose.

fn iso_gradient(layer_index: u32, point: vec3<f32>) -> vec4<f32> {
    let negative_x = sample_grid(layer_index, point - vec3<f32>(1.0, 0.0, 0.0));
    let positive_x = sample_grid(layer_index, point + vec3<f32>(1.0, 0.0, 0.0));
    let negative_y = sample_grid(layer_index, point - vec3<f32>(0.0, 1.0, 0.0));
    let positive_y = sample_grid(layer_index, point + vec3<f32>(0.0, 1.0, 0.0));
    let negative_z = sample_grid(layer_index, point - vec3<f32>(0.0, 0.0, 1.0));
    let positive_z = sample_grid(layer_index, point + vec3<f32>(0.0, 0.0, 1.0));
    if negative_x.kind == SAMPLE_MISSING || positive_x.kind == SAMPLE_MISSING
        || negative_y.kind == SAMPLE_MISSING || positive_y.kind == SAMPLE_MISSING
        || negative_z.kind == SAMPLE_MISSING || positive_z.kind == SAMPLE_MISSING {
        return vec4<f32>(0.0, 0.0, 0.0, f32(SAMPLE_MISSING));
    }
    if negative_x.kind != SAMPLE_VALID || positive_x.kind != SAMPLE_VALID
        || negative_y.kind != SAMPLE_VALID || positive_y.kind != SAMPLE_VALID
        || negative_z.kind != SAMPLE_VALID || positive_z.kind != SAMPLE_VALID {
        return vec4<f32>(0.0, 0.0, 0.0, f32(SAMPLE_INVALID));
    }
    let grid_gradient = vec3<f32>(
        positive_x.value - negative_x.value,
        positive_y.value - negative_y.value,
        positive_z.value - negative_z.value,
    );
    let world_gradient = vec3<f32>(
        layer_f32(layer_index, 32u) * grid_gradient.x
            + layer_f32(layer_index, 36u) * grid_gradient.y
            + layer_f32(layer_index, 40u) * grid_gradient.z,
        layer_f32(layer_index, 33u) * grid_gradient.x
            + layer_f32(layer_index, 37u) * grid_gradient.y
            + layer_f32(layer_index, 41u) * grid_gradient.z,
        layer_f32(layer_index, 34u) * grid_gradient.x
            + layer_f32(layer_index, 38u) * grid_gradient.y
            + layer_f32(layer_index, 42u) * grid_gradient.z,
    );
    let length_squared = dot(world_gradient, world_gradient);
    if length_squared <= EPSILON {
        return vec4<f32>(0.0, 0.0, 0.0, f32(SAMPLE_VALID));
    }
    return vec4<f32>(world_gradient * inverseSqrt(length_squared), f32(SAMPLE_VALID));
}
