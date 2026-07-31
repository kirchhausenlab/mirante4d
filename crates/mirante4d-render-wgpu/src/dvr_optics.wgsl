// Shared emission-absorption optical conversion.

fn dvr_effective_alpha(base_tau: f32, layer_opacity: f32) -> f32 {
    let base_alpha = 1.0 - exp(-max(base_tau, 0.0));
    return clamp(layer_opacity, 0.0, 1.0) * base_alpha;
}
