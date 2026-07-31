// Dedicated cross-section color kernel. This module is concatenated only with
// shader_common.wgsl and is structurally unable to reach volume traversal.

fn render_plane_layer(layer_index: u32, position: vec2<f32>) -> PixelResult {
    let width = f32(control[4u]);
    let height = f32(control[5u]);
    let screen_x = (position.x / width - 0.5) * control_f32(18u);
    let screen_y = (0.5 - position.y / height) * control_f32(19u);
    let center = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u));
    let right = vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u));
    let up = vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u));
    let world = center + (right * screen_x + up * screen_y) * control_f32(17u);
    let sample = sample_plane_multiscale(layer_index, world);
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

fn render_plane_fragment(position: vec4<f32>) -> PixelResult {
    var pixel = transparent_pixel();
    let layer_count = control[2u];
    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        pixel = composite_additive(
            pixel,
            render_plane_layer(layer_index, position.xy),
        );
    }
    return pixel;
}

@fragment
fn fs_plane_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = render_plane_fragment(position);
    return vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
}

@fragment
fn fs_plane_validation(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    let pixel = render_plane_fragment(position);
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}
