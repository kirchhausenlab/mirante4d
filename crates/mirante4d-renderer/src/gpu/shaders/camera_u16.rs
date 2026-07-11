const DENSE_SAMPLE_VOXEL_FUNCTION: &str = r#"fn brick_linear_index(z: u32, y: u32, x: u32) -> u32 {
    return 0u;
}

fn brick_skip_reason_exact(
    brick_linear: u32,
    render_mode: u32,
    current_max_value: u32,
    has_current_value: bool,
    iso_invert: u32,
    iso_display_level: f32,
) -> u32 {
    return 0u;
}

fn brick_skip_reason_smooth(
    point: vec3<f32>,
    render_mode: u32,
    current_max_value: f32,
    has_current_value: bool,
    iso_invert: u32,
    iso_display_level: f32,
    shape_x: u32,
    shape_y: u32,
    shape_z: u32,
) -> u32 {
    return 0u;
}

fn record_brick_skip(reason: u32) {
}

fn brick_axis_exit_t(index: i32, step: i32, next_t: f32, delta_t: f32, brick_size: u32) -> f32 {
    return BIG_T;
}

fn smooth_brick_exit_t(
    point: vec3<f32>,
    ray_direction: vec3<f32>,
    current_t: f32,
    shape_x: u32,
    shape_y: u32,
    shape_z: u32,
) -> f32 {
    return BIG_T;
}

fn sample_voxel(z: u32, y: u32, x: u32, shape_y: u32, shape_x: u32) -> u32 {
    let linear_index = (z * shape_y + y) * shape_x + x;
    return packed_voxels[linear_index];
}"#;

const BRICKED_SAMPLE_VOXEL_FUNCTION: &str = r#"const BRICK_METADATA_WORDS: u32 = 4u;
const BRICK_METADATA_HAS_VALID_FLAG: u32 = 0x2u;
const BRICK_METADATA_MIN_MAX_VALID_FLAG: u32 = 0x4u;
const BRICK_SKIP_REASON_NONE: u32 = 0u;
const BRICK_SKIP_REASON_EMPTY: u32 = 1u;
const BRICK_SKIP_REASON_MIP_RANGE: u32 = 2u;
const BRICK_SKIP_REASON_ISO_RANGE: u32 = 3u;
const BRICK_SKIP_REASON_DVR_RANGE: u32 = 4u;

struct BrickNeighborhoodRange {
    valid: bool,
    has_valid_samples: bool,
    min_value: f32,
    max_value: f32,
};

fn brick_linear_index(z: u32, y: u32, x: u32) -> u32 {
    let brick_x_size = params_u32[8];
    let brick_y_size = params_u32[9];
    let brick_z_size = params_u32[10];
    let grid_x = params_u32[11];
    let grid_y = params_u32[12];

    let brick_x = x / brick_x_size;
    let brick_y = y / brick_y_size;
    let brick_z = z / brick_z_size;
    return (brick_z * grid_y + brick_y) * grid_x + brick_x;
}

fn brick_linear_from_coords(brick_z: u32, brick_y: u32, brick_x: u32) -> u32 {
    let grid_x = params_u32[11];
    let grid_y = params_u32[12];
    return (brick_z * grid_y + brick_y) * grid_x + brick_x;
}

fn brick_metadata_flags(brick_linear: u32) -> u32 {
    return brick_metadata[brick_linear * BRICK_METADATA_WORDS];
}

fn brick_metadata_min(brick_linear: u32) -> f32 {
    return bitcast<f32>(brick_metadata[brick_linear * BRICK_METADATA_WORDS + 1u]);
}

fn brick_metadata_max(brick_linear: u32) -> f32 {
    return bitcast<f32>(brick_metadata[brick_linear * BRICK_METADATA_WORDS + 2u]);
}

fn brick_has_valid_samples(brick_linear: u32) -> bool {
    return (brick_metadata_flags(brick_linear) & BRICK_METADATA_HAS_VALID_FLAG) != 0u;
}

fn brick_has_valid_min_max(brick_linear: u32) -> bool {
    return (brick_metadata_flags(brick_linear) & BRICK_METADATA_MIN_MAX_VALID_FLAG) != 0u;
}

fn brick_iso_display_max(brick_linear: u32, iso_invert: u32) -> f32 {
    if (iso_invert != 0u) {
        return iso_display_value(brick_metadata_min(brick_linear), iso_invert);
    }
    return iso_display_value(brick_metadata_max(brick_linear), iso_invert);
}

fn brick_skip_reason_exact(
    brick_linear: u32,
    render_mode: u32,
    current_max_value: u32,
    has_current_value: bool,
    iso_invert: u32,
    iso_display_level: f32,
) -> u32 {
    if (page_table[brick_linear] == 0u) {
        return BRICK_SKIP_REASON_NONE;
    }
    if (!brick_has_valid_samples(brick_linear)) {
        return BRICK_SKIP_REASON_EMPTY;
    }
    if (!brick_has_valid_min_max(brick_linear)) {
        return BRICK_SKIP_REASON_NONE;
    }
    if (render_mode == 0u) {
        if (has_current_value && brick_metadata_max(brick_linear) <= f32(current_max_value)) {
            return BRICK_SKIP_REASON_MIP_RANGE;
        }
        return BRICK_SKIP_REASON_NONE;
    }
    if (render_mode == 1u) {
        if (brick_iso_display_max(brick_linear, iso_invert) < iso_display_level) {
            return BRICK_SKIP_REASON_ISO_RANGE;
        }
        return BRICK_SKIP_REASON_NONE;
    }
    if (render_mode == 2u) {
        if (params_f32[15] <= EPSILON || params_f32[34] <= EPSILON) {
            return BRICK_SKIP_REASON_DVR_RANGE;
        }
        let opacity_bound = max(
            dvr_opacity_value(brick_metadata_min(brick_linear)),
            dvr_opacity_value(brick_metadata_max(brick_linear))
        );
        if (opacity_bound <= EPSILON) {
            return BRICK_SKIP_REASON_DVR_RANGE;
        }
        return BRICK_SKIP_REASON_NONE;
    }
    return BRICK_SKIP_REASON_NONE;
}

fn brick_coord_inside(index: i32, limit: u32) -> bool {
    return index >= 0 && index < i32(limit);
}

fn brick_neighborhood_range(z: u32, y: u32, x: u32) -> BrickNeighborhoodRange {
    let brick_x_size = params_u32[8];
    let brick_y_size = params_u32[9];
    let brick_z_size = params_u32[10];
    let grid_x = params_u32[11];
    let grid_y = params_u32[12];
    let grid_z = params_u32[13];
    let center_x = x / brick_x_size;
    let center_y = y / brick_y_size;
    let center_z = z / brick_z_size;

    var result = BrickNeighborhoodRange(true, false, 0.0, 0.0);
    for (var dz = 0u; dz < 3u; dz = dz + 1u) {
        let neighbor_z = i32(center_z) + i32(dz) - 1;
        if (!brick_coord_inside(neighbor_z, grid_z)) {
            continue;
        }
        for (var dy = 0u; dy < 3u; dy = dy + 1u) {
            let neighbor_y = i32(center_y) + i32(dy) - 1;
            if (!brick_coord_inside(neighbor_y, grid_y)) {
                continue;
            }
            for (var dx = 0u; dx < 3u; dx = dx + 1u) {
                let neighbor_x = i32(center_x) + i32(dx) - 1;
                if (!brick_coord_inside(neighbor_x, grid_x)) {
                    continue;
                }
                let brick_linear = brick_linear_from_coords(
                    u32(neighbor_z),
                    u32(neighbor_y),
                    u32(neighbor_x)
                );
                if (page_table[brick_linear] == 0u) {
                    return BrickNeighborhoodRange(false, false, 0.0, 0.0);
                }
                if (!brick_has_valid_samples(brick_linear)) {
                    continue;
                }
                if (!brick_has_valid_min_max(brick_linear)) {
                    return BrickNeighborhoodRange(false, false, 0.0, 0.0);
                }
                let brick_min = brick_metadata_min(brick_linear);
                let brick_max = brick_metadata_max(brick_linear);
                if (!result.has_valid_samples) {
                    result.has_valid_samples = true;
                    result.min_value = brick_min;
                    result.max_value = brick_max;
                } else {
                    result.min_value = min(result.min_value, brick_min);
                    result.max_value = max(result.max_value, brick_max);
                }
            }
        }
    }
    return result;
}

fn brick_skip_reason_smooth(
    point: vec3<f32>,
    render_mode: u32,
    current_max_value: f32,
    has_current_value: bool,
    iso_invert: u32,
    iso_display_level: f32,
    shape_x: u32,
    shape_y: u32,
    shape_z: u32,
) -> u32 {
    if (render_mode == 1u) {
        return BRICK_SKIP_REASON_NONE;
    }
    let x = interpolation_axis(point.x, shape_x);
    let y = interpolation_axis(point.y, shape_y);
    let z = interpolation_axis(point.z, shape_z);
    if (!x.valid || !y.valid || !z.valid) {
        return BRICK_SKIP_REASON_NONE;
    }

    let range = brick_neighborhood_range(z.lower, y.lower, x.lower);
    if (!range.valid) {
        return BRICK_SKIP_REASON_NONE;
    }
    if (!range.has_valid_samples) {
        return BRICK_SKIP_REASON_EMPTY;
    }
    if (render_mode == 0u) {
        if (has_current_value && range.max_value <= current_max_value) {
            return BRICK_SKIP_REASON_MIP_RANGE;
        }
        return BRICK_SKIP_REASON_NONE;
    }
    if (render_mode == 2u) {
        if (params_f32[15] <= EPSILON || params_f32[34] <= EPSILON) {
            return BRICK_SKIP_REASON_DVR_RANGE;
        }
        let opacity_bound = max(
            dvr_opacity_value(range.min_value),
            dvr_opacity_value(range.max_value)
        );
        if (opacity_bound <= EPSILON) {
            return BRICK_SKIP_REASON_DVR_RANGE;
        }
    }
    return BRICK_SKIP_REASON_NONE;
}

fn record_brick_skip(reason: u32) {
    if (reason == BRICK_SKIP_REASON_NONE || reason > BRICK_SKIP_REASON_DVR_RANGE) {
        return;
    }
    atomicAdd(&brick_skip_diagnostics[0], 1u);
    atomicAdd(&brick_skip_diagnostics[reason], 1u);
}

fn brick_axis_exit_t(index: i32, step: i32, next_t: f32, delta_t: f32, brick_size: u32) -> f32 {
    if (step == 0 || brick_size == 0u) {
        return BIG_T;
    }
    let local_index = u32(index) % brick_size;
    var boundaries_to_exit = local_index + 1u;
    if (step > 0) {
        boundaries_to_exit = brick_size - local_index;
    }
    return next_t + f32(boundaries_to_exit - 1u) * delta_t;
}

fn smooth_brick_axis_exit_t(
    coordinate: f32,
    direction: f32,
    current_t: f32,
    brick_index: u32,
    brick_size: u32,
    shape: u32,
) -> f32 {
    if (abs(direction) <= EPSILON || brick_size == 0u || shape == 0u) {
        return BIG_T;
    }
    var boundary = f32(brick_index * brick_size) - 0.5;
    if (direction > EPSILON) {
        boundary = f32(min((brick_index + 1u) * brick_size, shape)) - 0.5;
    }
    return current_t + max((boundary - coordinate) / direction, 0.0);
}

fn smooth_brick_exit_t(
    point: vec3<f32>,
    ray_direction: vec3<f32>,
    current_t: f32,
    shape_x: u32,
    shape_y: u32,
    shape_z: u32,
) -> f32 {
    let x = interpolation_axis(point.x, shape_x);
    let y = interpolation_axis(point.y, shape_y);
    let z = interpolation_axis(point.z, shape_z);
    if (!x.valid || !y.valid || !z.valid) {
        return BIG_T;
    }
    let brick_x_size = params_u32[8];
    let brick_y_size = params_u32[9];
    let brick_z_size = params_u32[10];
    let brick_x = x.lower / brick_x_size;
    let brick_y = y.lower / brick_y_size;
    let brick_z = z.lower / brick_z_size;
    return min(
        smooth_brick_axis_exit_t(point.x, ray_direction.x, current_t, brick_x, brick_x_size, shape_x),
        min(
            smooth_brick_axis_exit_t(point.y, ray_direction.y, current_t, brick_y, brick_y_size, shape_y),
            smooth_brick_axis_exit_t(point.z, ray_direction.z, current_t, brick_z, brick_z_size, shape_z)
        )
    );
}

fn sample_voxel(z: u32, y: u32, x: u32, shape_y: u32, shape_x: u32) -> u32 {
    let brick_x_size = params_u32[8];
    let brick_y_size = params_u32[9];
    let brick_z_size = params_u32[10];

    let brick_x = x / brick_x_size;
    let brick_y = y / brick_y_size;
    let brick_z = z / brick_z_size;
    let brick_linear = brick_linear_index(z, y, x);
    let slot_plus_one = page_table[brick_linear];
    if (slot_plus_one == 0u) {
        return MISSING_SAMPLE_FLAG;
    }
    if (!brick_has_valid_samples(brick_linear)) {
        return SAMPLE_INVALID_FLAG;
    }

    let local_x = x - brick_x * brick_x_size;
    let local_y = y - brick_y * brick_y_size;
    let local_z = z - brick_z * brick_z_size;
    let local_index = (local_z * brick_y_size + local_y) * brick_x_size + local_x;
    let packed_u32_per_brick = params_u32[15];
    let values_per_word = params_u32[16];
    let bits_per_value = params_u32[17];
    let value_mask = params_u32[18];
    let valid_u32_per_brick = params_u32[19];
    let slot = slot_plus_one - 1u;

    let valid_word_index = slot * valid_u32_per_brick + local_index / 32u;
    let valid_bit = local_index % 32u;
    if ((validity_bits[valid_word_index] & (1u << valid_bit)) == 0u) {
        return SAMPLE_INVALID_FLAG;
    }

    let value_word_index = local_index / values_per_word;
    let value_offset = local_index % values_per_word;
    let value_shift = value_offset * bits_per_value;
    let atlas_index = slot * packed_u32_per_brick + value_word_index;
    return (packed_voxels[atlas_index] >> value_shift) & value_mask;
}"#;

pub(crate) fn bricked_camera_shader_source() -> String {
    CAMERA_MIP_SHADER
        .replace(
            "@group(0) @binding(3)\nvar<storage, read> params_f32: array<f32>;",
            "@group(0) @binding(3)\nvar<storage, read> params_f32: array<f32>;\n\n@group(0) @binding(4)\nvar<storage, read> page_table: array<u32>;\n\n@group(0) @binding(5)\nvar<storage, read> validity_bits: array<u32>;\n\n@group(0) @binding(6)\nvar<storage, read> brick_metadata: array<u32>;\n\n@group(0) @binding(7)\nvar<storage, read_write> brick_skip_diagnostics: array<atomic<u32>>;",
        )
        .replace(DENSE_SAMPLE_VOXEL_FUNCTION, BRICKED_SAMPLE_VOXEL_FUNCTION)
}

pub(crate) const CAMERA_MIP_SHADER: &str = r#"
const EPSILON: f32 = 0.00001;
const BIG_T: f32 = 1.0e30;
const SAMPLE_INVALID_FLAG: u32 = 0x00010000u;
const MISSING_SAMPLE_FLAG: u32 = 0x00020000u;
const OUTPUT_COVERED_FLAG: u32 = 0x80000000u;
const SMOOTH_RAY_STEP_VOXELS: f32 = 0.5;

struct CameraOutputU32 {
    packed_value: u32,
    source_material: u32,
    depth_bits: u32,
    normal_xy: u32,
    normal_z_diffuse: u32,
    specular: u32,
};

struct IsoSurfaceShading {
    normal: vec3<f32>,
    diffuse: f32,
    specular: f32,
};

@group(0) @binding(0)
var<storage, read> packed_voxels: array<u32>;

@group(0) @binding(1)
var<storage, read_write> output_pixels: array<CameraOutputU32>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> params_f32: array<f32>;

fn vector_param(offset: u32) -> vec3<f32> {
    return vec3<f32>(
        params_f32[offset],
        params_f32[offset + 1u],
        params_f32[offset + 2u]
    );
}

fn normalized_pixel_center(index: u32, extent: u32) -> f32 {
    return ((f32(index) + 0.5) / f32(extent)) * 2.0 - 1.0;
}

fn initial_voxel_index(coordinate: f32, direction: f32, limit: u32) -> i32 {
    var adjusted = coordinate + 0.5;
    if (direction < -EPSILON) {
        adjusted = adjusted - EPSILON;
    }
    return clamp(i32(floor(adjusted)), 0, i32(limit) - 1);
}

fn axis_step(direction: f32) -> i32 {
    if (direction > EPSILON) {
        return 1;
    }
    if (direction < -EPSILON) {
        return -1;
    }
    return 0;
}

fn axis_next_t(entry_coordinate: f32, direction: f32, entry_t: f32, index: i32) -> f32 {
    if (direction > EPSILON) {
        let next_boundary = f32(index) + 0.5;
        return entry_t + max((next_boundary - entry_coordinate) / direction, 0.0);
    }
    if (direction < -EPSILON) {
        let next_boundary = f32(index) - 0.5;
        return entry_t + max((next_boundary - entry_coordinate) / direction, 0.0);
    }
    return BIG_T;
}

fn axis_delta_t(direction: f32) -> f32 {
    if (direction > EPSILON) {
        return 1.0 / direction;
    }
    if (direction < -EPSILON) {
        return -1.0 / direction;
    }
    return BIG_T;
}

fn axis_inside(index: i32, limit: u32) -> bool {
    return index >= 0 && index < i32(limit);
}

struct SlabResult {
    hit: bool,
    enter: f32,
    exit: f32,
};

struct InterpAxis {
    valid: bool,
    lower: u32,
    upper: u32,
    fraction: f32,
};

struct LinearSample {
    value: f32,
    missing_samples: u32,
    valid: bool,
};

fn apply_slab(
    origin: f32,
    direction: f32,
    minimum: f32,
    maximum: f32,
    current_enter: f32,
    current_exit: f32,
) -> SlabResult {
    if (abs(direction) <= EPSILON) {
        if (origin < minimum || origin > maximum) {
            return SlabResult(false, current_enter, current_exit);
        }
        return SlabResult(true, current_enter, current_exit);
    }

    let near_t = (minimum - origin) / direction;
    let far_t = (maximum - origin) / direction;
    let axis_enter = min(near_t, far_t);
    let axis_exit = max(near_t, far_t);
    return SlabResult(
        true,
        max(current_enter, axis_enter),
        min(current_exit, axis_exit),
    );
}

fn brick_linear_index(z: u32, y: u32, x: u32) -> u32 {
    return 0u;
}

fn brick_skip_reason_exact(
    brick_linear: u32,
    render_mode: u32,
    current_max_value: u32,
    has_current_value: bool,
    iso_invert: u32,
    iso_display_level: f32,
) -> u32 {
    return 0u;
}

fn brick_skip_reason_smooth(
    point: vec3<f32>,
    render_mode: u32,
    current_max_value: f32,
    has_current_value: bool,
    iso_invert: u32,
    iso_display_level: f32,
    shape_x: u32,
    shape_y: u32,
    shape_z: u32,
) -> u32 {
    return 0u;
}

fn record_brick_skip(reason: u32) {
}

fn brick_axis_exit_t(index: i32, step: i32, next_t: f32, delta_t: f32, brick_size: u32) -> f32 {
    return BIG_T;
}

fn smooth_brick_exit_t(
    point: vec3<f32>,
    ray_direction: vec3<f32>,
    current_t: f32,
    shape_x: u32,
    shape_y: u32,
    shape_z: u32,
) -> f32 {
    return BIG_T;
}

fn sample_voxel(z: u32, y: u32, x: u32, shape_y: u32, shape_x: u32) -> u32 {
    let linear_index = (z * shape_y + y) * shape_x + x;
    return packed_voxels[linear_index];
}

fn sample_value(sample: u32) -> u32 {
    return sample & 0xffffu;
}

fn sample_missing(sample: u32) -> u32 {
    if ((sample & MISSING_SAMPLE_FLAG) != 0u) {
        return 1u;
    }
    return 0u;
}

fn sample_covered(sample: u32) -> bool {
    return sample_missing(sample) == 0u && ((sample & SAMPLE_INVALID_FLAG) == 0u);
}

fn round_u16(value: f32) -> u32 {
    return u32(round(clamp(value, 0.0, 65535.0)));
}

fn pack_u16_pair(low: u32, high: u32) -> u32 {
    return (low & 0xffffu) | ((high & 0xffffu) << 16u);
}

fn encode_normal_component(value: f32) -> u32 {
    let signed = i32(round(clamp(value, -1.0, 1.0) * 32767.0));
    return bitcast<u32>(signed) & 0xffffu;
}

fn pack_normal_xy(normal: vec3<f32>) -> u32 {
    return pack_u16_pair(encode_normal_component(normal.x), encode_normal_component(normal.y));
}

fn pack_normal_z_diffuse(normal: vec3<f32>, diffuse: f32) -> u32 {
    return pack_u16_pair(encode_normal_component(normal.z), round_u16(diffuse * 65535.0));
}

fn pack_output_value(value: u32, missing_samples: u32, covered: bool) -> u32 {
    let packed = (value & 0xffffu) |
        (min(missing_samples, 0x7fffu) << 16u) |
        select(0u, OUTPUT_COVERED_FLAG, covered);
    return packed;
}

fn empty_output() -> CameraOutputU32 {
    return CameraOutputU32(pack_output_value(0u, 0u, false), 0u, 0u, 0u, pack_u16_pair(0u, 65535u), 0u);
}

fn write_output(pixel_index: u32, value: u32, missing_samples: u32, covered: bool) {
    output_pixels[pixel_index] = CameraOutputU32(
        pack_output_value(value, missing_samples, covered),
        0u,
        0u,
        0u,
        pack_u16_pair(0u, 65535u),
        0u
    );
}

fn write_iso_output(
    pixel_index: u32,
    source_value: f32,
    display_scalar: f32,
    material_scalar: f32,
    hit_t: f32,
    missing_samples: u32,
    shading: IsoSurfaceShading,
) {
    output_pixels[pixel_index] = CameraOutputU32(
        pack_output_value(round_u16(display_scalar * 65535.0), missing_samples, true),
        pack_u16_pair(round_u16(source_value), round_u16(material_scalar * 65535.0)),
        bitcast<u32>(hit_t),
        pack_normal_xy(shading.normal),
        pack_normal_z_diffuse(shading.normal, shading.diffuse),
        round_u16(shading.specular * 65535.0)
    );
}

fn dvr_color() -> vec3<f32> {
    return vec3<f32>(
        clamp(params_f32[31], 0.0, 1.0),
        clamp(params_f32[32], 0.0, 1.0),
        clamp(params_f32[33], 0.0, 1.0)
    );
}

fn dvr_ray_physical_length(ray_direction: vec3<f32>) -> f32 {
    let grid_x_axis_world = vec3<f32>(params_f32[35], params_f32[36], params_f32[37]);
    let grid_y_axis_world = vec3<f32>(params_f32[38], params_f32[39], params_f32[40]);
    let grid_z_axis_world = vec3<f32>(params_f32[41], params_f32[42], params_f32[43]);
    let world_direction =
        grid_x_axis_world * ray_direction.x +
        grid_y_axis_world * ray_direction.y +
        grid_z_axis_world * ray_direction.z;
    return max(length(world_direction), EPSILON);
}

fn dvr_opacity_value(value: f32) -> f32 {
    let opacity_low = params_f32[44];
    let opacity_high = params_f32[45];
    let gamma = max(params_f32[46], EPSILON);
    var normalized = 0.0;
    if (opacity_high > opacity_low) {
        normalized = clamp((value - opacity_low) / (opacity_high - opacity_low), 0.0, 1.0);
    }
    return clamp(pow(normalized, 1.0 / gamma), 0.0, 1.0);
}

fn dvr_sample_alpha(opacity_value: f32, step_factor: f32) -> f32 {
    let density = max(opacity_value, 0.0) * max(params_f32[15], 0.0) * clamp(params_f32[34], 0.0, 1.0);
    return clamp(1.0 - exp(-density * max(step_factor, 0.0)), 0.0, 1.0);
}

fn dvr_accumulate(accumulated: vec4<f32>, color_value: f32, opacity_value: f32, step_factor: f32) -> vec4<f32> {
    let alpha = dvr_sample_alpha(opacity_value, step_factor);
    let transmittance = 1.0 - accumulated.a;
    return vec4<f32>(
        accumulated.rgb + transmittance * dvr_color() * color_value * alpha,
        accumulated.a + transmittance * alpha
    );
}

fn write_dvr_output(pixel_index: u32, rgba: vec4<f32>, missing_samples: u32, covered: bool) {
    let clamped = clamp(rgba, vec4<f32>(0.0), vec4<f32>(1.0));
    output_pixels[pixel_index] = CameraOutputU32(
        pack_output_value(round_u16(clamped.a * 65535.0), missing_samples, covered),
        bitcast<u32>(clamped.r),
        bitcast<u32>(clamped.g),
        bitcast<u32>(clamped.b),
        bitcast<u32>(clamped.a),
        0u
    );
}

fn interpolation_axis(coordinate: f32, limit: u32) -> InterpAxis {
    if (limit == 0u || coordinate < -0.5 - EPSILON || coordinate > f32(limit - 1u) + 0.5 + EPSILON) {
        return InterpAxis(false, 0u, 0u, 0.0);
    }
    let maximum = f32(limit - 1u);
    let clamped = clamp(coordinate, 0.0, maximum);
    let lower = u32(floor(clamped));
    let upper = min(lower + 1u, limit - 1u);
    var fraction = 0.0;
    if (upper != lower) {
        fraction = clamped - f32(lower);
    }
    return InterpAxis(true, lower, upper, fraction);
}

fn lerp_f32(a: f32, b: f32, fraction: f32) -> f32 {
    return a + (b - a) * fraction;
}

fn iso_display_value(value: f32, iso_invert: u32) -> f32 {
    let display_low = params_f32[23];
    let display_high = params_f32[24];
    let gamma = max(params_f32[27], EPSILON);
    var normalized = 0.0;
    if (display_high > display_low) {
        normalized = clamp((value - display_low) / (display_high - display_low), 0.0, 1.0);
    }
    var curved = pow(normalized, 1.0 / gamma);
    if (iso_invert != 0u) {
        curved = 1.0 - curved;
    }
    return clamp(curved, 0.0, 1.0);
}

fn sample_linear(point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32) -> LinearSample {
    let x = interpolation_axis(point.x, shape_x);
    let y = interpolation_axis(point.y, shape_y);
    let z = interpolation_axis(point.z, shape_z);
    if (!x.valid || !y.valid || !z.valid) {
        return LinearSample(0.0, 0u, false);
    }
    let c000 = sample_voxel(z.lower, y.lower, x.lower, shape_y, shape_x);
    let c100 = sample_voxel(z.lower, y.lower, x.upper, shape_y, shape_x);
    let c010 = sample_voxel(z.lower, y.upper, x.lower, shape_y, shape_x);
    let c110 = sample_voxel(z.lower, y.upper, x.upper, shape_y, shape_x);
    let c001 = sample_voxel(z.upper, y.lower, x.lower, shape_y, shape_x);
    let c101 = sample_voxel(z.upper, y.lower, x.upper, shape_y, shape_x);
    let c011 = sample_voxel(z.upper, y.upper, x.lower, shape_y, shape_x);
    let c111 = sample_voxel(z.upper, y.upper, x.upper, shape_y, shape_x);
    let covered =
        sample_covered(c000) && sample_covered(c100) &&
        sample_covered(c010) && sample_covered(c110) &&
        sample_covered(c001) && sample_covered(c101) &&
        sample_covered(c011) && sample_covered(c111);
    let c00 = lerp_f32(f32(sample_value(c000)), f32(sample_value(c100)), x.fraction);
    let c10 = lerp_f32(f32(sample_value(c010)), f32(sample_value(c110)), x.fraction);
    let c01 = lerp_f32(f32(sample_value(c001)), f32(sample_value(c101)), x.fraction);
    let c11 = lerp_f32(f32(sample_value(c011)), f32(sample_value(c111)), x.fraction);
    let c0 = lerp_f32(c00, c10, y.fraction);
    let c1 = lerp_f32(c01, c11, y.fraction);
    return LinearSample(
        lerp_f32(c0, c1, z.fraction),
        sample_missing(c000) + sample_missing(c100) + sample_missing(c010) + sample_missing(c110) +
        sample_missing(c001) + sample_missing(c101) + sample_missing(c011) + sample_missing(c111),
        covered
    );
}

fn gradient_linear(point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32) -> vec3<f32> {
    let iso_invert = params_u32[7];
    let center = sample_linear(point, shape_x, shape_y, shape_z);
    if (!center.valid || center.missing_samples != 0u) {
        return vec3<f32>(0.0);
    }
    let x_minus = sample_linear(point - vec3<f32>(1.0, 0.0, 0.0), shape_x, shape_y, shape_z);
    let x_plus = sample_linear(point + vec3<f32>(1.0, 0.0, 0.0), shape_x, shape_y, shape_z);
    let y_minus = sample_linear(point - vec3<f32>(0.0, 1.0, 0.0), shape_x, shape_y, shape_z);
    let y_plus = sample_linear(point + vec3<f32>(0.0, 1.0, 0.0), shape_x, shape_y, shape_z);
    let z_minus = sample_linear(point - vec3<f32>(0.0, 0.0, 1.0), shape_x, shape_y, shape_z);
    let z_plus = sample_linear(point + vec3<f32>(0.0, 0.0, 1.0), shape_x, shape_y, shape_z);
    var dx = 0.0;
    var dy = 0.0;
    var dz = 0.0;
    let x_minus_ok = x_minus.valid && x_minus.missing_samples == 0u;
    let x_plus_ok = x_plus.valid && x_plus.missing_samples == 0u;
    if (x_minus_ok && x_plus_ok) {
        dx = (iso_display_value(x_plus.value, iso_invert) - iso_display_value(x_minus.value, iso_invert)) * 0.5;
    } else if (x_plus_ok) {
        dx = iso_display_value(x_plus.value, iso_invert) - iso_display_value(center.value, iso_invert);
    } else if (x_minus_ok) {
        dx = iso_display_value(center.value, iso_invert) - iso_display_value(x_minus.value, iso_invert);
    }
    let y_minus_ok = y_minus.valid && y_minus.missing_samples == 0u;
    let y_plus_ok = y_plus.valid && y_plus.missing_samples == 0u;
    if (y_minus_ok && y_plus_ok) {
        dy = (iso_display_value(y_plus.value, iso_invert) - iso_display_value(y_minus.value, iso_invert)) * 0.5;
    } else if (y_plus_ok) {
        dy = iso_display_value(y_plus.value, iso_invert) - iso_display_value(center.value, iso_invert);
    } else if (y_minus_ok) {
        dy = iso_display_value(center.value, iso_invert) - iso_display_value(y_minus.value, iso_invert);
    }
    let z_minus_ok = z_minus.valid && z_minus.missing_samples == 0u;
    let z_plus_ok = z_plus.valid && z_plus.missing_samples == 0u;
    if (z_minus_ok && z_plus_ok) {
        dz = (iso_display_value(z_plus.value, iso_invert) - iso_display_value(z_minus.value, iso_invert)) * 0.5;
    } else if (z_plus_ok) {
        dz = iso_display_value(z_plus.value, iso_invert) - iso_display_value(center.value, iso_invert);
    } else if (z_minus_ok) {
        dz = iso_display_value(center.value, iso_invert) - iso_display_value(z_minus.value, iso_invert);
    }
    return vec3<f32>(dx, dy, dz);
}

fn flat_iso_surface_shading() -> IsoSurfaceShading {
    return IsoSurfaceShading(vec3<f32>(0.0), 1.0, 0.0);
}

fn normalize_or_zero(value: vec3<f32>) -> vec3<f32> {
    let len = length(value);
    if (len <= EPSILON) {
        return vec3<f32>(0.0);
    }
    return value / len;
}

fn world_space_normal_from_grid_gradient(gradient: vec3<f32>) -> vec3<f32> {
    let normal_x_axis_world = vector_param(47u);
    let normal_y_axis_world = vector_param(50u);
    let normal_z_axis_world = vector_param(53u);
    return normalize_or_zero(
        normal_x_axis_world * gradient.x +
        normal_y_axis_world * gradient.y +
        normal_z_axis_world * gradient.z
    );
}

fn iso_surface_shading(point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32, iso_shading: u32) -> IsoSurfaceShading {
    if (iso_shading == 0u) {
        return flat_iso_surface_shading();
    }
    let gradient = gradient_linear(point, shape_x, shape_y, shape_z);
    if (dot(gradient, gradient) <= EPSILON) {
        return flat_iso_surface_shading();
    }
    let normal = world_space_normal_from_grid_gradient(gradient);
    if (dot(normal, normal) <= EPSILON) {
        return flat_iso_surface_shading();
    }
    return IsoSurfaceShading(normal, 1.0, 0.0);
}

fn point_ray_t(ray_origin: vec3<f32>, ray_direction: vec3<f32>, point: vec3<f32>) -> f32 {
    let denom = dot(ray_direction, ray_direction);
    if (denom <= EPSILON) {
        return 0.0;
    }
    return dot(point - ray_origin, ray_direction) / denom;
}

@compute @workgroup_size(8, 8, 1)
fn camera_mip_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel_x = id.x;
    let pixel_y = id.y;
    let viewport_width = params_u32[0];
    let viewport_height = params_u32[1];
    if (pixel_x >= viewport_width || pixel_y >= viewport_height) {
        return;
    }
    let pixel_index = pixel_y * viewport_width + pixel_x;

    let shape_x = params_u32[2];
    let shape_y = params_u32[3];
    let shape_z = params_u32[4];
    let projection = params_u32[5];
    let render_mode = params_u32[6];
    let iso_invert = params_u32[7];
    let iso_display_level = params_f32[22];
    let sampling_policy = u32(params_f32[25] + 0.5);
    let iso_shading = u32(params_f32[26] + 0.5);

    let grid_eye = vector_param(0u);
    let grid_forward = vector_param(3u);
    let grid_right = vector_param(6u);
    let grid_up = vector_param(9u);
    let orthographic_world_per_screen_point = params_f32[12];
    let perspective_focal_length_screen_points = params_f32[13];
    let presentation_width_points = params_f32[14];
    let opacity_scale = params_f32[15];
    let presentation_height_points = params_f32[16];
    let screen_x_points = ((f32(pixel_x) + 0.5) / f32(viewport_width) - 0.5) *
        presentation_width_points;
    let screen_y_points = (0.5 - (f32(pixel_y) + 0.5) / f32(viewport_height)) *
        presentation_height_points;

    var ray_origin = grid_eye;
    var ray_direction = grid_forward;
    if (projection == 0u) {
        ray_direction =
            grid_forward +
            grid_right * (screen_x_points / perspective_focal_length_screen_points) +
            grid_up * (screen_y_points / perspective_focal_length_screen_points);
    } else {
        ray_origin =
            grid_eye +
            grid_right * (screen_x_points * orthographic_world_per_screen_point) +
            grid_up * (screen_y_points * orthographic_world_per_screen_point);
    }

    if (dot(ray_direction, ray_direction) <= EPSILON) {
        output_pixels[pixel_index] = empty_output();
        return;
    }

    var enter = -BIG_T;
    var exit = BIG_T;
    var slab = apply_slab(ray_origin.x, ray_direction.x, -0.5, f32(shape_x) - 0.5, enter, exit);
    if (!slab.hit) {
        output_pixels[pixel_index] = empty_output();
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    slab = apply_slab(ray_origin.y, ray_direction.y, -0.5, f32(shape_y) - 0.5, enter, exit);
    if (!slab.hit) {
        output_pixels[pixel_index] = empty_output();
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    slab = apply_slab(ray_origin.z, ray_direction.z, -0.5, f32(shape_z) - 0.5, enter, exit);
    if (!slab.hit) {
        output_pixels[pixel_index] = empty_output();
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    if (exit < enter || exit < 0.0) {
        output_pixels[pixel_index] = empty_output();
        return;
    }

    let hit_enter = max(enter, 0.0);
    if (sampling_policy == 1u) {
        let step_t = SMOOTH_RAY_STEP_VOXELS / length(ray_direction);
        let smooth_step_factor = step_t * dvr_ray_physical_length(ray_direction);
        var t = hit_enter;
        var max_value_smooth = 0.0;
        var has_value_smooth = false;
        var accumulated_rgba_smooth = vec4<f32>(0.0);
        var covered_smooth = false;
        var missing_samples_smooth = 0u;
        var previous_iso_valid = false;
        var previous_iso_t = 0.0;
        var previous_iso_source = 0.0;
        var previous_iso_display = 0.0;
        var smooth_steps = 0u;
        let max_smooth_steps = (shape_x + shape_y + shape_z) * 4u + 16u;
        loop {
            if (t > exit + EPSILON || smooth_steps >= max_smooth_steps) {
                break;
            }
            let point = ray_origin + ray_direction * t;
            let brick_skip_reason = brick_skip_reason_smooth(
                point,
                render_mode,
                max_value_smooth,
                has_value_smooth,
                iso_invert,
                iso_display_level,
                shape_x,
                shape_y,
                shape_z
            );
            if (brick_skip_reason != 0u) {
                let skip_t = smooth_brick_exit_t(
                    point,
                    ray_direction,
                    t,
                    shape_x,
                    shape_y,
                    shape_z
                );
                if (skip_t >= BIG_T * 0.5) {
                    break;
                }
                if (skip_t > t + EPSILON) {
                    record_brick_skip(brick_skip_reason);
                    if (skip_t > exit + EPSILON) {
                        break;
                    }
                    t = skip_t;
                    smooth_steps = smooth_steps + 1u;
                    continue;
                }
            }
            let sample = sample_linear(point, shape_x, shape_y, shape_z);
            missing_samples_smooth = missing_samples_smooth + sample.missing_samples;
            if (!sample.valid) {
                t = t + step_t;
                smooth_steps = smooth_steps + 1u;
                continue;
            }
            if (render_mode == 0u) {
                max_value_smooth = max(max_value_smooth, sample.value);
                has_value_smooth = true;
            } else if (render_mode == 1u) {
                let display_scalar = iso_display_value(sample.value, iso_invert);
                if (display_scalar >= iso_display_level) {
                    var hit_point = point;
                    var hit_source = sample.value;
                    var hit_display = display_scalar;
                    if (
                        previous_iso_valid &&
                        previous_iso_display < iso_display_level &&
                        abs(display_scalar - previous_iso_display) > EPSILON
                    ) {
                        let fraction = clamp(
                            (iso_display_level - previous_iso_display) /
                            (display_scalar - previous_iso_display),
                            0.0,
                            1.0
                        );
                        hit_point = ray_origin + ray_direction * lerp_f32(previous_iso_t, t, fraction);
                        hit_source = lerp_f32(previous_iso_source, sample.value, fraction);
                        hit_display = iso_display_level;
                    }
                    let shading = iso_surface_shading(
                        hit_point,
                        shape_x,
                        shape_y,
                        shape_z,
                        iso_shading
                    );
                    write_iso_output(
                        pixel_index,
                        hit_source,
                        hit_display,
                        display_scalar,
                        point_ray_t(ray_origin, ray_direction, hit_point),
                        missing_samples_smooth,
                        shading
                    );
                    return;
                }
                previous_iso_valid = true;
                previous_iso_t = t;
                previous_iso_source = sample.value;
                previous_iso_display = display_scalar;
            } else if (render_mode == 2u) {
                let color_intensity = iso_display_value(sample.value, iso_invert);
                let opacity_intensity = dvr_opacity_value(sample.value);
                let alpha = dvr_sample_alpha(opacity_intensity, smooth_step_factor);
                accumulated_rgba_smooth =
                    dvr_accumulate(accumulated_rgba_smooth, color_intensity, opacity_intensity, smooth_step_factor);
                covered_smooth = covered_smooth || alpha > EPSILON;
                if (accumulated_rgba_smooth.a >= 0.995) {
                    break;
                }
            }
            t = t + step_t;
            smooth_steps = smooth_steps + 1u;
        }

        var output_value_smooth = 0u;
        var output_covered_smooth = false;
        if (render_mode == 0u) {
            output_value_smooth = round_u16(max_value_smooth);
            output_covered_smooth = has_value_smooth;
        } else if (render_mode == 2u) {
            write_dvr_output(pixel_index, accumulated_rgba_smooth, missing_samples_smooth, covered_smooth);
            return;
        }
        write_output(pixel_index, output_value_smooth, missing_samples_smooth, output_covered_smooth);
        return;
    }

    let entry = ray_origin + ray_direction * hit_enter;

    var x_index = initial_voxel_index(entry.x, ray_direction.x, shape_x);
    var y_index = initial_voxel_index(entry.y, ray_direction.y, shape_y);
    var z_index = initial_voxel_index(entry.z, ray_direction.z, shape_z);
    let x_step = axis_step(ray_direction.x);
    let y_step = axis_step(ray_direction.y);
    let z_step = axis_step(ray_direction.z);
    var x_next_t = axis_next_t(entry.x, ray_direction.x, hit_enter, x_index);
    var y_next_t = axis_next_t(entry.y, ray_direction.y, hit_enter, y_index);
    var z_next_t = axis_next_t(entry.z, ray_direction.z, hit_enter, z_index);
    let x_delta_t = axis_delta_t(ray_direction.x);
    let y_delta_t = axis_delta_t(ray_direction.y);
    let z_delta_t = axis_delta_t(ray_direction.z);

    var max_value = 0u;
    var has_value = false;
    var accumulated_rgba = vec4<f32>(0.0);
    var covered = false;
    var missing_samples = 0u;
    var steps = 0u;
    var current_t = hit_enter;
    let ray_direction_physical_length = dvr_ray_physical_length(ray_direction);
    let max_steps = shape_x + shape_y + shape_z + 3u;
    loop {
        if (
            steps >= max_steps ||
            !axis_inside(x_index, shape_x) ||
            !axis_inside(y_index, shape_y) ||
            !axis_inside(z_index, shape_z)
        ) {
            break;
        }

        let current_brick_linear =
            brick_linear_index(u32(z_index), u32(y_index), u32(x_index));
        let brick_skip_reason = brick_skip_reason_exact(
            current_brick_linear,
            render_mode,
            max_value,
            has_value,
            iso_invert,
            iso_display_level
        );
        if (brick_skip_reason != 0u) {
            let skip_t = min(
                brick_axis_exit_t(x_index, x_step, x_next_t, x_delta_t, params_u32[8]),
                min(
                    brick_axis_exit_t(y_index, y_step, y_next_t, y_delta_t, params_u32[9]),
                    brick_axis_exit_t(z_index, z_step, z_next_t, z_delta_t, params_u32[10])
                )
            );
            if (skip_t >= BIG_T * 0.5) {
                break;
            }
            if (skip_t > current_t + EPSILON) {
                record_brick_skip(brick_skip_reason);
                if (skip_t > exit + EPSILON) {
                    break;
                }
                current_t = skip_t;
                let skipped_entry = ray_origin + ray_direction * current_t;
                x_index = initial_voxel_index(skipped_entry.x, ray_direction.x, shape_x);
                y_index = initial_voxel_index(skipped_entry.y, ray_direction.y, shape_y);
                z_index = initial_voxel_index(skipped_entry.z, ray_direction.z, shape_z);
                x_next_t = axis_next_t(skipped_entry.x, ray_direction.x, current_t, x_index);
                y_next_t = axis_next_t(skipped_entry.y, ray_direction.y, current_t, y_index);
                z_next_t = axis_next_t(skipped_entry.z, ray_direction.z, current_t, z_index);
                steps = steps + 1u;
                continue;
            }
        }

        let sample = sample_voxel(u32(z_index), u32(y_index), u32(x_index), shape_y, shape_x);
        missing_samples = missing_samples + sample_missing(sample);
        if (!sample_covered(sample)) {
            let next_t = min(x_next_t, min(y_next_t, z_next_t));
            if (next_t > exit + EPSILON || next_t >= BIG_T * 0.5) {
                break;
            }

            current_t = next_t;
            if (x_next_t <= next_t + EPSILON) {
                x_index = x_index + x_step;
                x_next_t = x_next_t + x_delta_t;
            }
            if (y_next_t <= next_t + EPSILON) {
                y_index = y_index + y_step;
                y_next_t = y_next_t + y_delta_t;
            }
            if (z_next_t <= next_t + EPSILON) {
                z_index = z_index + z_step;
                z_next_t = z_next_t + z_delta_t;
            }
            steps = steps + 1u;
            continue;
        }
        let value = sample_value(sample);
        if (render_mode == 0u) {
            max_value = max(max_value, value);
            has_value = true;
        } else if (render_mode == 1u) {
            let display_scalar = iso_display_value(f32(value), iso_invert);
            if (display_scalar >= iso_display_level) {
                let point = vec3<f32>(f32(x_index), f32(y_index), f32(z_index));
                let shading = iso_surface_shading(
                    point,
                    shape_x,
                    shape_y,
                    shape_z,
                    iso_shading
                );
                write_iso_output(
                    pixel_index,
                    f32(value),
                    display_scalar,
                    display_scalar,
                    point_ray_t(ray_origin, ray_direction, point),
                    missing_samples,
                    shading
                );
                return;
            }
        } else if (render_mode == 2u) {
            let next_t = min(x_next_t, min(y_next_t, z_next_t));
            let step_factor = max(min(next_t, exit) - current_t, 0.0) * ray_direction_physical_length;
            let color_intensity = iso_display_value(f32(value), iso_invert);
            let opacity_intensity = dvr_opacity_value(f32(value));
            let alpha = dvr_sample_alpha(opacity_intensity, step_factor);
            accumulated_rgba = dvr_accumulate(accumulated_rgba, color_intensity, opacity_intensity, step_factor);
            covered = covered || alpha > EPSILON;
            if (accumulated_rgba.a >= 0.995) {
                break;
            }
        }

        let next_t = min(x_next_t, min(y_next_t, z_next_t));
        if (next_t > exit + EPSILON || next_t >= BIG_T * 0.5) {
            break;
        }

        current_t = next_t;
        if (x_next_t <= next_t + EPSILON) {
            x_index = x_index + x_step;
            x_next_t = x_next_t + x_delta_t;
        }
        if (y_next_t <= next_t + EPSILON) {
            y_index = y_index + y_step;
            y_next_t = y_next_t + y_delta_t;
        }
        if (z_next_t <= next_t + EPSILON) {
            z_index = z_index + z_step;
            z_next_t = z_next_t + z_delta_t;
        }
        steps = steps + 1u;
    }

    var output_value = 0u;
    var output_covered = false;
    if (render_mode == 0u) {
        output_value = max_value;
        output_covered = has_value;
    } else if (render_mode == 2u) {
        write_dvr_output(pixel_index, accumulated_rgba, missing_samples, covered);
        return;
    }
    write_output(pixel_index, output_value, missing_samples, output_covered);
}
"#;
