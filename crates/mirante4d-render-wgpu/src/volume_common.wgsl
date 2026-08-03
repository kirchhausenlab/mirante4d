// Shared 3D ray and resident-page segment mechanics.

const EPSILON: f32 = 1.0e-6;
const F32_MAGNITUDE_MASK: u32 = 0x7fffffffu;
const F32_MIN_NORMAL_BITS: u32 = 0x00800000u;

fn portable_direction_component(value: f32) -> f32 {
    let magnitude_bits = bitcast<u32>(value) & F32_MAGNITUDE_MASK;
    return select(0.0, value, magnitude_bits >= F32_MIN_NORMAL_BITS);
}

fn portable_volume_direction(direction: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        portable_direction_component(direction.x),
        portable_direction_component(direction.y),
        portable_direction_component(direction.z),
    );
}

struct VolumeRay {
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    exit: f32,
    grid_speed: f32,
    intersects: bool,
};

fn resource_f32(resource_index: u32, field: u32) -> f32 {
    return bitcast<f32>(resource_word(resource_index, field));
}

fn page_fully_covered(page: PageResult) -> bool {
    if page.kind != PAGE_RESIDENT {
        return false;
    }
    let origin = vec3<f32>(
        f32(resource_word(page.resource_index, 1u)),
        f32(resource_word(page.resource_index, 2u)),
        f32(resource_word(page.resource_index, 3u)),
    );
    let shape = vec3<f32>(
        f32(resource_word(page.resource_index, 4u)),
        f32(resource_word(page.resource_index, 5u)),
        f32(resource_word(page.resource_index, 6u)),
    );
    let page_origin = page.lower + vec3<f32>(0.5);
    let page_end = page.upper + vec3<f32>(0.5);
    return all(page_origin >= origin) && all(page_end <= origin + shape);
}

fn sample_in_page(
    layer_index: u32,
    page: PageResult,
    grid: vec3<f32>,
) -> SampleResult {
    if layer_word(layer_index, 56u) == 0u {
        return sample_resource(page.resource_index, grid_coordinate(layer_index, grid));
    }
    return sample_grid_linear_in_page(layer_index, page, grid);
}

fn sample_in_resolved_page(
    layer_index: u32,
    page: PageResult,
    address: ResourceAddress,
    grid: vec3<f32>,
) -> SampleResult {
    if layer_word(layer_index, 56u) == 0u {
        return sample_resource_at(address, grid_coordinate(layer_index, grid));
    }
    return sample_grid_linear_in_resolved_page(layer_index, page, address, grid);
}

fn world_vector_to_grid(layer_index: u32, world: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(world, vec3<f32>(layer_f32(layer_index, 32u), layer_f32(layer_index, 33u), layer_f32(layer_index, 34u))),
        dot(world, vec3<f32>(layer_f32(layer_index, 36u), layer_f32(layer_index, 37u), layer_f32(layer_index, 38u))),
        dot(world, vec3<f32>(layer_f32(layer_index, 40u), layer_f32(layer_index, 41u), layer_f32(layer_index, 42u))),
    );
}

fn intersect_grid(origin: vec3<f32>, direction: vec3<f32>, shape: vec3<u32>) -> vec2<f32> {
    var entry = -LARGE_DISTANCE;
    var exit = LARGE_DISTANCE;
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let lower = -0.5;
        let upper = f32(shape[axis]) - 0.5;
        if direction[axis] == 0.0 {
            if origin[axis] < lower || origin[axis] >= upper {
                return vec2<f32>(1.0, 0.0);
            }
        } else {
            let first = (lower - origin[axis]) / direction[axis];
            let second = (upper - origin[axis]) / direction[axis];
            entry = max(entry, min(first, second));
            exit = min(exit, max(first, second));
            if exit <= entry {
                return vec2<f32>(1.0, 0.0);
            }
        }
    }
    return vec2<f32>(entry, exit);
}

fn page_exit_distance(
    page: PageResult,
    point: vec3<f32>,
    direction: vec3<f32>,
    current_distance: f32,
    ray_exit: f32,
) -> f32 {
    var result = ray_exit;
    let remaining = ray_exit - current_distance;
    if remaining <= 0.0 {
        return ray_exit;
    }
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let component = portable_direction_component(direction[axis]);
        let speed = abs(component);
        if speed == 0.0 {
            continue;
        }
        let boundary_distance = select(
            point[axis] - page.lower[axis],
            page.upper[axis] - point[axis],
            component > 0.0,
        );
        // Avoid an irrelevant far-boundary division. Only a positive finite
        // boundary proven strictly nearer than the finite ray exit is divided
        // by its already-normal direction component.
        if boundary_distance > 0.0 && boundary_distance < remaining * speed {
            let candidate = current_distance + boundary_distance / speed;
            if candidate > current_distance {
                result = min(result, candidate);
            }
        }
    }
    return result;
}

fn segment_end_index(
    entry: f32,
    step: f32,
    current_index: u32,
    segment_exit: f32,
    count: u32,
) -> u32 {
    let threshold = ceil((segment_exit - entry) / step - 0.5);
    return min(max(u32(max(threshold, 0.0)), current_index + 1u), count);
}

fn same_page(first: PageResult, second: PageResult) -> bool {
    return first.kind == second.kind
        && all(first.lower == second.lower)
        && all(first.upper == second.upper)
        && (first.kind != PAGE_RESIDENT || first.resource_index == second.resource_index);
}

// Continuous boundary division can place a segment end just after the first
// binary32 sample position that rounds into the next page. Validate the
// predicted exclusive end against the exact lookup representation and, only
// when needed, find the first different page with a bounded binary search.
// This preserves page batching while preventing a resolved resource address
// from owning even one sample that quantizes into its successor page.
fn page_segment_end_index(
    layer_index: u32,
    page: PageResult,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    step: f32,
    current_index: u32,
    predicted_end: u32,
) -> u32 {
    if predicted_end <= current_index + 1u {
        return predicted_end;
    }
    let last_index = predicted_end - 1u;
    let last_distance = entry + (f32(last_index) + 0.5) * step;
    if same_page(page, page_for_sample(layer_index, origin, direction, last_distance)) {
        return predicted_end;
    }

    var low = current_index + 1u;
    var high = last_index;
    loop {
        if low >= high {
            break;
        }
        let middle = low + (high - low) / 2u;
        let distance = entry + (f32(middle) + 0.5) * step;
        if same_page(page, page_for_sample(layer_index, origin, direction, distance)) {
            low = middle + 1u;
        } else {
            high = middle;
        }
    }
    return low;
}

// Clamp a continuously predicted segment at the first binary32 sample whose
// represented distance reaches `boundary`. General-affine DVR uses this for
// layers that enter the shared world interval after another layer. Like the
// page correction above, this only shortens `segment_end_index`; it never
// owns monotone progress independently.
fn distance_boundary_end_index(
    entry: f32,
    step: f32,
    current_index: u32,
    predicted_end: u32,
    boundary: f32,
) -> u32 {
    if predicted_end <= current_index + 1u {
        return predicted_end;
    }
    let last_index = predicted_end - 1u;
    let last_distance = entry + (f32(last_index) + 0.5) * step;
    if last_distance < boundary {
        return predicted_end;
    }

    var low = current_index + 1u;
    var high = last_index;
    loop {
        if low >= high {
            break;
        }
        let middle = low + (high - low) / 2u;
        let distance = entry + (f32(middle) + 0.5) * step;
        if distance < boundary {
            low = middle + 1u;
        } else {
            high = middle;
        }
    }
    return low;
}

fn page_for_sample(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    distance: f32,
) -> PageResult {
    let point = origin + direction * distance;
    return lookup_page(layer_index, grid_coordinate(layer_index, point));
}

fn volume_ray(layer_index: u32, world_origin: vec3<f32>, world_direction: vec3<f32>) -> VolumeRay {
    let origin = world_to_grid(layer_index, world_origin);
    let direction = portable_volume_direction(
        world_vector_to_grid(layer_index, world_direction),
    );
    let interval = intersect_grid(origin, direction, layer_shape(layer_index));
    let entry = max(interval.x, 0.0);
    let grid_speed = max(abs(direction.x), max(abs(direction.y), abs(direction.z)));
    return VolumeRay(
        origin,
        direction,
        entry,
        interval.y,
        grid_speed,
        interval.y > entry && grid_speed > 0.0,
    );
}
