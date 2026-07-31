// Plane-only target-to-coarser sampling. The fixed layer record points to one
// target-first catalog chain appended after the ordinary control records.
// Volume and pick compilation units do not include this module.

const PLANE_SCALE_WORDS: u32 = 19u;

fn plane_scale_word(layer_index: u32, scale_index: u32, field: u32) -> u32 {
    let offset = layer_word(layer_index, 30u);
    return control[offset + scale_index * PLANE_SCALE_WORDS + field];
}

fn plane_scale_f32(layer_index: u32, scale_index: u32, field: u32) -> f32 {
    return bitcast<f32>(plane_scale_word(layer_index, scale_index, field));
}

fn plane_scale_shape(layer_index: u32, scale_index: u32) -> vec3<u32> {
    return vec3<u32>(
        plane_scale_word(layer_index, scale_index, 1u),
        plane_scale_word(layer_index, scale_index, 2u),
        plane_scale_word(layer_index, scale_index, 3u),
    );
}

fn plane_scale_cell(layer_index: u32, scale_index: u32) -> vec3<u32> {
    return vec3<u32>(
        plane_scale_word(layer_index, scale_index, 4u),
        plane_scale_word(layer_index, scale_index, 5u),
        plane_scale_word(layer_index, scale_index, 6u),
    );
}

fn plane_world_to_grid(
    layer_index: u32,
    scale_index: u32,
    world: vec3<f32>,
) -> vec3<f32> {
    return vec3<f32>(
        dot(world, vec3<f32>(
            plane_scale_f32(layer_index, scale_index, 7u),
            plane_scale_f32(layer_index, scale_index, 8u),
            plane_scale_f32(layer_index, scale_index, 9u),
        )) + plane_scale_f32(layer_index, scale_index, 10u),
        dot(world, vec3<f32>(
            plane_scale_f32(layer_index, scale_index, 11u),
            plane_scale_f32(layer_index, scale_index, 12u),
            plane_scale_f32(layer_index, scale_index, 13u),
        )) + plane_scale_f32(layer_index, scale_index, 14u),
        dot(world, vec3<f32>(
            plane_scale_f32(layer_index, scale_index, 15u),
            plane_scale_f32(layer_index, scale_index, 16u),
            plane_scale_f32(layer_index, scale_index, 17u),
        )) + plane_scale_f32(layer_index, scale_index, 18u),
    );
}

fn plane_grid_inside(shape: vec3<u32>, grid: vec3<f32>) -> bool {
    return grid.x >= -0.5 && grid.y >= -0.5 && grid.z >= -0.5
        && grid.x < f32(shape.x) - 0.5
        && grid.y < f32(shape.y) - 0.5
        && grid.z < f32(shape.z) - 0.5;
}

fn plane_grid_coordinate(shape: vec3<u32>, grid: vec3<f32>) -> vec3<u32> {
    let rounded = floor(grid + vec3<f32>(0.5));
    return vec3<u32>(
        u32(clamp(rounded.x, 0.0, f32(shape.x - 1u))),
        u32(clamp(rounded.y, 0.0, f32(shape.y - 1u))),
        u32(clamp(rounded.z, 0.0, f32(shape.z - 1u))),
    );
}

fn sample_plane_coordinate(
    layer_index: u32,
    scale: u32,
    cell: vec3<u32>,
    coordinate: vec3<u32>,
) -> SampleResult {
    let page = lookup_page_at_scale(layer_index, coordinate, scale, cell);
    if page.kind != PAGE_RESIDENT {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    return sample_resource(page.resource_index, coordinate);
}

fn sample_plane_nearest(
    layer_index: u32,
    scale_index: u32,
    grid: vec3<f32>,
) -> SampleResult {
    let shape = plane_scale_shape(layer_index, scale_index);
    if !plane_grid_inside(shape, grid) {
        return SampleResult(SAMPLE_OUTSIDE, 0.0);
    }
    let coordinate = plane_grid_coordinate(shape, grid);
    return sample_plane_coordinate(
        layer_index,
        plane_scale_word(layer_index, scale_index, 0u),
        plane_scale_cell(layer_index, scale_index),
        coordinate,
    );
}

fn sample_plane_linear(
    layer_index: u32,
    scale_index: u32,
    grid: vec3<f32>,
) -> SampleResult {
    let shape = plane_scale_shape(layer_index, scale_index);
    if !plane_grid_inside(shape, grid) {
        return SampleResult(SAMPLE_OUTSIDE, 0.0);
    }
    let scale = plane_scale_word(layer_index, scale_index, 0u);
    let cell = plane_scale_cell(layer_index, scale_index);
    let clamped = clamp(grid, vec3<f32>(0.0), vec3<f32>(shape - vec3<u32>(1u)));
    let lower = vec3<u32>(floor(clamped));
    let upper = min(lower + vec3<u32>(1u), shape - vec3<u32>(1u));
    let fraction = clamped - vec3<f32>(lower);
    let page = lookup_page_at_scale(layer_index, lower, scale, cell);
    let footprint_in_page = page.kind == PAGE_RESIDENT
        && coordinate_in_resource(page.resource_index, lower)
        && coordinate_in_resource(page.resource_index, upper);
    var value = 0.0;
    for (var z = 0u; z < 2u; z += 1u) {
        let wz = select(1.0 - fraction.z, fraction.z, z != 0u);
        for (var y = 0u; y < 2u; y += 1u) {
            let wy = select(1.0 - fraction.y, fraction.y, y != 0u);
            for (var x = 0u; x < 2u; x += 1u) {
                let wx = select(1.0 - fraction.x, fraction.x, x != 0u);
                let weight = wx * wy * wz;
                if weight == 0.0 {
                    continue;
                }
                let coordinate = vec3<u32>(
                    select(lower.x, upper.x, x != 0u),
                    select(lower.y, upper.y, y != 0u),
                    select(lower.z, upper.z, z != 0u),
                );
                var sample = SampleResult(SAMPLE_MISSING, 0.0);
                if footprint_in_page {
                    sample = sample_resource(page.resource_index, coordinate);
                } else {
                    sample = sample_plane_coordinate(
                        layer_index,
                        scale,
                        cell,
                        coordinate,
                    );
                }
                // A missing tap retries the complete interpolation footprint
                // at the next coarser scale. Invalid data is scientific and
                // terminates fallback.
                if sample.kind == SAMPLE_MISSING {
                    return SampleResult(SAMPLE_MISSING, 0.0);
                }
                if sample.kind != SAMPLE_VALID {
                    return SampleResult(SAMPLE_INVALID, 0.0);
                }
                value += sample.value * weight;
            }
        }
    }
    return SampleResult(SAMPLE_VALID, value);
}

fn sample_plane_multiscale(layer_index: u32, world: vec3<f32>) -> SampleResult {
    let scale_count = layer_word(layer_index, 31u);
    var any_scale_inside = false;
    for (var scale_index = 0u; scale_index < scale_count; scale_index += 1u) {
        let grid = plane_world_to_grid(layer_index, scale_index, world);
        var sample = SampleResult(SAMPLE_MISSING, 0.0);
        if layer_word(layer_index, 56u) == 0u {
            sample = sample_plane_nearest(layer_index, scale_index, grid);
        } else {
            sample = sample_plane_linear(layer_index, scale_index, grid);
        }
        if sample.kind == SAMPLE_VALID || sample.kind == SAMPLE_INVALID {
            return sample;
        }
        if sample.kind != SAMPLE_OUTSIDE {
            any_scale_inside = true;
        }
    }
    if any_scale_inside {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    return SampleResult(SAMPLE_OUTSIDE, 0.0);
}
