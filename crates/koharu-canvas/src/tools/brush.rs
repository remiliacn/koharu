//! The brush tools, draw, eraser, and remove: a stroke's outline, which the
//! preview and the coverage both fill, and paint and erasure on a raster's
//! straight-alpha pixels.

use image::{RgbaImage, imageops};
use koharu_scene::{Content, Edit, LayerId, Op, PixelFormat, Raster, Ref, Rgba};
use kurbo::{
    Affine, BezPath, Cap, Circle, Join, Point, Shape as _, Size, Stroke as Style, StrokeOpts,
    stroke,
};
use vello_cpu::{Pixmap, RenderContext, Resources, color::palette::css::WHITE};

use super::added;
use crate::{
    Effect, Notice, Tool,
    draw::overlay::{MASKS, Overlay},
    gesture::{Context, Gesture, GestureLook, Press},
    view::Sample,
};

/// How closely a stroke's outline follows its round caps and joins, in page
/// pixels.
const TOLERANCE: f64 = 0.1;

/// A brush tool's press starts a stroke. While the masks show, the draw
/// tool and the eraser stroke them, whatever is selected, as the fork's
/// mask overlay took them. Otherwise the draw tool paints the one selected
/// raster or a new one, and the eraser erases the one selected or, with
/// none, the clean-ups and fills under it, as the fork's eraser cleared its
/// clean-up layer back to the scan.
pub(crate) fn press(cx: &Context, sample: Sample) -> Press {
    let selected = cx
        .selection
        .single()
        .filter(|&layer| cx.snapshot.writable(layer).is_some());
    let onto = match (cx.tool, selected) {
        (Tool::Remove, _) => Onto::Page,
        _ if cx.masks => Onto::Masks,
        (_, Some(layer)) => Onto::Layer(layer),
        (Tool::Eraser, None) => Onto::Fills,
        (_, None) => Onto::Page,
    };
    let erasable = || {
        cx.snapshot
            .fills()
            .iter()
            .any(|fill| cx.snapshot.writable(fill.picture).is_some())
    };
    if onto == Onto::Fills && !erasable() {
        return Press::answers(vec![Effect::Notice(Notice::EraserNeedsRaster)]);
    }
    let points = vec![sample.page];
    Press::starts(Gesture::Stroke(Stroke {
        tool: cx.tool,
        outline: outline(&points, cx.brush.diameter),
        points,
        onto,
    }))
}

/// Where a stroke goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Onto {
    /// The one selected raster, clean-up, or mask.
    Layer(LayerId),
    /// Every inpainting mask it reaches.
    Masks,
    /// The eraser's: every clean-up and fill under it.
    Fills,
    /// The draw tool's new raster, or the remove tool's inpainting.
    Page,
}

/// A brush stroke through page points.
pub(crate) struct Stroke {
    tool: Tool,
    points: Vec<Point>,
    onto: Onto,
    /// The points' outline, expanded again only when a point arrives.
    outline: BezPath,
}

impl Stroke {
    pub fn moved(&mut self, cx: &Context, samples: &[Sample]) -> Vec<Effect> {
        let before = self.points.len();
        for sample in samples {
            if self.points.last() != Some(&sample.page) {
                self.points.push(sample.page);
            }
        }
        if self.points.len() != before {
            self.outline = outline(&self.points, cx.brush.diameter);
        }
        Vec::new()
    }

    /// The remove tool's inpaint request, or the draw tool's or the eraser's
    /// edit.
    pub fn finish(self, cx: &Context) -> anyhow::Result<Vec<Effect>> {
        let page = cx.snapshot.page().id;
        let Some((rect, coverage)) = coverage(&self.outline, cx.snapshot.size()) else {
            return Ok(Vec::new());
        };
        if self.tool == Tool::Remove {
            return Ok(vec![Effect::Inpaint {
                page,
                rect,
                mask: coverage,
            }]);
        }
        self.stroked(cx, rect, &coverage)
    }

    pub fn preview(&self, cx: &Context, overlay: &mut Overlay) {
        // A mask takes coverage, not colour.
        let colored = match self.onto {
            Onto::Layer(layer) => cx
                .snapshot
                .writable(layer)
                .is_none_or(|(_, format)| format == PixelFormat::Rgba8),
            Onto::Masks => false,
            Onto::Fills | Onto::Page => true,
        };
        let tint = match self.tool {
            Tool::Draw if colored => cx.brush.color,
            Tool::Draw if self.onto == Onto::Masks => MASKS,
            _ => {
                let Rgba([r, g, b, _]) = cx.colors.selection;
                Rgba([r, g, b, 0.4])
            }
        };
        overlay.fill(&self.outline, tint);
    }

    pub fn look(&self) -> GestureLook {
        GestureLook::Stroke {
            points: self.points.len(),
            last: self.points.last().copied().unwrap_or_default(),
        }
    }

    /// A paint or erase stroke's coverage over the page pixels `rect` as one
    /// edit of each raster, clean-up, or mask it goes to; painting onto the
    /// page adds a raster at its top, selected. A mask takes coverage on an alpha scale, as Clip Studio
    /// Paint's does: a stroke of any colour uncovers the clean-up, and the
    /// eraser hides it.
    fn stroked(
        &self,
        cx: &Context,
        rect: koharu_scene::Rect,
        coverage: &[u8],
    ) -> anyhow::Result<Vec<Effect>> {
        let label = match self.tool {
            Tool::Draw => "paint",
            _ => "erase",
        };
        let bounds = kurbo::Rect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.x) + f64::from(rect.width),
            f64::from(rect.y) + f64::from(rect.height),
        );
        let fills = || cx.snapshot.fills().into_iter();
        let layers: Vec<LayerId> = match self.onto {
            Onto::Layer(layer) => vec![layer],
            // Painting reaches the masks of the fills it's over; erasing
            // clears every mask, wherever it has grown.
            Onto::Masks => fills()
                .filter(|fill| {
                    self.tool != Tool::Draw
                        || fill.region.is_some_and(|region| region.overlaps(bounds))
                })
                .filter_map(|fill| fill.mask.map(|(mask, _)| mask))
                .collect(),
            Onto::Fills => fills().map(|fill| fill.picture).collect(),
            Onto::Page => Vec::new(),
        };
        let mut ops = Vec::new();
        for layer in layers {
            ops.extend(self.written(cx, layer, rect, coverage)?);
        }
        let mut created = None;
        if self.onto == Onto::Page && self.tool == Tool::Draw {
            let pixels = paint(rect, coverage, cx.brush.color).into_raw();
            let (id, add) = added(cx.snapshot.page(), Content::Raster(Raster::default()));
            ops.push(add);
            ops.push(Op::WritePixels {
                layer: Ref::from(id),
                rect,
                pixels,
            });
            created = Some(id);
        }
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let edit = Effect::Edit(Edit {
            label: label.to_owned(),
            ops,
        });
        Ok(std::iter::once(edit)
            .chain(created.map(|id| Effect::Selection(vec![id])))
            .collect())
    }

    /// The stroke's coverage over the page pixels `rect` written to `layer`,
    /// read and composited in its own coordinates; `None` when the brush
    /// can't write it or the stroke changes nothing there.
    fn written(
        &self,
        cx: &Context,
        layer: LayerId,
        rect: koharu_scene::Rect,
        coverage: &[u8],
    ) -> anyhow::Result<Option<Op>> {
        let Some((offset, format)) = cx.snapshot.writable(layer) else {
            return Ok(None);
        };
        let own = koharu_scene::Rect {
            x: rect.x - offset.x,
            y: rect.y - offset.y,
            ..rect
        };
        let before = cx.session.lock().pixels(layer, own)?;
        let mut pixels = before.clone();
        match (self.tool, format) {
            (Tool::Draw, PixelFormat::Rgba8) => {
                let mut under = RgbaImage::from_raw(own.width, own.height, pixels)
                    .ok_or_else(|| anyhow::anyhow!("a raster's pixels are RGBA"))?;
                imageops::overlay(&mut under, &paint(rect, coverage, cx.brush.color), 0, 0);
                pixels = under.into_raw();
            }
            (Tool::Draw, PixelFormat::Gray8) => {
                uncovered(&mut pixels, coverage, cx.brush.color.0[3]);
            }
            (_, PixelFormat::Rgba8) => erased(&mut pixels, coverage, 4),
            (_, PixelFormat::Gray8) => erased(&mut pixels, coverage, 1),
        }
        Ok((pixels != before).then(|| Op::WritePixels {
            layer: Ref::from(layer),
            rect: own,
            pixels,
        }))
    }
}

/// A round stroke through `points`, `diameter` wide, as the outline it
/// fills; a single point is a dot.
fn outline(points: &[Point], diameter: f64) -> BezPath {
    match points {
        [] => BezPath::new(),
        [point] => Circle::new(*point, diameter * 0.5).to_path(TOLERANCE),
        [first, rest @ ..] => {
            let mut line = BezPath::new();
            line.move_to(*first);
            for point in rest {
                line.line_to(*point);
            }
            let style = Style::new(diameter)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            stroke(line.iter(), &style, &StrokeOpts::default(), TOLERANCE)
        }
    }
}

/// `outline`'s antialiased 8-bit coverage over the page pixels it touches.
pub(crate) fn coverage(outline: &BezPath, page: Size) -> Option<(koharu_scene::Rect, Vec<u8>)> {
    let rect = koharu_scene::Rect::covering(
        outline.bounding_box(),
        page.width as u32,
        page.height as u32,
    )?;
    let (width, height) = (
        u16::try_from(rect.width).ok()?,
        u16::try_from(rect.height).ok()?,
    );
    let mut context = RenderContext::new(width, height);
    context.set_transform(Affine::translate((-f64::from(rect.x), -f64::from(rect.y))));
    context.set_paint(WHITE);
    context.fill_path(outline);
    context.flush();
    let mut pixmap = Pixmap::new(width, height);
    context.render(&mut pixmap, &mut Resources::new());
    Some((rect, pixmap.data().iter().map(|pixel| pixel.a).collect()))
}

/// The brush's `color` over `rect`, as opaque as `coverage` says of it.
fn paint(rect: koharu_scene::Rect, coverage: &[u8], Rgba([r, g, b, a]): Rgba) -> RgbaImage {
    let [r, g, b] = [r, g, b].map(|channel| (channel * 255.0).round() as u8);
    let pixels = coverage
        .iter()
        .flat_map(|&covered| [r, g, b, (f32::from(covered) * a).round() as u8])
        .collect();
    RgbaImage::from_raw(rect.width, rect.height, pixels).expect("coverage covers `rect`")
}

/// `pixels`, `bytes` to a pixel, with their last channel, a raster's alpha
/// or a mask's grey, taken away where `coverage` says.
fn erased(pixels: &mut [u8], coverage: &[u8], bytes: usize) {
    for (pixel, &covered) in pixels.chunks_exact_mut(bytes).zip(coverage) {
        let kept = 1.0 - f32::from(covered) / 255.0;
        pixel[bytes - 1] = (f32::from(pixel[bytes - 1]) * kept).round() as u8;
    }
}

/// A mask's grey `pixels` raised toward white where `coverage` says, as
/// opaque as the brush's `alpha`.
fn uncovered(pixels: &mut [u8], coverage: &[u8], alpha: f32) {
    for (pixel, &covered) in pixels.iter_mut().zip(coverage) {
        let added = f32::from(covered) / 255.0 * alpha;
        let grey = f32::from(*pixel);
        *pixel = (grey + (255.0 - grey) * added).round() as u8;
    }
}
