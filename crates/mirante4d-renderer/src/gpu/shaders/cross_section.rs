pub(crate) const CROSS_SECTION_CHUNK_DISPLAY_INTEGER_SHADER: &str = r#"
const OUTPUT_COVERED_FLAG: u32 = 0x80000000u;
const BRICK_METADATA_WORDS: u32 = 4u;
const BRICK_METADATA_HAS_VALID_FLAG: u32 = 0x2u;
const CHUNK_DRAW_WORDS: u32 = 8u;

struct CameraOutputU32 {
    packed_value: u32,
    source_material: u32,
    depth_bits: u32,
    normal_xy: u32,
    normal_z_diffuse: u32,
    specular: u32,
};

@group(0) @binding(0)
var<storage, read> packed_voxels: array<u32>;

@group(0) @binding(1)
var<storage, read_write> output_pixels: array<CameraOutputU32>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> params_f32: array<f32>;

@group(0) @binding(4)
var<storage, read> page_table: array<u32>;

@group(0) @binding(5)
var<storage, read> validity_bits: array<u32>;

@group(0) @binding(6)
var<storage, read> brick_metadata: array<u32>;

@group(0) @binding(7)
var<storage, read> chunk_draws: array<u32>;

fn empty_record() -> CameraOutputU32 {
    return CameraOutputU32(0u, 0u, 0u, 0u, 0u, 0u);
}

fn intensity_record(packed_value: u32) -> CameraOutputU32 {
    return CameraOutputU32(packed_value, 0u, 0u, 0u, 0u, 0u);
}

fn pack_covered(value: u32) -> u32 {
    return (value & 0xffffu) | OUTPUT_COVERED_FLAG;
}

fn grid_point_for_pixel(pixel_x: u32, pixel_y: u32) -> vec3<f32> {
    let render_width = max(f32(params_u32[0]), 1.0);
    let render_height = max(f32(params_u32[1]), 1.0);
    let panel_width_points = params_f32[9];
    let panel_height_points = params_f32[10];
    let x_points = ((f32(pixel_x) + 0.5) / render_width) * panel_width_points;
    let y_points = ((f32(pixel_y) + 0.5) / render_height) * panel_height_points;
    let dx_points = x_points - panel_width_points * 0.5;
    let dy_points = y_points - panel_height_points * 0.5;
    let center_grid = vec3<f32>(params_f32[0], params_f32[1], params_f32[2]);
    let right_grid_per_point = vec3<f32>(params_f32[3], params_f32[4], params_f32[5]);
    let down_grid_per_point = vec3<f32>(params_f32[6], params_f32[7], params_f32[8]);
    return center_grid + right_grid_per_point * dx_points + down_grid_per_point * dy_points;
}

fn voxel_index(coordinate: f32) -> i32 {
    return i32(floor(coordinate + 0.5));
}

fn inside(index: i32, limit: u32) -> bool {
    return index >= 0 && index < i32(limit);
}

fn brick_has_valid_samples(brick_linear: u32) -> bool {
    return (brick_metadata[brick_linear * BRICK_METADATA_WORDS] & BRICK_METADATA_HAS_VALID_FLAG) != 0u;
}

fn sample_chunk_voxel(z: u32, y: u32, x: u32, brick_linear: u32) -> CameraOutputU32 {
    let slot_plus_one = page_table[brick_linear];
    if (slot_plus_one == 0u || !brick_has_valid_samples(brick_linear)) {
        return empty_record();
    }

    let brick_x_size = params_u32[5];
    let brick_y_size = params_u32[6];
    let brick_z_size = params_u32[7];
    let local_x = x - (x / brick_x_size) * brick_x_size;
    let local_y = y - (y / brick_y_size) * brick_y_size;
    let local_z = z - (z / brick_z_size) * brick_z_size;
    let local_index = (local_z * brick_y_size + local_y) * brick_x_size + local_x;
    let valid_u32_per_brick = params_u32[16];
    let slot = slot_plus_one - 1u;

    let valid_word_index = slot * valid_u32_per_brick + local_index / 32u;
    let valid_bit = local_index % 32u;
    if ((validity_bits[valid_word_index] & (1u << valid_bit)) == 0u) {
        return empty_record();
    }

    let packed_u32_per_brick = params_u32[12];
    let values_per_word = params_u32[13];
    let bits_per_value = params_u32[14];
    let value_mask = params_u32[15];
    let value_word_index = local_index / values_per_word;
    let value_offset = local_index % values_per_word;
    let value_shift = value_offset * bits_per_value;
    let atlas_index = slot * packed_u32_per_brick + value_word_index;
    let value = (packed_voxels[atlas_index] >> value_shift) & value_mask;
    return intensity_record(pack_covered(value));
}

@compute @workgroup_size(8, 8, 1)
fn cross_section_chunk_display_integer_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let base = id.z * CHUNK_DRAW_WORDS;
    let chunk_z = chunk_draws[base];
    let chunk_y = chunk_draws[base + 1u];
    let chunk_x = chunk_draws[base + 2u];
    let min_x = chunk_draws[base + 3u];
    let min_y = chunk_draws[base + 4u];
    let max_x = chunk_draws[base + 5u];
    let max_y = chunk_draws[base + 6u];
    let pixel_x = min_x + id.x;
    let pixel_y = min_y + id.y;
    if (pixel_x >= max_x || pixel_y >= max_y || pixel_x >= params_u32[0] || pixel_y >= params_u32[1]) {
        return;
    }

    let shape_x = params_u32[2];
    let shape_y = params_u32[3];
    let shape_z = params_u32[4];
    let point = grid_point_for_pixel(pixel_x, pixel_y);
    let x = voxel_index(point.x);
    let y = voxel_index(point.y);
    let z = voxel_index(point.z);
    if (!inside(x, shape_x) || !inside(y, shape_y) || !inside(z, shape_z)) {
        return;
    }

    let ux = u32(x);
    let uy = u32(y);
    let uz = u32(z);
    let brick_x_size = params_u32[5];
    let brick_y_size = params_u32[6];
    let brick_z_size = params_u32[7];
    if (ux / brick_x_size != chunk_x || uy / brick_y_size != chunk_y || uz / brick_z_size != chunk_z) {
        return;
    }

    let grid_x = params_u32[8];
    let grid_y = params_u32[9];
    let brick_linear = (chunk_z * grid_y + chunk_y) * grid_x + chunk_x;
    output_pixels[pixel_y * params_u32[0] + pixel_x] =
        sample_chunk_voxel(uz, uy, ux, brick_linear);
}
"#;

pub(crate) const CROSS_SECTION_CHUNK_DISPLAY_F32_SHADER: &str = r#"
const F32_PAGE_TABLE_WORDS: u32 = 7u;
const CHUNK_DRAW_WORDS: u32 = 8u;

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

@group(0) @binding(5)
var<storage, read> chunk_draws: array<u32>;

struct SampleF32 {
    missing: bool,
    value: f32,
};

fn grid_point_for_pixel(pixel_x: u32, pixel_y: u32) -> vec3<f32> {
    let render_width = max(f32(params_u32[0]), 1.0);
    let render_height = max(f32(params_u32[1]), 1.0);
    let panel_width_points = params_f32[9];
    let panel_height_points = params_f32[10];
    let x_points = ((f32(pixel_x) + 0.5) / render_width) * panel_width_points;
    let y_points = ((f32(pixel_y) + 0.5) / render_height) * panel_height_points;
    let dx_points = x_points - panel_width_points * 0.5;
    let dy_points = y_points - panel_height_points * 0.5;
    let center_grid = vec3<f32>(params_f32[0], params_f32[1], params_f32[2]);
    let right_grid_per_point = vec3<f32>(params_f32[3], params_f32[4], params_f32[5]);
    let down_grid_per_point = vec3<f32>(params_f32[6], params_f32[7], params_f32[8]);
    return center_grid + right_grid_per_point * dx_points + down_grid_per_point * dy_points;
}

fn voxel_index(coordinate: f32) -> i32 {
    return i32(floor(coordinate + 0.5));
}

fn inside(index: i32, limit: u32) -> bool {
    return index >= 0 && index < i32(limit);
}

fn empty_record() -> CameraOutputF32 {
    return CameraOutputF32(0u, bitcast<u32>(0.0), 0u, 0u, 0u, 0u, 0u, 0u);
}

fn value_record(value: f32) -> CameraOutputF32 {
    return CameraOutputF32(bitcast<u32>(value), bitcast<u32>(-1.0), 0u, 0u, 0u, 0u, 0u, 0u);
}

fn sample_chunk_voxel_f32(z: u32, y: u32, x: u32, brick_linear: u32) -> SampleF32 {
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

@compute @workgroup_size(8, 8, 1)
fn cross_section_chunk_display_f32_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let base = id.z * CHUNK_DRAW_WORDS;
    let chunk_z = chunk_draws[base];
    let chunk_y = chunk_draws[base + 1u];
    let chunk_x = chunk_draws[base + 2u];
    let min_x = chunk_draws[base + 3u];
    let min_y = chunk_draws[base + 4u];
    let max_x = chunk_draws[base + 5u];
    let max_y = chunk_draws[base + 6u];
    let pixel_x = min_x + id.x;
    let pixel_y = min_y + id.y;
    if (pixel_x >= max_x || pixel_y >= max_y || pixel_x >= params_u32[0] || pixel_y >= params_u32[1]) {
        return;
    }

    let shape_x = params_u32[2];
    let shape_y = params_u32[3];
    let shape_z = params_u32[4];
    let point = grid_point_for_pixel(pixel_x, pixel_y);
    let x = voxel_index(point.x);
    let y = voxel_index(point.y);
    let z = voxel_index(point.z);
    if (!inside(x, shape_x) || !inside(y, shape_y) || !inside(z, shape_z)) {
        return;
    }

    let ux = u32(x);
    let uy = u32(y);
    let uz = u32(z);
    let brick_x_size = params_u32[5];
    let brick_y_size = params_u32[6];
    let brick_z_size = params_u32[7];
    if (ux / brick_x_size != chunk_x || uy / brick_y_size != chunk_y || uz / brick_z_size != chunk_z) {
        return;
    }

    let grid_x = params_u32[8];
    let grid_y = params_u32[9];
    let brick_linear = (chunk_z * grid_y + chunk_y) * grid_x + chunk_x;
    let sample = sample_chunk_voxel_f32(uz, uy, ux, brick_linear);
    if (sample.missing || sample.value != sample.value) {
        return;
    }
    output_pixels[pixel_y * params_u32[0] + pixel_x] = value_record(sample.value);
}
"#;
