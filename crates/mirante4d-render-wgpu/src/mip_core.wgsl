// Canonical raw-maximum sampling and transfer-once layer implementation.

fn render_mip(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    exit: f32,
    step: f32,
    count: u32,
) -> PixelResult {
    var maximum = 0.0;
    var has_value = false;
    var covered = 1u;
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
        let predicted_end = segment_end_index(
            entry,
            step,
            index,
            page_exit_distance(page, point, direction, distance, exit),
            count,
        );
        let next = page_segment_end_index(
            layer_index,
            page,
            origin,
            direction,
            entry,
            step,
            index,
            predicted_end,
        );
        if page.kind != PAGE_RESIDENT {
            covered = 0u;
            index = next;
            continue;
        }
        let address = resource_address(page.resource_index);
        let any_valid = address.any_valid != 0u;
        if !any_valid {
            index = next;
            continue;
        }
        if (layer_word(layer_index, 56u) == 0u
            || layer_word(layer_index, 62u) != 0u)
            && page_fully_covered(page)
            && has_value
            && resource_f32(page.resource_index, 13u) <= maximum {
            index = next;
            continue;
        }
        loop {
            if index >= next {
                break;
            }
            let sample_distance = entry + (f32(index) + 0.5) * step;
            let sample = sample_in_resolved_page(
                layer_index,
                page,
                address,
                origin + direction * sample_distance,
            );
            if sample.kind == SAMPLE_VALID {
                maximum = select(sample.value, max(maximum, sample.value), has_value);
                has_value = true;
            } else if sample.kind == SAMPLE_MISSING {
                covered = 0u;
            }
            index += 1u;
        }
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
