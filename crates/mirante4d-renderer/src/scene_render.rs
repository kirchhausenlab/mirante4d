use glam::DVec3;
use mirante4d_domain::{LogicalLayerKey, Projection};
use mirante4d_render_api::CameraFrame;

use crate::{
    CoordinateSpace, RenderError, RenderViewport, SceneColorRgba, SceneDrawList, SceneGeometry,
    SceneLayerId, SceneLayerKind, SceneObjectId, SceneRenderPass, transform::GridToWorldExt,
};

const EPSILON: f64 = 1.0e-9;
const GLYPH_COLUMNS: usize = 5;
const GLYPH_PIXEL_PX: f32 = 2.0;
const GLYPH_ADVANCE_PX: f32 = 12.0;
const GLYPH_LINE_HEIGHT_PX: f32 = 16.0;
const MAX_SCREEN_LABEL_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneRgbaImage {
    pub width: u64,
    pub height: u64,
    pixels: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneRenderCommandKind {
    Point,
    LineSegment,
    Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneRenderCommand {
    pub kind: SceneRenderCommandKind,
    pub pass: SceneRenderPass,
    pub color: SceneColorRgba,
    pub pick_id: u32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub width_px: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneRenderCommandList {
    commands: Vec<SceneRenderCommand>,
    pick_records: Vec<SceneRenderPickRecord>,
    pub input_draw_items: usize,
    pub unsupported_draw_items: usize,
    pub skipped_draw_items: usize,
    pub commands_by_pass: [u64; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneRenderPickRecord {
    pub id: u32,
    pub layer_id: SceneLayerId,
    pub object_id: SceneObjectId,
    pub layer_kind: SceneLayerKind,
    pub source_layer_id: Option<LogicalLayerKey>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SceneRenderDiagnostics {
    pub input_draw_items: u64,
    pub render_commands: u64,
    pub unsupported_draw_items: u64,
    pub skipped_draw_items: u64,
    pub output_pixels: u64,
    pub changed_pixels: u64,
    pub command_buffer_bytes: u64,
    pub commands_by_pass: [u64; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneRenderOutput {
    pub image: SceneRgbaImage,
    pub diagnostics: SceneRenderDiagnostics,
}

impl SceneRgbaImage {
    pub fn new(width: u64, height: u64, pixels: Vec<u32>) -> Result<Self, RenderError> {
        RenderViewport::new(width, height)?;
        let expected = (width as usize).checked_mul(height as usize).ok_or(
            RenderError::InvalidRgbaImageBuffer {
                width,
                height,
                expected: usize::MAX,
                actual: pixels.len(),
            },
        )?;
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

    pub fn solid(width: u64, height: u64, color: SceneColorRgba) -> Result<Self, RenderError> {
        let viewport = RenderViewport::new(width, height)?;
        let pixel_count = (viewport.width as usize) * (viewport.height as usize);
        Self::new(width, height, vec![color.packed_rgba_u32(); pixel_count])
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u32> {
        self.pixels
    }

    pub fn pixel(&self, x: u64, y: u64) -> Option<SceneColorRgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels
            .get((y * self.width + x) as usize)
            .copied()
            .map(SceneColorRgba::from_packed_rgba_u32)
    }
}

impl SceneRenderCommandKind {
    pub fn shader_code(self) -> u32 {
        match self {
            Self::Point => 0,
            Self::LineSegment => 1,
            Self::Rect => 2,
        }
    }
}

impl SceneRenderCommand {
    fn point(
        pass: SceneRenderPass,
        color: SceneColorRgba,
        pick_id: u32,
        center: ScreenPoint,
        radius_px: f32,
    ) -> Self {
        Self {
            kind: SceneRenderCommandKind::Point,
            pass,
            color,
            pick_id,
            x0: center.x,
            y0: center.y,
            x1: 0.0,
            y1: 0.0,
            width_px: radius_px.max(0.0),
        }
    }

    fn line_segment(
        pass: SceneRenderPass,
        color: SceneColorRgba,
        pick_id: u32,
        start: ScreenPoint,
        end: ScreenPoint,
        width_px: f32,
    ) -> Self {
        Self {
            kind: SceneRenderCommandKind::LineSegment,
            pass,
            color,
            pick_id,
            x0: start.x,
            y0: start.y,
            x1: end.x,
            y1: end.y,
            width_px: width_px.max(1.0),
        }
    }

    fn rect(
        pass: SceneRenderPass,
        color: SceneColorRgba,
        pick_id: u32,
        min: ScreenPoint,
        max: ScreenPoint,
    ) -> Self {
        Self {
            kind: SceneRenderCommandKind::Rect,
            pass,
            color,
            pick_id,
            x0: min.x,
            y0: min.y,
            x1: max.x,
            y1: max.y,
            width_px: 0.0,
        }
    }

    pub fn shader_u32_fields(self) -> [u32; 4] {
        [
            self.kind.shader_code(),
            render_pass_index(self.pass) as u32,
            self.color.packed_rgba_u32(),
            self.pick_id,
        ]
    }

    pub fn shader_f32_fields(self) -> [f32; 6] {
        [self.x0, self.y0, self.x1, self.y1, self.width_px, 0.0]
    }
}

impl SceneRenderCommandList {
    pub fn commands(&self) -> &[SceneRenderCommand] {
        &self.commands
    }

    pub fn pick_records(&self) -> &[SceneRenderPickRecord] {
        &self.pick_records
    }

    pub fn pick_record(&self, pick_id: u32) -> Option<&SceneRenderPickRecord> {
        self.pick_records.iter().find(|record| record.id == pick_id)
    }

    pub fn command_buffer_bytes(&self) -> u64 {
        let bytes_per_command =
            (4 * std::mem::size_of::<u32>() + 6 * std::mem::size_of::<f32>()) as u64;
        self.commands.len() as u64 * bytes_per_command
    }
}

pub fn render_scene_layers_rgba_cpu(
    base: &SceneRgbaImage,
    draw_list: &SceneDrawList,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> Result<SceneRenderOutput, RenderError> {
    if base.width != viewport.width || base.height != viewport.height {
        return Err(RenderError::InvalidRgbaImageBuffer {
            width: viewport.width,
            height: viewport.height,
            expected: (viewport.width as usize) * (viewport.height as usize),
            actual: base.pixels().len(),
        });
    }
    let commands = build_scene_render_commands(draw_list, camera, viewport);
    let mut output_pixels = base.pixels().to_vec();
    for command in commands.commands() {
        draw_scene_render_command_cpu(&mut output_pixels, viewport, *command);
    }
    let diagnostics = scene_render_diagnostics(base, &commands, &output_pixels);
    Ok(SceneRenderOutput {
        image: SceneRgbaImage::new(viewport.width, viewport.height, output_pixels)?,
        diagnostics,
    })
}

pub fn build_scene_render_commands(
    draw_list: &SceneDrawList,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> SceneRenderCommandList {
    let projector = SceneProjector::new(camera, viewport);
    let mut occupied_screen_labels = Vec::new();
    let mut list = SceneRenderCommandList {
        commands: Vec::new(),
        pick_records: Vec::new(),
        input_draw_items: draw_list.len(),
        unsupported_draw_items: 0,
        skipped_draw_items: 0,
        commands_by_pass: [0; 3],
    };

    for item in draw_list.items() {
        let previous_len = list.commands.len();
        let supported = match &item.geometry {
            SceneGeometry::Point {
                position,
                radius_px,
            } => match project_item_point(&projector, item, *position) {
                Some(center) => {
                    let pick_id = push_scene_render_pick_record(&mut list, item);
                    list.commands.push(SceneRenderCommand::point(
                        item.pass,
                        item.style.color,
                        pick_id,
                        center,
                        *radius_px,
                    ));
                    true
                }
                None => item_coordinate_space_supported(item),
            },
            SceneGeometry::LineSegment {
                start,
                end,
                width_px,
            } => match (
                project_item_point(&projector, item, *start),
                project_item_point(&projector, item, *end),
            ) {
                (Some(start), Some(end)) => {
                    let pick_id = push_scene_render_pick_record(&mut list, item);
                    list.commands.push(SceneRenderCommand::line_segment(
                        item.pass,
                        item.style.color,
                        pick_id,
                        start,
                        end,
                        *width_px,
                    ));
                    true
                }
                _ => item_coordinate_space_supported(item),
            },
            SceneGeometry::Polyline { points, width_px } => {
                let mut emitted = false;
                let mut had_projection_failure = false;
                for segment in points.windows(2) {
                    match (
                        project_item_point(&projector, item, segment[0]),
                        project_item_point(&projector, item, segment[1]),
                    ) {
                        (Some(start), Some(end)) => {
                            let pick_id = push_scene_render_pick_record(&mut list, item);
                            list.commands.push(SceneRenderCommand::line_segment(
                                item.pass,
                                item.style.color,
                                pick_id,
                                start,
                                end,
                                *width_px,
                            ));
                            emitted = true;
                        }
                        _ => {
                            had_projection_failure = true;
                        }
                    }
                }
                emitted || (had_projection_failure && item_coordinate_space_supported(item))
            }
            SceneGeometry::Box3D { min, max } => {
                let corners = box_corners(*min, *max);
                for (start, end) in BOX_EDGES {
                    if let (Some(start), Some(end)) = (
                        project_item_point(&projector, item, corners[start]),
                        project_item_point(&projector, item, corners[end]),
                    ) {
                        let pick_id = push_scene_render_pick_record(&mut list, item);
                        list.commands.push(SceneRenderCommand::line_segment(
                            item.pass,
                            item.style.color,
                            pick_id,
                            start,
                            end,
                            1.0,
                        ));
                    }
                }
                item_coordinate_space_supported(item)
            }
            SceneGeometry::Ellipsoid { center, radii } => {
                match project_item_point(&projector, item, *center) {
                    Some(screen_center) => {
                        let pick_id = push_scene_render_pick_record(&mut list, item);
                        let radius_px =
                            projected_radius_px(&projector, item, *center, *radii).unwrap_or(4.0);
                        list.commands.push(SceneRenderCommand::point(
                            item.pass,
                            item.style.color,
                            pick_id,
                            screen_center,
                            radius_px.max(2.0),
                        ));
                        true
                    }
                    None => item_coordinate_space_supported(item),
                }
            }
            SceneGeometry::ScreenLabel { anchor, text } => {
                let anchor = ScreenPoint {
                    x: anchor.x,
                    y: anchor.y,
                };
                if let Some((placed_anchor, bounds)) =
                    place_screen_label(anchor, text, viewport, &occupied_screen_labels)
                {
                    let pick_id = push_scene_render_pick_record(&mut list, item);
                    push_screen_label_glyph_commands(
                        &mut list.commands,
                        item.pass,
                        item.style.color,
                        pick_id,
                        placed_anchor,
                        text,
                    );
                    occupied_screen_labels.push(bounds);
                    true
                } else {
                    true
                }
            }
        };

        if !supported {
            list.unsupported_draw_items += 1;
            continue;
        }
        if list.commands.len() == previous_len {
            list.skipped_draw_items += 1;
            continue;
        }
        let command_count = list.commands.len() - previous_len;
        list.commands_by_pass[render_pass_index(item.pass)] += command_count as u64;
    }

    list
}

pub fn scene_render_diagnostics(
    base: &SceneRgbaImage,
    commands: &SceneRenderCommandList,
    output_pixels: &[u32],
) -> SceneRenderDiagnostics {
    let changed_pixels = base
        .pixels()
        .iter()
        .zip(output_pixels)
        .filter(|(base, output)| base != output)
        .count();
    SceneRenderDiagnostics {
        input_draw_items: commands.input_draw_items as u64,
        render_commands: commands.commands.len() as u64,
        unsupported_draw_items: commands.unsupported_draw_items as u64,
        skipped_draw_items: commands.skipped_draw_items as u64,
        output_pixels: output_pixels.len() as u64,
        changed_pixels: changed_pixels as u64,
        command_buffer_bytes: commands.command_buffer_bytes(),
        commands_by_pass: commands.commands_by_pass,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct SceneProjector {
    camera: CameraFrame,
    viewport: RenderViewport,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
}

impl SceneProjector {
    pub fn new(camera: CameraFrame, viewport: RenderViewport) -> Self {
        let forward = crate::current_camera::forward(camera);
        let right = crate::current_camera::right(camera);
        let up = crate::current_camera::up(camera);
        Self {
            camera,
            viewport,
            forward,
            right,
            up,
        }
    }

    pub fn project_world(self, position: DVec3) -> Option<ScreenPoint> {
        let view = position - crate::current_camera::eye(self.camera);
        let depth = view.dot(self.forward);
        let (screen_x_points, screen_y_points) =
            match crate::current_camera::projection(self.camera) {
                Projection::Perspective => {
                    if depth <= EPSILON {
                        return None;
                    }
                    (
                        crate::current_camera::perspective_focal_length_screen_points(self.camera)
                            * view.dot(self.right)
                            / depth,
                        crate::current_camera::perspective_focal_length_screen_points(self.camera)
                            * view.dot(self.up)
                            / depth,
                    )
                }
                Projection::Orthographic => {
                    let from_target = position - crate::current_camera::target(self.camera);
                    (
                        from_target.dot(self.right)
                            / crate::current_camera::orthographic_world_per_screen_point(
                                self.camera,
                            ),
                        from_target.dot(self.up)
                            / crate::current_camera::orthographic_world_per_screen_point(
                                self.camera,
                            ),
                    )
                }
            };
        if !(screen_x_points.is_finite() && screen_y_points.is_finite()) {
            return None;
        }
        Some(ScreenPoint {
            x: ((screen_x_points / crate::current_camera::presentation_width_points(self.camera)
                + 0.5)
                * self.viewport.width as f64) as f32,
            y: ((0.5
                - screen_y_points / crate::current_camera::presentation_height_points(self.camera))
                * self.viewport.height as f64) as f32,
        })
    }
}

fn draw_scene_render_command_cpu(
    output_pixels: &mut [u32],
    viewport: RenderViewport,
    command: SceneRenderCommand,
) {
    match command.kind {
        SceneRenderCommandKind::Point => draw_point_cpu(output_pixels, viewport, command),
        SceneRenderCommandKind::LineSegment => {
            draw_line_segment_cpu(output_pixels, viewport, command)
        }
        SceneRenderCommandKind::Rect => draw_rect_cpu(output_pixels, viewport, command),
    }
}

fn draw_point_cpu(
    output_pixels: &mut [u32],
    viewport: RenderViewport,
    command: SceneRenderCommand,
) {
    let radius = command.width_px.max(0.0);
    let min_x = (command.x0 - radius).floor() as i64;
    let max_x = (command.x0 + radius).ceil() as i64;
    let min_y = (command.y0 - radius).floor() as i64;
    let max_y = (command.y0 + radius).ceil() as i64;
    let radius_sq = radius * radius;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 + 0.5 - command.x0;
            let dy = y as f32 + 0.5 - command.y0;
            if dx * dx + dy * dy <= radius_sq {
                blend_scene_pixel_cpu(output_pixels, viewport, x, y, command.color);
            }
        }
    }
}

fn draw_line_segment_cpu(
    output_pixels: &mut [u32],
    viewport: RenderViewport,
    command: SceneRenderCommand,
) {
    let radius = (command.width_px.max(1.0) * 0.5).max(0.5);
    let min_x = command.x0.min(command.x1).floor() as i64 - radius.ceil() as i64;
    let max_x = command.x0.max(command.x1).ceil() as i64 + radius.ceil() as i64;
    let min_y = command.y0.min(command.y1).floor() as i64 - radius.ceil() as i64;
    let max_y = command.y0.max(command.y1).ceil() as i64 + radius.ceil() as i64;
    let start = ScreenPoint {
        x: command.x0,
        y: command.y0,
    };
    let end = ScreenPoint {
        x: command.x1,
        y: command.y1,
    };
    let radius_sq = radius * radius;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let point = ScreenPoint {
                x: x as f32 + 0.5,
                y: y as f32 + 0.5,
            };
            if distance_to_screen_segment_squared(point, start, end) <= radius_sq {
                blend_scene_pixel_cpu(output_pixels, viewport, x, y, command.color);
            }
        }
    }
}

fn draw_rect_cpu(output_pixels: &mut [u32], viewport: RenderViewport, command: SceneRenderCommand) {
    let min_x = command.x0.min(command.x1).floor() as i64;
    let max_x = command.x0.max(command.x1).ceil() as i64;
    let min_y = command.y0.min(command.y1).floor() as i64;
    let max_y = command.y0.max(command.y1).ceil() as i64;
    for y in min_y..max_y {
        for x in min_x..max_x {
            blend_scene_pixel_cpu(output_pixels, viewport, x, y, command.color);
        }
    }
}

fn blend_scene_pixel_cpu(
    output_pixels: &mut [u32],
    viewport: RenderViewport,
    x: i64,
    y: i64,
    source: SceneColorRgba,
) {
    if x < 0 || y < 0 || x >= viewport.width as i64 || y >= viewport.height as i64 {
        return;
    }
    let index = (y as u64 * viewport.width + x as u64) as usize;
    let Some(destination) = output_pixels.get_mut(index) else {
        return;
    };
    let dest = SceneColorRgba::from_packed_rgba_u32(*destination);
    let alpha = f32::from(source.alpha) / 255.0;
    let inv_alpha = 1.0 - alpha;
    let blend_channel = |src: u8, dst: u8| -> u8 {
        (f32::from(src) * alpha + f32::from(dst) * inv_alpha).round() as u8
    };
    *destination = SceneColorRgba::new(
        blend_channel(source.red, dest.red),
        blend_channel(source.green, dest.green),
        blend_channel(source.blue, dest.blue),
        dest.alpha.max(source.alpha),
    )
    .packed_rgba_u32();
}

fn distance_to_screen_segment_squared(
    point: ScreenPoint,
    start: ScreenPoint,
    end: ScreenPoint,
) -> f32 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_sq = dx * dx + dy * dy;
    if length_sq <= f32::EPSILON {
        let px = point.x - start.x;
        let py = point.y - start.y;
        return px * px + py * py;
    }
    let t = (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_sq).clamp(0.0, 1.0);
    let closest = ScreenPoint {
        x: start.x + dx * t,
        y: start.y + dy * t,
    };
    let px = point.x - closest.x;
    let py = point.y - closest.y;
    px * px + py * py
}

fn project_item_point(
    projector: &SceneProjector,
    item: &crate::SceneDrawItem,
    position: DVec3,
) -> Option<ScreenPoint> {
    match &item.coordinate_space {
        CoordinateSpace::World => projector.project_world(position),
        CoordinateSpace::Grid { .. } => item.grid_to_world.and_then(|grid_to_world| {
            projector.project_world(grid_to_world.transform_point_vec(position))
        }),
        CoordinateSpace::Plane { .. } => item.plane_to_world.and_then(|plane_to_world| {
            projector.project_world(plane_to_world.transform_point3(position))
        }),
        CoordinateSpace::Screen => Some(ScreenPoint {
            x: position.x as f32,
            y: position.y as f32,
        }),
    }
}

fn projected_radius_px(
    projector: &SceneProjector,
    item: &crate::SceneDrawItem,
    center: DVec3,
    radii: DVec3,
) -> Option<f32> {
    let center = project_item_point(projector, item, center)?;
    let axis = if radii.x >= radii.y && radii.x >= radii.z {
        DVec3::X * radii.x
    } else if radii.y >= radii.x && radii.y >= radii.z {
        DVec3::Y * radii.y
    } else {
        DVec3::Z * radii.z
    };
    let edge = project_item_point(projector, item, center_world(item, axis)?)?;
    Some(((edge.x - center.x).powi(2) + (edge.y - center.y).powi(2)).sqrt())
}

fn center_world(item: &crate::SceneDrawItem, offset: DVec3) -> Option<DVec3> {
    if let SceneGeometry::Ellipsoid { center, .. } = &item.geometry {
        Some(*center + offset)
    } else {
        None
    }
}

fn item_coordinate_space_supported(item: &crate::SceneDrawItem) -> bool {
    match item.coordinate_space {
        CoordinateSpace::World | CoordinateSpace::Screen => true,
        CoordinateSpace::Grid { .. } => item.grid_to_world.is_some(),
        CoordinateSpace::Plane { .. } => item.plane_to_world.is_some(),
    }
}

fn source_layer_id_for_coordinate_space(
    coordinate_space: &CoordinateSpace,
) -> Option<LogicalLayerKey> {
    match coordinate_space {
        CoordinateSpace::Grid { layer_id } => Some(*layer_id),
        CoordinateSpace::World | CoordinateSpace::Plane { .. } | CoordinateSpace::Screen => None,
    }
}

fn push_screen_label_glyph_commands(
    commands: &mut Vec<SceneRenderCommand>,
    pass: SceneRenderPass,
    color: SceneColorRgba,
    pick_id: u32,
    anchor: ScreenPoint,
    text: &str,
) {
    let mut y = anchor.y;
    for line in text.lines().take(4) {
        let mut x = anchor.x;
        for character in line.chars().take(MAX_SCREEN_LABEL_CHARS) {
            let glyph = glyph_bitmap(character);
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..GLYPH_COLUMNS {
                    let mask = 1 << (GLYPH_COLUMNS - 1 - column);
                    if bits & mask == 0 {
                        continue;
                    }
                    let min = ScreenPoint {
                        x: x + column as f32 * GLYPH_PIXEL_PX,
                        y: y + row as f32 * GLYPH_PIXEL_PX,
                    };
                    let max = ScreenPoint {
                        x: min.x + GLYPH_PIXEL_PX,
                        y: min.y + GLYPH_PIXEL_PX,
                    };
                    commands.push(SceneRenderCommand::rect(pass, color, pick_id, min, max));
                }
            }
            x += GLYPH_ADVANCE_PX;
        }
        y += GLYPH_LINE_HEIGHT_PX;
    }
}

fn push_scene_render_pick_record(
    list: &mut SceneRenderCommandList,
    item: &crate::SceneDrawItem,
) -> u32 {
    if !item.selectable {
        return 0;
    }
    let Ok(id) = u32::try_from(list.pick_records.len() + 1) else {
        return 0;
    };
    list.pick_records.push(SceneRenderPickRecord {
        id,
        layer_id: item.layer_id.clone(),
        object_id: item.object_id.clone(),
        layer_kind: item.layer_kind,
        source_layer_id: source_layer_id_for_coordinate_space(&item.coordinate_space),
    });
    id
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenLabelBounds {
    min: ScreenPoint,
    max: ScreenPoint,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenLabelSize {
    width: f32,
    height: f32,
}

fn place_screen_label(
    anchor: ScreenPoint,
    text: &str,
    viewport: RenderViewport,
    occupied: &[ScreenLabelBounds],
) -> Option<(ScreenPoint, ScreenLabelBounds)> {
    let size = screen_label_size(text)?;
    let candidates = [
        anchor,
        ScreenPoint {
            x: anchor.x - size.width,
            y: anchor.y,
        },
        ScreenPoint {
            x: anchor.x,
            y: anchor.y - size.height,
        },
        ScreenPoint {
            x: anchor.x - size.width,
            y: anchor.y - size.height,
        },
        ScreenPoint {
            x: anchor.x,
            y: anchor.y + GLYPH_LINE_HEIGHT_PX,
        },
    ];
    for candidate in candidates {
        let placed_anchor = clamp_screen_label_anchor(candidate, size, viewport);
        let bounds = screen_label_bounds_from_size(placed_anchor, size);
        if !screen_label_overlaps(bounds, occupied) {
            return Some((placed_anchor, bounds));
        }
    }
    None
}

fn screen_label_size(text: &str) -> Option<ScreenLabelSize> {
    let line_count = text.lines().take(4).count();
    if line_count == 0 {
        return None;
    }
    let max_line_chars = text
        .lines()
        .take(4)
        .map(|line| line.chars().take(MAX_SCREEN_LABEL_CHARS).count())
        .max()
        .unwrap_or(0);
    if max_line_chars == 0 {
        return None;
    }
    Some(ScreenLabelSize {
        width: max_line_chars as f32 * GLYPH_ADVANCE_PX,
        height: line_count as f32 * GLYPH_LINE_HEIGHT_PX,
    })
}

fn clamp_screen_label_anchor(
    anchor: ScreenPoint,
    size: ScreenLabelSize,
    viewport: RenderViewport,
) -> ScreenPoint {
    ScreenPoint {
        x: anchor
            .x
            .clamp(0.0, (viewport.width as f32 - size.width).max(0.0)),
        y: anchor
            .y
            .clamp(0.0, (viewport.height as f32 - size.height).max(0.0)),
    }
}

fn screen_label_bounds_from_size(anchor: ScreenPoint, size: ScreenLabelSize) -> ScreenLabelBounds {
    ScreenLabelBounds {
        min: anchor,
        max: ScreenPoint {
            x: anchor.x + size.width,
            y: anchor.y + size.height,
        },
    }
}

fn screen_label_overlaps(bounds: ScreenLabelBounds, occupied: &[ScreenLabelBounds]) -> bool {
    occupied.iter().any(|other| {
        bounds.min.x < other.max.x
            && bounds.max.x > other.min.x
            && bounds.min.y < other.max.y
            && bounds.max.y > other.min.y
    })
}

fn glyph_bitmap(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        ':' => [
            0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        ' ' => [0; 7],
        _ => [
            0b11111, 0b10001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
    }
}

const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0),
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

fn box_corners(min: DVec3, max: DVec3) -> [DVec3; 8] {
    [
        DVec3::new(min.x, min.y, min.z),
        DVec3::new(max.x, min.y, min.z),
        DVec3::new(min.x, max.y, min.z),
        DVec3::new(max.x, max.y, min.z),
        DVec3::new(min.x, min.y, max.z),
        DVec3::new(max.x, min.y, max.z),
        DVec3::new(min.x, max.y, max.z),
        DVec3::new(max.x, max.y, max.z),
    ]
}

fn render_pass_index(pass: SceneRenderPass) -> usize {
    match pass {
        SceneRenderPass::WorldSpace => 0,
        SceneRenderPass::Interaction => 1,
        SceneRenderPass::ScreenSpace => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CoordinateSpace, OcclusionPolicy, SceneFrameContext, SceneGeometry, SceneLayer,
        SceneLayerId, SceneLayerKind, SceneObject, SceneObjectId, SceneStyle,
        extract_scene_draw_list,
    };
    use glam::DMat4;
    use mirante4d_domain::TimeIndex;
    use mirante4d_render_api::CameraFrame;

    fn camera() -> CameraFrame {
        camera_with_height(
            Projection::Orthographic,
            DVec3::new(0.0, 0.0, 10.0),
            DVec3::ZERO,
            DVec3::Y,
            10.0,
        )
    }

    fn camera_with_height(
        projection: Projection,
        eye: DVec3,
        target: DVec3,
        up: DVec3,
        height: f64,
    ) -> CameraFrame {
        crate::current_camera::frame_from_look_at(
            projection,
            eye,
            target,
            up,
            1.0,
            height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
            crate::current_camera::presentation(height, height),
        )
    }

    #[test]
    fn rgba_image_validates_buffer_length() {
        assert!(SceneRgbaImage::solid(2, 2, SceneColorRgba::WHITE).is_ok());
        assert_eq!(
            SceneRgbaImage::new(2, 2, vec![0; 3]).unwrap_err(),
            RenderError::InvalidRgbaImageBuffer {
                width: 2,
                height: 2,
                expected: 4,
                actual: 3,
            }
        );
    }

    #[test]
    fn cpu_scene_renderer_composites_projected_commands() {
        let viewport = RenderViewport::new(32, 32).unwrap();
        let camera = camera_with_height(
            Projection::Orthographic,
            DVec3::new(0.0, 0.0, 10.0),
            DVec3::ZERO,
            DVec3::Y,
            16.0,
        );
        let draw_list = extract_scene_draw_list(
            &[SceneLayer::new(
                SceneLayerId::new("handles").unwrap(),
                SceneLayerKind::Interaction,
            )
            .with_object(
                SceneObject::new(
                    SceneObjectId::new("handle-a").unwrap(),
                    CoordinateSpace::World,
                    crate::SceneTime::Timepoint(TimeIndex::new(0)),
                    OcclusionPolicy::AlwaysOnTop,
                    SceneGeometry::Point {
                        position: DVec3::ZERO,
                        radius_px: 3.0,
                    },
                )
                .with_style(SceneStyle::new(SceneColorRgba::MAGENTA)),
            )],
            SceneFrameContext::new(TimeIndex::new(0)),
        );
        let base = SceneRgbaImage::solid(32, 32, SceneColorRgba::new(0, 0, 0, 255)).unwrap();

        let output = render_scene_layers_rgba_cpu(&base, &draw_list, camera, viewport).unwrap();

        assert!(output.diagnostics.changed_pixels > 0);
        assert_eq!(output.image.pixel(16, 16), Some(SceneColorRgba::MAGENTA));
    }

    #[test]
    fn scene_style_flows_from_layer_or_object_to_draw_item() {
        let layer_style = SceneStyle::new(SceneColorRgba::new(10, 20, 30, 255));
        let object_style = SceneStyle::new(SceneColorRgba::new(200, 100, 50, 255));
        let styled_object = SceneObject::new(
            SceneObjectId::new("object-style").unwrap(),
            CoordinateSpace::World,
            crate::SceneTime::Static,
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::Point {
                position: DVec3::ZERO,
                radius_px: 2.0,
            },
        )
        .with_style(object_style);
        let default_object = SceneObject::new(
            SceneObjectId::new("layer-style").unwrap(),
            CoordinateSpace::World,
            crate::SceneTime::Static,
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::Point {
                position: DVec3::X,
                radius_px: 2.0,
            },
        );
        let layer = SceneLayer::new(
            SceneLayerId::new("annotations").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_style(layer_style)
        .with_object(styled_object)
        .with_object(default_object);
        let draw_list =
            extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0)));

        let styled = draw_list
            .items()
            .iter()
            .find(|item| item.object_id.as_str() == "object-style")
            .unwrap();
        let defaulted = draw_list
            .items()
            .iter()
            .find(|item| item.object_id.as_str() == "layer-style")
            .unwrap();
        assert_eq!(styled.style, object_style);
        assert_eq!(defaulted.style, layer_style);
    }

    #[test]
    fn command_extraction_assigns_pick_ids_to_selectable_commands() {
        let selectable = SceneObject::new(
            SceneObjectId::new("selectable").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::Point {
                position: DVec3::new(10.0, 10.0, 0.0),
                radius_px: 3.0,
            },
        );
        let non_selectable = SceneObject::new(
            SceneObjectId::new("display-only").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::Point {
                position: DVec3::new(20.0, 20.0, 0.0),
                radius_px: 3.0,
            },
        )
        .non_selectable();
        let layer = SceneLayer::new(
            SceneLayerId::new("annotations").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_object(non_selectable)
        .with_object(selectable);
        let draw_list =
            extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0)));

        let commands =
            build_scene_render_commands(&draw_list, camera(), RenderViewport::new(32, 32).unwrap());

        assert_eq!(commands.pick_records().len(), 1);
        assert_eq!(commands.pick_records()[0].object_id.as_str(), "selectable");
        assert!(
            commands
                .commands()
                .iter()
                .any(|command| command.pick_id == commands.pick_records()[0].id)
        );
        assert!(
            commands
                .commands()
                .iter()
                .any(|command| command.pick_id == 0)
        );
    }

    #[test]
    fn command_extraction_projects_world_space_line() {
        let layer = SceneLayer::new(SceneLayerId::new("tracks").unwrap(), SceneLayerKind::Track)
            .with_style(SceneStyle::new(SceneColorRgba::new(255, 0, 0, 255)))
            .with_object(SceneObject::new(
                SceneObjectId::new("track-a").unwrap(),
                CoordinateSpace::World,
                crate::SceneTime::Static,
                OcclusionPolicy::VolumeDepthCued,
                SceneGeometry::LineSegment {
                    start: DVec3::new(-1.0, 0.0, 0.0),
                    end: DVec3::new(1.0, 0.0, 0.0),
                    width_px: 3.0,
                },
            ));
        let draw_list =
            extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0)));
        let commands = build_scene_render_commands(
            &draw_list,
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );

        assert_eq!(commands.commands().len(), 1);
        let command = commands.commands()[0];
        assert_eq!(command.kind, SceneRenderCommandKind::LineSegment);
        assert_eq!(command.x0.round(), 40.0);
        assert_eq!(command.x1.round(), 60.0);
        assert_eq!(command.y0.round(), 50.0);
        assert_eq!(command.y1.round(), 50.0);
        assert_eq!(commands.commands_by_pass, [1, 0, 0]);
    }

    #[test]
    fn command_extraction_expands_box_edges_and_screen_label_glyphs() {
        let layer = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("box").unwrap(),
            CoordinateSpace::World,
            crate::SceneTime::Static,
            OcclusionPolicy::DepthTestGeometry,
            SceneGeometry::Box3D {
                min: DVec3::new(-1.0, -1.0, 0.0),
                max: DVec3::new(1.0, 1.0, 1.0),
            },
        ))
        .with_object(SceneObject::new(
            SceneObjectId::new("label").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(4.0, 5.0),
                text: "t0".to_owned(),
            },
        ));
        let draw_list =
            extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0)));
        let commands = build_scene_render_commands(
            &draw_list,
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );

        assert!(commands.commands().len() > 13);
        assert_eq!(
            commands
                .commands()
                .iter()
                .filter(|command| command.kind == SceneRenderCommandKind::LineSegment)
                .count(),
            12
        );
        assert!(
            commands
                .commands()
                .iter()
                .filter(|command| command.kind == SceneRenderCommandKind::Rect)
                .count()
                > 1
        );
        assert_eq!(commands.commands_by_pass[0], 12);
        assert!(commands.commands_by_pass[2] > 1);
    }

    #[test]
    fn command_extraction_projects_grid_space_with_registered_transform() {
        let source_layer_id = LogicalLayerKey::new(0);
        let layer = SceneLayer::new(
            SceneLayerId::new("grid-objects").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("grid-point").unwrap(),
            CoordinateSpace::Grid {
                layer_id: source_layer_id,
            },
            crate::SceneTime::Static,
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::Point {
                position: DVec3::new(1.0, 0.0, 0.0),
                radius_px: 2.0,
            },
        ));
        let draw_list = extract_scene_draw_list(
            &[layer],
            SceneFrameContext::new(TimeIndex::new(0)).with_grid_to_world(
                source_layer_id,
                mirante4d_domain::GridToWorld::scale(2.0, 1.0, 1.0).unwrap(),
            ),
        );
        let commands = build_scene_render_commands(
            &draw_list,
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );

        assert_eq!(commands.commands().len(), 1);
        let command = commands.commands()[0];
        assert_eq!(command.kind, SceneRenderCommandKind::Point);
        assert_eq!(command.x0.round(), 70.0);
        assert_eq!(command.y0.round(), 50.0);
        assert_eq!(commands.unsupported_draw_items, 0);
    }

    #[test]
    fn command_extraction_projects_plane_space_with_registered_transform() {
        let plane_id = crate::PlaneId::new("reference-plane").unwrap();
        let layer = SceneLayer::new(
            SceneLayerId::new("plane-objects").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("plane-point").unwrap(),
            CoordinateSpace::Plane {
                plane_id: plane_id.clone(),
            },
            crate::SceneTime::Static,
            OcclusionPolicy::AlwaysOnTop,
            SceneGeometry::Point {
                position: DVec3::new(1.0, 0.0, 0.0),
                radius_px: 2.0,
            },
        ));
        let draw_list = extract_scene_draw_list(
            &[layer],
            SceneFrameContext::new(TimeIndex::new(0))
                .with_plane_to_world(plane_id, DMat4::from_translation(DVec3::new(2.0, 0.0, 0.0))),
        );
        let commands = build_scene_render_commands(
            &draw_list,
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );

        assert_eq!(commands.commands().len(), 1);
        let command = commands.commands()[0];
        assert_eq!(command.kind, SceneRenderCommandKind::Point);
        assert_eq!(command.x0.round(), 80.0);
        assert_eq!(command.y0.round(), 50.0);
        assert_eq!(commands.unsupported_draw_items, 0);
    }

    #[test]
    fn screen_label_glyph_commands_depend_on_text() {
        let text_a = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("label-a").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(4.0, 5.0),
                text: "A".to_owned(),
            },
        ));
        let text_ab = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("label-ab").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(4.0, 5.0),
                text: "AB".to_owned(),
            },
        ));
        let a_commands = build_scene_render_commands(
            &extract_scene_draw_list(&[text_a], SceneFrameContext::new(TimeIndex::new(0))),
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );
        let ab_commands = build_scene_render_commands(
            &extract_scene_draw_list(&[text_ab], SceneFrameContext::new(TimeIndex::new(0))),
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );

        assert!(a_commands.commands_by_pass[2] > 1);
        assert!(ab_commands.commands_by_pass[2] > a_commands.commands_by_pass[2]);
    }

    #[test]
    fn screen_label_layout_repositions_overlapping_labels() {
        let layer = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("label-a").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(10.0, 10.0),
                text: "ROI-A".to_owned(),
            },
        ))
        .with_object(SceneObject::new(
            SceneObjectId::new("label-b").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(12.0, 12.0),
                text: "ROI-B".to_owned(),
            },
        ));
        let commands = build_scene_render_commands(
            &extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0))),
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );
        let expected_commands =
            (glyph_command_count("ROI-A") + glyph_command_count("ROI-B")) as u64;

        assert_eq!(commands.commands_by_pass[2], expected_commands);
        assert_eq!(commands.skipped_draw_items, 0);
    }

    #[test]
    fn screen_label_decluttering_skips_when_no_candidate_fits() {
        let layer = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("label-a").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(0.0, 0.0),
                text: "ROI-A".to_owned(),
            },
        ))
        .with_object(SceneObject::new(
            SceneObjectId::new("label-b").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(0.0, 0.0),
                text: "ROI-B".to_owned(),
            },
        ));
        let commands = build_scene_render_commands(
            &extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0))),
            camera(),
            RenderViewport::new(40, 16).unwrap(),
        );
        let expected_first_label_commands = glyph_command_count("ROI-A") as u64;

        assert_eq!(commands.commands_by_pass[2], expected_first_label_commands);
        assert_eq!(commands.skipped_draw_items, 1);
    }

    #[test]
    fn screen_label_decluttering_keeps_non_overlapping_labels() {
        let layer = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("label-a").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(10.0, 10.0),
                text: "A".to_owned(),
            },
        ))
        .with_object(SceneObject::new(
            SceneObjectId::new("label-b").unwrap(),
            CoordinateSpace::Screen,
            crate::SceneTime::Static,
            OcclusionPolicy::ScreenSpace,
            SceneGeometry::ScreenLabel {
                anchor: crate::ScreenPosition::new(40.0, 10.0),
                text: "B".to_owned(),
            },
        ));
        let commands = build_scene_render_commands(
            &extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0))),
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );
        let expected_commands = (glyph_command_count("A") + glyph_command_count("B")) as u64;

        assert_eq!(commands.commands_by_pass[2], expected_commands);
        assert_eq!(commands.skipped_draw_items, 0);
    }

    fn glyph_command_count(text: &str) -> usize {
        text.lines()
            .take(4)
            .flat_map(|line| line.chars().take(MAX_SCREEN_LABEL_CHARS))
            .map(|character| {
                glyph_bitmap(character)
                    .into_iter()
                    .map(|bits| bits.count_ones() as usize)
                    .sum::<usize>()
            })
            .sum()
    }

    #[test]
    fn unsupported_coordinate_space_is_reported() {
        let layer_id = LogicalLayerKey::new(0);
        let layer = SceneLayer::new(
            SceneLayerId::new("grid-objects").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_object(SceneObject::new(
            SceneObjectId::new("grid-point").unwrap(),
            CoordinateSpace::Grid { layer_id },
            crate::SceneTime::Static,
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::Point {
                position: DVec3::ZERO,
                radius_px: 2.0,
            },
        ));
        let draw_list =
            extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex::new(0)));
        let commands = build_scene_render_commands(
            &draw_list,
            camera(),
            RenderViewport::new(100, 100).unwrap(),
        );

        assert!(commands.commands().is_empty());
        assert_eq!(commands.unsupported_draw_items, 1);
        assert_eq!(commands.skipped_draw_items, 0);
    }
}
