//! Moving, resizing, and turning selected layers: the drag, its preview, and
//! the ops it leaves. `transform_layer` moves each kind of layer by its own
//! values, and a text's frame resizes as the scene's own `Frame`.

use cursor_icon::CursorIcon;
use koharu_renderer::{Frame, Lettering as Laid, Renderer};
use koharu_scene::{
    Bounds, Content, Edit, Fit, Frame as TextFrame, LayerId, Op, Path, PathFit, Ref, Text,
};
use kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape as _};
use serde_json::{Value, json};

use crate::{
    Effect,
    control::{Control, Handle},
    draw::overlay::Overlay,
    gesture::Context,
    snapshot::Snapshot,
    view::Sample,
};

/// The shortest side a resize leaves, in a layer's own units: a page pixel
/// for every layer the canvas resizes but a scaled image.
const MINIMUM: f64 = 1.0;

/// What a drag on selected layers does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Move,
    Resize(Handle),
    Rotate,
}

/// Selected layers dragged, by their body or one's handle, from `start` to
/// `current` in page points.
pub(crate) struct Transform {
    mode: Mode,
    start: Point,
    current: Point,
    controls: Vec<Control>,
    /// What a move or a turn lifts out of the page, made once when it
    /// starts: the layers, the frame without them, and each one's own
    /// drawing beside the control it follows.
    lifted: Vec<LayerId>,
    under: Option<Frame>,
    drawn: Vec<(usize, Frame)>,
    /// What a resize re-wraps as it goes, laid out again in its new box at
    /// every move: each text's control, its text, and its lettering in the
    /// box the drag gives it now.
    rewrapped: Vec<Rewrap>,
    renderer: Renderer,
}

impl Transform {
    pub fn new(cx: &Context, mode: Mode, start: Point, controls: Vec<Control>) -> Self {
        let frame = cx.snapshot.frame();
        let rewrapped: Vec<Rewrap> = match mode {
            Mode::Resize(_) => controls
                .iter()
                .enumerate()
                .filter_map(|(index, control)| {
                    let content = &cx.snapshot.layer(control.layer)?.content;
                    match Lettering::of(content)? {
                        // A block's source isn't drawn, so there's nothing to re-wrap.
                        (Lettering::Source, _) => None,
                        (lettering, text) => Some(Rewrap {
                            index,
                            text: text.clone(),
                            lettering,
                            balloon: cx.snapshot.balloon(control.layer).cloned(),
                            laid: cx.renderer.lettering(control.layer, text).ok(),
                        }),
                    }
                })
                .collect(),
            Mode::Move | Mode::Rotate => Vec::new(),
        };
        let drawn: Vec<(usize, Frame)> = match mode {
            Mode::Resize(_) => Vec::new(),
            Mode::Move | Mode::Rotate => controls
                .iter()
                .enumerate()
                .filter_map(|(index, control)| {
                    // A block draws only its translation, so one without has
                    // nothing to lift; a clean-up isn't in the layer tree.
                    let content = cx.snapshot.layer(control.layer).map(|layer| &layer.content);
                    let drawn = match content {
                        Some(Content::Block(_)) => frame.translation(control.layer),
                        _ => frame.layer(control.layer),
                    };
                    drawn.map(|drawn| (index, drawn.cropped()))
                })
                .collect(),
        };
        let lifted: Vec<LayerId> = drawn
            .iter()
            .map(|(index, _)| *index)
            .chain(rewrapped.iter().map(|rewrap| rewrap.index))
            .map(|index| controls[index].layer)
            .collect();
        let under = (!lifted.is_empty()).then(|| frame.without(&lifted));
        Self {
            mode,
            start,
            current: start,
            controls,
            lifted,
            under,
            drawn,
            rewrapped,
            renderer: cx.renderer.clone(),
        }
    }

    pub fn current(&self) -> Point {
        self.current
    }

    pub fn lifted(&self) -> &[LayerId] {
        &self.lifted
    }

    pub fn under(&self) -> Option<&Frame> {
        self.under.as_ref()
    }

    pub fn moved(&mut self, samples: &[Sample]) {
        let Some(last) = samples.last() else {
            return;
        };
        self.current = last.page;
        let Mode::Resize(handle) = self.mode else {
            return;
        };
        for rewrap in &mut self.rewrapped {
            let control = &self.controls[rewrap.index];
            let local = control.resize_to(handle, self.current, MINIMUM);
            let framed = frame(
                &rewrap.text,
                local,
                rewrap.lettering,
                rewrap.balloon.as_ref(),
            )
            .map(|frame| Text {
                frame,
                ..rewrap.text.clone()
            });
            if let Some(framed) = framed {
                rewrap.laid = self.renderer.lettering(control.layer, &framed).ok();
            }
        }
    }

    pub fn cursor(&self) -> CursorIcon {
        match self.mode {
            Mode::Move => CursorIcon::Default,
            Mode::Resize(handle) => handle.cursor(),
            Mode::Rotate => Handle::Rotate.cursor(),
        }
    }

    /// The edit the drag leaves, if it changed anything.
    pub fn finish(self, cx: &Context) -> Vec<Effect> {
        let (label, ops): (&str, Vec<Op>) = match self.mode {
            Mode::Move | Mode::Rotate => {
                let ops = self
                    .controls
                    .iter()
                    .filter_map(|control| {
                        let affine = self.turn(control);
                        (affine != Affine::IDENTITY).then(|| transformed(control, affine))
                    })
                    .collect();
                (
                    if self.mode == Mode::Move {
                        "move"
                    } else {
                        "rotate"
                    },
                    ops,
                )
            }
            Mode::Resize(handle) => {
                let ops = self
                    .controls
                    .iter()
                    .filter_map(|control| {
                        let local = control.resize_to(handle, self.current, MINIMUM);
                        (local != control.local())
                            .then(|| resized(cx.snapshot, control, local))
                            .flatten()
                    })
                    .collect();
                ("resize", ops)
            }
        };
        if ops.is_empty() {
            return Vec::new();
        }
        vec![Effect::Edit(Edit {
            label: label.to_owned(),
            ops,
        })]
    }

    /// The controls, where the drag puts them.
    pub fn dragged(&self) -> Vec<Control> {
        self.controls
            .iter()
            .map(|control| match self.mode {
                Mode::Resize(handle) => {
                    control.with_local(control.resize_to(handle, self.current, MINIMUM))
                }
                Mode::Move | Mode::Rotate => control.moved(self.turn(control)),
            })
            .collect()
    }

    /// Each lifted layer's own drawing, where the drag takes it, and each
    /// resized text laid out in its box so far.
    pub fn preview(&self, overlay: &mut Overlay) -> anyhow::Result<()> {
        for (index, own) in &self.drawn {
            overlay.frame(own, self.turn(&self.controls[*index]))?;
        }
        for rewrap in &self.rewrapped {
            if let Some(laid) = &rewrap.laid {
                overlay.lettering(laid, Affine::from(rewrap.text.transform))?;
            }
        }
        Ok(())
    }

    /// What a move or a turn does to `control` so far, in page space.
    fn turn(&self, control: &Control) -> Affine {
        match self.mode {
            Mode::Move => Affine::translate(self.current - self.start),
            Mode::Rotate => control.turn(self.start, self.current),
            Mode::Resize(_) => Affine::IDENTITY,
        }
    }
}

/// A text a resize lays out again at every move: its control's index, its
/// text, where that text lives, the balloon it's in, and its lettering in
/// the box the drag gives it now.
struct Rewrap {
    index: usize,
    text: Text,
    lettering: Lettering,
    balloon: Option<BezPath>,
    laid: Option<Laid>,
}

/// The op that carries `control`'s layer along `affine`, a page-space move
/// or turn. A picture, which can't turn, moves by whole pixels, as the scene
/// takes it.
pub(crate) fn transformed(control: &Control, affine: Affine) -> Op {
    let affine = match control.rotates {
        true => affine,
        false => Affine::translate(affine.translation().round()),
    };
    Op::TransformLayer {
        layer: Ref::from(control.layer),
        transform: affine.into(),
    }
}

/// The op that gives `control` the frame `local` in its own coordinates,
/// for a resize; `None` for what doesn't resize. Text resizes its frame
/// without scaling its type, as Photoshop resizes a paragraph's box, and the
/// box it's given decides its size and wrapping; an image or a shape is
/// stretched.
pub(crate) fn resized(snapshot: &Snapshot, control: &Control, local: Rect) -> Option<Op> {
    let stretch = map(control.local(), local);
    let layer = Ref::from(control.layer);
    let content = &snapshot.layer(control.layer)?.content;
    if let Content::Image(_) | Content::Shape(_) = content {
        let page = control.transform() * stretch * control.transform().inverse();
        return Some(Op::TransformLayer {
            layer,
            transform: page.into(),
        });
    }
    let (lettering, text) = Lettering::of(content)?;
    let balloon = snapshot.balloon(control.layer);
    let mut frame = serde_json::to_value(frame(text, local, lettering, balloon)?).ok()?;
    // A merge patch switches the frame's kind by clearing the other.
    let other = if frame.get("box").is_some() {
        "path"
    } else {
        "box"
    };
    frame[other] = Value::Null;
    Some(Op::UpdateLayer {
        layer,
        patch: lettering.patch(frame),
    })
}

/// Where a layer's drawn text lives among its values: a text layer's own, a
/// block's translation, or a block's source while it has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lettering {
    Text,
    Translation,
    Source,
}

impl Lettering {
    pub fn of(content: &Content) -> Option<(Self, &Text)> {
        match content {
            Content::Text(text) => Some((Self::Text, text)),
            Content::Block(block) => Some(match &block.translation {
                Some(text) => (Self::Translation, text),
                None => (Self::Source, &block.source),
            }),
            _ => None,
        }
    }

    /// The layer's patch giving that text `frame`.
    pub fn patch(self, frame: Value) -> Value {
        match self {
            Self::Text => json!({"frame": frame}),
            Self::Translation => json!({"translation": {"frame": frame}}),
            Self::Source => json!({"source": {"frame": frame}}),
        }
    }
}

/// `text`'s frame resized to `local`, the rectangle the handles outline,
/// keeping how it fits. A text layer's is that box, as Photoshop resizes a
/// paragraph's. A block's translation flows in its balloon's shape within the
/// rectangle, so a round balloon's lines follow its curve, kept the default
/// inset off the edge; outside a balloon, in the rectangle itself.
fn frame(
    text: &Text,
    local: Rect,
    lettering: Lettering,
    balloon: Option<&BezPath>,
) -> Option<TextFrame> {
    let bounds = Bounds {
        x: local.x0,
        y: local.y0,
        width: local.width(),
        height: local.height(),
    };
    let (fit, anchor) = match &text.frame {
        TextFrame::Box { fit, anchor, .. } => (*fit, *anchor),
        TextFrame::Path { fit, anchor, .. } => (
            match fit {
                PathFit::Shrink => Fit::Shrink,
                PathFit::None => Fit::None,
                PathFit::Best => Fit::Best,
            },
            *anchor,
        ),
        TextFrame::Point => return None,
    };
    if lettering == Lettering::Text {
        return Some(TextFrame::Box {
            bounds,
            fit,
            anchor,
        });
    }
    let fit = match fit {
        Fit::Shrink | Fit::Grow => PathFit::Shrink,
        Fit::None => PathFit::None,
        Fit::Best => PathFit::Best,
    };
    let outline = balloon
        .and_then(|balloon| {
            let own = Affine::from(text.transform).inverse() * balloon.clone();
            clipped(&own, local)
        })
        .unwrap_or_else(|| local.to_path(0.1));
    Some(TextFrame::Path {
        path: Path::try_from(outline.to_svg()).ok()?,
        fit,
        anchor,
        inset: TextFrame::DEFAULT_INSET,
    })
}

/// `outline`, flattened to a polygon, cut to `rect` with Sutherland and
/// Hodgman's clip against each of its sides; `None` when nothing of it is
/// inside.
fn clipped(outline: &BezPath, rect: Rect) -> Option<BezPath> {
    let mut points = Vec::new();
    kurbo::flatten(outline.iter(), 0.25, |element| match element {
        PathEl::MoveTo(point) | PathEl::LineTo(point) => points.push(point),
        _ => {}
    });
    let sides: [(fn(Point, Rect) -> bool, fn(Point, Point, Rect) -> Point); 4] = [
        (|p, r| p.x >= r.x0, |a, b, r| across_x(a, b, r.x0)),
        (|p, r| p.x <= r.x1, |a, b, r| across_x(a, b, r.x1)),
        (|p, r| p.y >= r.y0, |a, b, r| across_y(a, b, r.y0)),
        (|p, r| p.y <= r.y1, |a, b, r| across_y(a, b, r.y1)),
    ];
    for (inside, cross) in sides {
        let input = std::mem::take(&mut points);
        for (at, &current) in input.iter().enumerate() {
            let previous = input[(at + input.len() - 1) % input.len()];
            match (inside(previous, rect), inside(current, rect)) {
                (true, true) => points.push(current),
                (true, false) => points.push(cross(previous, current, rect)),
                (false, true) => {
                    points.push(cross(previous, current, rect));
                    points.push(current);
                }
                (false, false) => {}
            }
        }
    }
    if points.len() < 3 {
        return None;
    }
    let mut path = BezPath::new();
    path.move_to(points[0]);
    for &point in &points[1..] {
        path.line_to(point);
    }
    path.close_path();
    Some(path)
}

/// Where the segment from `a` to `b` crosses the vertical line at `x`.
fn across_x(a: Point, b: Point, x: f64) -> Point {
    let t = (x - a.x) / (b.x - a.x);
    Point::new(x, a.y + (b.y - a.y) * t)
}

/// Where the segment from `a` to `b` crosses the horizontal line at `y`.
fn across_y(a: Point, b: Point, y: f64) -> Point {
    let t = (y - a.y) / (b.y - a.y);
    Point::new(a.x + (b.x - a.x) * t, y)
}

/// The affine taking `from` onto `to`.
fn map(from: Rect, to: Rect) -> Affine {
    Affine::translate(to.origin().to_vec2())
        * Affine::scale_non_uniform(to.width() / from.width(), to.height() / from.height())
        * Affine::translate(-from.origin().to_vec2())
}
