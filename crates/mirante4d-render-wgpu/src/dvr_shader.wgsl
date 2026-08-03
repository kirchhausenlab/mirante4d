// Dedicated homogeneous emission-absorption fragment program.

fn dvr_effective_tau(base_tau: f32, layer_opacity: f32) -> f32 {
    return -log(max(1.0 - dvr_effective_alpha(base_tau, layer_opacity), EPSILON));
}

// Compatible layers share grid geometry (proved by the CPU control builder),
// so optical density and color are combined at each sample in one
// front-to-back traversal rather than compositing finished 2D channel images.
fn render_fused_dvr(world_origin: vec3<f32>, world_direction: vec3<f32>) -> PixelResult {
    let ray = volume_ray(0u, world_origin, world_direction);
    if !ray.intersects {
        return transparent_pixel();
    }
    let origin = ray.origin;
    let direction = ray.direction;
    let step = 1.0 / ray.grid_speed;
    let step_world = step * length(world_direction);
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
        let predicted_end = segment_end_index(ray.entry, step, index, segment_exit, count);
        var next = predicted_end;
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            let page = lookup_page(layer_index, coordinate);
            next = page_segment_end_index(
                layer_index,
                page,
                origin,
                direction,
                ray.entry,
                step,
                index,
                next,
            );
        }
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
                    opacity * layer_f32(layer_index, 23u) * step_world,
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

// Correct common-world path for non-compatible affine layouts. All channels
// are sampled at the same monotonically increasing world-space distances and
// their optical depths are integrated jointly, so result semantics do not
// depend on layer order. Compatible exact grids retain the faster
// brick-segment specialization above.
fn render_general_dvr(world_origin: vec3<f32>, world_direction: vec3<f32>) -> PixelResult {
    let layer_count = control[2u];
    var entry = LARGE_DISTANCE;
    var exit = -LARGE_DISTANCE;
    var step = LARGE_DISTANCE;
    var has_intersection = false;
    for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
        let ray = volume_ray(layer_index, world_origin, world_direction);
        if !ray.intersects {
            continue;
        }
        has_intersection = true;
        entry = min(entry, ray.entry);
        exit = max(exit, ray.exit);
        step = min(step, 1.0 / ray.grid_speed);
    }
    if !has_intersection || exit <= entry || step <= 0.0 {
        return transparent_pixel();
    }

    let step_world = step * length(world_direction);
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

        // Resolve one page per layer for the common-world segment. Affine
        // grids can leave pages at different distances, so the nearest exit
        // across all layers is the only segment boundary. Missing,
        // metadata-empty, and exact-mode extrema-zero pages remain in the
        // boundary calculation but do no per-sample work.
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            resources[layer_index] = 0xffffffffu;
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
                || ((layer_word(layer_index, 56u) == 0u
                        || layer_word(layer_index, 62u) != 0u)
                    && page_fully_covered(page)
                    && resource_f32(page.resource_index, 13u) <= layer_f32(layer_index, 20u));
            if can_infer_valid && has_no_contribution {
                any_valid = true;
                continue;
            }
            resources[layer_index] = page.resource_index;
            any_work = true;
        }

        let predicted_end = segment_end_index(entry, step, index, segment_exit, count);
        var next = predicted_end;
        for (var layer_index = 0u; layer_index < layer_count; layer_index += 1u) {
            let ray = volume_ray(layer_index, world_origin, world_direction);
            if !ray.intersects || distance >= ray.exit {
                continue;
            }
            if distance < ray.entry {
                next = distance_boundary_end_index(
                    entry,
                    step,
                    index,
                    next,
                    ray.entry,
                );
                continue;
            }
            let grid = ray.origin + ray.direction * distance;
            if !grid_inside(layer_index, grid) {
                continue;
            }
            let page = lookup_page(layer_index, grid_coordinate(layer_index, grid));
            next = page_segment_end_index(
                layer_index,
                page,
                ray.origin,
                ray.direction,
                entry,
                step,
                index,
                next,
            );
        }
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
                    opacity * layer_f32(layer_index, 23u) * step_world,
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

fn render_dvr_fragment(position: vec4<f32>) -> PixelResult {
    let pixel_index = position.xy - vec2<f32>(0.5);
    let ray_origin = vec3<f32>(control_f32(8u), control_f32(9u), control_f32(10u))
        + vec3<f32>(control_f32(11u), control_f32(12u), control_f32(13u)) * pixel_index.x
        + vec3<f32>(control_f32(14u), control_f32(15u), control_f32(16u)) * pixel_index.y;
    let ray_direction =
        vec3<f32>(control_f32(17u), control_f32(18u), control_f32(19u))
            + vec3<f32>(control_f32(20u), control_f32(21u), control_f32(22u)) * pixel_index.x
            + vec3<f32>(control_f32(23u), control_f32(24u), control_f32(25u)) * pixel_index.y;

    if control[27u] != 0u {
        return render_fused_dvr(ray_origin, ray_direction);
    }
    return render_general_dvr(ray_origin, ray_direction);
}

@fragment
fn fs_dvr_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = render_dvr_fragment(position);
    return vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
}

@fragment
fn fs_dvr_validation(@builtin(position) position: vec4<f32>) -> FragmentOutput {
    let pixel = render_dvr_fragment(position);
    var output: FragmentOutput;
    output.rgba = vec4<f32>(pixel.premultiplied_rgb, pixel.alpha);
    output.facts = vec2<u32>(pixel.covered, pixel.valid);
    return output;
}
