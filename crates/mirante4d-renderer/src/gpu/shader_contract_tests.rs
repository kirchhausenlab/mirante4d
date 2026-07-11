use super::shaders::{
    BRICKED_CAMERA_F32_SHADER, CROSS_SECTION_CHUNK_DISPLAY_F32_SHADER,
    CROSS_SECTION_CHUNK_DISPLAY_INTEGER_SHADER, DISPLAY_FRAME_BLEND_SHADER, SCENE_RENDER_SHADER,
    SCENE_RENDER_TEXTURE_SHADER,
};

#[test]
fn display_frame_blend_shader_supports_additive_and_source_over_modes() {
    let shader = DISPLAY_FRAME_BLEND_SHADER;

    for required in [
        "var base_texture: texture_2d<f32>;",
        "var overlay_texture: texture_2d<f32>;",
        "var output_texture: texture_storage_2d<rgba8unorm, write>;",
        "fn additive_display_frame",
        "fn source_over_display_frame",
        "fn display_frame_blend_main",
        "if (mode == 1u)",
    ] {
        assert!(
            shader.contains(required),
            "display frame blend shader missing contract fragment: {required}"
        );
    }
}

#[test]
fn scene_overlay_shaders_accept_premultiplied_base_texture() {
    for shader in [SCENE_RENDER_SHADER, SCENE_RENDER_TEXTURE_SHADER] {
        assert!(
            shader.contains("source.rgb * source_alpha + base.rgb * inverse_alpha"),
            "scene overlay must treat source colors as straight alpha and base colors as premultiplied"
        );
        assert!(
            shader.contains("source_alpha + base.a * inverse_alpha"),
            "scene overlay alpha accumulation must use source-over alpha"
        );
    }
}

#[test]
fn f32_camera_shader_uses_compact_region_page_table_contract() {
    let shader = BRICKED_CAMERA_F32_SHADER;

    assert!(
        shader.contains("const F32_PAGE_TABLE_WORDS: u32 = 7u;"),
        "F32 resident camera shader must match the compact atlas page-table stride"
    );
    for required in [
        "let brick_start_x = page_table[page_table_base + 4u];",
        "let brick_start_y = page_table[page_table_base + 5u];",
        "let brick_start_z = page_table[page_table_base + 6u];",
        "let local_x = x - brick_start_x;",
        "let local_y = y - brick_start_y;",
        "let local_z = z - brick_start_z;",
    ] {
        assert!(
            shader.contains(required),
            "F32 resident camera shader missing page-table contract fragment: {required}"
        );
    }
}

#[test]
fn chunked_cross_section_shader_uses_draw_buffer_contract() {
    let shader = CROSS_SECTION_CHUNK_DISPLAY_INTEGER_SHADER;

    for required in [
        "const CHUNK_DRAW_WORDS: u32 = 8u;",
        "var<storage, read> chunk_draws: array<u32>;",
        "let base = id.z * CHUNK_DRAW_WORDS;",
        "let min_x = chunk_draws[base + 3u];",
        "if (ux / brick_x_size != chunk_x || uy / brick_y_size != chunk_y || uz / brick_z_size != chunk_z)",
        "output_pixels[pixel_y * params_u32[0] + pixel_x]",
    ] {
        assert!(
            shader.contains(required),
            "chunked cross-section shader missing draw-buffer contract fragment: {required}"
        );
    }
}

#[test]
fn chunked_f32_cross_section_shader_uses_draw_buffer_contract() {
    let shader = CROSS_SECTION_CHUNK_DISPLAY_F32_SHADER;

    for required in [
        "const F32_PAGE_TABLE_WORDS: u32 = 7u;",
        "const CHUNK_DRAW_WORDS: u32 = 8u;",
        "var<storage, read> chunk_draws: array<u32>;",
        "@group(0) @binding(5)",
        "let page_table_base = brick_linear * F32_PAGE_TABLE_WORDS;",
        "let brick_start_x = page_table[page_table_base + 4u];",
        "if (ux / brick_x_size != chunk_x || uy / brick_y_size != chunk_y || uz / brick_z_size != chunk_z)",
        "output_pixels[pixel_y * params_u32[0] + pixel_x] = value_record(sample.value);",
    ] {
        assert!(
            shader.contains(required),
            "chunked f32 cross-section shader missing draw-buffer contract fragment: {required}"
        );
    }
}
