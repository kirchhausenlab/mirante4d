pub(crate) const INTENSITY_SUMMARY_SHADER: &str = r#"
@group(0) @binding(0)
var<storage, read> packed_voxels: array<u32>;

@group(0) @binding(1)
var<storage, read_write> partials: array<u32>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

fn sample_voxel(linear_index: u32) -> u32 {
    return packed_voxels[linear_index] & 0xffffu;
}

fn sample_index_to_volume_linear_index(sample_index: u32) -> u32 {
    let mode = params_u32[3];
    if (mode == 0u) {
        return sample_index;
    }

    let shape_y = params_u32[5];
    let shape_x = params_u32[6];
    let z_start = params_u32[7];
    let y_start = params_u32[8];
    let x_start = params_u32[9];
    let z_size = params_u32[10];
    let y_size = params_u32[11];
    let x_size = params_u32[12];
    let samples_per_z = y_size * x_size;
    let local_z = sample_index / samples_per_z;
    let rem = sample_index - local_z * samples_per_z;
    let local_y = rem / x_size;
    let local_x = rem - local_y * x_size;
    let z = min(local_z, z_size - 1u) + z_start;
    let y = local_y + y_start;
    let x = local_x + x_start;
    return (z * shape_y + y) * shape_x + x;
}

@compute @workgroup_size(64, 1, 1)
fn intensity_summary_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let partial_index = id.x;
    let voxel_count = params_u32[0];
    let chunk_voxels = params_u32[1];
    let partial_fields = params_u32[2];
    let start = partial_index * chunk_voxels;
    if (start >= voxel_count) {
        return;
    }
    let end = min(start + chunk_voxels, voxel_count);

    var min_value = 65535u;
    var max_value = 0u;
    var nonzero_count = 0u;
    var count = 0u;
    var sum = 0u;
    var index = start;
    loop {
        if (index >= end) {
            break;
        }
        let value = sample_voxel(sample_index_to_volume_linear_index(index));
        min_value = min(min_value, value);
        max_value = max(max_value, value);
        if (value != 0u) {
            nonzero_count = nonzero_count + 1u;
        }
        sum = sum + value;
        count = count + 1u;
        index = index + 1u;
    }

    if (count == 0u) {
        min_value = 0u;
    }
    let base = partial_index * partial_fields;
    partials[base + 0u] = min_value;
    partials[base + 1u] = max_value;
    partials[base + 2u] = nonzero_count;
    partials[base + 3u] = count;
    partials[base + 4u] = sum;
    partials[base + 5u] = 0u;
    partials[base + 6u] = 0u;
    partials[base + 7u] = 0u;
}
"#;

pub(crate) const INTENSITY_SUMMARY_F32_SHADER: &str = r#"
@group(0) @binding(0)
var<storage, read> voxels: array<f32>;

@group(0) @binding(1)
var<storage, read_write> partials: array<f32>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

fn sample_index_to_volume_linear_index(sample_index: u32) -> u32 {
    let mode = params_u32[3];
    if (mode == 0u) {
        return sample_index;
    }

    let shape_y = params_u32[5];
    let shape_x = params_u32[6];
    let z_start = params_u32[7];
    let y_start = params_u32[8];
    let x_start = params_u32[9];
    let z_size = params_u32[10];
    let y_size = params_u32[11];
    let x_size = params_u32[12];
    let samples_per_z = y_size * x_size;
    let local_z = sample_index / samples_per_z;
    let rem = sample_index - local_z * samples_per_z;
    let local_y = rem / x_size;
    let local_x = rem - local_y * x_size;
    let z = min(local_z, z_size - 1u) + z_start;
    let y = local_y + y_start;
    let x = local_x + x_start;
    return (z * shape_y + y) * shape_x + x;
}

@compute @workgroup_size(64, 1, 1)
fn intensity_summary_f32_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let partial_index = id.x;
    let voxel_count = params_u32[0];
    let chunk_voxels = params_u32[1];
    let partial_fields = params_u32[2];
    let start = partial_index * chunk_voxels;
    if (start >= voxel_count) {
        return;
    }
    let end = min(start + chunk_voxels, voxel_count);

    var min_value = 3.4028234663852886e38f;
    var max_value = -3.4028234663852886e38f;
    var nonzero_count = 0u;
    var count = 0u;
    var sum = 0.0f;
    var index = start;
    loop {
        if (index >= end) {
            break;
        }
        let value = voxels[sample_index_to_volume_linear_index(index)];
        min_value = min(min_value, value);
        max_value = max(max_value, value);
        if (value != 0.0f) {
            nonzero_count = nonzero_count + 1u;
        }
        sum = sum + value;
        count = count + 1u;
        index = index + 1u;
    }

    if (count == 0u) {
        min_value = 0.0f;
        max_value = 0.0f;
    }
    let base = partial_index * partial_fields;
    partials[base + 0u] = min_value;
    partials[base + 1u] = max_value;
    partials[base + 2u] = f32(nonzero_count);
    partials[base + 3u] = f32(count);
    partials[base + 4u] = sum;
    partials[base + 5u] = 0.0f;
    partials[base + 6u] = 0.0f;
    partials[base + 7u] = 0.0f;
}
"#;
