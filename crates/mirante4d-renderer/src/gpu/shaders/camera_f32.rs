pub(crate) const BRICKED_CAMERA_F32_SHADER: &str = r#"
const EPSILON: f32 = 0.00001;
const BIG_T: f32 = 1.0e30;
const SMOOTH_RAY_STEP_VOXELS: f32 = 0.5;
const F32_PAGE_TABLE_WORDS: u32 = 7u;

struct CameraOutputF32 {
    value_bits: u32,
    marker_bits: u32,
    source_bits: u32,
    material_bits: u32,
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
var<storage, read> brick_values: array<f32>;

@group(0) @binding(1)
var<storage, read_write> output_pixels: array<CameraOutputF32>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> params_f32: array<f32>;

@group(0) @binding(4)
var<storage, read> page_table: array<u32>;

struct SampleF32 {
    missing: bool,
    value: f32,
};

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

struct LinearSampleF32 {
    value: f32,
    missing_samples: u32,
    valid: bool,
};

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

fn sample_voxel_f32(z: u32, y: u32, x: u32, shape_y: u32, shape_x: u32) -> SampleF32 {
    let brick_x_size = params_u32[8];
    let brick_y_size = params_u32[9];
    let brick_z_size = params_u32[10];
    let grid_x = params_u32[11];
    let grid_y = params_u32[12];

    let brick_x = x / brick_x_size;
    let brick_y = y / brick_y_size;
    let brick_z = z / brick_z_size;
    let brick_linear = (brick_z * grid_y + brick_y) * grid_x + brick_x;
    let page_table_base = brick_linear * F32_PAGE_TABLE_WORDS;
    let value_offset_plus_one = page_table[page_table_base];
    if (value_offset_plus_one == 0u) {
        return SampleF32(true, 0.0);
    }
    let brick_actual_x = page_table[page_table_base + 1u];
    let brick_actual_y = page_table[page_table_base + 2u];
    let brick_actual_z = page_table[page_table_base + 3u];
    let brick_start_x = page_table[page_table_base + 4u];
    let brick_start_y = page_table[page_table_base + 5u];
    let brick_start_z = page_table[page_table_base + 6u];

    if (x < brick_start_x || y < brick_start_y || z < brick_start_z) {
        return SampleF32(true, 0.0);
    }
    let local_x = x - brick_start_x;
    let local_y = y - brick_start_y;
    let local_z = z - brick_start_z;
    if (local_x >= brick_actual_x || local_y >= brick_actual_y || local_z >= brick_actual_z) {
        return SampleF32(true, 0.0);
    }
    let local_index = (local_z * brick_actual_y + local_y) * brick_actual_x + local_x;
    let atlas_index = (value_offset_plus_one - 1u) + local_index;
    return SampleF32(false, brick_values[atlas_index]);
}

fn sample_covered_f32(sample: SampleF32) -> bool {
    return !sample.missing && sample.value == sample.value;
}

fn normalized_f32_value(value: f32, low: f32, high: f32) -> f32 {
    let width = high - low;
    if (width <= 0.000001) {
        return 0.0;
    }
    return clamp((value - low) / width, 0.0, 1.0);
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

fn iso_display_value_f32(value: f32, iso_invert: u32) -> f32 {
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

fn sample_linear_f32(point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32) -> LinearSampleF32 {
    let x = interpolation_axis(point.x, shape_x);
    let y = interpolation_axis(point.y, shape_y);
    let z = interpolation_axis(point.z, shape_z);
    if (!x.valid || !y.valid || !z.valid) {
        return LinearSampleF32(0.0, 0u, false);
    }
    let c000 = sample_voxel_f32(z.lower, y.lower, x.lower, shape_y, shape_x);
    let c100 = sample_voxel_f32(z.lower, y.lower, x.upper, shape_y, shape_x);
    let c010 = sample_voxel_f32(z.lower, y.upper, x.lower, shape_y, shape_x);
    let c110 = sample_voxel_f32(z.lower, y.upper, x.upper, shape_y, shape_x);
    let c001 = sample_voxel_f32(z.upper, y.lower, x.lower, shape_y, shape_x);
    let c101 = sample_voxel_f32(z.upper, y.lower, x.upper, shape_y, shape_x);
    let c011 = sample_voxel_f32(z.upper, y.upper, x.lower, shape_y, shape_x);
    let c111 = sample_voxel_f32(z.upper, y.upper, x.upper, shape_y, shape_x);
    let covered =
        sample_covered_f32(c000) && sample_covered_f32(c100) &&
        sample_covered_f32(c010) && sample_covered_f32(c110) &&
        sample_covered_f32(c001) && sample_covered_f32(c101) &&
        sample_covered_f32(c011) && sample_covered_f32(c111);
    let c00 = lerp_f32(c000.value, c100.value, x.fraction);
    let c10 = lerp_f32(c010.value, c110.value, x.fraction);
    let c01 = lerp_f32(c001.value, c101.value, x.fraction);
    let c11 = lerp_f32(c011.value, c111.value, x.fraction);
    let c0 = lerp_f32(c00, c10, y.fraction);
    let c1 = lerp_f32(c01, c11, y.fraction);
    return LinearSampleF32(
        lerp_f32(c0, c1, z.fraction),
        select(0u, 1u, c000.missing) + select(0u, 1u, c100.missing) +
        select(0u, 1u, c010.missing) + select(0u, 1u, c110.missing) +
        select(0u, 1u, c001.missing) + select(0u, 1u, c101.missing) +
        select(0u, 1u, c011.missing) + select(0u, 1u, c111.missing),
        covered
    );
}

fn gradient_linear_f32(point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32) -> vec3<f32> {
    let iso_invert = params_u32[7];
    let center = sample_linear_f32(point, shape_x, shape_y, shape_z);
    if (!center.valid || center.missing_samples != 0u) {
        return vec3<f32>(0.0);
    }
    let x_minus = sample_linear_f32(point - vec3<f32>(1.0, 0.0, 0.0), shape_x, shape_y, shape_z);
    let x_plus = sample_linear_f32(point + vec3<f32>(1.0, 0.0, 0.0), shape_x, shape_y, shape_z);
    let y_minus = sample_linear_f32(point - vec3<f32>(0.0, 1.0, 0.0), shape_x, shape_y, shape_z);
    let y_plus = sample_linear_f32(point + vec3<f32>(0.0, 1.0, 0.0), shape_x, shape_y, shape_z);
    let z_minus = sample_linear_f32(point - vec3<f32>(0.0, 0.0, 1.0), shape_x, shape_y, shape_z);
    let z_plus = sample_linear_f32(point + vec3<f32>(0.0, 0.0, 1.0), shape_x, shape_y, shape_z);
    var dx = 0.0;
    var dy = 0.0;
    var dz = 0.0;
    let x_minus_ok = x_minus.valid && x_minus.missing_samples == 0u;
    let x_plus_ok = x_plus.valid && x_plus.missing_samples == 0u;
    if (x_minus_ok && x_plus_ok) {
        dx = (iso_display_value_f32(x_plus.value, iso_invert) - iso_display_value_f32(x_minus.value, iso_invert)) * 0.5;
    } else if (x_plus_ok) {
        dx = iso_display_value_f32(x_plus.value, iso_invert) - iso_display_value_f32(center.value, iso_invert);
    } else if (x_minus_ok) {
        dx = iso_display_value_f32(center.value, iso_invert) - iso_display_value_f32(x_minus.value, iso_invert);
    }
    let y_minus_ok = y_minus.valid && y_minus.missing_samples == 0u;
    let y_plus_ok = y_plus.valid && y_plus.missing_samples == 0u;
    if (y_minus_ok && y_plus_ok) {
        dy = (iso_display_value_f32(y_plus.value, iso_invert) - iso_display_value_f32(y_minus.value, iso_invert)) * 0.5;
    } else if (y_plus_ok) {
        dy = iso_display_value_f32(y_plus.value, iso_invert) - iso_display_value_f32(center.value, iso_invert);
    } else if (y_minus_ok) {
        dy = iso_display_value_f32(center.value, iso_invert) - iso_display_value_f32(y_minus.value, iso_invert);
    }
    let z_minus_ok = z_minus.valid && z_minus.missing_samples == 0u;
    let z_plus_ok = z_plus.valid && z_plus.missing_samples == 0u;
    if (z_minus_ok && z_plus_ok) {
        dz = (iso_display_value_f32(z_plus.value, iso_invert) - iso_display_value_f32(z_minus.value, iso_invert)) * 0.5;
    } else if (z_plus_ok) {
        dz = iso_display_value_f32(z_plus.value, iso_invert) - iso_display_value_f32(center.value, iso_invert);
    } else if (z_minus_ok) {
        dz = iso_display_value_f32(center.value, iso_invert) - iso_display_value_f32(z_minus.value, iso_invert);
    }
    return vec3<f32>(dx, dy, dz);
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

fn iso_surface_shading_f32(point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32, iso_shading: u32) -> IsoSurfaceShading {
    if (iso_shading == 0u) {
        return flat_iso_surface_shading();
    }
    let gradient = gradient_linear_f32(point, shape_x, shape_y, shape_z);
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

fn write_output(pixel_index: u32, value: f32, missing_samples: u32, covered: bool) {
    let marker = select(f32(missing_samples), -f32(missing_samples) - 1.0, covered);
    output_pixels[pixel_index] = CameraOutputF32(
        bitcast<u32>(value),
        bitcast<u32>(marker),
        0u,
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
    let marker = -f32(missing_samples) - 1.0;
    output_pixels[pixel_index] = CameraOutputF32(
        bitcast<u32>(display_scalar),
        bitcast<u32>(marker),
        bitcast<u32>(source_value),
        bitcast<u32>(material_scalar),
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
    let marker = select(f32(missing_samples), -f32(missing_samples) - 1.0, covered);
    output_pixels[pixel_index] = CameraOutputF32(
        bitcast<u32>(clamped.a),
        bitcast<u32>(marker),
        bitcast<u32>(clamped.r),
        bitcast<u32>(clamped.g),
        bitcast<u32>(clamped.b),
        bitcast<u32>(clamped.a),
        0u,
        0u
    );
}

@compute @workgroup_size(8, 8, 1)
fn camera_f32_main(@builtin(global_invocation_id) id: vec3<u32>) {
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

    let grid_eye = vector_param(0u);
    let grid_forward = vector_param(3u);
    let grid_right = vector_param(6u);
    let grid_up = vector_param(9u);
    let orthographic_world_per_screen_point = params_f32[12];
    let perspective_focal_length_screen_points = params_f32[13];
    let presentation_width_points = params_f32[14];
    let opacity_scale = params_f32[15];
    let presentation_height_points = params_f32[16];
    let iso_display_level = params_f32[22];
    let display_low = params_f32[23];
    let display_high = params_f32[24];
    let iso_invert = params_u32[7];
    let sampling_policy = u32(params_f32[25] + 0.5);
    let iso_shading = u32(params_f32[26] + 0.5);

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
        write_output(pixel_index, 0.0, 0u, false);
        return;
    }

    var enter = -BIG_T;
    var exit = BIG_T;
    var slab = apply_slab(ray_origin.x, ray_direction.x, -0.5, f32(shape_x) - 0.5, enter, exit);
    if (!slab.hit) {
        write_output(pixel_index, 0.0, 0u, false);
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    slab = apply_slab(ray_origin.y, ray_direction.y, -0.5, f32(shape_y) - 0.5, enter, exit);
    if (!slab.hit) {
        write_output(pixel_index, 0.0, 0u, false);
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    slab = apply_slab(ray_origin.z, ray_direction.z, -0.5, f32(shape_z) - 0.5, enter, exit);
    if (!slab.hit) {
        write_output(pixel_index, 0.0, 0u, false);
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    if (exit < enter || exit < 0.0) {
        write_output(pixel_index, 0.0, 0u, false);
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
            let sample = sample_linear_f32(point, shape_x, shape_y, shape_z);
            missing_samples_smooth = missing_samples_smooth + sample.missing_samples;
            if (!sample.valid) {
                t = t + step_t;
                smooth_steps = smooth_steps + 1u;
                continue;
            }
            if (render_mode == 0u) {
                if (!has_value_smooth || sample.value > max_value_smooth) {
                    max_value_smooth = sample.value;
                    has_value_smooth = true;
                }
            } else if (render_mode == 1u) {
                let display_scalar = iso_display_value_f32(sample.value, iso_invert);
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
                    let shading = iso_surface_shading_f32(
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
                let color_intensity = iso_display_value_f32(sample.value, iso_invert);
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

        var output_value_smooth = 0.0;
        var output_covered_smooth = false;
        if (render_mode == 0u && has_value_smooth) {
            output_value_smooth = max_value_smooth;
            output_covered_smooth = true;
        } else if (render_mode == 2u) {
            write_dvr_output(pixel_index, accumulated_rgba_smooth, missing_samples_smooth, covered_smooth);
            return;
        }
        write_output(
            pixel_index,
            output_value_smooth,
            missing_samples_smooth,
            output_covered_smooth
        );
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

    var max_value = 0.0;
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

        let sample = sample_voxel_f32(u32(z_index), u32(y_index), u32(x_index), shape_y, shape_x);
        if (sample.missing) {
            missing_samples = missing_samples + 1u;
        } else if (sample_covered_f32(sample)) {
            if (render_mode == 0u) {
                if (!has_value || sample.value > max_value) {
                    max_value = sample.value;
                    has_value = true;
                }
            } else if (render_mode == 1u) {
                let display_scalar = iso_display_value_f32(sample.value, iso_invert);
                if (display_scalar >= iso_display_level) {
                    let point = vec3<f32>(f32(x_index), f32(y_index), f32(z_index));
                    let shading = iso_surface_shading_f32(
                        point,
                        shape_x,
                        shape_y,
                        shape_z,
                        iso_shading
                    );
                    write_iso_output(
                        pixel_index,
                        sample.value,
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
                let color_intensity = iso_display_value_f32(sample.value, iso_invert);
                let opacity_intensity = dvr_opacity_value(sample.value);
                let alpha = dvr_sample_alpha(opacity_intensity, step_factor);
                accumulated_rgba = dvr_accumulate(accumulated_rgba, color_intensity, opacity_intensity, step_factor);
                covered = covered || alpha > EPSILON;
                if (accumulated_rgba.a >= 0.995) {
                    break;
                }
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

    var output_value = 0.0;
    var output_covered = false;
    if (render_mode == 0u && has_value) {
        output_value = max_value;
        output_covered = true;
    } else if (render_mode == 2u) {
        write_dvr_output(pixel_index, accumulated_rgba, missing_samples, covered);
        return;
    }
    write_output(pixel_index, output_value, missing_samples, output_covered);
}
"#;
