// Accepted color-kernel ABI and plane-safe sampling foundation. This source
// is concatenated with each dedicated color kernel and owns no mode-specific
// traversal or shading behavior.

const LAYER_WORDS: u32 = 64u;
const RESOURCE_WORDS: u32 = 16u;
const LARGE_DISTANCE: f32 = 3.402823e38;

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

struct ResourceAddress {
    origin: vec3<u32>,
    shape: vec3<u32>,
    segment: u32,
    base: u32,
    validity_offset: u32,
    dtype_bytes: u32,
    any_valid: u32,
    all_valid: u32,
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

@group(0) @binding(5)
var<storage, read> residency_directory: array<u32>;

@group(0) @binding(6)
var<storage, read> page_records: array<u32>;

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
    return page_records[resource_index * RESOURCE_WORDS + field];
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

fn resource_address(resource_index: u32) -> ResourceAddress {
    return ResourceAddress(
        vec3<u32>(
            resource_word(resource_index, 1u),
            resource_word(resource_index, 2u),
            resource_word(resource_index, 3u),
        ),
        vec3<u32>(
            resource_word(resource_index, 4u),
            resource_word(resource_index, 5u),
            resource_word(resource_index, 6u),
        ),
        resource_word(resource_index, 10u),
        resource_word(resource_index, 7u),
        resource_word(resource_index, 8u),
        resource_word(resource_index, 9u),
        resource_word(resource_index, 14u),
        resource_word(resource_index, 15u),
    );
}

fn sample_value_at(address: ResourceAddress, sample_index: u32) -> f32 {
    let offset = address.base + sample_index * address.dtype_bytes;
    if address.dtype_bytes == 1u {
        return f32(read_byte(address.segment, offset));
    }
    if address.dtype_bytes == 2u {
        let word = payload_word(address.segment, offset >> 2u);
        return f32((word >> ((offset & 2u) * 8u)) & 0xffffu);
    }
    return bitcast<f32>(payload_word(address.segment, offset >> 2u));
}

fn sample_is_valid_at(address: ResourceAddress, sample_index: u32) -> bool {
    if address.all_valid != 0u {
        return true;
    }
    if address.validity_offset == 0xffffffffu {
        return true;
    }
    let validity_byte = read_byte(
        address.segment,
        address.validity_offset + sample_index / 8u,
    );
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

fn rotate_left_15(word: u32) -> u32 {
    return (word << 15u) | (word >> 17u);
}

fn rotate_left_13(word: u32) -> u32 {
    return (word << 13u) | (word >> 19u);
}

fn directory_hash_mix(hash_in: u32, key_word: u32) -> u32 {
    var word = key_word * 0xcc9e2d51u;
    word = rotate_left_15(word);
    word = word * 0x1b873593u;
    var hash = hash_in ^ word;
    hash = rotate_left_13(hash);
    return hash * 5u + 0xe6546b64u;
}

// Exact MurmurHash3 x86-32 projection shared with the CPU residency owner.
//
// Keep the seven fixed key words scalar so the bounded probe body contains no
// secondary dynamically indexed key loop.
fn directory_hash(
    layer: u32,
    time_low: u32,
    time_high: u32,
    scale: u32,
    page_x: u32,
    page_y: u32,
    page_z: u32,
) -> u32 {
    var hash = 0x4d344431u;
    hash = directory_hash_mix(hash, layer);
    hash = directory_hash_mix(hash, time_low);
    hash = directory_hash_mix(hash, time_high);
    hash = directory_hash_mix(hash, scale);
    hash = directory_hash_mix(hash, page_x);
    hash = directory_hash_mix(hash, page_y);
    hash = directory_hash_mix(hash, page_z);
    hash = hash ^ 28u;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x85ebca6bu;
    hash = hash ^ (hash >> 13u);
    hash = hash * 0xc2b2ae35u;
    return hash ^ (hash >> 16u);
}

// Exact renderer-global lookup with a fixed 32-probe ceiling. Camera and
// presentation-body changes never mutate this directory.
fn lookup_page_at_scale(
    layer_index: u32,
    coordinate: vec3<u32>,
    key_scale: u32,
    cell: vec3<u32>,
) -> PageResult {
    let capacity = control[7u];
    let page = coordinate / cell;
    let page_origin = page * cell;
    let lower = vec3<f32>(page_origin) - vec3<f32>(0.5);
    let upper = lower + vec3<f32>(cell);
    let key_layer = layer_word(layer_index, 0u);
    let key_time_low = layer_word(layer_index, 25u);
    let key_time_high = layer_word(layer_index, 26u);
    if capacity == 0u {
        return PageResult(PAGE_MISSING, 0u, lower, upper);
    }
    var slot = directory_hash(
        key_layer,
        key_time_low,
        key_time_high,
        key_scale,
        page.x,
        page.y,
        page.z,
    ) & (capacity - 1u);
    var resource_index_plus_one = 0u;
    for (var probe = 0u; probe < 32u; probe += 1u) {
        let slot_offset = slot * 8u;
        let entry_resource = residency_directory[slot_offset + 7u];
        if entry_resource == 0u {
            break;
        }
        let matches = entry_resource != PAGE_TOMBSTONE &&
            residency_directory[slot_offset] == key_layer &&
            residency_directory[slot_offset + 1u] == key_time_low &&
            residency_directory[slot_offset + 2u] == key_time_high &&
            residency_directory[slot_offset + 3u] == key_scale &&
            residency_directory[slot_offset + 4u] == page.x &&
            residency_directory[slot_offset + 5u] == page.y &&
            residency_directory[slot_offset + 6u] == page.z;
        if matches {
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

fn lookup_page(layer_index: u32, coordinate: vec3<u32>) -> PageResult {
    let full_resource_plus_one = layer_word(layer_index, 62u);
    if full_resource_plus_one != 0u {
        let resource_index = full_resource_plus_one - 1u;
        let shape = layer_shape(layer_index);
        if resource_word(resource_index, 9u) == 0u
            || any(coordinate >= shape) {
            return PageResult(
                PAGE_MISSING,
                0u,
                vec3<f32>(-0.5),
                vec3<f32>(shape) - vec3<f32>(0.5),
            );
        }
        return PageResult(
            PAGE_RESIDENT,
            resource_index,
            vec3<f32>(-0.5),
            vec3<f32>(shape) - vec3<f32>(0.5),
        );
    }
    return lookup_page_at_scale(
        layer_index,
        coordinate,
        layer_word(layer_index, 24u),
        vec3<u32>(
            layer_word(layer_index, 27u),
            layer_word(layer_index, 28u),
            layer_word(layer_index, 29u),
        ),
    );
}

fn coordinate_in_address(address: ResourceAddress, coordinate: vec3<u32>) -> bool {
    return all(coordinate >= address.origin)
        && all(coordinate < address.origin + address.shape);
}

fn sample_resource_at(address: ResourceAddress, coordinate: vec3<u32>) -> SampleResult {
    if address.dtype_bytes == 0u {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    // All-invalid pages are metadata-only residents with no payload allocation.
    // This guard must precede every origin/value/validity access.
    if address.any_valid == 0u {
        return SampleResult(SAMPLE_INVALID, 0.0);
    }
    if !coordinate_in_address(address, coordinate) {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    let local = coordinate - address.origin;
    let sample_index =
        (local.z * address.shape.y + local.y) * address.shape.x + local.x;
    if !sample_is_valid_at(address, sample_index) {
        return SampleResult(SAMPLE_INVALID, 0.0);
    }
    return SampleResult(SAMPLE_VALID, sample_value_at(address, sample_index));
}

fn sample_resource(resource_index: u32, coordinate: vec3<u32>) -> SampleResult {
    return sample_resource_at(resource_address(resource_index), coordinate);
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
    return coordinate_in_address(resource_address(resource_index), coordinate);
}

fn sample_linear_tap(
    layer_index: u32,
    page: PageResult,
    coordinate: vec3<u32>,
) -> SampleResult {
    // The normal case is an interpolation footprint wholly inside the page
    // already resolved for the sample. Only a true boundary tap performs
    // another sparse-page lookup.
    if page.kind == PAGE_RESIDENT && coordinate_in_resource(page.resource_index, coordinate) {
        return sample_resource(page.resource_index, coordinate);
    }
    return sample_grid_coordinate(layer_index, coordinate);
}

fn sample_grid_linear_in_resolved_page(
    layer_index: u32,
    page: PageResult,
    address: ResourceAddress,
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
    if coordinate_in_address(address, lower) && coordinate_in_address(address, upper) {
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
                    let sample = sample_resource_at(address, coordinate);
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
                sample = sample_linear_tap(layer_index, page, coordinate);
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

fn sample_grid_linear_in_page(
    layer_index: u32,
    page: PageResult,
    grid: vec3<f32>,
) -> SampleResult {
    if page.kind != PAGE_RESIDENT {
        return SampleResult(SAMPLE_MISSING, 0.0);
    }
    return sample_grid_linear_in_resolved_page(
        layer_index,
        page,
        resource_address(page.resource_index),
        grid,
    );
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

fn world_to_grid(layer_index: u32, world: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(world, vec3<f32>(layer_f32(layer_index, 32u), layer_f32(layer_index, 33u), layer_f32(layer_index, 34u))) + layer_f32(layer_index, 35u),
        dot(world, vec3<f32>(layer_f32(layer_index, 36u), layer_f32(layer_index, 37u), layer_f32(layer_index, 38u))) + layer_f32(layer_index, 39u),
        dot(world, vec3<f32>(layer_f32(layer_index, 40u), layer_f32(layer_index, 41u), layer_f32(layer_index, 42u))) + layer_f32(layer_index, 43u),
    );
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

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let position = vec2<f32>(
        f32((vertex_index << 1u) & 2u),
        f32(vertex_index & 2u),
    );
    return vec4<f32>(position * 2.0 - 1.0, 0.0, 1.0);
}
