// One bounded asynchronous scientific pick over the same control/page/payload
// representation used by the product render pass. This file is compiled
// together with shader.wgsl; bindings 0--4 and all traversal helpers are
// therefore shared rather than reimplemented through a second residency path.

@group(0) @binding(5)
var<storage, read> pick_query: array<u32>;

@group(0) @binding(6)
var<storage, read_write> pick_output: array<u32>;

const PICK_OUTPUT_MAGIC: u32 = 0x4d34504bu;
const PICK_EMPTY: u32 = 0u;
const PICK_VOXEL: u32 = 1u;
const PICK_INTERPOLATED: u32 = 2u;
const PICK_EXACT: u32 = 0u;
const PICK_INCOMPLETE: u32 = 2u;
const PICK_FIRST_THRESHOLD: u32 = 0u;
const PICK_MIP_ARGMAX: u32 = 1u;
const PICK_DVR_MAX_CONTRIBUTION: u32 = 2u;

fn dvr_pick_can_terminate(
    policy: u32,
    has_hit: bool,
    transmittance: f32,
    best_score: f32,
) -> bool {
    // A future contribution is bounded by the remaining transmittance because
    // sample alpha cannot exceed one. Early exit is truthful only with exact
    // coverage; otherwise unvisited missing pages still affect completeness.
    return policy == PICK_DVR_MAX_CONTRIBUTION
        && control[28u] != 0u
        && has_hit
        && transmittance <= best_score;
}

fn pick_layer_index(layer_ordinal: u32) -> u32 {
    for (var layer_index = 0u; layer_index < control[2u]; layer_index += 1u) {
        if layer_word(layer_index, 0u) == layer_ordinal {
            return layer_index;
        }
    }
    return 0xffffffffu;
}

fn pick_completeness(incomplete: bool) -> u32 {
    return select(PICK_EXACT, PICK_INCOMPLETE, incomplete);
}

fn iso_pick_gradient_is_missing(layer_index: u32, point: vec3<f32>) -> bool {
    if layer_word(layer_index, 18u) != 2u || layer_word(layer_index, 57u) == 0u {
        return false;
    }
    return u32(iso_gradient(layer_index, point).w) == SAMPLE_MISSING;
}

fn write_empty_pick(incomplete: bool) {
    pick_output[0u] = PICK_EMPTY;
    pick_output[1u] = pick_completeness(incomplete);
    pick_output[2u] = 0u;
    pick_output[3u] = 0u;
    pick_output[4u] = 0u;
    pick_output[5u] = 0u;
    pick_output[6u] = 0u;
    pick_output[7u] = 0u;
    pick_output[8u] = PICK_OUTPUT_MAGIC;
}

fn grid_voxel_to_world(layer_index: u32, coordinate: vec3<u32>) -> vec3<f32> {
    let grid = vec3<f32>(coordinate);
    return vec3<f32>(
        dot(grid, vec3<f32>(layer_f32(layer_index, 44u), layer_f32(layer_index, 45u), layer_f32(layer_index, 46u))) + layer_f32(layer_index, 47u),
        dot(grid, vec3<f32>(layer_f32(layer_index, 48u), layer_f32(layer_index, 49u), layer_f32(layer_index, 50u))) + layer_f32(layer_index, 51u),
        dot(grid, vec3<f32>(layer_f32(layer_index, 52u), layer_f32(layer_index, 53u), layer_f32(layer_index, 54u))) + layer_f32(layer_index, 55u),
    );
}

fn grid_point_to_world(layer_index: u32, grid: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(grid, vec3<f32>(layer_f32(layer_index, 44u), layer_f32(layer_index, 45u), layer_f32(layer_index, 46u))) + layer_f32(layer_index, 47u),
        dot(grid, vec3<f32>(layer_f32(layer_index, 48u), layer_f32(layer_index, 49u), layer_f32(layer_index, 50u))) + layer_f32(layer_index, 51u),
        dot(grid, vec3<f32>(layer_f32(layer_index, 52u), layer_f32(layer_index, 53u), layer_f32(layer_index, 54u))) + layer_f32(layer_index, 55u),
    );
}

fn raw_pick_bits(dtype_bytes: u32, value: f32) -> u32 {
    if dtype_bytes == 1u || dtype_bytes == 2u {
        return u32(value);
    }
    return bitcast<u32>(value);
}

fn write_voxel_pick(
    layer_index: u32,
    resource_index: u32,
    coordinate: vec3<u32>,
    sample_point: vec3<f32>,
    value: f32,
    distance: f32,
    incomplete: bool,
) {
    let smooth_sampling = layer_word(layer_index, 56u) != 0u;
    let world = select(
        grid_voxel_to_world(layer_index, coordinate),
        grid_point_to_world(layer_index, sample_point),
        smooth_sampling,
    );
    // SmoothLinear is an interpolated scalar even for integer source data, so
    // encode it through the existing finite f32 value contract.
    let dtype_bytes = select(resource_word(resource_index, 9u), 4u, smooth_sampling);
    pick_output[0u] = select(PICK_VOXEL, PICK_INTERPOLATED, smooth_sampling);
    pick_output[1u] = pick_completeness(incomplete);
    pick_output[2u] = dtype_bytes;
    pick_output[3u] = select(
        raw_pick_bits(dtype_bytes, value),
        bitcast<u32>(value),
        smooth_sampling,
    );
    pick_output[4u] = bitcast<u32>(world.x);
    pick_output[5u] = bitcast<u32>(world.y);
    pick_output[6u] = bitcast<u32>(world.z);
    pick_output[7u] = bitcast<u32>(distance);
    pick_output[8u] = PICK_OUTPUT_MAGIC;
}

@compute @workgroup_size(1)
fn pick_main() {
    let layer_index = pick_layer_index(pick_query[2u]);
    if layer_index == 0xffffffffu || control[3u] != 0u {
        write_empty_pick(true);
        return;
    }

    let render_pixel = vec2<f32>(
        bitcast<f32>(pick_query[0u]),
        bitcast<f32>(pick_query[1u]),
    );
    let world_origin = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u))
        + vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u))
            * render_pixel.x
        + vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u))
            * render_pixel.y;
    let world_direction = normalize(
        vec3<f32>(control_f32(17u), control_f32(18u), control_f32(19u))
            + vec3<f32>(control_f32(20u), control_f32(21u), control_f32(22u)) * render_pixel.x
            + vec3<f32>(control_f32(23u), control_f32(24u), control_f32(25u)) * render_pixel.y,
    );
    let ray = volume_ray(layer_index, world_origin, world_direction);
    if !ray.intersects {
        write_empty_pick(false);
        return;
    }

    let origin = ray.origin;
    let direction = ray.direction;
    let step = 1.0 / ray.grid_speed;
    let count = max(u32(ceil((ray.exit - ray.entry) / step)), 1u);
    let policy = pick_query[3u];
    var incomplete = false;
    var has_hit = false;
    var best_value = 0.0;
    var best_score = 0.0;
    var best_coordinate = vec3<u32>(0u);
    var best_point = vec3<f32>(0.0);
    var best_distance = 0.0;
    var best_resource = 0u;
    var transmittance = 1.0;
    var index = 0u;

    loop {
        if index >= count
            || dvr_pick_can_terminate(policy, has_hit, transmittance, best_score) {
            break;
        }
        let distance = ray.entry + (f32(index) + 0.5) * step;
        let point = origin + direction * distance;
        if !grid_inside(layer_index, point) {
            index += 1u;
            continue;
        }
        let page = page_for_sample(layer_index, origin, direction, distance);
        let next = segment_end_index(
            ray.entry,
            step,
            index,
            page_exit_distance(page, point, direction, distance, ray.exit),
            count,
        );
        if page.kind != PAGE_RESIDENT {
            incomplete = true;
            index = next;
            continue;
        }
        if resource_word(page.resource_index, 14u) == 0u {
            index = next;
            continue;
        }
        if layer_word(layer_index, 56u) == 0u
            && policy == PICK_MIP_ARGMAX
            && has_hit
            && page_fully_covered(page)
            && resource_f32(page.resource_index, 13u) <= best_value {
            index = next;
            continue;
        }

        loop {
            if index >= next
                || dvr_pick_can_terminate(policy, has_hit, transmittance, best_score) {
                break;
            }
            let sample_distance = ray.entry + (f32(index) + 0.5) * step;
            let sample_point = origin + direction * sample_distance;
            let coordinate = grid_coordinate(
                layer_index,
                sample_point,
            );
            let sample = sample_in_page(layer_index, page, sample_point);
            if sample.kind == SAMPLE_MISSING {
                incomplete = true;
                index += 1u;
                continue;
            }
            if sample.kind != SAMPLE_VALID {
                index += 1u;
                continue;
            }

            if policy == PICK_FIRST_THRESHOLD {
                if transfer_value(layer_index, sample.value) >= layer_f32(layer_index, 19u) {
                    incomplete =
                        incomplete || iso_pick_gradient_is_missing(layer_index, sample_point);
                    write_voxel_pick(
                        layer_index,
                        page.resource_index,
                        coordinate,
                        sample_point,
                        sample.value,
                        sample_distance,
                        incomplete,
                    );
                    return;
                }
            } else if policy == PICK_MIP_ARGMAX {
                if !has_hit || sample.value > best_value {
                    has_hit = true;
                    best_value = sample.value;
                    best_coordinate = coordinate;
                    best_point = sample_point;
                    best_distance = sample_distance;
                    best_resource = page.resource_index;
                }
            } else {
                let opacity_display = curve_value(
                    sample.value,
                    layer_f32(layer_index, 20u),
                    layer_f32(layer_index, 21u),
                    layer_f32(layer_index, 22u),
                    0u,
                );
                let base_tau = opacity_display
                    * layer_f32(layer_index, 23u)
                    * step;
                let sample_alpha = dvr_effective_alpha(
                    base_tau,
                    layer_f32(layer_index, 15u),
                );
                let contribution = transmittance * sample_alpha;
                if !has_hit || contribution > best_score {
                    has_hit = true;
                    best_score = contribution;
                    best_value = sample.value;
                    best_coordinate = coordinate;
                    best_point = sample_point;
                    best_distance = sample_distance;
                    best_resource = page.resource_index;
                }
                transmittance *= 1.0 - sample_alpha;
            }
            index += 1u;
        }
    }

    if has_hit {
        write_voxel_pick(
            layer_index,
            best_resource,
            best_coordinate,
            best_point,
            best_value,
            best_distance,
            incomplete,
        );
    } else {
        write_empty_pick(incomplete);
    }
}
