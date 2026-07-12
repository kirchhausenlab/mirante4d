const HEADER_WORDS: u32 = 32u;
const LAYER_WORDS: u32 = 32u;
const RESOURCE_WORDS: u32 = 16u;
const MAX_RAY_SAMPLES: u32 = 16384u;

const SAMPLE_OUTSIDE: u32 = 0u;
const SAMPLE_MISSING: u32 = 1u;
const SAMPLE_INVALID: u32 = 2u;
const SAMPLE_VALID: u32 = 3u;

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
};

@group(0) @binding(0)
var<storage, read> control: array<u32>;

@group(0) @binding(1)
var<storage, read> arena: array<u32>;

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

fn read_byte(byte_offset: u32) -> u32 {
    let word = arena[byte_offset >> 2u];
    let shift = (byte_offset & 3u) * 8u;
    return (word >> shift) & 255u;
}

fn sample_value(resource_index: u32, sample_index: u32) -> f32 {
    let base = resource_word(resource_index, 7u);
    let dtype_bytes = resource_word(resource_index, 9u);
    let offset = base + sample_index * dtype_bytes;
    if dtype_bytes == 1u {
        return f32(read_byte(offset));
    }
    if dtype_bytes == 2u {
        return f32(read_byte(offset) | (read_byte(offset + 1u) << 8u));
    }
    let bits = read_byte(offset)
        | (read_byte(offset + 1u) << 8u)
        | (read_byte(offset + 2u) << 16u)
        | (read_byte(offset + 3u) << 24u);
    return bitcast<f32>(bits);
}

fn sample_is_valid(resource_index: u32, sample_index: u32) -> bool {
    let validity_offset = resource_word(resource_index, 8u);
    if validity_offset == 0xffffffffu {
        return true;
    }
    let validity_byte = read_byte(validity_offset + sample_index / 8u);
    return (validity_byte & (1u << (sample_index & 7u))) != 0u;
}

fn sample_grid(layer_index: u32, grid: vec3<f32>) -> SampleResult {
    let layer_shape = vec3<u32>(
        layer_word(layer_index, 1u),
        layer_word(layer_index, 2u),
        layer_word(layer_index, 3u),
    );
    if grid.x < -0.5 || grid.y < -0.5 || grid.z < -0.5
        || grid.x >= f32(layer_shape.x) - 0.5
        || grid.y >= f32(layer_shape.y) - 0.5
        || grid.z >= f32(layer_shape.z) - 0.5 {
        return SampleResult(SAMPLE_OUTSIDE, 0.0);
    }
    let rounded = floor(grid + vec3<f32>(0.5));
    let coordinate = vec3<u32>(
        u32(clamp(rounded.x, 0.0, f32(layer_shape.x - 1u))),
        u32(clamp(rounded.y, 0.0, f32(layer_shape.y - 1u))),
        u32(clamp(rounded.z, 0.0, f32(layer_shape.z - 1u))),
    );
    let layer_key = layer_word(layer_index, 0u);
    let resource_count = control[1u];
    var resource_index = 0u;
    loop {
        if resource_index >= resource_count {
            break;
        }
        if resource_word(resource_index, 0u) == layer_key {
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
            let end = origin + shape;
            if all(coordinate >= origin) && all(coordinate < end) {
                let local = coordinate - origin;
                let sample_index = (local.z * shape.y + local.y) * shape.x + local.x;
                if !sample_is_valid(resource_index, sample_index) {
                    return SampleResult(SAMPLE_INVALID, 0.0);
                }
                return SampleResult(SAMPLE_VALID, sample_value(resource_index, sample_index));
            }
        }
        resource_index += 1u;
    }
    return SampleResult(SAMPLE_MISSING, 0.0);
}

fn world_to_grid(layer_index: u32, world: vec3<f32>) -> vec3<f32> {
    let inverse_scale = vec3<f32>(
        layer_f32(layer_index, 4u),
        layer_f32(layer_index, 5u),
        layer_f32(layer_index, 6u),
    );
    let translation = vec3<f32>(
        layer_f32(layer_index, 7u),
        layer_f32(layer_index, 8u),
        layer_f32(layer_index, 9u),
    );
    return (world - translation) * inverse_scale;
}

fn world_vector_to_grid(layer_index: u32, world: vec3<f32>) -> vec3<f32> {
    return world * vec3<f32>(
        layer_f32(layer_index, 4u),
        layer_f32(layer_index, 5u),
        layer_f32(layer_index, 6u),
    );
}

fn transparent_pixel() -> PixelResult {
    return PixelResult(vec3<f32>(0.0), 0.0, 1u, 0u);
}

fn displayed_pixel(layer_index: u32, display: f32, alpha_value: f32) -> PixelResult {
    let alpha = clamp(alpha_value, 0.0, 1.0);
    let color = vec3<f32>(
        layer_f32(layer_index, 12u),
        layer_f32(layer_index, 13u),
        layer_f32(layer_index, 14u),
    );
    return PixelResult(color * display * alpha, alpha, 1u, 1u);
}

fn composite(under: PixelResult, over: PixelResult) -> PixelResult {
    let remaining = 1.0 - under.alpha;
    return PixelResult(
        under.premultiplied_rgb + over.premultiplied_rgb * remaining,
        under.alpha + over.alpha * remaining,
        under.covered & over.covered,
        under.valid | over.valid,
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
    var entry = -3.402823e38;
    var exit = 3.402823e38;
    var axis = 0u;
    loop {
        if axis >= 3u {
            break;
        }
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
        axis += 1u;
    }
    return vec2<f32>(entry, exit);
}

fn render_mip(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var maximum = 0.0;
    var has_value = false;
    var covered = 1u;
    var index = 0u;
    loop {
        if index >= count || index >= MAX_RAY_SAMPLES {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        let sample = sample_grid(layer_index, origin + direction * distance);
        if sample.kind == SAMPLE_VALID {
            if !has_value || sample.value > maximum {
                maximum = sample.value;
            }
            has_value = true;
        } else if sample.kind == SAMPLE_MISSING {
            covered = 0u;
        }
        index += 1u;
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

fn render_dvr(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var result = transparent_pixel();
    var any_valid = false;
    var index = 0u;
    loop {
        if index >= count || index >= MAX_RAY_SAMPLES {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        let sample = sample_grid(layer_index, origin + direction * distance);
        if sample.kind == SAMPLE_VALID {
            any_valid = true;
            let opacity_display = curve_value(
                sample.value,
                layer_f32(layer_index, 20u),
                layer_f32(layer_index, 21u),
                layer_f32(layer_index, 22u),
                0u,
            );
            let sample_alpha = (1.0 - exp(
                -opacity_display * layer_f32(layer_index, 23u) * step,
            )) * layer_f32(layer_index, 15u);
            result = composite(
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
    result.valid = select(0u, 1u, any_valid);
    return result;
}

fn render_iso(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var covered = 1u;
    var any_valid = false;
    var index = 0u;
    loop {
        if index >= count || index >= MAX_RAY_SAMPLES {
            break;
        }
        let distance = entry + (f32(index) + 0.5) * step;
        let sample = sample_grid(layer_index, origin + direction * distance);
        if sample.kind == SAMPLE_VALID {
            any_valid = true;
            let display = transfer_value(layer_index, sample.value);
            if display >= layer_f32(layer_index, 19u) {
                var result = displayed_pixel(
                    layer_index,
                    display,
                    layer_f32(layer_index, 15u),
                );
                result.covered = covered;
                return result;
            }
        } else if sample.kind == SAMPLE_MISSING {
            covered = 0u;
        }
        index += 1u;
    }
    var result = transparent_pixel();
    result.covered = covered;
    result.valid = select(0u, 1u, any_valid);
    return result;
}

fn render_volume_layer(
    layer_index: u32,
    world_origin: vec3<f32>,
    world_direction: vec3<f32>,
) -> PixelResult {
    let origin = world_to_grid(layer_index, world_origin);
    let direction = world_vector_to_grid(layer_index, world_direction);
    let shape = vec3<u32>(
        layer_word(layer_index, 1u),
        layer_word(layer_index, 2u),
        layer_word(layer_index, 3u),
    );
    let interval = intersect_grid(origin, direction, shape);
    let entry = max(interval.x, 0.0);
    if interval.y <= entry {
        return transparent_pixel();
    }
    let grid_speed = max(abs(direction.x), max(abs(direction.y), abs(direction.z)));
    if grid_speed == 0.0 {
        return transparent_pixel();
    }
    let step = 1.0 / grid_speed;
    let count = max(u32(ceil((interval.y - entry) / step)), 1u);
    let mode = layer_word(layer_index, 18u);
    if mode == 0u {
        return render_mip(layer_index, origin, direction, entry, step, count);
    }
    if mode == 1u {
        return render_dvr(layer_index, origin, direction, entry, step, count);
    }
    return render_iso(layer_index, origin, direction, entry, step, count);
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
    if sample.kind == SAMPLE_MISSING {
        var result = transparent_pixel();
        result.covered = 0u;
        return result;
    }
    return transparent_pixel();
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let position = vec2<f32>(
        f32((vertex_index << 1u) & 2u),
        f32(vertex_index & 2u),
    );
    return vec4<f32>(position * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    var pixel = transparent_pixel();
    let layer_count = control[2u];
    let view_kind = control[3u];
    let pixel_index = position.xy - vec2<f32>(0.5);
    let ray_origin = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u))
        + vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u)) * pixel_index.x
        + vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u)) * pixel_index.y;
    let ray_direction = vec3<f32>(control_f32(17u), control_f32(18u), control_f32(19u));
    var layer_index = 0u;
    loop {
        if layer_index >= layer_count {
            break;
        }
        var layer_pixel: PixelResult;
        if view_kind == 0u {
            layer_pixel = render_volume_layer(layer_index, ray_origin, ray_direction);
        } else {
            layer_pixel = render_cross_section_layer(layer_index, position.xy);
        }
        pixel = composite(pixel, layer_pixel);
        layer_index += 1u;
    }
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}
