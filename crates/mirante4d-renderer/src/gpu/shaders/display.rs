pub(crate) const DISPLAY_COMPOSITE_SHADER: &str = r#"
const OUTPUT_COVERED_FLAG: u32 = 0x80000000u;
const EPSILON: f32 = 0.00001;
const ISO_AMBIENT: f32 = 0.20;
const ISO_DIFFUSE: f32 = 0.80;
const ISO_SPECULAR: f32 = 0.25;
const ISO_SHININESS: f32 = 48.0;

struct CameraOutputU32 {
    packed_value: u32,
    source_material: u32,
    depth_bits: u32,
    normal_xy: u32,
    normal_z_diffuse: u32,
    specular: u32,
};

@group(0) @binding(0)
var<storage, read> output_pixels: array<CameraOutputU32>;

@group(0) @binding(1)
var<storage, read_write> accumulator: array<vec4<f32>>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> params_f32: array<f32>;

@group(0) @binding(0)
var<storage, read> final_accumulator: array<vec4<f32>>;

@group(0) @binding(1)
var output_texture: texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(2)
var<storage, read> final_params_u32: array<u32>;

fn decode_low_u16(word: u32) -> u32 {
    return word & 0xffffu;
}

fn decode_high_u16(word: u32) -> u32 {
    return (word >> 16u) & 0xffffu;
}

fn decode_i16_component(word: u32) -> f32 {
    let low = word & 0xffffu;
    var signed = i32(low);
    if (low >= 32768u) {
        signed = signed - 65536;
    }
    return clamp(f32(signed) / 32767.0, -1.0, 1.0);
}

fn normalize_or_zero(value: vec3<f32>) -> vec3<f32> {
    let len = length(value);
    if (len <= EPSILON) {
        return vec3<f32>(0.0);
    }
    return value / len;
}

fn output_covered(packed: u32) -> bool {
    return (packed & OUTPUT_COVERED_FLAG) != 0u;
}

fn output_value(packed: u32) -> f32 {
    return f32(packed & 0xffffu);
}

fn transfer_value(value: f32) -> f32 {
    let low = params_f32[0];
    let high = params_f32[1];
    let gamma = max(params_f32[2], EPSILON);
    var mapped = 0.0;
    if (high > low) {
        mapped = clamp((value - low) / (high - low), 0.0, 1.0);
    }
    mapped = pow(mapped, 1.0 / gamma);
    if (params_u32[4] != 0u) {
        mapped = 1.0 - mapped;
    }
    return clamp(mapped, 0.0, 1.0);
}

fn channel_alpha() -> f32 {
    return clamp(params_f32[3], 0.0, 1.0) * clamp(params_f32[7], 0.0, 1.0);
}

fn source_over(dst: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let inverse_alpha = 1.0 - clamp(source.a, 0.0, 1.0);
    return vec4<f32>(
        source.r + dst.r * inverse_alpha,
        source.g + dst.g * inverse_alpha,
        source.b + dst.b * inverse_alpha,
        source.a + dst.a * inverse_alpha
    );
}

fn composite_additive_intensity(record: CameraOutputU32, current: vec4<f32>) -> vec4<f32> {
    if (!output_covered(record.packed_value)) {
        return current;
    }
    let alpha = channel_alpha();
    if (alpha <= EPSILON) {
        return current;
    }
    let mapped = transfer_value(output_value(record.packed_value));
    let red = clamp(params_f32[4], 0.0, 1.0);
    let green = clamp(params_f32[5], 0.0, 1.0);
    let blue = clamp(params_f32[6], 0.0, 1.0);
    return vec4<f32>(
        clamp(current.r + mapped * red * alpha, 0.0, 1.0),
        clamp(current.g + mapped * green * alpha, 0.0, 1.0),
        clamp(current.b + mapped * blue * alpha, 0.0, 1.0),
        1.0 - (1.0 - current.a) * (1.0 - alpha)
    );
}

fn iso_normal(record: CameraOutputU32) -> vec3<f32> {
    return normalize_or_zero(vec3<f32>(
        decode_i16_component(record.normal_xy),
        decode_i16_component(record.normal_xy >> 16u),
        decode_i16_component(record.normal_z_diffuse)
    ));
}

fn iso_view_world(pixel_x: u32, pixel_y: u32) -> vec3<f32> {
    let forward = normalize_or_zero(vec3<f32>(params_f32[11], params_f32[12], params_f32[13]));
    let right = normalize_or_zero(vec3<f32>(params_f32[14], params_f32[15], params_f32[16]));
    let up = normalize_or_zero(vec3<f32>(params_f32[17], params_f32[18], params_f32[19]));
    if (params_u32[5] == 0u) {
        let width = max(f32(params_u32[0]), 1.0);
        let height = max(f32(params_u32[1]), 1.0);
        let focal = max(params_f32[20], EPSILON);
        let screen_x_points = ((f32(pixel_x) + 0.5) / width - 0.5) * params_f32[21];
        let screen_y_points = (0.5 - (f32(pixel_y) + 0.5) / height) * params_f32[22];
        return normalize_or_zero(-(
            forward +
            right * (screen_x_points / focal) +
            up * (screen_y_points / focal)
        ));
    }
    return normalize_or_zero(-forward);
}

fn visible_iso_normal(normal: vec3<f32>, view: vec3<f32>) -> vec3<f32> {
    if (dot(normal, view) < 0.0) {
        return -normal;
    }
    return normal;
}

fn composite_iso(record: CameraOutputU32, current: vec4<f32>, pixel_x: u32, pixel_y: u32) -> vec4<f32> {
    if (!output_covered(record.packed_value)) {
        return current;
    }
    let alpha = channel_alpha();
    if (alpha <= EPSILON) {
        return current;
    }
    let material = f32(decode_high_u16(record.source_material)) / 65535.0;
    let normal = iso_normal(record);
    var diffuse = 1.0;
    var specular = 0.0;
    if (dot(normal, normal) > EPSILON) {
        let light = normalize_or_zero(vec3<f32>(params_f32[8], params_f32[9], params_f32[10]));
        let view = iso_view_world(pixel_x, pixel_y);
        if (dot(light, light) > EPSILON && dot(view, view) > EPSILON) {
            let shading_normal = visible_iso_normal(normal, view);
            diffuse = clamp(ISO_AMBIENT + ISO_DIFFUSE * max(dot(shading_normal, light), 0.0), 0.0, 1.0);
            let half_vector = normalize_or_zero(light + view);
            if (dot(half_vector, half_vector) > EPSILON) {
                specular = ISO_SPECULAR * pow(max(dot(shading_normal, half_vector), 0.0), ISO_SHININESS);
            }
        }
    }
    let lit = clamp(material, 0.0, 1.0) * diffuse;
    let source = vec4<f32>(
        clamp(lit * clamp(params_f32[4], 0.0, 1.0) + specular, 0.0, 1.0) * alpha,
        clamp(lit * clamp(params_f32[5], 0.0, 1.0) + specular, 0.0, 1.0) * alpha,
        clamp(lit * clamp(params_f32[6], 0.0, 1.0) + specular, 0.0, 1.0) * alpha,
        alpha
    );
    return source_over(current, source);
}

fn composite_dvr(record: CameraOutputU32, current: vec4<f32>) -> vec4<f32> {
    if (!output_covered(record.packed_value)) {
        return current;
    }
    let source = vec4<f32>(
        clamp(bitcast<f32>(record.source_material), 0.0, 1.0),
        clamp(bitcast<f32>(record.depth_bits), 0.0, 1.0),
        clamp(bitcast<f32>(record.normal_xy), 0.0, 1.0),
        clamp(bitcast<f32>(record.normal_z_diffuse), 0.0, 1.0)
    );
    return source_over(current, source);
}

@compute @workgroup_size(8, 8, 1)
fn display_composite_channel_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let width = params_u32[0];
    let height = params_u32[1];
    if (id.x >= width || id.y >= height) {
        return;
    }
    let index = id.y * width + id.x;
    if (params_u32[3] == 0u) {
        return;
    }
    let record = output_pixels[index];
    let mode = params_u32[2];
    let current = accumulator[index];
    if (mode == 1u) {
        accumulator[index] = composite_iso(record, current, id.x, id.y);
    } else if (mode == 2u) {
        accumulator[index] = composite_dvr(record, current);
    } else {
        accumulator[index] = composite_additive_intensity(record, current);
    }
}

fn display_rgba(premultiplied: vec4<f32>) -> vec4<f32> {
    let alpha = max(
        clamp(premultiplied.a, 0.0, 1.0),
        clamp(max(premultiplied.r, max(premultiplied.g, premultiplied.b)), 0.0, 1.0)
    );
    if (alpha <= EPSILON) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(
        clamp(premultiplied.r / alpha, 0.0, 1.0),
        clamp(premultiplied.g / alpha, 0.0, 1.0),
        clamp(premultiplied.b / alpha, 0.0, 1.0),
        alpha
    );
}

@compute @workgroup_size(8, 8, 1)
fn display_finalize_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let width = final_params_u32[0];
    let height = final_params_u32[1];
    if (id.x >= width || id.y >= height) {
        return;
    }
    let index = id.y * width + id.x;
    textureStore(output_texture, vec2<i32>(i32(id.x), i32(id.y)), display_rgba(final_accumulator[index]));
}
"#;

pub(crate) const DISPLAY_FRAME_BLEND_SHADER: &str = r#"
@group(0) @binding(0)
var base_texture: texture_2d<f32>;

@group(0) @binding(1)
var overlay_texture: texture_2d<f32>;

@group(0) @binding(2)
var output_texture: texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(3)
var<storage, read> params_u32: array<u32>;

fn source_over_display_frame(base: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(source.a, 0.0, 1.0);
    let inverse_alpha = 1.0 - alpha;
    return vec4<f32>(
        clamp(source.r * alpha + base.r * inverse_alpha, 0.0, 1.0),
        clamp(source.g * alpha + base.g * inverse_alpha, 0.0, 1.0),
        clamp(source.b * alpha + base.b * inverse_alpha, 0.0, 1.0),
        clamp(alpha + base.a * inverse_alpha, 0.0, 1.0)
    );
}

fn additive_display_frame(base: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let source_alpha = clamp(source.a, 0.0, 1.0);
    let base_alpha = clamp(base.a, 0.0, 1.0);
    return vec4<f32>(
        clamp(base.r + source.r * source_alpha, 0.0, 1.0),
        clamp(base.g + source.g * source_alpha, 0.0, 1.0),
        clamp(base.b + source.b * source_alpha, 0.0, 1.0),
        clamp(1.0 - (1.0 - base_alpha) * (1.0 - source_alpha), 0.0, 1.0)
    );
}

@compute @workgroup_size(8, 8, 1)
fn display_frame_blend_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let width = params_u32[0];
    let height = params_u32[1];
    if (id.x >= width || id.y >= height) {
        return;
    }
    let coord = vec2<i32>(i32(id.x), i32(id.y));
    let base = textureLoad(base_texture, coord, 0);
    let overlay = textureLoad(overlay_texture, coord, 0);
    let mode = params_u32[2];
    var out = additive_display_frame(base, overlay);
    if (mode == 1u) {
        out = source_over_display_frame(base, overlay);
    }
    textureStore(output_texture, coord, out);
}
"#;

pub(crate) const DISPLAY_COMPOSITE_F32_SHADER: &str = r#"
const EPSILON: f32 = 0.00001;
const ISO_AMBIENT: f32 = 0.20;
const ISO_DIFFUSE: f32 = 0.80;
const ISO_SPECULAR: f32 = 0.25;
const ISO_SHININESS: f32 = 48.0;

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
var<storage, read> output_pixels: array<CameraOutputF32>;

@group(0) @binding(1)
var<storage, read_write> accumulator: array<vec4<f32>>;

@group(0) @binding(2)
var<storage, read> params_u32: array<u32>;

@group(0) @binding(3)
var<storage, read> params_f32: array<f32>;

fn decode_low_u16(word: u32) -> u32 {
    return word & 0xffffu;
}

fn decode_i16_component(word: u32) -> f32 {
    let low = word & 0xffffu;
    var signed = i32(low);
    if (low >= 32768u) {
        signed = signed - 65536;
    }
    return clamp(f32(signed) / 32767.0, -1.0, 1.0);
}

fn normalize_or_zero(value: vec3<f32>) -> vec3<f32> {
    let len = length(value);
    if (len <= EPSILON) {
        return vec3<f32>(0.0);
    }
    return value / len;
}

fn output_covered(marker_bits: u32) -> bool {
    return bitcast<f32>(marker_bits) < 0.0;
}

fn transfer_value(value: f32) -> f32 {
    let low = params_f32[0];
    let high = params_f32[1];
    let gamma = max(params_f32[2], EPSILON);
    var mapped = 0.0;
    if (high > low) {
        mapped = clamp((value - low) / (high - low), 0.0, 1.0);
    }
    mapped = pow(mapped, 1.0 / gamma);
    if (params_u32[4] != 0u) {
        mapped = 1.0 - mapped;
    }
    return clamp(mapped, 0.0, 1.0);
}

fn channel_alpha() -> f32 {
    return clamp(params_f32[3], 0.0, 1.0) * clamp(params_f32[7], 0.0, 1.0);
}

fn source_over(dst: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let inverse_alpha = 1.0 - clamp(source.a, 0.0, 1.0);
    return vec4<f32>(
        source.r + dst.r * inverse_alpha,
        source.g + dst.g * inverse_alpha,
        source.b + dst.b * inverse_alpha,
        source.a + dst.a * inverse_alpha
    );
}

fn composite_additive_intensity(record: CameraOutputF32, current: vec4<f32>) -> vec4<f32> {
    if (!output_covered(record.marker_bits)) {
        return current;
    }
    let alpha = channel_alpha();
    if (alpha <= EPSILON) {
        return current;
    }
    let mapped = transfer_value(bitcast<f32>(record.value_bits));
    let red = clamp(params_f32[4], 0.0, 1.0);
    let green = clamp(params_f32[5], 0.0, 1.0);
    let blue = clamp(params_f32[6], 0.0, 1.0);
    return vec4<f32>(
        clamp(current.r + mapped * red * alpha, 0.0, 1.0),
        clamp(current.g + mapped * green * alpha, 0.0, 1.0),
        clamp(current.b + mapped * blue * alpha, 0.0, 1.0),
        1.0 - (1.0 - current.a) * (1.0 - alpha)
    );
}

fn iso_normal(record: CameraOutputF32) -> vec3<f32> {
    return normalize_or_zero(vec3<f32>(
        decode_i16_component(record.normal_xy),
        decode_i16_component(record.normal_xy >> 16u),
        decode_i16_component(record.normal_z_diffuse)
    ));
}

fn iso_view_world(pixel_x: u32, pixel_y: u32) -> vec3<f32> {
    let forward = normalize_or_zero(vec3<f32>(params_f32[11], params_f32[12], params_f32[13]));
    let right = normalize_or_zero(vec3<f32>(params_f32[14], params_f32[15], params_f32[16]));
    let up = normalize_or_zero(vec3<f32>(params_f32[17], params_f32[18], params_f32[19]));
    if (params_u32[5] == 0u) {
        let width = max(f32(params_u32[0]), 1.0);
        let height = max(f32(params_u32[1]), 1.0);
        let focal = max(params_f32[20], EPSILON);
        let screen_x_points = ((f32(pixel_x) + 0.5) / width - 0.5) * params_f32[21];
        let screen_y_points = (0.5 - (f32(pixel_y) + 0.5) / height) * params_f32[22];
        return normalize_or_zero(-(
            forward +
            right * (screen_x_points / focal) +
            up * (screen_y_points / focal)
        ));
    }
    return normalize_or_zero(-forward);
}

fn visible_iso_normal(normal: vec3<f32>, view: vec3<f32>) -> vec3<f32> {
    if (dot(normal, view) < 0.0) {
        return -normal;
    }
    return normal;
}

fn composite_iso(record: CameraOutputF32, current: vec4<f32>, pixel_x: u32, pixel_y: u32) -> vec4<f32> {
    if (!output_covered(record.marker_bits)) {
        return current;
    }
    let alpha = channel_alpha();
    if (alpha <= EPSILON) {
        return current;
    }
    let material = clamp(bitcast<f32>(record.material_bits), 0.0, 1.0);
    let normal = iso_normal(record);
    var diffuse = 1.0;
    var specular = 0.0;
    if (dot(normal, normal) > EPSILON) {
        let light = normalize_or_zero(vec3<f32>(params_f32[8], params_f32[9], params_f32[10]));
        let view = iso_view_world(pixel_x, pixel_y);
        if (dot(light, light) > EPSILON && dot(view, view) > EPSILON) {
            let shading_normal = visible_iso_normal(normal, view);
            diffuse = clamp(ISO_AMBIENT + ISO_DIFFUSE * max(dot(shading_normal, light), 0.0), 0.0, 1.0);
            let half_vector = normalize_or_zero(light + view);
            if (dot(half_vector, half_vector) > EPSILON) {
                specular = ISO_SPECULAR * pow(max(dot(shading_normal, half_vector), 0.0), ISO_SHININESS);
            }
        }
    }
    let lit = material * diffuse;
    let source = vec4<f32>(
        clamp(lit * clamp(params_f32[4], 0.0, 1.0) + specular, 0.0, 1.0) * alpha,
        clamp(lit * clamp(params_f32[5], 0.0, 1.0) + specular, 0.0, 1.0) * alpha,
        clamp(lit * clamp(params_f32[6], 0.0, 1.0) + specular, 0.0, 1.0) * alpha,
        alpha
    );
    return source_over(current, source);
}

fn composite_dvr(record: CameraOutputF32, current: vec4<f32>) -> vec4<f32> {
    if (!output_covered(record.marker_bits)) {
        return current;
    }
    let source = vec4<f32>(
        clamp(bitcast<f32>(record.source_bits), 0.0, 1.0),
        clamp(bitcast<f32>(record.material_bits), 0.0, 1.0),
        clamp(bitcast<f32>(record.depth_bits), 0.0, 1.0),
        clamp(bitcast<f32>(record.normal_xy), 0.0, 1.0)
    );
    return source_over(current, source);
}

@compute @workgroup_size(8, 8, 1)
fn display_composite_f32_channel_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let width = params_u32[0];
    let height = params_u32[1];
    if (id.x >= width || id.y >= height) {
        return;
    }
    let index = id.y * width + id.x;
    if (params_u32[3] == 0u) {
        return;
    }
    let record = output_pixels[index];
    let mode = params_u32[2];
    let current = accumulator[index];
    if (mode == 1u) {
        accumulator[index] = composite_iso(record, current, id.x, id.y);
    } else if (mode == 2u) {
        accumulator[index] = composite_dvr(record, current);
    } else {
        accumulator[index] = composite_additive_intensity(record, current);
    }
}
"#;

pub(crate) const DISPLAY_ISO_MULTI_CHANNEL_SHADER: &str = r#"
const OUTPUT_COVERED_FLAG: u32 = 0x80000000u;
const EPSILON: f32 = 0.00001;
const BIG_DEPTH: f32 = 1.0e30;
const CHANNEL_U32_STRIDE: u32 = 5u;
const CHANNEL_F32_STRIDE: u32 = 5u;
const RECORD_DTYPE_INTEGER: u32 = 0u;
const RECORD_DTYPE_F32: u32 = 1u;
const ISO_AMBIENT: f32 = 0.20;
const ISO_DIFFUSE: f32 = 0.80;
const ISO_SPECULAR: f32 = 0.25;
const ISO_SHININESS: f32 = 48.0;

@group(0) @binding(0)
var<storage, read> output_words: array<u32>;

@group(0) @binding(1)
var<storage, read> channel_params_u32: array<u32>;

@group(0) @binding(2)
var<storage, read> channel_params_f32: array<f32>;

@group(0) @binding(3)
var<storage, read> frame_params_u32: array<u32>;

@group(0) @binding(4)
var<storage, read> frame_params_f32: array<f32>;

@group(0) @binding(5)
var output_texture: texture_storage_2d<rgba8unorm, write>;

struct IsoRecord {
    covered: bool,
    depth: f32,
    material: f32,
    normal: vec3<f32>,
};

fn channel_u32(channel_index: u32, field: u32) -> u32 {
    return channel_params_u32[channel_index * CHANNEL_U32_STRIDE + field];
}

fn channel_f32(channel_index: u32, field: u32) -> f32 {
    return channel_params_f32[channel_index * CHANNEL_F32_STRIDE + field];
}

fn decode_high_u16(word: u32) -> u32 {
    return (word >> 16u) & 0xffffu;
}

fn decode_i16_component(word: u32) -> f32 {
    let low = word & 0xffffu;
    var signed = i32(low);
    if (low >= 32768u) {
        signed = signed - 65536;
    }
    return clamp(f32(signed) / 32767.0, -1.0, 1.0);
}

fn normalize_or_zero(value: vec3<f32>) -> vec3<f32> {
    let len = length(value);
    if (len <= EPSILON) {
        return vec3<f32>(0.0);
    }
    return value / len;
}

fn source_over(dst: vec4<f32>, source: vec4<f32>) -> vec4<f32> {
    let inverse_alpha = 1.0 - clamp(source.a, 0.0, 1.0);
    return vec4<f32>(
        source.r + dst.r * inverse_alpha,
        source.g + dst.g * inverse_alpha,
        source.b + dst.b * inverse_alpha,
        source.a + dst.a * inverse_alpha
    );
}

fn display_rgba(premultiplied: vec4<f32>) -> vec4<f32> {
    let alpha = max(
        clamp(premultiplied.a, 0.0, 1.0),
        clamp(max(premultiplied.r, max(premultiplied.g, premultiplied.b)), 0.0, 1.0)
    );
    if (alpha <= EPSILON) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(
        clamp(premultiplied.r / alpha, 0.0, 1.0),
        clamp(premultiplied.g / alpha, 0.0, 1.0),
        clamp(premultiplied.b / alpha, 0.0, 1.0),
        alpha
    );
}

fn channel_alpha(channel_index: u32) -> f32 {
    return clamp(channel_f32(channel_index, 0u), 0.0, 1.0) *
        clamp(channel_f32(channel_index, 4u), 0.0, 1.0);
}

fn decode_integer_iso_record(channel_index: u32, pixel_index: u32) -> IsoRecord {
    let offset = channel_u32(channel_index, 3u);
    let stride = channel_u32(channel_index, 4u);
    let base = offset + pixel_index * stride;
    let packed_value = output_words[base + 0u];
    let source_material = output_words[base + 1u];
    let normal_xy = output_words[base + 3u];
    let normal_z_diffuse = output_words[base + 4u];
    return IsoRecord(
        (packed_value & OUTPUT_COVERED_FLAG) != 0u,
        bitcast<f32>(output_words[base + 2u]),
        f32(decode_high_u16(source_material)) / 65535.0,
        normalize_or_zero(vec3<f32>(
            decode_i16_component(normal_xy),
            decode_i16_component(normal_xy >> 16u),
            decode_i16_component(normal_z_diffuse)
        ))
    );
}

fn decode_f32_iso_record(channel_index: u32, pixel_index: u32) -> IsoRecord {
    let offset = channel_u32(channel_index, 3u);
    let stride = channel_u32(channel_index, 4u);
    let base = offset + pixel_index * stride;
    let marker = bitcast<f32>(output_words[base + 1u]);
    let normal_xy = output_words[base + 5u];
    let normal_z_diffuse = output_words[base + 6u];
    return IsoRecord(
        marker < 0.0,
        bitcast<f32>(output_words[base + 4u]),
        clamp(bitcast<f32>(output_words[base + 3u]), 0.0, 1.0),
        normalize_or_zero(vec3<f32>(
            decode_i16_component(normal_xy),
            decode_i16_component(normal_xy >> 16u),
            decode_i16_component(normal_z_diffuse)
        ))
    );
}

fn decode_iso_record(channel_index: u32, pixel_index: u32) -> IsoRecord {
    if (channel_u32(channel_index, 0u) == RECORD_DTYPE_F32) {
        return decode_f32_iso_record(channel_index, pixel_index);
    }
    return decode_integer_iso_record(channel_index, pixel_index);
}

fn channel_visible(channel_index: u32, record: IsoRecord) -> bool {
    return channel_u32(channel_index, 1u) != 0u && record.covered && channel_alpha(channel_index) > EPSILON;
}

fn iso_view_world(pixel_x: u32, pixel_y: u32) -> vec3<f32> {
    let forward = normalize_or_zero(vec3<f32>(frame_params_f32[3], frame_params_f32[4], frame_params_f32[5]));
    let right = normalize_or_zero(vec3<f32>(frame_params_f32[6], frame_params_f32[7], frame_params_f32[8]));
    let up = normalize_or_zero(vec3<f32>(frame_params_f32[9], frame_params_f32[10], frame_params_f32[11]));
    if (frame_params_u32[3] == 0u) {
        let width = max(f32(frame_params_u32[0]), 1.0);
        let height = max(f32(frame_params_u32[1]), 1.0);
        let focal = max(frame_params_f32[12], EPSILON);
        let screen_x_points = ((f32(pixel_x) + 0.5) / width - 0.5) * frame_params_f32[13];
        let screen_y_points = (0.5 - (f32(pixel_y) + 0.5) / height) * frame_params_f32[14];
        return normalize_or_zero(-(
            forward +
            right * (screen_x_points / focal) +
            up * (screen_y_points / focal)
        ));
    }
    return normalize_or_zero(-forward);
}

fn visible_iso_normal(normal: vec3<f32>, view: vec3<f32>) -> vec3<f32> {
    if (dot(normal, view) < 0.0) {
        return -normal;
    }
    return normal;
}

fn iso_lighting(normal: vec3<f32>, pixel_x: u32, pixel_y: u32) -> vec2<f32> {
    var diffuse = 1.0;
    var specular = 0.0;
    if (dot(normal, normal) > EPSILON) {
        let light = normalize_or_zero(vec3<f32>(frame_params_f32[0], frame_params_f32[1], frame_params_f32[2]));
        let view = iso_view_world(pixel_x, pixel_y);
        if (dot(light, light) > EPSILON && dot(view, view) > EPSILON) {
            let shading_normal = visible_iso_normal(normal, view);
            diffuse = clamp(ISO_AMBIENT + ISO_DIFFUSE * max(dot(shading_normal, light), 0.0), 0.0, 1.0);
            let half_vector = normalize_or_zero(light + view);
            if (dot(half_vector, half_vector) > EPSILON) {
                specular = ISO_SPECULAR * pow(max(dot(shading_normal, half_vector), 0.0), ISO_SHININESS);
            }
        }
    }
    return vec2<f32>(diffuse, specular);
}

fn iso_source_rgba(channel_index: u32, record: IsoRecord, pixel_x: u32, pixel_y: u32) -> vec4<f32> {
    let alpha = channel_alpha(channel_index);
    let light = iso_lighting(record.normal, pixel_x, pixel_y);
    let lit = clamp(record.material, 0.0, 1.0) * light.x;
    let red = clamp(channel_f32(channel_index, 1u), 0.0, 1.0);
    let green = clamp(channel_f32(channel_index, 2u), 0.0, 1.0);
    let blue = clamp(channel_f32(channel_index, 3u), 0.0, 1.0);
    let specular = light.y;
    return vec4<f32>(
        clamp(lit * red + specular, 0.0, 1.0) * alpha,
        clamp(lit * green + specular, 0.0, 1.0) * alpha,
        clamp(lit * blue + specular, 0.0, 1.0) * alpha,
        alpha
    );
}

fn candidate_after_last(depth: f32, channel_index: u32, have_last: bool, last_depth: f32, last_index: u32) -> bool {
    if (!have_last) {
        return true;
    }
    return depth < last_depth || (depth == last_depth && channel_index > last_index);
}

fn candidate_better(depth: f32, channel_index: u32, have_best: bool, best_depth: f32, best_index: u32) -> bool {
    if (!have_best) {
        return true;
    }
    return depth > best_depth || (depth == best_depth && channel_index < best_index);
}

@compute @workgroup_size(8, 8, 1)
fn display_iso_multi_channel_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let width = frame_params_u32[0];
    let height = frame_params_u32[1];
    if (id.x >= width || id.y >= height) {
        return;
    }
    let pixel_index = id.y * width + id.x;
    let channel_count = frame_params_u32[2];
    var out = vec4<f32>(0.0);
    var have_last = false;
    var last_depth = BIG_DEPTH;
    var last_index = 0u;

    for (var rank = 0u; rank < channel_count; rank = rank + 1u) {
        var have_best = false;
        var best_index = 0u;
        var best_depth = -BIG_DEPTH;
        for (var channel_index = 0u; channel_index < channel_count; channel_index = channel_index + 1u) {
            let record = decode_iso_record(channel_index, pixel_index);
            if (channel_visible(channel_index, record) &&
                candidate_after_last(record.depth, channel_index, have_last, last_depth, last_index) &&
                candidate_better(record.depth, channel_index, have_best, best_depth, best_index)) {
                have_best = true;
                best_index = channel_index;
                best_depth = record.depth;
            }
        }
        if (!have_best) {
            break;
        }
        let best_record = decode_iso_record(best_index, pixel_index);
        out = source_over(out, iso_source_rgba(best_index, best_record, id.x, id.y));
        have_last = true;
        last_depth = best_depth;
        last_index = best_index;
    }

    textureStore(output_texture, vec2<i32>(i32(id.x), i32(id.y)), display_rgba(out));
}
"#;

pub(crate) const DISPLAY_DVR_MULTI_CHANNEL_SHADER: &str = r#"
const EPSILON: f32 = 0.00001;
const BIG_T: f32 = 1.0e30;
const SAMPLE_INVALID_FLAG: u32 = 0x00010000u;
const MISSING_SAMPLE_FLAG: u32 = 0x00020000u;
const SMOOTH_RAY_STEP_VOXELS: f32 = 0.5;
const CHANNEL_U32_STRIDE: u32 = 18u;
const CHANNEL_F32_STRIDE: u32 = 11u;
const BRICK_METADATA_WORDS: u32 = 4u;
const F32_PAGE_TABLE_WORDS: u32 = 7u;
const BRICK_METADATA_HAS_VALID_FLAG: u32 = 0x2u;
const BRICK_METADATA_MIN_MAX_VALID_FLAG: u32 = 0x4u;
const DVR_CHANNEL_DTYPE_U8: u32 = 0u;
const DVR_CHANNEL_DTYPE_U16: u32 = 1u;
const DVR_CHANNEL_DTYPE_F32: u32 = 2u;

@group(0) @binding(0)
var<storage, read> packed_voxels: array<u32>;

@group(0) @binding(1)
var<storage, read> validity_bits: array<u32>;

@group(0) @binding(2)
var<storage, read> page_tables: array<u32>;

@group(0) @binding(3)
var<storage, read> brick_metadata: array<u32>;

@group(0) @binding(4)
var<storage, read> f32_voxels: array<f32>;

@group(0) @binding(5)
var<storage, read> channel_params_u32: array<u32>;

@group(0) @binding(6)
var<storage, read> channel_params_f32: array<f32>;

struct DvrFrameParamsU32 {
    values: array<vec4<u32>, 2>,
};

struct DvrFrameParamsF32 {
    values: array<vec4<f32>, 14>,
};

@group(0) @binding(7)
var<uniform> frame_params_u32: DvrFrameParamsU32;

@group(0) @binding(8)
var<uniform> frame_params_f32: DvrFrameParamsF32;

@group(0) @binding(9)
var output_texture: texture_storage_2d<rgba8unorm, write>;

struct SlabResult {
    hit: bool,
    enter: f32,
    exit: f32,
};

struct InterpAxis {
    valid: bool,
    lower: u32,
    upper: u32,
    fraction: f32,
};

struct LinearSample {
    value: f32,
    valid: bool,
};

fn channel_u32(channel_index: u32, offset: u32) -> u32 {
    return channel_params_u32[channel_index * CHANNEL_U32_STRIDE + offset];
}

fn channel_f32(channel_index: u32, offset: u32) -> f32 {
    return channel_params_f32[channel_index * CHANNEL_F32_STRIDE + offset];
}

fn frame_u32(offset: u32) -> u32 {
    let packed = frame_params_u32.values[offset / 4u];
    let lane = offset % 4u;
    if (lane == 0u) {
        return packed.x;
    }
    if (lane == 1u) {
        return packed.y;
    }
    if (lane == 2u) {
        return packed.z;
    }
    return packed.w;
}

fn frame_f32(offset: u32) -> f32 {
    let packed = frame_params_f32.values[offset / 4u];
    let lane = offset % 4u;
    if (lane == 0u) {
        return packed.x;
    }
    if (lane == 1u) {
        return packed.y;
    }
    if (lane == 2u) {
        return packed.z;
    }
    return packed.w;
}

fn frame_vector(offset: u32) -> vec3<f32> {
    return vec3<f32>(
        frame_f32(offset),
        frame_f32(offset + 1u),
        frame_f32(offset + 2u)
    );
}

fn normalized_pixel_center(index: u32, extent: u32) -> f32 {
    return ((f32(index) + 0.5) / f32(extent)) * 2.0 - 1.0;
}

fn apply_slab(
    origin: f32,
    direction: f32,
    minimum: f32,
    maximum: f32,
    current_enter: f32,
    current_exit: f32,
) -> SlabResult {
    if (abs(direction) <= EPSILON) {
        if (origin < minimum || origin > maximum) {
            return SlabResult(false, current_enter, current_exit);
        }
        return SlabResult(true, current_enter, current_exit);
    }

    let near_t = (minimum - origin) / direction;
    let far_t = (maximum - origin) / direction;
    let axis_enter = min(near_t, far_t);
    let axis_exit = max(near_t, far_t);
    return SlabResult(
        true,
        max(current_enter, axis_enter),
        min(current_exit, axis_exit),
    );
}

fn initial_voxel_index(coordinate: f32, direction: f32, limit: u32) -> i32 {
    var adjusted = coordinate + 0.5;
    if (direction < -EPSILON) {
        adjusted = adjusted - EPSILON;
    }
    return clamp(i32(floor(adjusted)), 0, i32(limit) - 1);
}

fn axis_step(direction: f32) -> i32 {
    if (direction > EPSILON) {
        return 1;
    }
    if (direction < -EPSILON) {
        return -1;
    }
    return 0;
}

fn axis_next_t(entry_coordinate: f32, direction: f32, entry_t: f32, index: i32) -> f32 {
    if (direction > EPSILON) {
        let next_boundary = f32(index) + 0.5;
        return entry_t + max((next_boundary - entry_coordinate) / direction, 0.0);
    }
    if (direction < -EPSILON) {
        let next_boundary = f32(index) - 0.5;
        return entry_t + max((next_boundary - entry_coordinate) / direction, 0.0);
    }
    return BIG_T;
}

fn axis_delta_t(direction: f32) -> f32 {
    if (direction > EPSILON) {
        return 1.0 / direction;
    }
    if (direction < -EPSILON) {
        return -1.0 / direction;
    }
    return BIG_T;
}

fn axis_inside(index: i32, limit: u32) -> bool {
    return index >= 0 && index < i32(limit);
}

fn sample_value(sample: u32) -> u32 {
    return sample & 0xffffu;
}

fn sample_covered(sample: u32) -> bool {
    return (sample & MISSING_SAMPLE_FLAG) == 0u && (sample & SAMPLE_INVALID_FLAG) == 0u;
}

fn channel_dtype(channel_index: u32) -> u32 {
    return channel_u32(channel_index, 17u);
}

fn channel_visible(channel_index: u32) -> bool {
    return channel_u32(channel_index, 0u) != 0u &&
        channel_f32(channel_index, 6u) > EPSILON &&
        channel_f32(channel_index, 7u) > EPSILON;
}

fn channel_transfer_value(value: f32, channel_index: u32, base: u32, invert: bool) -> f32 {
    let low = channel_f32(channel_index, base);
    let high = channel_f32(channel_index, base + 1u);
    let gamma = max(channel_f32(channel_index, base + 2u), EPSILON);
    var normalized = 0.0;
    if (high > low) {
        normalized = clamp((value - low) / (high - low), 0.0, 1.0);
    }
    var mapped = pow(normalized, 1.0 / gamma);
    if (invert) {
        mapped = 1.0 - mapped;
    }
    return clamp(mapped, 0.0, 1.0);
}

fn channel_color_scalar(value: f32, channel_index: u32) -> f32 {
    return channel_transfer_value(value, channel_index, 0u, channel_u32(channel_index, 1u) != 0u);
}

fn channel_opacity_scalar(value: f32, channel_index: u32) -> f32 {
    return channel_transfer_value(value, channel_index, 3u, false);
}

fn channel_rgb(channel_index: u32) -> vec3<f32> {
    return vec3<f32>(
        clamp(channel_f32(channel_index, 8u), 0.0, 1.0),
        clamp(channel_f32(channel_index, 9u), 0.0, 1.0),
        clamp(channel_f32(channel_index, 10u), 0.0, 1.0)
    );
}

fn dvr_ray_physical_length(ray_direction: vec3<f32>) -> f32 {
    let grid_x_axis_world = frame_vector(35u);
    let grid_y_axis_world = frame_vector(38u);
    let grid_z_axis_world = frame_vector(41u);
    let world_direction =
        grid_x_axis_world * ray_direction.x +
        grid_y_axis_world * ray_direction.y +
        grid_z_axis_world * ray_direction.z;
    return max(length(world_direction), EPSILON);
}

fn channel_brick_linear_index(channel_index: u32, z: u32, y: u32, x: u32) -> u32 {
    let brick_x_size = channel_u32(channel_index, 2u);
    let brick_y_size = channel_u32(channel_index, 3u);
    let brick_z_size = channel_u32(channel_index, 4u);
    let grid_x = channel_u32(channel_index, 5u);
    let grid_y = channel_u32(channel_index, 6u);

    let brick_x = x / brick_x_size;
    let brick_y = y / brick_y_size;
    let brick_z = z / brick_z_size;
    return (brick_z * grid_y + brick_y) * grid_x + brick_x;
}

fn channel_metadata_base(channel_index: u32, brick_linear: u32) -> u32 {
    return channel_u32(channel_index, 16u) + brick_linear * BRICK_METADATA_WORDS;
}

fn channel_metadata_flags(channel_index: u32, brick_linear: u32) -> u32 {
    return brick_metadata[channel_metadata_base(channel_index, brick_linear)];
}

fn channel_metadata_min(channel_index: u32, brick_linear: u32) -> f32 {
    return bitcast<f32>(brick_metadata[channel_metadata_base(channel_index, brick_linear) + 1u]);
}

fn channel_metadata_max(channel_index: u32, brick_linear: u32) -> f32 {
    return bitcast<f32>(brick_metadata[channel_metadata_base(channel_index, brick_linear) + 2u]);
}

fn channel_brick_has_valid_samples(channel_index: u32, brick_linear: u32) -> bool {
    return (channel_metadata_flags(channel_index, brick_linear) & BRICK_METADATA_HAS_VALID_FLAG) != 0u;
}

fn channel_brick_has_valid_min_max(channel_index: u32, brick_linear: u32) -> bool {
    return (channel_metadata_flags(channel_index, brick_linear) & BRICK_METADATA_MIN_MAX_VALID_FLAG) != 0u;
}

fn channel_brick_can_skip_exact(channel_index: u32, brick_linear: u32) -> bool {
    let page_table_offset = channel_u32(channel_index, 15u);
    if (page_tables[page_table_offset + brick_linear] == 0u) {
        return false;
    }
    if (!channel_visible(channel_index)) {
        return true;
    }
    if (channel_dtype(channel_index) == DVR_CHANNEL_DTYPE_F32) {
        return false;
    }
    if (!channel_brick_has_valid_samples(channel_index, brick_linear)) {
        return true;
    }
    if (!channel_brick_has_valid_min_max(channel_index, brick_linear)) {
        return false;
    }
    let opacity_bound = max(
        channel_opacity_scalar(channel_metadata_min(channel_index, brick_linear), channel_index),
        channel_opacity_scalar(channel_metadata_max(channel_index, brick_linear), channel_index)
    );
    return opacity_bound <= EPSILON;
}

fn channel_brick_axis_exit_t(
    channel_index: u32,
    axis: u32,
    index: i32,
    step: i32,
    next_t: f32,
    delta_t: f32,
) -> f32 {
    if (step == 0) {
        return BIG_T;
    }
    let brick_size = channel_u32(channel_index, axis);
    if (brick_size == 0u) {
        return BIG_T;
    }
    let local_index = u32(index) % brick_size;
    var boundaries_to_exit = local_index + 1u;
    if (step > 0) {
        boundaries_to_exit = brick_size - local_index;
    }
    return next_t + f32(boundaries_to_exit - 1u) * delta_t;
}

fn sample_integer_channel_voxel(channel_index: u32, z: u32, y: u32, x: u32) -> u32 {
    let brick_x_size = channel_u32(channel_index, 2u);
    let brick_y_size = channel_u32(channel_index, 3u);
    let brick_z_size = channel_u32(channel_index, 4u);
    let brick_x = x / brick_x_size;
    let brick_y = y / brick_y_size;
    let brick_z = z / brick_z_size;
    let brick_linear = channel_brick_linear_index(channel_index, z, y, x);
    let page_table_offset = channel_u32(channel_index, 15u);
    let slot_plus_one = page_tables[page_table_offset + brick_linear];
    if (slot_plus_one == 0u) {
        return MISSING_SAMPLE_FLAG;
    }
    if (!channel_brick_has_valid_samples(channel_index, brick_linear)) {
        return SAMPLE_INVALID_FLAG;
    }

    let local_x = x - brick_x * brick_x_size;
    let local_y = y - brick_y * brick_y_size;
    let local_z = z - brick_z * brick_z_size;
    let local_index = (local_z * brick_y_size + local_y) * brick_x_size + local_x;
    let packed_u32_per_brick = channel_u32(channel_index, 8u);
    let values_per_word = channel_u32(channel_index, 9u);
    let bits_per_value = channel_u32(channel_index, 10u);
    let value_mask = channel_u32(channel_index, 11u);
    let valid_u32_per_brick = channel_u32(channel_index, 12u);
    let packed_values_offset = channel_u32(channel_index, 13u);
    let validity_offset = channel_u32(channel_index, 14u);
    let slot = slot_plus_one - 1u;

    let valid_word_index = validity_offset + slot * valid_u32_per_brick + local_index / 32u;
    let valid_bit = local_index % 32u;
    if ((validity_bits[valid_word_index] & (1u << valid_bit)) == 0u) {
        return SAMPLE_INVALID_FLAG;
    }

    let value_word_index = local_index / values_per_word;
    let value_offset = local_index % values_per_word;
    let value_shift = value_offset * bits_per_value;
    let atlas_index = packed_values_offset + slot * packed_u32_per_brick + value_word_index;
    return (packed_voxels[atlas_index] >> value_shift) & value_mask;
}

fn sample_f32_channel_voxel(channel_index: u32, z: u32, y: u32, x: u32) -> LinearSample {
    let brick_linear = channel_brick_linear_index(channel_index, z, y, x);
    let page_table_offset = channel_u32(channel_index, 15u);
    let page_table_base = page_table_offset + brick_linear * F32_PAGE_TABLE_WORDS;
    let value_offset_plus_one = page_tables[page_table_base];
    if (value_offset_plus_one == 0u) {
        return LinearSample(0.0, false);
    }
    let brick_actual_x = page_tables[page_table_base + 1u];
    let brick_actual_y = page_tables[page_table_base + 2u];
    let brick_actual_z = page_tables[page_table_base + 3u];
    let brick_start_x = page_tables[page_table_base + 4u];
    let brick_start_y = page_tables[page_table_base + 5u];
    let brick_start_z = page_tables[page_table_base + 6u];

    if (x < brick_start_x || y < brick_start_y || z < brick_start_z) {
        return LinearSample(0.0, false);
    }
    let local_x = x - brick_start_x;
    let local_y = y - brick_start_y;
    let local_z = z - brick_start_z;
    if (local_x >= brick_actual_x || local_y >= brick_actual_y || local_z >= brick_actual_z) {
        return LinearSample(0.0, false);
    }
    let local_index = (local_z * brick_actual_y + local_y) * brick_actual_x + local_x;
    let values_offset = channel_u32(channel_index, 13u);
    return LinearSample(f32_voxels[values_offset + value_offset_plus_one - 1u + local_index], true);
}

fn sample_channel_value(channel_index: u32, z: u32, y: u32, x: u32) -> LinearSample {
    if (channel_dtype(channel_index) == DVR_CHANNEL_DTYPE_F32) {
        return sample_f32_channel_voxel(channel_index, z, y, x);
    }
    let sample = sample_integer_channel_voxel(channel_index, z, y, x);
    if (!sample_covered(sample)) {
        return LinearSample(0.0, false);
    }
    return LinearSample(f32(sample_value(sample)), true);
}

fn interpolation_axis(coordinate: f32, limit: u32) -> InterpAxis {
    if (limit == 0u || coordinate < -0.5 - EPSILON || coordinate > f32(limit - 1u) + 0.5 + EPSILON) {
        return InterpAxis(false, 0u, 0u, 0.0);
    }
    let maximum = f32(limit - 1u);
    let clamped = clamp(coordinate, 0.0, maximum);
    let lower = u32(floor(clamped));
    let upper = min(lower + 1u, limit - 1u);
    var fraction = 0.0;
    if (upper != lower) {
        fraction = clamped - f32(lower);
    }
    return InterpAxis(true, lower, upper, fraction);
}

fn lerp_f32(a: f32, b: f32, fraction: f32) -> f32 {
    return a + (b - a) * fraction;
}

fn sample_channel_linear(channel_index: u32, point: vec3<f32>, shape_x: u32, shape_y: u32, shape_z: u32) -> LinearSample {
    let x = interpolation_axis(point.x, shape_x);
    let y = interpolation_axis(point.y, shape_y);
    let z = interpolation_axis(point.z, shape_z);
    if (!x.valid || !y.valid || !z.valid) {
        return LinearSample(0.0, false);
    }
    let c000 = sample_channel_value(channel_index, z.lower, y.lower, x.lower);
    let c100 = sample_channel_value(channel_index, z.lower, y.lower, x.upper);
    let c010 = sample_channel_value(channel_index, z.lower, y.upper, x.lower);
    let c110 = sample_channel_value(channel_index, z.lower, y.upper, x.upper);
    let c001 = sample_channel_value(channel_index, z.upper, y.lower, x.lower);
    let c101 = sample_channel_value(channel_index, z.upper, y.lower, x.upper);
    let c011 = sample_channel_value(channel_index, z.upper, y.upper, x.lower);
    let c111 = sample_channel_value(channel_index, z.upper, y.upper, x.upper);
    let covered =
        c000.valid && c100.valid &&
        c010.valid && c110.valid &&
        c001.valid && c101.valid &&
        c011.valid && c111.valid;
    if (!covered) {
        return LinearSample(0.0, false);
    }
    let c00 = lerp_f32(c000.value, c100.value, x.fraction);
    let c10 = lerp_f32(c010.value, c110.value, x.fraction);
    let c01 = lerp_f32(c001.value, c101.value, x.fraction);
    let c11 = lerp_f32(c011.value, c111.value, x.fraction);
    let c0 = lerp_f32(c00, c10, y.fraction);
    let c1 = lerp_f32(c01, c11, y.fraction);
    return LinearSample(lerp_f32(c0, c1, z.fraction), true);
}

fn accumulate_dvr_step(
    accumulated: vec4<f32>,
    tau_total: f32,
    weighted_rgb: vec3<f32>,
) -> vec4<f32> {
    if (tau_total <= EPSILON) {
        return accumulated;
    }
    let alpha = clamp(1.0 - exp(-tau_total), 0.0, 1.0);
    if (alpha <= EPSILON) {
        return accumulated;
    }
    let rgb = weighted_rgb / tau_total;
    let transmittance = 1.0 - accumulated.a;
    return vec4<f32>(
        accumulated.rgb + transmittance * rgb * alpha,
        accumulated.a + transmittance * alpha
    );
}

fn add_channel_contribution(
    value: f32,
    channel_index: u32,
    step_factor: f32,
    tau_total: f32,
    weighted_rgb: vec3<f32>,
) -> vec4<f32> {
    if (!channel_visible(channel_index)) {
        return vec4<f32>(tau_total, weighted_rgb);
    }
    let opacity_scalar = channel_opacity_scalar(value, channel_index);
    if (opacity_scalar <= EPSILON) {
        return vec4<f32>(tau_total, weighted_rgb);
    }
    let density =
        opacity_scalar *
        max(channel_f32(channel_index, 6u), 0.0) *
        clamp(channel_f32(channel_index, 7u), 0.0, 1.0);
    if (density <= EPSILON) {
        return vec4<f32>(tau_total, weighted_rgb);
    }
    let tau = density * max(step_factor, EPSILON);
    if (tau <= EPSILON) {
        return vec4<f32>(tau_total, weighted_rgb);
    }
    let color_scalar = channel_color_scalar(value, channel_index);
    let rgb = channel_rgb(channel_index) * color_scalar;
    return vec4<f32>(tau_total + tau, weighted_rgb + rgb * tau);
}

fn display_rgba(premultiplied: vec4<f32>) -> vec4<f32> {
    let alpha = max(
        clamp(premultiplied.a, 0.0, 1.0),
        clamp(max(premultiplied.r, max(premultiplied.g, premultiplied.b)), 0.0, 1.0)
    );
    if (alpha <= EPSILON) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(
        clamp(premultiplied.r / alpha, 0.0, 1.0),
        clamp(premultiplied.g / alpha, 0.0, 1.0),
        clamp(premultiplied.b / alpha, 0.0, 1.0),
        alpha
    );
}

@compute @workgroup_size(8, 8, 1)
fn display_dvr_multi_channel_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel_x = id.x;
    let pixel_y = id.y;
    let viewport_width = frame_u32(0u);
    let viewport_height = frame_u32(1u);
    if (pixel_x >= viewport_width || pixel_y >= viewport_height) {
        return;
    }

    let shape_x = frame_u32(2u);
    let shape_y = frame_u32(3u);
    let shape_z = frame_u32(4u);
    let projection = frame_u32(5u);
    let channel_count = frame_u32(6u);
    let sampling_policy = frame_u32(7u);

    let grid_eye = frame_vector(0u);
    let grid_forward = frame_vector(3u);
    let grid_right = frame_vector(6u);
    let grid_up = frame_vector(9u);
    let orthographic_world_per_screen_point = frame_f32(12u);
    let perspective_focal_length_screen_points = frame_f32(13u);
    let presentation_width_points = frame_f32(14u);
    let presentation_height_points = frame_f32(16u);

    let screen_x_points = ((f32(pixel_x) + 0.5) / f32(viewport_width) - 0.5) *
        presentation_width_points;
    let screen_y_points = (0.5 - (f32(pixel_y) + 0.5) / f32(viewport_height)) *
        presentation_height_points;

    var ray_origin = grid_eye;
    var ray_direction = grid_forward;
    if (projection == 0u) {
        ray_direction =
            grid_forward +
            grid_right * (screen_x_points / perspective_focal_length_screen_points) +
            grid_up * (screen_y_points / perspective_focal_length_screen_points);
    } else {
        ray_origin =
            grid_eye +
            grid_right * (screen_x_points * orthographic_world_per_screen_point) +
            grid_up * (screen_y_points * orthographic_world_per_screen_point);
    }

    var accumulated = vec4<f32>(0.0);
    if (dot(ray_direction, ray_direction) <= EPSILON) {
        textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
        return;
    }

    var enter = -BIG_T;
    var exit = BIG_T;
    var slab = apply_slab(ray_origin.x, ray_direction.x, -0.5, f32(shape_x) - 0.5, enter, exit);
    if (!slab.hit) {
        textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    slab = apply_slab(ray_origin.y, ray_direction.y, -0.5, f32(shape_y) - 0.5, enter, exit);
    if (!slab.hit) {
        textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    slab = apply_slab(ray_origin.z, ray_direction.z, -0.5, f32(shape_z) - 0.5, enter, exit);
    if (!slab.hit) {
        textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
        return;
    }
    enter = slab.enter;
    exit = slab.exit;

    if (exit < enter || exit < 0.0) {
        textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
        return;
    }

    let hit_enter = max(enter, 0.0);
    let ray_direction_physical_length = dvr_ray_physical_length(ray_direction);

    if (sampling_policy == 1u) {
        let step_t = SMOOTH_RAY_STEP_VOXELS / length(ray_direction);
        let step_factor = step_t * ray_direction_physical_length;
        var t = hit_enter;
        var smooth_steps = 0u;
        let max_smooth_steps = (shape_x + shape_y + shape_z) * 4u + 16u;
        loop {
            if (t > exit + EPSILON || smooth_steps >= max_smooth_steps || accumulated.a >= 0.995) {
                break;
            }
            let point = ray_origin + ray_direction * t;
            var tau_total = 0.0;
            var weighted_rgb = vec3<f32>(0.0);
            for (var channel_index = 0u; channel_index < channel_count; channel_index = channel_index + 1u) {
                let sample = sample_channel_linear(channel_index, point, shape_x, shape_y, shape_z);
                if (!sample.valid) {
                    continue;
                }
                let contribution = add_channel_contribution(
                    sample.value,
                    channel_index,
                    step_factor,
                    tau_total,
                    weighted_rgb
                );
                tau_total = contribution.x;
                weighted_rgb = contribution.yzw;
            }
            accumulated = accumulate_dvr_step(accumulated, tau_total, weighted_rgb);
            t = t + step_t;
            smooth_steps = smooth_steps + 1u;
        }
        textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
        return;
    }

    let entry = ray_origin + ray_direction * hit_enter;
    var x_index = initial_voxel_index(entry.x, ray_direction.x, shape_x);
    var y_index = initial_voxel_index(entry.y, ray_direction.y, shape_y);
    var z_index = initial_voxel_index(entry.z, ray_direction.z, shape_z);
    let x_step = axis_step(ray_direction.x);
    let y_step = axis_step(ray_direction.y);
    let z_step = axis_step(ray_direction.z);
    var x_next_t = axis_next_t(entry.x, ray_direction.x, hit_enter, x_index);
    var y_next_t = axis_next_t(entry.y, ray_direction.y, hit_enter, y_index);
    var z_next_t = axis_next_t(entry.z, ray_direction.z, hit_enter, z_index);
    let x_delta_t = axis_delta_t(ray_direction.x);
    let y_delta_t = axis_delta_t(ray_direction.y);
    let z_delta_t = axis_delta_t(ray_direction.z);
    var current_t = hit_enter;
    var steps = 0u;
    let max_steps = shape_x + shape_y + shape_z + 3u;

    loop {
        if (
            steps >= max_steps ||
            accumulated.a >= 0.995 ||
            !axis_inside(x_index, shape_x) ||
            !axis_inside(y_index, shape_y) ||
            !axis_inside(z_index, shape_z)
        ) {
            break;
        }

        var all_channels_can_skip = channel_count > 0u;
        var skip_t = BIG_T;
        for (var channel_index = 0u; channel_index < channel_count; channel_index = channel_index + 1u) {
            let brick_linear = channel_brick_linear_index(
                channel_index,
                u32(z_index),
                u32(y_index),
                u32(x_index)
            );
            if (!channel_brick_can_skip_exact(channel_index, brick_linear)) {
                all_channels_can_skip = false;
            }
            skip_t = min(
                skip_t,
                min(
                    channel_brick_axis_exit_t(channel_index, 2u, x_index, x_step, x_next_t, x_delta_t),
                    min(
                        channel_brick_axis_exit_t(channel_index, 3u, y_index, y_step, y_next_t, y_delta_t),
                        channel_brick_axis_exit_t(channel_index, 4u, z_index, z_step, z_next_t, z_delta_t)
                    )
                )
            );
        }
        if (all_channels_can_skip) {
            if (skip_t > exit + EPSILON || skip_t >= BIG_T * 0.5) {
                break;
            }
            if (skip_t > current_t + EPSILON) {
                current_t = skip_t;
                let skipped_entry = ray_origin + ray_direction * current_t;
                x_index = initial_voxel_index(skipped_entry.x, ray_direction.x, shape_x);
                y_index = initial_voxel_index(skipped_entry.y, ray_direction.y, shape_y);
                z_index = initial_voxel_index(skipped_entry.z, ray_direction.z, shape_z);
                x_next_t = axis_next_t(skipped_entry.x, ray_direction.x, current_t, x_index);
                y_next_t = axis_next_t(skipped_entry.y, ray_direction.y, current_t, y_index);
                z_next_t = axis_next_t(skipped_entry.z, ray_direction.z, current_t, z_index);
                steps = steps + 1u;
                continue;
            }
        }

        let next_t = min(x_next_t, min(y_next_t, z_next_t));
        let step_factor = max(min(next_t, exit) - current_t, 0.0) * ray_direction_physical_length;
        var tau_total = 0.0;
        var weighted_rgb = vec3<f32>(0.0);
        for (var channel_index = 0u; channel_index < channel_count; channel_index = channel_index + 1u) {
            let sample = sample_channel_value(channel_index, u32(z_index), u32(y_index), u32(x_index));
            if (!sample.valid) {
                continue;
            }
            let contribution = add_channel_contribution(
                sample.value,
                channel_index,
                step_factor,
                tau_total,
                weighted_rgb
            );
            tau_total = contribution.x;
            weighted_rgb = contribution.yzw;
        }
        accumulated = accumulate_dvr_step(accumulated, tau_total, weighted_rgb);

        if (next_t > exit + EPSILON || next_t >= BIG_T * 0.5) {
            break;
        }

        current_t = next_t;
        if (x_next_t <= next_t + EPSILON) {
            x_index = x_index + x_step;
            x_next_t = x_next_t + x_delta_t;
        }
        if (y_next_t <= next_t + EPSILON) {
            y_index = y_index + y_step;
            y_next_t = y_next_t + y_delta_t;
        }
        if (z_next_t <= next_t + EPSILON) {
            z_index = z_index + z_step;
            z_next_t = z_next_t + z_delta_t;
        }
        steps = steps + 1u;
    }

    textureStore(output_texture, vec2<i32>(i32(pixel_x), i32(pixel_y)), display_rgba(accumulated));
}
"#;
