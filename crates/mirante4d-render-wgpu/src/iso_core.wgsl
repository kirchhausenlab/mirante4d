// Canonical isosurface lighting and first-hit layer traversal.

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
        let address = resource_address(page.resource_index);
        if address.any_valid == 0u {
            index = next;
            continue;
        }
        let minimum_display = transfer_value(layer_index, resource_f32(page.resource_index, 12u));
        let maximum_display = transfer_value(layer_index, resource_f32(page.resource_index, 13u));
        if (layer_word(layer_index, 56u) == 0u
            || layer_word(layer_index, 62u) != 0u)
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
            let sample = sample_in_resolved_page(
                layer_index,
                page,
                address,
                sample_point,
            );
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
                    result.depth = sample_distance * length(world_direction);
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
