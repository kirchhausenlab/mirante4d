const LAYER_WORDS: u32 = 64u;
const RESOURCE_WORDS: u32 = 16u;
const MAX_RENDER_LAYERS: u32 = 64u;
const LARGE_DISTANCE: f32 = 3.402823e38;
const EPSILON: f32 = 1.0e-6;
// Residual radiance below this threshold is < 0.5 RGBA8 level even for a
// saturated tail. Progressive frames may only terminate when the host proves
// the complete requirement body resident; otherwise the tail is still walked
// so missing coverage cannot be hidden behind an opaque prefix.
const ALPHA_TERMINATION: f32 = 0.999;

const SAMPLE_OUTSIDE: u32 = 0u;
const SAMPLE_MISSING: u32 = 1u;
const SAMPLE_INVALID: u32 = 2u;
const SAMPLE_VALID: u32 = 3u;

const PAGE_MISSING: u32 = 0u;
const PAGE_RESIDENT: u32 = 1u;
const PAGE_TOMBSTONE: u32 = 0xffffffffu;

struct FragmentOutput {
    @location(0) rgba: vec4<f32>,
    @location(1) facts: vec2<u32>,
};

struct SampleResult {
    kind: u32,
    value: f32,
};

struct PixelResult {
    premultiplied_rgb: vec3<f32>,
    alpha: f32,
    covered: u32,
    valid: u32,
    depth: f32,
};

struct PageResult {
    kind: u32,
    resource_index: u32,
    lower: vec3<f32>,
    upper: vec3<f32>,
};

struct VolumeRay {
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    exit: f32,
    grid_speed: f32,
    intersects: bool,
};

@group(0) @binding(0)
var<storage, read> control: array<u32>;

@group(0) @binding(1)
var<storage, read> payload_0: array<u32>;

@group(0) @binding(2)
var<storage, read> payload_1: array<u32>;

@group(0) @binding(3)
var<storage, read> payload_2: array<u32>;

@group(0) @binding(4)
var<storage, read> payload_3: array<u32>;

fn control_f32(index: u32) -> f32 {
    return bitcast<f32>(control[index]);
}

fn layer_word(layer_index: u32, field: u32) -> u32 {
    return control[control[6u] + layer_index * LAYER_WORDS + field];
}

fn layer_f32(layer_index: u32, field: u32) -> f32 {
    return bitcast<f32>(layer_word(layer_index, field));
}

fn resource_word(resource_index: u32, field: u32) -> u32 {
    return control[control[7u] + resource_index * RESOURCE_WORDS + field];
}

fn resource_f32(resource_index: u32, field: u32) -> f32 {
    return bitcast<f32>(resource_word(resource_index, field));
}

fn payload_word(segment: u32, word_offset: u32) -> u32 {
    switch segment {
        case 0u: { return payload_0[word_offset]; }
        case 1u: { return payload_1[word_offset]; }
        case 2u: { return payload_2[word_offset]; }
        default: { return payload_3[word_offset]; }
    }
}

fn read_byte(segment: u32, byte_offset: u32) -> u32 {
    let word = payload_word(segment, byte_offset >> 2u);
    let shift = (byte_offset & 3u) * 8u;
    return (word >> shift) & 255u;
}

fn sample_value(resource_index: u32, sample_index: u32) -> f32 {
    let segment = resource_word(resource_index, 10u);
    let base = resource_word(resource_index, 7u);
    let dtype_bytes = resource_word(resource_index, 9u);
    let offset = base + sample_index * dtype_bytes;
    if dtype_bytes == 1u {
        return f32(read_byte(segment, offset));
    }
    if dtype_bytes == 2u {
        let word = payload_word(segment, offset >> 2u);
        return f32((word >> ((offset & 2u) * 8u)) & 0xffffu);
    }
    return bitcast<f32>(payload_word(segment, offset >> 2u));
}

fn sample_is_valid(resource_index: u32, sample_index: u32) -> bool {
    if resource_word(resource_index, 15u) != 0u {
        return true;
    }
    let validity_offset = resource_word(resource_index, 8u);
    if validity_offset == 0xffffffffu {
        return true;
    }
    let segment = resource_word(resource_index, 10u);
    let validity_byte = read_byte(segment, validity_offset + sample_index / 8u);
    return (validity_byte & (1u << (sample_index & 7u))) != 0u;
}

fn layer_shape(layer_index: u32) -> vec3<u32> {
    return vec3<u32>(
        layer_word(layer_index, 1u),
        layer_word(layer_index, 2u),
        layer_word(layer_index, 3u),
    );
}

fn grid_inside(layer_index: u32, grid: vec3<f32>) -> bool {
    let shape = layer_shape(layer_index);
    return grid.x >= -0.5 && grid.y >= -0.5 && grid.z >= -0.5
        && grid.x < f32(shape.x) - 0.5
        && grid.y < f32(shape.y) - 0.5
        && grid.z < f32(shape.z) - 0.5;
}

fn grid_coordinate(layer_index: u32, grid: vec3<f32>) -> vec3<u32> {
    let shape = layer_shape(layer_index);
    let rounded = floor(grid + vec3<f32>(0.5));
    return vec3<u32>(
        u32(clamp(rounded.x, 0.0, f32(shape.x - 1u))),
        u32(clamp(rounded.y, 0.0, f32(shape.y - 1u))),
        u32(clamp(rounded.z, 0.0, f32(shape.z - 1u))),
    );
}

fn page_hash(coordinate: vec3<u32>, seed: u32) -> u32 {
    var hash = coordinate.x * 0x9e3779b1u
        + coordinate.y * 0x85ebca77u
        + coordinate.z * 0xc2b2ae3du
        ^ seed * 0x27d4eb2du;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7feb352du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846ca68bu;
    return hash ^ (hash >> 16u);
}

// Exact sparse lookup with a fixed 32-probe ceiling. The CPU chooses a seed
// that proves every resident key fits that ceiling at <= 0.5 load, so lookup
// cost and metadata are independent of the sparse demand bounding-box volume.
fn lookup_page(layer_index: u32, coordinate: vec3<u32>) -> PageResult {
    let page_offset = layer_word(layer_index, 25u);
    let capacity = control[page_offset];
    let seed = control[page_offset + 1u];
    let origin = vec3<f32>(
        f32(layer_word(layer_index, 26u)),
        f32(layer_word(layer_index, 27u)),
        f32(layer_word(layer_index, 28u)),
    );
    let cell = vec3<f32>(
        f32(layer_word(layer_index, 29u)),
        f32(layer_word(layer_index, 30u)),
        f32(layer_word(layer_index, 31u)),
    );
    let page_coordinate = floor((vec3<f32>(coordinate) - origin) / cell);
    let lower = origin + page_coordinate * cell - vec3<f32>(0.5);
    let upper = lower + cell;
    if any(page_coordinate < vec3<f32>(0.0)) || capacity == 0u {
        return PageResult(PAGE_MISSING, 0u, lower, upper);
    }
    let page = vec3<u32>(page_coordinate);
    var slot = page_hash(page, seed) & (capacity - 1u);
    var resource_index_plus_one = 0u;
    for (var probe = 0u; probe < 32u; probe += 1u) {
        let slot_offset = page_offset + 2u + slot * 4u;
        let entry_resource = control[slot_offset + 3u];
        if entry_resource == 0u {
            break;
        }
        if entry_resource != PAGE_TOMBSTONE
            && control[slot_offset] == page.x
            && control[slot_offset + 1u] == page.y
            && control[slot_offset + 2u] == page.z {
            resource_index_plus_one = entry_resource;
            break;
        }
        slot = (slot + 1u) & (capacity - 1u);
    }
    if resource_index_plus_one == 0u {
        return PageResult(PAGE_MISSING, 0u, lower, upper);
    }
    let resource_index = resource_index_plus_one - 1u;
    if resource_word(resource_index, 9u) == 0u {
        return PageResult(PAGE_MISSING, 0u, lower, upper);
    }
    let resource_origin = vec3<u32>(
        resource_word(resource_index, 1u),
        resource_word(resource_index, 2u),
        resource_word(resource_index, 3u),
    );
    let resource_shape = vec3<u32>(
        resource_word(resource_index, 4u),
        resource_word(resource_index, 5u),
        resource_word(resource_index, 6u),
    );
    if any(coordinate < resource_origin) || any(coordinate >= resource_origin + resource_shape) {
        return PageResult(PAGE_MISSING, 0u, lower, upper);
    }
    return PageResult(PAGE_RESIDENT, resource_index, lower, upper);
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

fn sample_resource(resource_index: u32, coordinate: vec3<u32>) -> SampleResult {
    if resource_word(resource_index, 9u) == 0u {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    // All-invalid pages are metadata-only residents with no payload allocation.
    // This guard must precede every origin/value/validity access.
    if resource_word(resource_index, 14u) == 0u {
        return SampleResult(SAMPLE_INVALID, 0.0);
    }
    let origin = vec3<u32>(
        resource_word(resource_index, 1u),
        resource_word(resource_index, 2u),
        resource_word(resource_index, 3u),
    );
    let shape = vec3<u32>(
        resource_word(resource_index, 4u),
        resource_word(resource_index, 5u),
        resource_word(resource_index, 6u),
    );
    if any(coordinate < origin) || any(coordinate >= origin + shape) {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    let local = coordinate - origin;
    let sample_index = (local.z * shape.y + local.y) * shape.x + local.x;
    if !sample_is_valid(resource_index, sample_index) {
        return SampleResult(SAMPLE_INVALID, 0.0);
    }
    return SampleResult(SAMPLE_VALID, sample_value(resource_index, sample_index));
}

fn sample_grid_nearest(layer_index: u32, grid: vec3<f32>) -> SampleResult {
    if !grid_inside(layer_index, grid) {
        return SampleResult(SAMPLE_OUTSIDE, 0.0);
    }
    let coordinate = grid_coordinate(layer_index, grid);
    let page = lookup_page(layer_index, coordinate);
    if page.kind != PAGE_RESIDENT {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    return sample_resource(page.resource_index, coordinate);
}

fn sample_grid_coordinate(layer_index: u32, coordinate: vec3<u32>) -> SampleResult {
    let page = lookup_page(layer_index, coordinate);
    if page.kind != PAGE_RESIDENT {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    return sample_resource(page.resource_index, coordinate);
}

fn coordinate_in_resource(resource_index: u32, coordinate: vec3<u32>) -> bool {
    let origin = vec3<u32>(
        resource_word(resource_index, 1u),
        resource_word(resource_index, 2u),
        resource_word(resource_index, 3u),
    );
    let shape = vec3<u32>(
        resource_word(resource_index, 4u),
        resource_word(resource_index, 5u),
        resource_word(resource_index, 6u),
    );
    return all(coordinate >= origin) && all(coordinate < origin + shape);
}

fn sample_linear_tap(
    layer_index: u32,
    page: PageResult,
    coordinate: vec3<u32>,
) -> SampleResult {
    // The normal case is an interpolation footprint wholly inside the page
    // already resolved by the ray segment. Only a true brick-boundary tap
    // performs another sparse-page lookup.
    if page.kind == PAGE_RESIDENT && coordinate_in_resource(page.resource_index, coordinate) {
        return sample_resource(page.resource_index, coordinate);
    }
    return sample_grid_coordinate(layer_index, coordinate);
}

fn sample_grid_linear_in_page(
    layer_index: u32,
    page: PageResult,
    grid: vec3<f32>,
) -> SampleResult {
    if !grid_inside(layer_index, grid) {
        return SampleResult(SAMPLE_OUTSIDE, 0.0);
    }
    let shape = layer_shape(layer_index);
    let clamped = clamp(grid, vec3<f32>(0.0), vec3<f32>(shape - vec3<u32>(1u)));
    let lower = vec3<u32>(floor(clamped));
    let upper = min(lower + vec3<u32>(1u), shape - vec3<u32>(1u));
    let fraction = clamped - vec3<f32>(lower);
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
                    sample = sample_linear_tap(layer_index, page, coordinate);
                }
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

fn sample_grid_linear(layer_index: u32, grid: vec3<f32>) -> SampleResult {
    if !grid_inside(layer_index, grid) {
        return SampleResult(SAMPLE_OUTSIDE, 0.0);
    }
    let shape = layer_shape(layer_index);
    let clamped = clamp(grid, vec3<f32>(0.0), vec3<f32>(shape - vec3<u32>(1u)));
    let page = lookup_page(layer_index, vec3<u32>(floor(clamped)));
    return sample_grid_linear_in_page(layer_index, page, grid);
}

fn sample_grid(layer_index: u32, grid: vec3<f32>) -> SampleResult {
    if layer_word(layer_index, 56u) == 0u {
        return sample_grid_nearest(layer_index, grid);
    }
    return sample_grid_linear(layer_index, grid);
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

fn world_to_grid(layer_index: u32, world: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(world, vec3<f32>(layer_f32(layer_index, 32u), layer_f32(layer_index, 33u), layer_f32(layer_index, 34u))) + layer_f32(layer_index, 35u),
        dot(world, vec3<f32>(layer_f32(layer_index, 36u), layer_f32(layer_index, 37u), layer_f32(layer_index, 38u))) + layer_f32(layer_index, 39u),
        dot(world, vec3<f32>(layer_f32(layer_index, 40u), layer_f32(layer_index, 41u), layer_f32(layer_index, 42u))) + layer_f32(layer_index, 43u),
    );
}

fn world_vector_to_grid(layer_index: u32, world: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(world, vec3<f32>(layer_f32(layer_index, 32u), layer_f32(layer_index, 33u), layer_f32(layer_index, 34u))),
        dot(world, vec3<f32>(layer_f32(layer_index, 36u), layer_f32(layer_index, 37u), layer_f32(layer_index, 38u))),
        dot(world, vec3<f32>(layer_f32(layer_index, 40u), layer_f32(layer_index, 41u), layer_f32(layer_index, 42u))),
    );
}

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

fn iso_lighting(
    layer_index: u32,
    point: vec3<f32>,
    world_direction: vec3<f32>,
) -> SampleResult {
    if layer_word(layer_index, 57u) == 0u {
        return SampleResult(SAMPLE_VALID, 1.0);
    }
    let gradient = iso_gradient(layer_index, point);
    let gradient_kind = u32(gradient.w);
    if gradient_kind != SAMPLE_VALID {
        return SampleResult(gradient_kind, 0.2);
    }
    var light = -world_direction;
    if layer_word(layer_index, 58u) != 0u {
        light = vec3<f32>(
            layer_f32(layer_index, 59u),
            layer_f32(layer_index, 60u),
            layer_f32(layer_index, 61u),
        );
    }
    light = normalize(light);
    if dot(gradient.xyz, gradient.xyz) <= EPSILON {
        return SampleResult(SAMPLE_VALID, 0.2);
    }
    return SampleResult(SAMPLE_VALID, 0.2 + 0.8 * abs(dot(gradient.xyz, light)));
}

fn dvr_effective_alpha(base_tau: f32, layer_opacity: f32) -> f32 {
    let base_alpha = 1.0 - exp(-max(base_tau, 0.0));
    return clamp(layer_opacity, 0.0, 1.0) * base_alpha;
}

fn dvr_effective_tau(base_tau: f32, layer_opacity: f32) -> f32 {
    return -log(max(1.0 - dvr_effective_alpha(base_tau, layer_opacity), EPSILON));
}

fn dvr_can_terminate(alpha: f32) -> bool {
    return alpha >= ALPHA_TERMINATION && control[28u] != 0u;
}

fn transparent_pixel() -> PixelResult {
    return PixelResult(vec3<f32>(0.0), 0.0, 1u, 0u, LARGE_DISTANCE);
}

fn displayed_pixel(layer_index: u32, display: f32, alpha_value: f32) -> PixelResult {
    let alpha = clamp(alpha_value, 0.0, 1.0);
    let color = vec3<f32>(
        layer_f32(layer_index, 12u),
        layer_f32(layer_index, 13u),
        layer_f32(layer_index, 14u),
    );
    return PixelResult(color * display * alpha, alpha, 1u, 1u, LARGE_DISTANCE);
}

fn composite_over(near: PixelResult, far: PixelResult) -> PixelResult {
    let remaining = 1.0 - near.alpha;
    return PixelResult(
        near.premultiplied_rgb + far.premultiplied_rgb * remaining,
        near.alpha + far.alpha * remaining,
        near.covered & far.covered,
        near.valid | far.valid,
        min(near.depth, far.depth),
    );
}

fn composite_additive(first: PixelResult, second: PixelResult) -> PixelResult {
    return PixelResult(
        clamp(first.premultiplied_rgb + second.premultiplied_rgb, vec3<f32>(0.0), vec3<f32>(1.0)),
        1.0 - (1.0 - first.alpha) * (1.0 - second.alpha),
        first.covered & second.covered,
        first.valid | second.valid,
        min(first.depth, second.depth),
    );
}

fn curve_value(value: f32, low: f32, high: f32, gamma: f32, invert: u32) -> f32 {
    var normalized = clamp((value - low) / (high - low), 0.0, 1.0);
    if invert != 0u {
        normalized = 1.0 - normalized;
    }
    return pow(normalized, gamma);
}

fn transfer_value(layer_index: u32, value: f32) -> f32 {
    return curve_value(
        value,
        layer_f32(layer_index, 10u),
        layer_f32(layer_index, 11u),
        layer_f32(layer_index, 16u),
        layer_word(layer_index, 17u),
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

fn render_mip(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    exit: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var maximum = 0.0;
    var has_value = false;
    var covered = 1u;
    var index = 0u;
    loop {
        if index >= count {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        let point = origin + direction * distance;
        if !grid_inside(layer_index, point) {
            index += 1u;
            continue;
        }
        let page = page_for_sample(layer_index, origin, direction, distance);
        let next = segment_end_index(
            entry,
            step,
            index,
            page_exit_distance(page, point, direction, distance, exit),
            count,
        );
        if page.kind != PAGE_RESIDENT {
            covered = 0u;
            index = next;
            continue;
        }
        let any_valid = resource_word(page.resource_index, 14u) != 0u;
        if !any_valid {
            index = next;
            continue;
        }
        if layer_word(layer_index, 56u) == 0u
            && page_fully_covered(page)
            && has_value
            && resource_f32(page.resource_index, 13u) <= maximum {
            index = next;
            continue;
        }
        loop {
            if index >= next {
                break;
            }
            let sample_distance = entry + (f32(index) + 0.5) * step;
            let sample = sample_in_page(
                layer_index,
                page,
                origin + direction * sample_distance,
            );
            if sample.kind == SAMPLE_VALID {
                maximum = select(sample.value, max(maximum, sample.value), has_value);
                has_value = true;
            } else if sample.kind == SAMPLE_MISSING {
                covered = 0u;
            }
            index += 1u;
        }
    }
    if !has_value {
        var result = transparent_pixel();
        result.covered = covered;
        return result;
    }
    var result = displayed_pixel(
        layer_index,
        transfer_value(layer_index, maximum),
        layer_f32(layer_index, 15u),
    );
    result.covered = covered;
    return result;
}

fn render_dvr_layer(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    exit: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var result = transparent_pixel();
    var any_valid = false;
    var index = 0u;
    loop {
        if index >= count || dvr_can_terminate(result.alpha) {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        let point = origin + direction * distance;
        if !grid_inside(layer_index, point) {
            index += 1u;
            continue;
        }
        let page = page_for_sample(layer_index, origin, direction, distance);
        let next = segment_end_index(
            entry,
            step,
            index,
            page_exit_distance(page, point, direction, distance, exit),
            count,
        );
        if page.kind != PAGE_RESIDENT {
            result.covered = 0u;
            index = next;
            continue;
        }
        if resource_word(page.resource_index, 14u) == 0u {
            index = next;
            continue;
        }
        if layer_word(layer_index, 56u) == 0u
            && page_fully_covered(page)
            && resource_word(page.resource_index, 15u) != 0u
            && (resource_f32(page.resource_index, 13u) <= layer_f32(layer_index, 20u)
                || layer_f32(layer_index, 15u) <= 0.0
                || layer_f32(layer_index, 23u) <= 0.0) {
            any_valid = true;
            index = next;
            continue;
        }
        loop {
            if index >= next || dvr_can_terminate(result.alpha) {
                break;
            }
            let sample_distance = entry + (f32(index) + 0.5) * step;
            let sample = sample_in_page(
                layer_index,
                page,
                origin + direction * sample_distance,
            );
            if sample.kind == SAMPLE_VALID {
                any_valid = true;
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
                result = composite_over(
                    result,
                    displayed_pixel(
                        layer_index,
                        transfer_value(layer_index, sample.value),
                        sample_alpha,
                    ),
                );
            } else if sample.kind == SAMPLE_MISSING {
                result.covered = 0u;
            }
            index += 1u;
        }
    }
    result.valid = select(0u, 1u, any_valid);
    return result;
}

fn render_iso(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    world_direction: vec3<f32>,
    entry: f32,
    exit: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var covered = 1u;
    var any_valid = false;
    var index = 0u;
    loop {
        if index >= count {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        let point = origin + direction * distance;
        if !grid_inside(layer_index, point) {
            index += 1u;
            continue;
        }
        let page = page_for_sample(layer_index, origin, direction, distance);
        let next = segment_end_index(
            entry,
            step,
            index,
            page_exit_distance(page, point, direction, distance, exit),
            count,
        );
        if page.kind != PAGE_RESIDENT {
            covered = 0u;
            index = next;
            continue;
        }
        if resource_word(page.resource_index, 14u) == 0u {
            index = next;
            continue;
        }
        let minimum_display = transfer_value(layer_index, resource_f32(page.resource_index, 12u));
        let maximum_display = transfer_value(layer_index, resource_f32(page.resource_index, 13u));
        if layer_word(layer_index, 56u) == 0u
            && page_fully_covered(page)
            && resource_word(page.resource_index, 15u) != 0u
            && max(minimum_display, maximum_display) < layer_f32(layer_index, 19u) {
            any_valid = true;
            index = next;
            continue;
        }
        loop {
            if index >= next {
                break;
            }
            let sample_distance = entry + (f32(index) + 0.5) * step;
            let sample_point = origin + direction * sample_distance;
            let sample = sample_in_page(layer_index, page, sample_point);
            if sample.kind == SAMPLE_VALID {
                any_valid = true;
                let display = transfer_value(layer_index, sample.value);
                if display >= layer_f32(layer_index, 19u) {
                    let lighting = iso_lighting(layer_index, sample_point, world_direction);
                    var result = displayed_pixel(
                        layer_index,
                        display * lighting.value,
                        layer_f32(layer_index, 15u),
                    );
                    result.covered = covered & select(1u, 0u, lighting.kind == SAMPLE_MISSING);
                    result.depth = sample_distance;
                    return result;
                }
            } else if sample.kind == SAMPLE_MISSING {
                covered = 0u;
            }
            index += 1u;
        }
    }
    var result = transparent_pixel();
    result.covered = covered;
    result.valid = select(0u, 1u, any_valid);
    return result;
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

fn render_volume_layer(
    layer_index: u32,
    world_origin: vec3<f32>,
    world_direction: vec3<f32>,
) -> PixelResult {
    let ray = volume_ray(layer_index, world_origin, world_direction);
    if !ray.intersects {
        return transparent_pixel();
    }
    let step = 1.0 / ray.grid_speed;
    let count = max(u32(ceil((ray.exit - ray.entry) / step)), 1u);
    let mode = layer_word(layer_index, 18u);
    if mode == 0u {
        return render_mip(
            layer_index,
            ray.origin,
            ray.direction,
            ray.entry,
            ray.exit,
            step,
            count,
        );
    }
    if mode == 1u {
        return render_dvr_layer(
            layer_index,
            ray.origin,
            ray.direction,
            ray.entry,
            ray.exit,
            step,
            count,
        );
    }
    return render_iso(
        layer_index,
        ray.origin,
        ray.direction,
        world_direction,
        ray.entry,
        ray.exit,
        step,
        count,
    );
}

// Compatible DVR layers share grid geometry (proved by the CPU control
// builder), so optical density and color are combined at each sample in one
// front-to-back traversal rather than compositing finished 2D channel images.
fn render_fused_dvr(world_origin: vec3<f32>, world_direction: vec3<f32>) -> PixelResult {
    let ray = volume_ray(0u, world_origin, world_direction);
    if !ray.intersects {
        return transparent_pixel();
    }
    let origin = ray.origin;
    let direction = ray.direction;
    let step = 1.0 / ray.grid_speed;
    let count = max(u32(ceil((ray.exit - ray.entry) / step)), 1u);
    let layer_count = control[2u];
    var result = transparent_pixel();
    var any_valid = false;
    var index = 0u;
    var resources: array<u32, 64>;
    loop {
        if index >= count || dvr_can_terminate(result.alpha) {
            break;
        }
        let distance = ray.entry + (f32(index) + 0.5) * step;
        let point = origin + direction * distance;
        if !grid_inside(0u, point) {
            index += 1u;
            continue;
        }
        let coordinate = grid_coordinate(0u, point);
        var segment_exit = ray.exit;
        var any_work = false;
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            let page = lookup_page(layer_index, coordinate);
            segment_exit = min(
                segment_exit,
                page_exit_distance(page, point, direction, distance, ray.exit),
            );
            if page.kind != PAGE_RESIDENT {
                resources[layer_index] = 0xffffffffu;
                result.covered = 0u;
                continue;
            }
            resources[layer_index] = page.resource_index;
            if resource_word(page.resource_index, 14u) == 0u {
                resources[layer_index] = 0xffffffffu;
                continue;
            }
            if page_fully_covered(page)
                && resource_word(page.resource_index, 15u) != 0u
                && (resource_f32(page.resource_index, 13u) <= layer_f32(layer_index, 20u)
                    || layer_f32(layer_index, 15u) <= 0.0
                    || layer_f32(layer_index, 23u) <= 0.0) {
                any_valid = true;
                resources[layer_index] = 0xffffffffu;
                continue;
            }
            any_work = true;
        }
        let next = segment_end_index(ray.entry, step, index, segment_exit, count);
        if !any_work {
            index = next;
            continue;
        }
        loop {
            if index >= next || dvr_can_terminate(result.alpha) {
                break;
            }
            let sample_distance = ray.entry + (f32(index) + 0.5) * step;
            let sample_coordinate = grid_coordinate(0u, origin + direction * sample_distance);
            var tau_total = 0.0;
            var weighted_rgb = vec3<f32>(0.0);
            for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
                let resource_index = resources[layer_index];
                if resource_index == 0xffffffffu {
                    continue;
                }
                let sample = sample_resource(resource_index, sample_coordinate);
                if sample.kind == SAMPLE_MISSING {
                    result.covered = 0u;
                    continue;
                }
                if sample.kind != SAMPLE_VALID {
                    continue;
                }
                any_valid = true;
                let opacity = curve_value(
                    sample.value,
                    layer_f32(layer_index, 20u),
                    layer_f32(layer_index, 21u),
                    layer_f32(layer_index, 22u),
                    0u,
                );
                let tau = dvr_effective_tau(
                    opacity * layer_f32(layer_index, 23u) * step,
                    layer_f32(layer_index, 15u),
                );
                if tau <= 0.0 {
                    continue;
                }
                let color = vec3<f32>(
                    layer_f32(layer_index, 12u),
                    layer_f32(layer_index, 13u),
                    layer_f32(layer_index, 14u),
                ) * transfer_value(layer_index, sample.value);
                tau_total += tau;
                weighted_rgb += color * tau;
            }
            if tau_total > 0.0 {
                let alpha = 1.0 - exp(-tau_total);
                let remaining = 1.0 - result.alpha;
                result.premultiplied_rgb += remaining * (weighted_rgb / tau_total) * alpha;
                result.alpha += remaining * alpha;
            }
            index += 1u;
        }
    }
    result.valid = select(0u, 1u, any_valid);
    return result;
}

// Correct fallback for SmoothLinear and mixed-affine DVR. All channels are
// sampled at the same monotonically increasing world-space distances and
// their optical depths are integrated jointly, so result semantics do not
// depend on layer order. Compatible VoxelExact grids retain the faster
// brick-segment specialization above.
fn render_general_dvr(world_origin: vec3<f32>, world_direction: vec3<f32>) -> PixelResult {
    let layer_count = control[2u];
    var entry = LARGE_DISTANCE;
    var exit = -LARGE_DISTANCE;
    var step = LARGE_DISTANCE;
    var has_dvr = false;
    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        if layer_word(layer_index, 18u) != 1u {
            continue;
        }
        let ray = volume_ray(layer_index, world_origin, world_direction);
        if !ray.intersects {
            continue;
        }
        has_dvr = true;
        entry = min(entry, ray.entry);
        exit = max(exit, ray.exit);
        step = min(step, 1.0 / ray.grid_speed);
    }
    if !has_dvr || exit <= entry || step <= 0.0 {
        return transparent_pixel();
    }

    let count = max(u32(ceil((exit - entry) / step)), 1u);
    var result = transparent_pixel();
    var any_valid = false;
    var index = 0u;
    var resources: array<u32, 64>;
    loop {
        if index >= count || dvr_can_terminate(result.alpha) {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        var segment_exit = exit;
        var any_work = false;

        // Resolve one page per layer for the common-world segment. Mixed
        // affine grids can leave pages at different distances, so the nearest
        // exit across all layers is the only segment boundary. Missing,
        // metadata-empty, and exact-mode extrema-zero pages remain in the
        // boundary calculation but do no per-sample work.
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            resources[layer_index] = 0xffffffffu;
            if layer_word(layer_index, 18u) != 1u {
                continue;
            }
            let ray = volume_ray(layer_index, world_origin, world_direction);
            if !ray.intersects || distance >= ray.exit {
                continue;
            }
            if distance < ray.entry {
                segment_exit = min(segment_exit, ray.entry);
                continue;
            }
            let grid_direction = ray.direction;
            let grid = ray.origin + ray.direction * distance;
            if !grid_inside(layer_index, grid) {
                segment_exit = min(segment_exit, ray.exit);
                continue;
            }
            let page = lookup_page(layer_index, grid_coordinate(layer_index, grid));
            segment_exit = min(
                segment_exit,
                page_exit_distance(page, grid, grid_direction, distance, ray.exit),
            );
            if page.kind != PAGE_RESIDENT {
                result.covered = 0u;
                continue;
            }
            if resource_word(page.resource_index, 14u) == 0u {
                continue;
            }
            let can_infer_valid = resource_word(page.resource_index, 15u) != 0u;
            let has_no_contribution = layer_f32(layer_index, 15u) <= 0.0
                || layer_f32(layer_index, 23u) <= 0.0
                || (layer_word(layer_index, 56u) == 0u
                    && page_fully_covered(page)
                    && resource_f32(page.resource_index, 13u) <= layer_f32(layer_index, 20u));
            if can_infer_valid && has_no_contribution {
                any_valid = true;
                continue;
            }
            resources[layer_index] = page.resource_index;
            any_work = true;
        }

        let next = segment_end_index(entry, step, index, segment_exit, count);
        if !any_work {
            index = next;
            continue;
        }
        loop {
            if index >= next || dvr_can_terminate(result.alpha) {
                break;
            }
            let sample_distance = entry + (f32(index) + 0.5) * step;
            let sample_world = world_origin + world_direction * sample_distance;
            var tau_total = 0.0;
            var weighted_rgb = vec3<f32>(0.0);
            for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
                let resource_index = resources[layer_index];
                if resource_index == 0xffffffffu {
                    continue;
                }
                let page = PageResult(
                    PAGE_RESIDENT,
                    resource_index,
                    vec3<f32>(0.0),
                    vec3<f32>(0.0),
                );
                let sample = sample_in_page(
                    layer_index,
                    page,
                    world_to_grid(layer_index, sample_world),
                );
                if sample.kind == SAMPLE_MISSING {
                    result.covered = 0u;
                    continue;
                }
                if sample.kind != SAMPLE_VALID {
                    continue;
                }
                any_valid = true;
                let opacity = curve_value(
                    sample.value,
                    layer_f32(layer_index, 20u),
                    layer_f32(layer_index, 21u),
                    layer_f32(layer_index, 22u),
                    0u,
                );
                let tau = dvr_effective_tau(
                    opacity * layer_f32(layer_index, 23u) * step,
                    layer_f32(layer_index, 15u),
                );
                if tau <= 0.0 {
                    continue;
                }
                let color = vec3<f32>(
                    layer_f32(layer_index, 12u),
                    layer_f32(layer_index, 13u),
                    layer_f32(layer_index, 14u),
                ) * transfer_value(layer_index, sample.value);
                tau_total += tau;
                weighted_rgb += color * tau;
            }
            if tau_total > 0.0 {
                let alpha = 1.0 - exp(-tau_total);
                let remaining = 1.0 - result.alpha;
                result.premultiplied_rgb += remaining * (weighted_rgb / tau_total) * alpha;
                result.alpha += remaining * alpha;
            }
            index += 1u;
        }
    }
    result.valid = select(0u, 1u, any_valid);
    return result;
}

fn render_cross_section_layer(layer_index: u32, position: vec2<f32>) -> PixelResult {
    let width = f32(control[4u]);
    let height = f32(control[5u]);
    let screen_x = (position.x / width - 0.5) * control_f32(18u);
    let screen_y = (0.5 - position.y / height) * control_f32(19u);
    let center = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u));
    let right = vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u));
    let up = vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u));
    let world = center + (right * screen_x + up * screen_y) * control_f32(17u);
    let sample = sample_grid(layer_index, world_to_grid(layer_index, world));
    if sample.kind == SAMPLE_VALID {
        return displayed_pixel(
            layer_index,
            transfer_value(layer_index, sample.value),
            layer_f32(layer_index, 15u),
        );
    }
    var result = transparent_pixel();
    if sample.kind == SAMPLE_MISSING {
        result.covered = 0u;
    }
    return result;
}

fn render_iso_stack(world_origin: vec3<f32>, world_direction: vec3<f32>) -> PixelResult {
    var hits: array<PixelResult, 64>;
    var hit_count = 0u;
    var facts = transparent_pixel();
    let layer_count = control[2u];
    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        if layer_word(layer_index, 18u) != 2u {
            continue;
        }
        let hit = render_volume_layer(layer_index, world_origin, world_direction);
        facts.covered &= hit.covered;
        facts.valid |= hit.valid;
        if hit.alpha <= 0.0 {
            continue;
        }
        var insertion = hit_count;
        loop {
            if insertion == 0u || hits[insertion - 1u].depth <= hit.depth {
                break;
            }
            hits[insertion] = hits[insertion - 1u];
            insertion -= 1u;
        }
        hits[insertion] = hit;
        hit_count += 1u;
    }
    var result = transparent_pixel();
    var index = hit_count;
    loop {
        if index == 0u {
            break;
        }
        index -= 1u;
        result = composite_over(hits[index], result);
    }
    result.covered = facts.covered;
    result.valid = facts.valid;
    return result;
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let position = vec2<f32>(
        f32((vertex_index << 1u) & 2u),
        f32(vertex_index & 2u),
    );
    return vec4<f32>(position * 2.0 - 1.0, 0.0, 1.0);
}

fn render_fragment(position: vec4<f32>) -> PixelResult {
    var pixel = transparent_pixel();
    let layer_count = control[2u];
    let view_kind = control[3u];
    let pixel_index = position.xy - vec2<f32>(0.5);
    let ray_origin = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u))
        + vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u)) * pixel_index.x
        + vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u)) * pixel_index.y;
    // Traversal is invariant to direction magnitude: intersection distances
    // and the grid-space step scale inversely. Keep the camera ray in its
    // native scale here; attached ISO lighting normalizes it only where a
    // unit vector is actually required.
    let ray_direction =
        vec3<f32>(control_f32(17u), control_f32(18u), control_f32(19u))
            + vec3<f32>(control_f32(20u), control_f32(21u), control_f32(22u)) * pixel_index.x
            + vec3<f32>(control_f32(23u), control_f32(24u), control_f32(25u)) * pixel_index.y;

    if view_kind == 0u {
        var all_dvr = layer_count != 0u;
        var all_iso = layer_count != 0u;
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            let mode = layer_word(layer_index, 18u);
            all_dvr = all_dvr && mode == 1u;
            all_iso = all_iso && mode == 2u;
        }
        if all_dvr {
            if control[27u] != 0u {
                pixel = render_fused_dvr(ray_origin, ray_direction);
            } else {
                pixel = render_general_dvr(ray_origin, ray_direction);
            }
        } else if all_iso {
            pixel = render_iso_stack(ray_origin, ray_direction);
        } else {
            // Mixed modes retain semantic view order. Joint DVR integration
            // and ISO depth sorting are whole-stack operations; applying
            // either to a subset would move it across neighboring layers.
            for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
                pixel = composite_over(
                    pixel,
                    render_volume_layer(layer_index, ray_origin, ray_direction),
                );
            }
        }
    } else {
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            pixel = composite_additive(
                pixel,
                render_cross_section_layer(layer_index, position.xy),
            );
        }
    }

    return pixel;
}

// Keep the overwhelmingly common MIP path in its own entry-point call graph.
// In particular, this path cannot reach the ISO hit-sort array or the fused
// DVR resource array, allowing the backend compiler to generate a materially
// smaller fragment program for interactive MIP navigation.
fn render_mip_fragment(position: vec4<f32>) -> PixelResult {
    var pixel = transparent_pixel();
    let layer_count = control[2u];
    let pixel_index = position.xy - vec2<f32>(0.5);
    let ray_origin = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u))
        + vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u)) * pixel_index.x
        + vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u)) * pixel_index.y;
    let ray_direction =
        vec3<f32>(control_f32(17u), control_f32(18u), control_f32(19u))
            + vec3<f32>(control_f32(20u), control_f32(21u), control_f32(22u)) * pixel_index.x
            + vec3<f32>(control_f32(23u), control_f32(24u), control_f32(25u)) * pixel_index.y;

    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        let ray = volume_ray(layer_index, ray_origin, ray_direction);
        if !ray.intersects {
            continue;
        }
        let step = 1.0 / ray.grid_speed;
        let count = max(u32(ceil((ray.exit - ray.entry) / step)), 1u);
        pixel = composite_additive(
            pixel,
            render_mip(
                layer_index,
                ray.origin,
                ray.direction,
                ray.entry,
                ray.exit,
                step,
                count,
            ),
        );
    }
    return pixel;
}

@fragment
fn fs_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = render_fragment(position);
    return vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
}

@fragment
fn fs_validation(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    let pixel = render_fragment(position);
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}

@fragment
fn fs_mip_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = render_mip_fragment(position);
    return vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
}

@fragment
fn fs_mip_validation(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    let pixel = render_mip_fragment(position);
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}
