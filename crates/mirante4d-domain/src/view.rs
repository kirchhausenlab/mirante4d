use thiserror::Error;

use crate::{UnitQuaternion, WorldPoint3};

#[derive(Debug, Error, Clone, Copy, PartialEq)]
pub enum ViewError {
    #[error("{field} must be finite and positive")]
    InvalidPositive { field: &'static str },
    #[error("detached ISO light coordinates must be finite")]
    NonFiniteDetachedLight,
    #[error("detached ISO light coordinates must lie inside the unit disc")]
    DetachedLightOutsideUnitDisc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Projection {
    Perspective,
    Orthographic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraView {
    projection: Projection,
    target: WorldPoint3,
    orientation: UnitQuaternion,
    orthographic_world_per_screen_point: f64,
    perspective_focal_length_screen_points: f64,
    perspective_view_distance_world: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewerLayout {
    Single3d,
    FourPanel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionView {
    center_world: WorldPoint3,
    orientation: UnitQuaternion,
    scale_world_per_screen_point: f64,
    depth_world: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IsoLightState(IsoLightKind);

#[derive(Debug, Clone, Copy, PartialEq)]
enum IsoLightKind {
    AttachedCamera,
    DetachedScreen { x: f32, y: f32 },
}

impl CameraView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        projection: Projection,
        target: WorldPoint3,
        orientation: UnitQuaternion,
        orthographic_world_per_screen_point: f64,
        perspective_focal_length_screen_points: f64,
        perspective_view_distance_world: f64,
    ) -> Result<Self, ViewError> {
        validate_positive(
            "orthographic_world_per_screen_point",
            orthographic_world_per_screen_point,
        )?;
        validate_positive(
            "perspective_focal_length_screen_points",
            perspective_focal_length_screen_points,
        )?;
        validate_positive(
            "perspective_view_distance_world",
            perspective_view_distance_world,
        )?;
        Ok(Self {
            projection,
            target,
            orientation,
            orthographic_world_per_screen_point,
            perspective_focal_length_screen_points,
            perspective_view_distance_world,
        })
    }

    pub const fn projection(self) -> Projection {
        self.projection
    }

    pub const fn target(self) -> WorldPoint3 {
        self.target
    }

    pub const fn orientation(self) -> UnitQuaternion {
        self.orientation
    }

    pub const fn orthographic_world_per_screen_point(self) -> f64 {
        self.orthographic_world_per_screen_point
    }

    pub const fn perspective_focal_length_screen_points(self) -> f64 {
        self.perspective_focal_length_screen_points
    }

    pub const fn perspective_view_distance_world(self) -> f64 {
        self.perspective_view_distance_world
    }
}

impl CrossSectionView {
    pub fn new(
        center_world: WorldPoint3,
        orientation: UnitQuaternion,
        scale_world_per_screen_point: f64,
        depth_world: f64,
    ) -> Result<Self, ViewError> {
        validate_positive(
            "cross_section.scale_world_per_screen_point",
            scale_world_per_screen_point,
        )?;
        validate_positive("cross_section.depth_world", depth_world)?;
        Ok(Self {
            center_world,
            orientation,
            scale_world_per_screen_point,
            depth_world,
        })
    }

    pub const fn center_world(self) -> WorldPoint3 {
        self.center_world
    }

    pub const fn orientation(self) -> UnitQuaternion {
        self.orientation
    }

    pub const fn scale_world_per_screen_point(self) -> f64 {
        self.scale_world_per_screen_point
    }

    pub const fn depth_world(self) -> f64 {
        self.depth_world
    }
}

impl IsoLightState {
    pub const fn attached_camera() -> Self {
        Self(IsoLightKind::AttachedCamera)
    }

    pub fn detached_screen(x: f32, y: f32) -> Result<Self, ViewError> {
        if !x.is_finite() || !y.is_finite() {
            return Err(ViewError::NonFiniteDetachedLight);
        }
        if x * x + y * y > 1.0 {
            return Err(ViewError::DetachedLightOutsideUnitDisc);
        }
        Ok(Self(IsoLightKind::DetachedScreen {
            x: canonical_zero(x),
            y: canonical_zero(y),
        }))
    }

    pub const fn is_attached_camera(self) -> bool {
        matches!(self.0, IsoLightKind::AttachedCamera)
    }

    pub const fn detached_screen_position(self) -> Option<[f32; 2]> {
        match self.0 {
            IsoLightKind::AttachedCamera => None,
            IsoLightKind::DetachedScreen { x, y } => Some([x, y]),
        }
    }
}

impl Default for IsoLightState {
    fn default() -> Self {
        Self::attached_camera()
    }
}

fn validate_positive(field: &'static str, value: f64) -> Result<(), ViewError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(ViewError::InvalidPositive { field })
    }
}

fn canonical_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::*;

    fn camera() -> CameraView {
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            0.25,
            320.0,
            40.0,
        )
        .unwrap()
    }

    #[test]
    fn camera_view_preserves_validated_framing_values() {
        let camera = camera();
        assert_eq!(camera.projection(), Projection::Orthographic);
        assert_eq!(camera.orientation(), UnitQuaternion::identity());
        assert_eq!(camera.orthographic_world_per_screen_point(), 0.25);
    }

    #[test]
    fn camera_view_rejects_nonpositive_scales() {
        assert_eq!(
            CameraView::new(
                Projection::Perspective,
                WorldPoint3::origin(),
                UnitQuaternion::identity(),
                1.0,
                0.0,
                1.0,
            ),
            Err(ViewError::InvalidPositive {
                field: "perspective_focal_length_screen_points"
            })
        );
    }

    #[test]
    fn cross_section_view_rejects_invalid_depth() {
        assert_eq!(
            CrossSectionView::new(
                WorldPoint3::origin(),
                UnitQuaternion::identity(),
                1.0,
                f64::NAN,
            ),
            Err(ViewError::InvalidPositive {
                field: "cross_section.depth_world"
            })
        );
    }

    #[test]
    fn detached_light_rejects_values_outside_the_unit_disc() {
        assert_eq!(
            IsoLightState::detached_screen(1.0, 1.0),
            Err(ViewError::DetachedLightOutsideUnitDisc)
        );
        assert_eq!(
            IsoLightState::detached_screen(0.25, -0.5)
                .unwrap()
                .detached_screen_position(),
            Some([0.25, -0.5])
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_444f_4d56_4945),
            ..ProptestConfig::default()
        })]

        #[test]
        fn valid_cross_section_scales_round_trip(scale in 1.0e-9_f64..1.0e6, depth in 1.0e-9_f64..1.0e6) {
            let view = CrossSectionView::new(
                WorldPoint3::origin(),
                UnitQuaternion::identity(),
                scale,
                depth,
            ).unwrap();
            prop_assert_eq!(view.scale_world_per_screen_point(), scale);
            prop_assert_eq!(view.depth_world(), depth);
        }
    }
}
