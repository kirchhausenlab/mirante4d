pub(crate) const SCENE_RENDER_SHADER: &str = r#"
@group(0) @binding(0)
var<storage, read> base_pixels: array<u32>;

@group(0) @binding(1)
var<storage, read_write> output_pixels: array<u32>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> commands_u32: array<u32>;

@group(0) @binding(4)
var<storage, read> commands_f32: array<f32>;

fn unpack_rgba(value: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(value & 0xffu) / 255.0,
        f32((value >> 8u) & 0xffu) / 255.0,
        f32((value >> 16u) & 0xffu) / 255.0,
        f32((value >> 24u) & 0xffu) / 255.0,
    );
}

fn pack_rgba(color: vec4<f32>) -> u32 {
    let clamped = clamp(color, vec4<f32>(0.0), vec4<f32>(1.0));
    let red = u32(round(clamped.r * 255.0));
    let green = u32(round(clamped.g * 255.0));
    let blue = u32(round(clamped.b * 255.0));
    let alpha = u32(round(clamped.a * 255.0));
    return red | (green << 8u) | (blue << 16u) | (alpha << 24u);
}

fn blend_over(base: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let source_alpha = source.a;
    let inverse_alpha = 1.0 - source_alpha;
    return vec4<f32>(
        source.rgb * source_alpha + base.rgb * inverse_alpha,
        source_alpha + base.a * inverse_alpha,
    );
}

fn command_u32(command_index: u32, field: u32) -> u32 {
    return commands_u32[command_index * 4u + field];
}

fn command_f32(command_index: u32, field: u32) -> f32 {
    return commands_f32[command_index * 6u + field];
}

fn distance_to_segment_squared(point: vec2<f32>, start: vec2<f32>, end: vec2<f32>) -> f32 {
    let segment = end - start;
    let length_squared = dot(segment, segment);
    if (length_squared <= 0.000001) {
        let delta = point - start;
        return dot(delta, delta);
    }
    let t = clamp(dot(point - start, segment) / length_squared, 0.0, 1.0);
    let closest = start + segment * t;
    let delta = point - closest;
    return dot(delta, delta);
}

fn command_contains(command_index: u32, pixel: vec2<f32>) -> bool {
    let kind = command_u32(command_index, 0u);
    let x0 = command_f32(command_index, 0u);
    let y0 = command_f32(command_index, 1u);
    let x1 = command_f32(command_index, 2u);
    let y1 = command_f32(command_index, 3u);
    let width = command_f32(command_index, 4u);

    if (kind == 0u) {
        let delta = pixel - vec2<f32>(x0, y0);
        return dot(delta, delta) <= width * width;
    }
    if (kind == 1u) {
        let half_width = max(width * 0.5, 0.5);
        return distance_to_segment_squared(pixel, vec2<f32>(x0, y0), vec2<f32>(x1, y1)) <=
            half_width * half_width;
    }
    if (kind == 2u) {
        return pixel.x >= min(x0, x1) &&
            pixel.x <= max(x0, x1) &&
            pixel.y >= min(y0, y1) &&
            pixel.y <= max(y0, y1);
    }
    return false;
}

@compute @workgroup_size(8, 8, 1)
fn scene_render_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel_x = id.x;
    let pixel_y = id.y;
    let width = params_u32[0];
    let height = params_u32[1];
    let command_count = params_u32[2];
    if (pixel_x >= width || pixel_y >= height) {
        return;
    }

    let pixel_index = pixel_y * width + pixel_x;
    let pixel_center = vec2<f32>(f32(pixel_x) + 0.5, f32(pixel_y) + 0.5);
    var color = unpack_rgba(base_pixels[pixel_index]);

    var command_index = 0u;
    loop {
        if (command_index >= command_count) {
            break;
        }
        if (command_contains(command_index, pixel_center)) {
            color = blend_over(color, unpack_rgba(command_u32(command_index, 2u)));
        }
        command_index = command_index + 1u;
    }

    output_pixels[pixel_index] = pack_rgba(color);
}
"#;

pub(crate) const SCENE_RENDER_TEXTURE_SHADER: &str = r#"
@group(0) @binding(0)
var base_texture: texture_2d<f32>;

@group(0) @binding(1)
var output_texture: texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> commands_u32: array<u32>;

@group(0) @binding(4)
var<storage, read> commands_f32: array<f32>;

fn unpack_rgba(value: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(value & 0xffu) / 255.0,
        f32((value >> 8u) & 0xffu) / 255.0,
        f32((value >> 16u) & 0xffu) / 255.0,
        f32((value >> 24u) & 0xffu) / 255.0,
    );
}

fn blend_over(base: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let source_alpha = source.a;
    let inverse_alpha = 1.0 - source_alpha;
    return vec4<f32>(
        source.rgb * source_alpha + base.rgb * inverse_alpha,
        source_alpha + base.a * inverse_alpha,
    );
}

fn command_u32(command_index: u32, field: u32) -> u32 {
    return commands_u32[command_index * 4u + field];
}

fn command_f32(command_index: u32, field: u32) -> f32 {
    return commands_f32[command_index * 6u + field];
}

fn distance_to_segment_squared(point: vec2<f32>, start: vec2<f32>, end: vec2<f32>) -> f32 {
    let segment = end - start;
    let length_squared = dot(segment, segment);
    if (length_squared <= 0.000001) {
        let delta = point - start;
        return dot(delta, delta);
    }
    let t = clamp(dot(point - start, segment) / length_squared, 0.0, 1.0);
    let closest = start + segment * t;
    let delta = point - closest;
    return dot(delta, delta);
}

fn command_contains(command_index: u32, pixel: vec2<f32>) -> bool {
    let kind = command_u32(command_index, 0u);
    let x0 = command_f32(command_index, 0u);
    let y0 = command_f32(command_index, 1u);
    let x1 = command_f32(command_index, 2u);
    let y1 = command_f32(command_index, 3u);
    let width = command_f32(command_index, 4u);

    if (kind == 0u) {
        let delta = pixel - vec2<f32>(x0, y0);
        return dot(delta, delta) <= width * width;
    }
    if (kind == 1u) {
        let half_width = max(width * 0.5, 0.5);
        return distance_to_segment_squared(pixel, vec2<f32>(x0, y0), vec2<f32>(x1, y1)) <=
            half_width * half_width;
    }
    if (kind == 2u) {
        return pixel.x >= min(x0, x1) &&
            pixel.x <= max(x0, x1) &&
            pixel.y >= min(y0, y1) &&
            pixel.y <= max(y0, y1);
    }
    return false;
}

@compute @workgroup_size(8, 8, 1)
fn scene_render_texture_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel_x = id.x;
    let pixel_y = id.y;
    let width = params_u32[0];
    let height = params_u32[1];
    let command_count = params_u32[2];
    if (pixel_x >= width || pixel_y >= height) {
        return;
    }

    let pixel_center = vec2<f32>(f32(pixel_x) + 0.5, f32(pixel_y) + 0.5);
    var color = textureLoad(base_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), 0);

    var command_index = 0u;
    loop {
        if (command_index >= command_count) {
            break;
        }
        if (command_contains(command_index, pixel_center)) {
            color = blend_over(color, unpack_rgba(command_u32(command_index, 2u)));
        }
        command_index = command_index + 1u;
    }

    textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), color);
}
"#;

pub(crate) const SCENE_PICK_SHADER: &str = r#"
@group(0) @binding(0)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(1)
var<storage, read> params_f32: array<f32>;

@group(0) @binding(2)
var<storage, read> commands_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> commands_f32: array<f32>;

@group(0) @binding(4)
var<storage, read_write> output_pick_id: array<u32>;

fn command_u32(command_index: u32, field: u32) -> u32 {
    return commands_u32[command_index * 4u + field];
}

fn command_f32(command_index: u32, field: u32) -> f32 {
    return commands_f32[command_index * 6u + field];
}

fn distance_to_segment_squared(point: vec2<f32>, start: vec2<f32>, end: vec2<f32>) -> f32 {
    let segment = end - start;
    let length_squared = dot(segment, segment);
    if (length_squared <= 0.000001) {
        let delta = point - start;
        return dot(delta, delta);
    }
    let t = clamp(dot(point - start, segment) / length_squared, 0.0, 1.0);
    let closest = start + segment * t;
    let delta = point - closest;
    return dot(delta, delta);
}

fn command_contains(command_index: u32, pixel: vec2<f32>) -> bool {
    let kind = command_u32(command_index, 0u);
    let x0 = command_f32(command_index, 0u);
    let y0 = command_f32(command_index, 1u);
    let x1 = command_f32(command_index, 2u);
    let y1 = command_f32(command_index, 3u);
    let width = command_f32(command_index, 4u);

    if (kind == 0u) {
        let delta = pixel - vec2<f32>(x0, y0);
        return dot(delta, delta) <= width * width;
    }
    if (kind == 1u) {
        let half_width = max(width * 0.5, 0.5);
        return distance_to_segment_squared(pixel, vec2<f32>(x0, y0), vec2<f32>(x1, y1)) <=
            half_width * half_width;
    }
    if (kind == 2u) {
        return pixel.x >= min(x0, x1) &&
            pixel.x <= max(x0, x1) &&
            pixel.y >= min(y0, y1) &&
            pixel.y <= max(y0, y1);
    }
    return false;
}

@compute @workgroup_size(1, 1, 1)
fn scene_pick_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x != 0u || id.y != 0u || id.z != 0u) {
        return;
    }
    let command_count = params_u32[0];
    let pixel = vec2<f32>(params_f32[0], params_f32[1]);
    output_pick_id[0] = 0u;

    var reverse_index = command_count;
    loop {
        if (reverse_index == 0u) {
            break;
        }
        reverse_index = reverse_index - 1u;
        let pick_id = command_u32(reverse_index, 3u);
        if (pick_id != 0u && command_contains(reverse_index, pixel)) {
            output_pick_id[0] = pick_id;
            return;
        }
    }
}
"#;
