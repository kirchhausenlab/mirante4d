// Dedicated heterogeneous authored-order volume fragment program.

fn render_mixed_layer(
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
            step * length(world_direction),
            count,
        );
    }
    if mode == 2u {
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
    var invalid = transparent_pixel();
    invalid.covered = 0u;
    return invalid;
}

fn render_mixed_fragment(position: vec4<f32>) -> PixelResult {
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

    // Heterogeneous modes intentionally retain authored view order. Joint DVR
    // integration and ISO depth sorting are homogeneous whole-stack laws and
    // cannot be applied to a subset without moving it across neighboring
    // layers.
    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        pixel = composite_over(
            pixel,
            render_mixed_layer(layer_index, ray_origin, ray_direction),
        );
    }
    return pixel;
}

@fragment
fn fs_mixed_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = render_mixed_fragment(position);
    return vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
}

@fragment
fn fs_mixed_validation(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    let pixel = render_mixed_fragment(position);
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}
