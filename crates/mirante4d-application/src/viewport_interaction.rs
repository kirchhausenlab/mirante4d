//! Framework-neutral state retained across one viewport drag.

use mirante4d_domain::CameraView;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportOrbitDrag {
    start_camera: CameraView,
}

impl ViewportOrbitDrag {
    pub const fn new(start_camera: CameraView) -> Self {
        Self { start_camera }
    }

    pub const fn start_camera(self) -> CameraView {
        self.start_camera
    }
}
