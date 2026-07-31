// Shared 3D ray and resident-page segment mechanics.

const EPSILON: f32 = 1.0e-6;

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
        if abs(direction[axis]) <= 1.0e-7 {
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
    for (var axis = 0u; axis < 3u; axis += 1u) {
        if direction[axis] > EPSILON {
            let candidate = current_distance + (page.upper[axis] - point[axis]) / direction[axis];
            if candidate > current_distance + EPSILON {
                result = min(result, candidate);
            }
        } else if direction[axis] < -EPSILON {
            let candidate = current_distance + (page.lower[axis] - point[axis]) / direction[axis];
            if candidate > current_distance + EPSILON {
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
    let threshold = ceil((segment_exit - entry) / step - 0.5 - EPSILON);
    return min(max(u32(max(threshold, 0.0)), current_index + 1u), count);
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
    let direction = world_vector_to_grid(layer_index, world_direction);
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
