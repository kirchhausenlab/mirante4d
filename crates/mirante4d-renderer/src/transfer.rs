use glam::DVec3;
use mirante4d_domain::{
    DisplayWindow, IsoLightState, LayerTransfer, Opacity, RgbColor, TransferCurve,
};
use mirante4d_render_api::CameraAxes;

use crate::{
    DvrRgbaFrame, IsoSurfaceFrameF32, IsoSurfaceFrameU16, MipImageF32, MipImageU16, RenderError,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntensityTransfer {
    visible: bool,
    window: DisplayWindow,
    color: RgbColor,
    opacity: Opacity,
    curve: TransferCurve,
    invert: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarDisplayTransfer {
    pub window: DisplayWindow,
    pub curve: TransferCurve,
    pub invert: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct IntensityChannelFrame<'a> {
    pub image: &'a MipImageU16,
    pub transfer: IntensityTransfer,
}

#[derive(Debug, Clone, Copy)]
pub struct IntensityChannelFrameF32<'a> {
    pub image: &'a MipImageF32,
    pub transfer: IntensityTransfer,
}

#[derive(Debug, Clone, Copy)]
pub struct IsoSurfaceChannelFrame<'a> {
    pub surface: &'a IsoSurfaceFrameU16,
    pub transfer: IntensityTransfer,
}

#[derive(Debug, Clone, Copy)]
pub struct IsoSurfaceChannelFrameF32<'a> {
    pub surface: &'a IsoSurfaceFrameF32,
    pub transfer: IntensityTransfer,
}

#[derive(Debug, Clone, Copy)]
pub struct DvrRgbaChannelFrame<'a> {
    pub frame: &'a DvrRgbaFrame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRgbaImage {
    pub width: u64,
    pub height: u64,
    pixels: Vec<u8>,
}

impl IntensityTransfer {
    pub fn new(visible: bool, transfer: LayerTransfer) -> Self {
        Self {
            visible,
            window: transfer.window(),
            color: transfer.color(),
            opacity: transfer.opacity(),
            curve: transfer.curve(),
            invert: transfer.invert(),
        }
    }

    pub const fn visible(self) -> bool {
        self.visible
    }

    pub const fn window(self) -> DisplayWindow {
        self.window
    }

    pub const fn color(self) -> RgbColor {
        self.color
    }

    pub const fn opacity(self) -> Opacity {
        self.opacity
    }

    pub const fn curve(self) -> TransferCurve {
        self.curve
    }

    pub const fn invert(self) -> bool {
        self.invert
    }

    pub fn layer_transfer(self) -> LayerTransfer {
        LayerTransfer::new(
            self.window,
            self.color,
            self.opacity,
            self.curve,
            self.invert,
        )
    }

    pub const fn with_curve(mut self, curve: TransferCurve) -> Self {
        self.curve = curve;
        self
    }

    pub const fn with_invert(mut self, invert: bool) -> Self {
        self.invert = invert;
        self
    }

    pub fn color_rgba(self) -> [f32; 4] {
        let [red, green, blue] = self.color().rgb();
        [red, green, blue, 1.0]
    }

    pub fn scalar_transfer(self) -> ScalarDisplayTransfer {
        ScalarDisplayTransfer::new(self.window(), self.curve(), self.invert())
    }
}

impl ScalarDisplayTransfer {
    pub fn new(window: DisplayWindow, curve: TransferCurve, invert: bool) -> Self {
        Self {
            window,
            curve,
            invert,
        }
    }

    pub fn from_intensity_transfer(transfer: IntensityTransfer) -> Self {
        Self::new(transfer.window(), transfer.curve(), transfer.invert())
    }

    pub fn identity_u16() -> Self {
        Self::new(
            DisplayWindow::new(0.0, f32::from(u16::MAX)).expect("identity window is valid"),
            TransferCurve::linear(),
            false,
        )
    }

    pub fn identity_f32() -> Self {
        Self::new(
            DisplayWindow::new(0.0, 1.0).expect("identity window is valid"),
            TransferCurve::linear(),
            false,
        )
    }

    pub fn map_source_value(self, value: f32) -> f32 {
        let window_width = self.window.high() - self.window.low();
        if window_width <= f32::EPSILON {
            return 0.0;
        }
        let normalized = ((value - self.window.low()) / window_width).clamp(0.0, 1.0);
        apply_invert(map_curve(self.curve, normalized), self.invert)
    }

    pub fn map_source_value_f64(self, value: f64) -> f64 {
        f64::from(self.map_source_value(value as f32))
    }
}

impl<'a> IntensityChannelFrame<'a> {
    pub fn new(image: &'a MipImageU16, transfer: IntensityTransfer) -> Self {
        Self { image, transfer }
    }
}

impl<'a> IntensityChannelFrameF32<'a> {
    pub fn new(image: &'a MipImageF32, transfer: IntensityTransfer) -> Self {
        Self { image, transfer }
    }
}

impl<'a> IsoSurfaceChannelFrame<'a> {
    pub fn new(surface: &'a IsoSurfaceFrameU16, transfer: IntensityTransfer) -> Self {
        Self { surface, transfer }
    }
}

impl<'a> IsoSurfaceChannelFrameF32<'a> {
    pub fn new(surface: &'a IsoSurfaceFrameF32, transfer: IntensityTransfer) -> Self {
        Self { surface, transfer }
    }
}

impl<'a> DvrRgbaChannelFrame<'a> {
    pub fn new(frame: &'a DvrRgbaFrame) -> Self {
        Self { frame }
    }
}

impl DisplayRgbaImage {
    pub fn new(width: u64, height: u64, pixels: Vec<u8>) -> Result<Self, RenderError> {
        let expected = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(RenderError::InvalidChannelComposite(
                "RGBA image dimensions exceed addressable memory",
            ))?;
        if pixels.len() != expected {
            return Err(RenderError::InvalidRgbaImageBuffer {
                width,
                height,
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    pub fn pixel_rgba(&self, x: u64, y: u64) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = ((y * self.width + x) * 4) as usize;
        Some([
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ])
    }
}

pub fn composite_intensity_channels(
    channels: &[IntensityChannelFrame<'_>],
) -> Result<DisplayRgbaImage, RenderError> {
    let first = channels
        .first()
        .ok_or(RenderError::InvalidChannelComposite(
            "at least one channel frame is required",
        ))?;
    let width = first.image.width;
    let height = first.image.height;
    let pixel_count = first.image.pixels().len();

    for channel in channels {
        if channel.image.width != width || channel.image.height != height {
            return Err(RenderError::InvalidChannelComposite(
                "all channel frames must have matching dimensions",
            ));
        }
        if channel.image.pixels().len() != pixel_count {
            return Err(RenderError::InvalidChannelComposite(
                "channel pixel count does not match image dimensions",
            ));
        }
    }

    let mut premultiplied = vec![[0.0_f32; 4]; pixel_count];
    for channel in channels {
        composite_one_channel(channel, &mut premultiplied);
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for [red, green, blue, alpha] in premultiplied {
        let alpha = alpha
            .clamp(0.0, 1.0)
            .max(red.max(green).max(blue).clamp(0.0, 1.0));
        if alpha <= f32::EPSILON {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            rgba.extend_from_slice(&[
                to_u8(red / alpha),
                to_u8(green / alpha),
                to_u8(blue / alpha),
                to_u8(alpha),
            ]);
        }
    }

    DisplayRgbaImage::new(width, height, rgba)
}

pub fn composite_f32_intensity_channels(
    channels: &[IntensityChannelFrameF32<'_>],
) -> Result<DisplayRgbaImage, RenderError> {
    let first = channels
        .first()
        .ok_or(RenderError::InvalidChannelComposite(
            "at least one channel frame is required",
        ))?;
    let width = first.image.width;
    let height = first.image.height;
    let pixel_count = first.image.pixels().len();

    for channel in channels {
        if channel.image.width != width || channel.image.height != height {
            return Err(RenderError::InvalidChannelComposite(
                "all channel frames must have matching dimensions",
            ));
        }
        if channel.image.pixels().len() != pixel_count {
            return Err(RenderError::InvalidChannelComposite(
                "channel pixel count does not match image dimensions",
            ));
        }
    }

    let mut premultiplied = vec![[0.0_f32; 4]; pixel_count];
    for channel in channels {
        composite_one_f32_channel(channel, &mut premultiplied);
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for [red, green, blue, alpha] in premultiplied {
        let alpha = alpha
            .clamp(0.0, 1.0)
            .max(red.max(green).max(blue).clamp(0.0, 1.0));
        if alpha <= f32::EPSILON {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            rgba.extend_from_slice(&[
                to_u8(red / alpha),
                to_u8(green / alpha),
                to_u8(blue / alpha),
                to_u8(alpha),
            ]);
        }
    }

    DisplayRgbaImage::new(width, height, rgba)
}

pub fn composite_iso_surface_channels(
    channels: &[IsoSurfaceChannelFrame<'_>],
    light_state: IsoLightState,
    camera_axes: CameraAxes,
) -> Result<DisplayRgbaImage, RenderError> {
    let first = channels
        .first()
        .ok_or(RenderError::InvalidChannelComposite(
            "at least one ISO surface channel frame is required",
        ))?;
    let width = first.surface.width;
    let height = first.surface.height;
    let pixel_count = first.surface.material_scalars().len();

    for channel in channels {
        if channel.surface.width != width || channel.surface.height != height {
            return Err(RenderError::InvalidChannelComposite(
                "all ISO surface frames must have matching dimensions",
            ));
        }
        if channel.surface.material_scalars().len() != pixel_count {
            return Err(RenderError::InvalidChannelComposite(
                "ISO surface pixel count does not match image dimensions",
            ));
        }
    }

    composite_iso_surface_pixels(
        pixel_count,
        width,
        height,
        channels,
        light_state,
        camera_axes,
        |channel, index| f32::from(channel.surface.material_scalars()[index]) / f32::from(u16::MAX),
    )
}

pub fn composite_iso_surface_f32_channels(
    channels: &[IsoSurfaceChannelFrameF32<'_>],
    light_state: IsoLightState,
    camera_axes: CameraAxes,
) -> Result<DisplayRgbaImage, RenderError> {
    let first = channels
        .first()
        .ok_or(RenderError::InvalidChannelComposite(
            "at least one ISO surface channel frame is required",
        ))?;
    let width = first.surface.width;
    let height = first.surface.height;
    let pixel_count = first.surface.material_scalars().len();

    for channel in channels {
        if channel.surface.width != width || channel.surface.height != height {
            return Err(RenderError::InvalidChannelComposite(
                "all ISO surface frames must have matching dimensions",
            ));
        }
        if channel.surface.material_scalars().len() != pixel_count {
            return Err(RenderError::InvalidChannelComposite(
                "ISO surface pixel count does not match image dimensions",
            ));
        }
    }

    composite_iso_surface_f32_pixels(
        pixel_count,
        width,
        height,
        channels,
        light_state,
        camera_axes,
        |channel, index| channel.surface.material_scalars()[index].clamp(0.0, 1.0),
    )
}

pub fn composite_dvr_rgba_channels(
    channels: &[DvrRgbaChannelFrame<'_>],
) -> Result<DisplayRgbaImage, RenderError> {
    let first = channels
        .first()
        .ok_or(RenderError::InvalidChannelComposite(
            "at least one DVR RGBA channel frame is required",
        ))?;
    let width = first.frame.width;
    let height = first.frame.height;
    let pixel_count = first.frame.premultiplied_rgba().len();

    for channel in channels {
        if channel.frame.width != width || channel.frame.height != height {
            return Err(RenderError::InvalidChannelComposite(
                "all DVR RGBA frames must have matching dimensions",
            ));
        }
        if channel.frame.premultiplied_rgba().len() != pixel_count {
            return Err(RenderError::InvalidChannelComposite(
                "DVR RGBA pixel count does not match image dimensions",
            ));
        }
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for index in 0..pixel_count {
        let mut out = [0.0; 4];
        for channel in channels {
            if channel.frame.is_covered_index(index) {
                source_over(&mut out, channel.frame.premultiplied_rgba()[index]);
            }
        }
        append_premultiplied_rgba(&mut rgba, out);
    }
    DisplayRgbaImage::new(width, height, rgba)
}

fn composite_one_channel(channel: &IntensityChannelFrame<'_>, premultiplied: &mut [[f32; 4]]) {
    let transfer = channel.transfer;
    if !transfer.visible() {
        return;
    }
    let window_width = transfer.window().high() - transfer.window().low();
    if window_width <= f32::EPSILON {
        return;
    }
    let [red, green, blue, color_alpha] = transfer.color_rgba();
    let channel_alpha = transfer.opacity().get().clamp(0.0, 1.0) * color_alpha.clamp(0.0, 1.0);
    if channel_alpha <= f32::EPSILON {
        return;
    }

    for (index, (&value, out)) in channel
        .image
        .pixels()
        .iter()
        .zip(premultiplied.iter_mut())
        .enumerate()
    {
        if !channel.image.is_covered_index(index) {
            continue;
        }
        let mapped = transfer
            .scalar_transfer()
            .map_source_value(f32::from(value));
        let lit_mapped = mapped * channel.image.surface_lighting_factor_index(index);
        out[0] = (out[0] + lit_mapped * red.clamp(0.0, 1.0) * channel_alpha).clamp(0.0, 1.0);
        out[1] = (out[1] + lit_mapped * green.clamp(0.0, 1.0) * channel_alpha).clamp(0.0, 1.0);
        out[2] = (out[2] + lit_mapped * blue.clamp(0.0, 1.0) * channel_alpha).clamp(0.0, 1.0);
        out[3] = 1.0 - (1.0 - out[3]) * (1.0 - channel_alpha);
    }
}

fn composite_one_f32_channel(
    channel: &IntensityChannelFrameF32<'_>,
    premultiplied: &mut [[f32; 4]],
) {
    let transfer = channel.transfer;
    if !transfer.visible() {
        return;
    }
    let window_width = transfer.window().high() - transfer.window().low();
    if window_width <= f32::EPSILON {
        return;
    }
    let [red, green, blue, color_alpha] = transfer.color_rgba();
    let channel_alpha = transfer.opacity().get().clamp(0.0, 1.0) * color_alpha.clamp(0.0, 1.0);
    if channel_alpha <= f32::EPSILON {
        return;
    }

    for (index, (&value, out)) in channel
        .image
        .pixels()
        .iter()
        .zip(premultiplied.iter_mut())
        .enumerate()
    {
        if !channel.image.is_covered_index(index) {
            continue;
        }
        let mapped = transfer.scalar_transfer().map_source_value(value);
        let lit_mapped = mapped * channel.image.surface_lighting_factor_index(index);
        out[0] = (out[0] + lit_mapped * red.clamp(0.0, 1.0) * channel_alpha).clamp(0.0, 1.0);
        out[1] = (out[1] + lit_mapped * green.clamp(0.0, 1.0) * channel_alpha).clamp(0.0, 1.0);
        out[2] = (out[2] + lit_mapped * blue.clamp(0.0, 1.0) * channel_alpha).clamp(0.0, 1.0);
        out[3] = 1.0 - (1.0 - out[3]) * (1.0 - channel_alpha);
    }
}

#[derive(Debug, Clone, Copy)]
struct IsoCompositeCandidate {
    channel_index: usize,
    depth: f32,
}

fn composite_iso_surface_pixels(
    pixel_count: usize,
    width: u64,
    height: u64,
    channels: &[IsoSurfaceChannelFrame<'_>],
    light_state: IsoLightState,
    camera_axes: CameraAxes,
    material_scalar: impl Fn(&IsoSurfaceChannelFrame<'_>, usize) -> f32,
) -> Result<DisplayRgbaImage, RenderError> {
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    let mut candidates = Vec::with_capacity(channels.len());
    let light = IsoLightVectors::new(light_state, camera_axes);
    for index in 0..pixel_count {
        candidates.clear();
        for (channel_index, channel) in channels.iter().enumerate() {
            if iso_surface_channel_visible(channel.transfer)
                && channel.surface.is_covered_index(index)
            {
                candidates.push(IsoCompositeCandidate {
                    channel_index,
                    depth: channel.surface.hit_depth()[index],
                });
            }
        }
        candidates.sort_by(|left, right| {
            right
                .depth
                .partial_cmp(&left.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.channel_index.cmp(&right.channel_index))
        });
        let color = composite_iso_candidates(index, channels, &candidates, &material_scalar, light);
        append_premultiplied_rgba(&mut rgba, color);
    }
    DisplayRgbaImage::new(width, height, rgba)
}

fn composite_iso_surface_f32_pixels(
    pixel_count: usize,
    width: u64,
    height: u64,
    channels: &[IsoSurfaceChannelFrameF32<'_>],
    light_state: IsoLightState,
    camera_axes: CameraAxes,
    material_scalar: impl Fn(&IsoSurfaceChannelFrameF32<'_>, usize) -> f32,
) -> Result<DisplayRgbaImage, RenderError> {
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    let mut candidates = Vec::with_capacity(channels.len());
    let light = IsoLightVectors::new(light_state, camera_axes);
    for index in 0..pixel_count {
        candidates.clear();
        for (channel_index, channel) in channels.iter().enumerate() {
            if iso_surface_channel_visible(channel.transfer)
                && channel.surface.is_covered_index(index)
            {
                candidates.push(IsoCompositeCandidate {
                    channel_index,
                    depth: channel.surface.hit_depth()[index],
                });
            }
        }
        candidates.sort_by(|left, right| {
            right
                .depth
                .partial_cmp(&left.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.channel_index.cmp(&right.channel_index))
        });
        let color =
            composite_iso_f32_candidates(index, channels, &candidates, &material_scalar, light);
        append_premultiplied_rgba(&mut rgba, color);
    }
    DisplayRgbaImage::new(width, height, rgba)
}

fn composite_iso_candidates(
    index: usize,
    channels: &[IsoSurfaceChannelFrame<'_>],
    candidates: &[IsoCompositeCandidate],
    material_scalar: &impl Fn(&IsoSurfaceChannelFrame<'_>, usize) -> f32,
    light: IsoLightVectors,
) -> [f32; 4] {
    let mut out = [0.0; 4];
    for candidate in candidates {
        let channel = &channels[candidate.channel_index];
        let surface = channel.surface;
        let lighting = iso_surface_lighting(surface.normals()[index], light);
        let source = iso_source_rgba(
            material_scalar(channel, index),
            lighting.diffuse,
            lighting.specular,
            channel.transfer,
        );
        source_over(&mut out, source);
    }
    out
}

fn composite_iso_f32_candidates(
    index: usize,
    channels: &[IsoSurfaceChannelFrameF32<'_>],
    candidates: &[IsoCompositeCandidate],
    material_scalar: &impl Fn(&IsoSurfaceChannelFrameF32<'_>, usize) -> f32,
    light: IsoLightVectors,
) -> [f32; 4] {
    let mut out = [0.0; 4];
    for candidate in candidates {
        let channel = &channels[candidate.channel_index];
        let surface = channel.surface;
        let lighting = iso_surface_lighting(surface.normals()[index], light);
        let source = iso_source_rgba(
            material_scalar(channel, index),
            lighting.diffuse,
            lighting.specular,
            channel.transfer,
        );
        source_over(&mut out, source);
    }
    out
}

const ISO_AMBIENT: f32 = 0.20;
const ISO_DIFFUSE: f32 = 0.80;
const ISO_SPECULAR: f32 = 0.25;
const ISO_SHININESS: f32 = 48.0;

#[derive(Debug, Clone, Copy)]
struct IsoLightVectors {
    light: [f32; 3],
    view: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
struct IsoLighting {
    diffuse: u16,
    specular: u16,
}

impl IsoLightVectors {
    fn new(light_state: IsoLightState, camera_axes: CameraAxes) -> Self {
        let forward = DVec3::from_array(camera_axes.forward());
        let light = crate::current_camera::iso_light_direction(light_state, camera_axes);
        Self {
            light: dvec3_to_f32(light),
            view: dvec3_to_f32((-forward).normalize_or_zero()),
        }
    }
}

fn iso_surface_lighting(normal: crate::IsoSurfaceNormal, light: IsoLightVectors) -> IsoLighting {
    let normal = normalize3(normal.components_f32());
    if length_squared3(normal) <= f32::EPSILON {
        return IsoLighting {
            diffuse: u16::MAX,
            specular: 0,
        };
    }
    let light_direction = normalize3(light.light);
    let view_direction = normalize3(light.view);
    if length_squared3(light_direction) <= f32::EPSILON
        || length_squared3(view_direction) <= f32::EPSILON
    {
        return IsoLighting {
            diffuse: u16::MAX,
            specular: 0,
        };
    }
    let diffuse =
        (ISO_AMBIENT + ISO_DIFFUSE * dot3(normal, light_direction).max(0.0)).clamp(0.0, 1.0);
    let half_vector = normalize3(add3(light_direction, view_direction));
    let specular = if length_squared3(half_vector) <= f32::EPSILON {
        0.0
    } else {
        ISO_SPECULAR * dot3(normal, half_vector).max(0.0).powf(ISO_SHININESS)
    };
    IsoLighting {
        diffuse: to_u16_lighting(diffuse),
        specular: to_u16_lighting(specular.clamp(0.0, 1.0)),
    }
}

fn dvec3_to_f32(value: glam::DVec3) -> [f32; 3] {
    [value.x as f32, value.y as f32, value.z as f32]
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn length_squared3(value: [f32; 3]) -> f32 {
    dot3(value, value)
}

fn normalize3(value: [f32; 3]) -> [f32; 3] {
    let length = length_squared3(value).sqrt();
    if length <= f32::EPSILON {
        [0.0, 0.0, 0.0]
    } else {
        [value[0] / length, value[1] / length, value[2] / length]
    }
}

fn to_u16_lighting(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}

fn iso_surface_channel_visible(transfer: IntensityTransfer) -> bool {
    transfer.visible()
        && transfer.opacity().get() > f32::EPSILON
        && transfer.color_rgba()[3] > f32::EPSILON
}

fn iso_source_rgba(
    material_scalar: f32,
    diffuse_lighting: u16,
    specular_lighting: u16,
    transfer: IntensityTransfer,
) -> [f32; 4] {
    let [red, green, blue, color_alpha] = transfer.color_rgba();
    let alpha = transfer.opacity().get().clamp(0.0, 1.0) * color_alpha.clamp(0.0, 1.0);
    let material = material_scalar.clamp(0.0, 1.0);
    let diffuse = f32::from(diffuse_lighting) / f32::from(u16::MAX);
    let specular = f32::from(specular_lighting) / f32::from(u16::MAX);
    let lit = material * diffuse;
    [
        (lit * red.clamp(0.0, 1.0) + specular).clamp(0.0, 1.0) * alpha,
        (lit * green.clamp(0.0, 1.0) + specular).clamp(0.0, 1.0) * alpha,
        (lit * blue.clamp(0.0, 1.0) + specular).clamp(0.0, 1.0) * alpha,
        alpha,
    ]
}

fn source_over(out: &mut [f32; 4], source: [f32; 4]) {
    let inverse_alpha = 1.0 - source[3].clamp(0.0, 1.0);
    out[0] = source[0] + out[0] * inverse_alpha;
    out[1] = source[1] + out[1] * inverse_alpha;
    out[2] = source[2] + out[2] * inverse_alpha;
    out[3] = source[3] + out[3] * inverse_alpha;
}

fn append_premultiplied_rgba(rgba: &mut Vec<u8>, [red, green, blue, alpha]: [f32; 4]) {
    let alpha = alpha
        .clamp(0.0, 1.0)
        .max(red.max(green).max(blue).clamp(0.0, 1.0));
    if alpha <= f32::EPSILON {
        rgba.extend_from_slice(&[0, 0, 0, 0]);
    } else {
        rgba.extend_from_slice(&[
            to_u8(red / alpha),
            to_u8(green / alpha),
            to_u8(blue / alpha),
            to_u8(alpha),
        ]);
    }
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn apply_invert(mapped: f32, invert: bool) -> f32 {
    if invert {
        1.0 - mapped.clamp(0.0, 1.0)
    } else {
        mapped
    }
}

fn map_curve(curve: TransferCurve, value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if curve.is_linear() {
        value
    } else {
        value.powf(1.0 / curve.gamma_value())
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use mirante4d_domain::{DisplayWindow, IsoLightState, Projection, RgbColor, TransferCurve};
    use mirante4d_render_api::CameraAxes;

    use crate::{IsoSurfaceFrameF32, IsoSurfaceFrameU16, IsoSurfaceNormal, PixelCoverage};

    use super::*;

    struct LayerDisplay {
        visible: bool,
        window: DisplayWindow,
        opacity: Opacity,
    }

    impl LayerDisplay {
        fn new(
            visible: bool,
            window: DisplayWindow,
            opacity: f32,
        ) -> Result<Self, mirante4d_domain::DisplayError> {
            Ok(Self {
                visible,
                window,
                opacity: Opacity::new(opacity)?,
            })
        }

        fn visible(&self) -> bool {
            self.visible
        }

        fn layer_transfer(&self, color: RgbColor) -> LayerTransfer {
            LayerTransfer::new(
                self.window,
                color,
                self.opacity,
                TransferCurve::linear(),
                false,
            )
        }
    }

    fn default_iso_light() -> (IsoLightState, CameraAxes) {
        let camera = crate::current_camera::frame_from_look_at(
            Projection::Orthographic,
            DVec3::ZERO,
            -DVec3::Z,
            DVec3::Y,
            10.0,
            320.0,
            crate::current_camera::presentation(512.0, 512.0),
        );
        (IsoLightState::attached_camera(), camera.axes())
    }

    #[test]
    fn single_channel_transfer_matches_window_and_opacity() {
        let frame = MipImageU16::new(4, 1, vec![500, 1_000, 1_058, 1_115]);
        let display =
            LayerDisplay::new(true, DisplayWindow::new(1_000.0, 1_115.0).unwrap(), 0.5).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_intensity_channels(&[IntensityChannelFrame::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color)),
        )])
        .unwrap();

        assert_eq!(
            image.pixels(),
            &[
                0, 0, 0, 128, //
                0, 0, 0, 128, //
                129, 129, 129, 128, //
                255, 255, 255, 128,
            ]
        );
    }

    #[test]
    fn multi_channel_composite_is_order_independent_additive_color() {
        let red_frame = MipImageU16::new(1, 1, vec![100]);
        let green_frame = MipImageU16::new(1, 1, vec![100]);
        let display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let red = RgbColor::new([1.0, 0.0, 0.0]).unwrap();
        let green = RgbColor::new([0.0, 1.0, 0.0]).unwrap();

        let red_green = composite_intensity_channels(&[
            IntensityChannelFrame::new(
                &red_frame,
                IntensityTransfer::new(display.visible(), display.layer_transfer(red)),
            ),
            IntensityChannelFrame::new(
                &green_frame,
                IntensityTransfer::new(display.visible(), display.layer_transfer(green)),
            ),
        ])
        .unwrap();
        let green_red = composite_intensity_channels(&[
            IntensityChannelFrame::new(
                &green_frame,
                IntensityTransfer::new(display.visible(), display.layer_transfer(green)),
            ),
            IntensityChannelFrame::new(
                &red_frame,
                IntensityTransfer::new(display.visible(), display.layer_transfer(red)),
            ),
        ])
        .unwrap();

        assert_eq!(red_green.pixels(), green_red.pixels());
        assert_eq!(red_green.pixel_rgba(0, 0), Some([255, 255, 0, 255]));
    }

    #[test]
    fn invisible_channel_does_not_contribute() {
        let frame = MipImageU16::new(1, 1, vec![100]);
        let display =
            LayerDisplay::new(false, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 0.0, 0.0]).unwrap();

        let image = composite_intensity_channels(&[IntensityChannelFrame::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color)),
        )])
        .unwrap();

        assert_eq!(image.pixel_rgba(0, 0), Some([0, 0, 0, 0]));
    }

    #[test]
    fn gamma_curve_changes_display_mapping_only() {
        let frame = MipImageU16::new(1, 1, vec![25]);
        let display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_intensity_channels(&[IntensityChannelFrame::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color))
                .with_curve(TransferCurve::gamma(2.0).unwrap()),
        )])
        .unwrap();

        assert_eq!(image.pixel_rgba(0, 0), Some([128, 128, 128, 255]));
        assert_eq!(frame.pixels(), &[25]);
    }

    #[test]
    fn surface_lighting_modulates_rgb_after_scalar_transfer() {
        let surface = IsoSurfaceFrameU16::try_new(
            1,
            1,
            vec![100],
            vec![100],
            vec![100],
            vec![0.0],
            vec![IsoSurfaceNormal::ZERO],
            vec![32_768],
            vec![0],
            PixelCoverage::All,
        )
        .unwrap();
        let frame = MipImageU16::try_new_with_iso_surface(
            1,
            1,
            vec![100],
            PixelCoverage::All,
            Some(surface),
        )
        .unwrap();
        let display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 0.0, 0.0]).unwrap();

        let image = composite_intensity_channels(&[IntensityChannelFrame::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color)),
        )])
        .unwrap();

        assert_eq!(frame.pixels(), &[100]);
        assert_eq!(image.pixel_rgba(0, 0), Some([128, 0, 0, 255]));
    }

    #[test]
    fn iso_surface_composite_depth_orders_overlapping_channels() {
        let far_surface = IsoSurfaceFrameU16::try_new(
            1,
            1,
            vec![1],
            vec![u16::MAX],
            vec![u16::MAX],
            vec![10.0],
            vec![IsoSurfaceNormal::ZERO],
            vec![u16::MAX],
            vec![0],
            PixelCoverage::All,
        )
        .unwrap();
        let near_surface = IsoSurfaceFrameU16::try_new(
            1,
            1,
            vec![1],
            vec![u16::MAX],
            vec![u16::MAX],
            vec![1.0],
            vec![IsoSurfaceNormal::ZERO],
            vec![u16::MAX],
            vec![0],
            PixelCoverage::All,
        )
        .unwrap();
        let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let far_red = IntensityTransfer::new(
            display.visible(),
            display.layer_transfer(RgbColor::new([1.0, 0.0, 0.0]).unwrap()),
        );
        let near_green = IntensityTransfer::new(
            display.visible(),
            display.layer_transfer(RgbColor::new([0.0, 1.0, 0.0]).unwrap()),
        );

        let (light_state, camera_axes) = default_iso_light();
        let far_then_near = composite_iso_surface_channels(
            &[
                IsoSurfaceChannelFrame::new(&far_surface, far_red),
                IsoSurfaceChannelFrame::new(&near_surface, near_green),
            ],
            light_state,
            camera_axes,
        )
        .unwrap();
        let near_then_far = composite_iso_surface_channels(
            &[
                IsoSurfaceChannelFrame::new(&near_surface, near_green),
                IsoSurfaceChannelFrame::new(&far_surface, far_red),
            ],
            light_state,
            camera_axes,
        )
        .unwrap();

        assert_eq!(far_then_near.pixel_rgba(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(near_then_far.pixel_rgba(0, 0), Some([0, 255, 0, 255]));
    }

    #[test]
    fn iso_surface_relighting_uses_cached_normals_not_baked_lighting_planes() {
        let normal = IsoSurfaceNormal::from_unit_components(0.0, 0.0, 1.0);
        let surface = IsoSurfaceFrameU16::try_new(
            1,
            1,
            vec![12_345],
            vec![u16::MAX],
            vec![u16::MAX / 2],
            vec![4.0],
            vec![normal],
            vec![0],
            vec![0],
            PixelCoverage::All,
        )
        .unwrap();
        let before = surface.clone();
        let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let transfer = IntensityTransfer::new(
            display.visible(),
            display.layer_transfer(RgbColor::new([1.0, 0.0, 0.0]).unwrap()),
        );
        let (attached, camera_axes) = default_iso_light();

        let attached_image = composite_iso_surface_channels(
            &[IsoSurfaceChannelFrame::new(&surface, transfer)],
            attached,
            camera_axes,
        )
        .unwrap();
        let detached_image = composite_iso_surface_channels(
            &[IsoSurfaceChannelFrame::new(&surface, transfer)],
            IsoLightState::detached_screen(1.0, 0.0).unwrap(),
            camera_axes,
        )
        .unwrap();

        assert_eq!(surface, before);
        assert_eq!(surface.diffuse_lighting(), &[0]);
        assert_eq!(surface.specular_lighting(), &[0]);
        assert_eq!(attached_image.pixel_rgba(0, 0), Some([191, 64, 64, 255]));
        assert_eq!(detached_image.pixel_rgba(0, 0), Some([25, 0, 0, 255]));
    }

    #[test]
    fn iso_surface_f32_relighting_matches_u16_light_state_contract() {
        let normal = IsoSurfaceNormal::from_unit_components(0.0, 0.0, 1.0);
        let surface = IsoSurfaceFrameF32::try_new(
            1,
            1,
            vec![0.5],
            vec![1.0],
            vec![0.5],
            vec![2.0],
            vec![normal],
            vec![0],
            vec![0],
            PixelCoverage::All,
        )
        .unwrap();
        let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let transfer = IntensityTransfer::new(
            display.visible(),
            display.layer_transfer(RgbColor::new([0.0, 1.0, 0.0]).unwrap()),
        );
        let (attached, camera_axes) = default_iso_light();

        let attached_image = composite_iso_surface_f32_channels(
            &[IsoSurfaceChannelFrameF32::new(&surface, transfer)],
            attached,
            camera_axes,
        )
        .unwrap();
        let detached_image = composite_iso_surface_f32_channels(
            &[IsoSurfaceChannelFrameF32::new(&surface, transfer)],
            IsoLightState::detached_screen(1.0, 0.0).unwrap(),
            camera_axes,
        )
        .unwrap();

        assert_eq!(attached_image.pixel_rgba(0, 0), Some([64, 191, 64, 255]));
        assert_eq!(detached_image.pixel_rgba(0, 0), Some([0, 26, 0, 255]));
    }

    #[test]
    fn invisible_iso_channel_does_not_participate_in_relighting() {
        let hidden_surface = IsoSurfaceFrameU16::try_new(
            1,
            1,
            vec![1],
            vec![u16::MAX],
            vec![u16::MAX],
            vec![0.5],
            vec![IsoSurfaceNormal::from_unit_components(0.0, 0.0, 1.0)],
            vec![u16::MAX],
            vec![u16::MAX],
            PixelCoverage::All,
        )
        .unwrap();
        let visible_surface = IsoSurfaceFrameU16::try_new(
            1,
            1,
            vec![1],
            vec![u16::MAX],
            vec![u16::MAX],
            vec![1.0],
            vec![IsoSurfaceNormal::ZERO],
            vec![u16::MAX],
            vec![0],
            PixelCoverage::All,
        )
        .unwrap();
        let hidden_display =
            LayerDisplay::new(false, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let visible_display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let hidden_transfer = IntensityTransfer::new(
            hidden_display.visible(),
            hidden_display.layer_transfer(RgbColor::new([1.0, 0.0, 0.0]).unwrap()),
        );
        let visible_transfer = IntensityTransfer::new(
            visible_display.visible(),
            visible_display.layer_transfer(RgbColor::new([0.0, 1.0, 0.0]).unwrap()),
        );
        let (light_state, camera_axes) = default_iso_light();

        let image = composite_iso_surface_channels(
            &[
                IsoSurfaceChannelFrame::new(&hidden_surface, hidden_transfer),
                IsoSurfaceChannelFrame::new(&visible_surface, visible_transfer),
            ],
            light_state,
            camera_axes,
        )
        .unwrap();

        assert_eq!(image.pixel_rgba(0, 0), Some([0, 255, 0, 255]));
    }

    #[test]
    fn invert_lut_reverses_mapped_display_values() {
        let frame = MipImageU16::new(3, 1, vec![0, 50, 100]);
        let display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_intensity_channels(&[IntensityChannelFrame::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color))
                .with_invert(true),
        )])
        .unwrap();

        assert_eq!(
            image.pixels(),
            &[
                255, 255, 255, 255, //
                128, 128, 128, 255, //
                0, 0, 0, 255,
            ]
        );
    }

    #[test]
    fn invert_lut_skips_uncovered_u16_pixels() {
        let frame = MipImageU16::with_coverage(3, 1, vec![0, 0, 100], vec![0, 1, 1]).unwrap();
        let display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_intensity_channels(&[IntensityChannelFrame::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color))
                .with_invert(true),
        )])
        .unwrap();

        assert_eq!(
            image.pixels(),
            &[
                0, 0, 0, 0, //
                255, 255, 255, 255, //
                0, 0, 0, 255,
            ]
        );
    }

    #[test]
    fn channel_dimensions_must_match() {
        let left = MipImageU16::new(1, 1, vec![1]);
        let right = MipImageU16::new(2, 1, vec![1, 2]);
        let display =
            LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let result = composite_intensity_channels(&[
            IntensityChannelFrame::new(
                &left,
                IntensityTransfer::new(display.visible(), display.layer_transfer(color)),
            ),
            IntensityChannelFrame::new(
                &right,
                IntensityTransfer::new(display.visible(), display.layer_transfer(color)),
            ),
        ]);

        assert_eq!(
            result.unwrap_err(),
            RenderError::InvalidChannelComposite(
                "all channel frames must have matching dimensions"
            )
        );
    }

    #[test]
    fn float32_channel_transfer_uses_explicit_display_window_without_quantizing_source_values() {
        let frame = MipImageF32::new(4, 1, vec![-1.0, 0.0, 0.5, 1.0]);
        let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_f32_intensity_channels(&[IntensityChannelFrameF32::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color)),
        )])
        .unwrap();

        assert_eq!(
            image.pixels(),
            &[
                0, 0, 0, 255, //
                0, 0, 0, 255, //
                128, 128, 128, 255, //
                255, 255, 255, 255,
            ]
        );
    }

    #[test]
    fn float32_invert_lut_reverses_mapped_display_values() {
        let frame = MipImageF32::new(3, 1, vec![0.0, 0.5, 1.0]);
        let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_f32_intensity_channels(&[IntensityChannelFrameF32::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color))
                .with_invert(true),
        )])
        .unwrap();

        assert_eq!(
            image.pixels(),
            &[
                255, 255, 255, 255, //
                128, 128, 128, 255, //
                0, 0, 0, 255,
            ]
        );
    }

    #[test]
    fn invert_lut_skips_uncovered_f32_pixels() {
        let frame = MipImageF32::with_coverage(3, 1, vec![0.0, 0.0, 1.0], vec![0, 1, 1]).unwrap();
        let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
        let color = RgbColor::new([1.0, 1.0, 1.0]).unwrap();

        let image = composite_f32_intensity_channels(&[IntensityChannelFrameF32::new(
            &frame,
            IntensityTransfer::new(display.visible(), display.layer_transfer(color))
                .with_invert(true),
        )])
        .unwrap();

        assert_eq!(
            image.pixels(),
            &[
                0, 0, 0, 0, //
                255, 255, 255, 255, //
                0, 0, 0, 255,
            ]
        );
    }

    #[test]
    fn coverage_mask_validation_rejects_invalid_inputs() {
        assert!(matches!(
            MipImageU16::with_coverage(2, 1, vec![0, 1], vec![1]),
            Err(RenderError::InvalidPixelCoverageBuffer { .. })
        ));
        assert!(matches!(
            MipImageF32::with_coverage(2, 1, vec![0.0, 1.0], vec![1, 2]),
            Err(RenderError::InvalidPixelCoverageValue { index: 1, value: 2 })
        ));
    }
}
