use super::*;

pub(super) fn phase13_refinement_budget_probe_report(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    input: Phase11LodPlanningInput,
    selected_plan: &Phase11LodPlan,
) -> anyhow::Result<Value> {
    let scale_count = dataset.scale_count(layer_id)?;
    let mut scales = Vec::with_capacity(scale_count);
    let mut first_settled_fit_scale = None;
    let mut first_responsive_fit_scale = None;
    let mut target_rejection_reasons = Vec::new();

    for scale_index in 0..scale_count {
        let scale_level = scale_index as u32;
        let plan = phase11_visible_bricks_at_scale(
            dataset,
            layer_id,
            input.camera,
            input.viewport,
            input.brick_pixel_stride,
            scale_level,
        )?;
        let visible_fits = plan.visible_bricks.len() <= input.max_visible_bricks;
        let decoded_fits = plan.estimated_decoded_bytes <= input.max_decoded_bytes;
        let settled_fit = visible_fits && decoded_fits;
        let responsive_fit = settled_fit
            && plan.visible_bricks.len() <= PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS;
        if settled_fit && first_settled_fit_scale.is_none() {
            first_settled_fit_scale = Some(scale_level);
        }
        if responsive_fit && first_responsive_fit_scale.is_none() {
            first_responsive_fit_scale = Some(scale_level);
        }
        let rejection_reasons =
            phase13_refinement_rejection_reasons(visible_fits, decoded_fits, responsive_fit);
        if scale_level == selected_plan.target_scale_level {
            target_rejection_reasons = rejection_reasons.clone();
        }
        let roles = phase13_refinement_scale_roles(scale_level, selected_plan);
        scales.push(json!({
            "scale_level": scale_level,
            "roles": roles,
            "visible_bricks": plan.visible_bricks.len(),
            "estimated_decoded_bytes": plan.estimated_decoded_bytes,
            "fits_visible_brick_budget": visible_fits,
            "fits_decoded_byte_budget": decoded_fits,
            "fits_settled_budget": settled_fit,
            "fits_responsive_current_frame_budget": responsive_fit,
            "rejection_reasons": rejection_reasons,
        }));
    }

    Ok(json!({
        "ok": true,
        "purpose": "per_scale_high_lod_refinement_budget_probe",
        "policy": "highest screen-meaningful LOD is pursued when it fits settled budgets; coarse displayed frames may be used while larger selected/refinement sets load",
        "budget": {
            "max_visible_bricks": input.max_visible_bricks,
            "max_decoded_bytes": input.max_decoded_bytes,
            "max_responsive_current_frame_visible_bricks": PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS,
        },
        "selection": {
            "target_scale_level": selected_plan.target_scale_level,
            "displayed_scale_level": selected_plan.displayed_scale_level,
            "refinement_scale_level": selected_plan.refinement_scale_level,
            "reason": selected_plan.reason,
            "first_settled_fit_scale": first_settled_fit_scale,
            "first_responsive_fit_scale": first_responsive_fit_scale,
            "target_rejection_reasons": target_rejection_reasons,
        },
        "scales": scales,
    }))
}

pub(super) fn phase13_refinement_rejection_reasons(
    visible_fits: bool,
    decoded_fits: bool,
    responsive_fit: bool,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !visible_fits {
        reasons.push("visible_brick_budget_limited");
    }
    if !decoded_fits {
        reasons.push("decoded_byte_budget_limited");
    }
    if visible_fits && decoded_fits && !responsive_fit {
        reasons.push("responsive_current_frame_budget_limited");
    }
    reasons
}

fn phase13_refinement_scale_roles(
    scale_level: u32,
    selected_plan: &Phase11LodPlan,
) -> Vec<&'static str> {
    let mut roles = Vec::new();
    if scale_level == selected_plan.target_scale_level {
        roles.push("target");
    }
    if scale_level == selected_plan.displayed_scale_level {
        roles.push("displayed");
    }
    if Some(scale_level) == selected_plan.refinement_scale_level {
        roles.push("refinement");
    }
    roles
}
