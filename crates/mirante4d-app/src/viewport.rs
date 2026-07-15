#[cfg(test)]
use mirante4d_application::viewport_interaction::default_camera_for_shape;
#[cfg(test)]
use mirante4d_domain::GridToWorld;
use mirante4d_domain::Shape3D;
use mirante4d_render_api::{DEFAULT_PRESENTATION_VIEWPORT, PresentationViewport, RenderExtent};

const DEFAULT_INITIAL_VIEWPORT_SIDE: u32 = 512;

pub(crate) fn default_render_viewport_for_shape(shape: Shape3D) -> anyhow::Result<RenderExtent> {
    let _ = shape;
    RenderExtent::new(DEFAULT_INITIAL_VIEWPORT_SIDE, DEFAULT_INITIAL_VIEWPORT_SIDE)
        .map_err(Into::into)
}

pub(crate) fn default_presentation_viewport() -> PresentationViewport {
    DEFAULT_PRESENTATION_VIEWPORT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_camera_targets_the_affine_world_center() {
        let shape = Shape3D::new(7, 5, 3).unwrap();
        let grid_to_world = GridToWorld::from_row_major([
            2.0, 0.0, 0.0, 10.0, 0.0, 3.0, 0.0, 20.0, 0.0, 0.0, 4.0, 30.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();

        let camera = default_camera_for_shape(shape, grid_to_world);

        assert_eq!(camera.target().components(), [12.0, 26.0, 42.0]);
    }
}
