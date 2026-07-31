// Shared emission-absorption layer integration and front-to-back compositing.

// Residual radiance below this threshold is < 0.5 RGBA8 level even for a
// saturated tail. Progressive frames may only terminate when the host proves
// the complete requirement body resident; otherwise the tail is still walked
// so missing coverage cannot be hidden behind an opaque prefix.
const ALPHA_TERMINATION: f32 = 0.999;

fn dvr_can_terminate(alpha: f32) -> bool {
    return alpha >= ALPHA_TERMINATION && control[28u] != 0u;
}

fn render_dvr_layer(
    layer_index: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    entry: f32,
    exit: f32,
    step: f32,
    step_world: f32,
    count: u32,
) -> PixelResult {
    var result = transparent_pixel();
    var any_valid = false;
    var index = 0u;
    loop {
        if index >= count || dvr_can_terminate(result.alpha) {
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
            result.covered = 0u;
            index = next;
            continue;
        }
        let address = resource_address(page.resource_index);
        if address.any_valid == 0u {
            index = next;
            continue;
        }
        if (layer_word(layer_index, 56u) == 0u
            || layer_word(layer_index, 62u) != 0u)
            && page_fully_covered(page)
            && resource_word(page.resource_index, 15u) != 0u
            && (resource_f32(page.resource_index, 13u) <= layer_f32(layer_index, 20u)
                || layer_f32(layer_index, 15u) <= 0.0
                || layer_f32(layer_index, 23u) <= 0.0) {
            any_valid = true;
            index = next;
            continue;
        }
        loop {
            if index >= next || dvr_can_terminate(result.alpha) {
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
                any_valid = true;
                let opacity_display = curve_value(
                    sample.value,
                    layer_f32(layer_index, 20u),
                    layer_f32(layer_index, 21u),
                    layer_f32(layer_index, 22u),
                    0u,
                );
                let base_tau = opacity_display
                    * layer_f32(layer_index, 23u)
                    * step_world;
                let sample_alpha = dvr_effective_alpha(
                    base_tau,
                    layer_f32(layer_index, 15u),
                );
                result = composite_over(
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
    }
    result.valid = select(0u, 1u, any_valid);
    return result;
}
