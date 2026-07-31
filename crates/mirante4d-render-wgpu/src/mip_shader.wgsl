// Dedicated homogeneous raw-maximum fragment program.

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
