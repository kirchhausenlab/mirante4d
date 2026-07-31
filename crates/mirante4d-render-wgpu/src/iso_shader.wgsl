// Dedicated homogeneous isosurface fragment program.

fn render_iso_layer(
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

fn render_iso_stack(world_origin: vec3<f32>, world_direction: vec3<f32>) -> PixelResult {
    var hits: array<PixelResult, 64>;
    var hit_count = 0u;
    var facts = transparent_pixel();
    let layer_count = control[2u];
    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        let hit = render_iso_layer(layer_index, world_origin, world_direction);
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

fn render_iso_fragment(position: vec4<f32>) -> PixelResult {
    let pixel_index = position.xy - vec2<f32>(0.5);
    let ray_origin = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u))
        + vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u)) * pixel_index.x
        + vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u)) * pixel_index.y;
    // Keep the camera ray in its native scale. Intersection distances and
    // grid-space steps scale together, and lighting normalizes only where a
    // unit vector is required.
    let ray_direction =
        vec3<f32>(control_f32(17u), control_f32(18u), control_f32(19u))
            + vec3<f32>(control_f32(20u), control_f32(21u), control_f32(22u)) * pixel_index.x
            + vec3<f32>(control_f32(23u), control_f32(24u), control_f32(25u)) * pixel_index.y;
    return render_iso_stack(ray_origin, ray_direction);
}

@fragment
fn fs_iso_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = render_iso_fragment(position);
    return vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
}

@fragment
fn fs_iso_validation(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    let pixel = render_iso_fragment(position);
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}
