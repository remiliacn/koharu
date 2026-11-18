//! The ops adding a page's detected parts: panels, balloons, and blocks, each
//! block with its mask.

use std::collections::HashMap;

use anyhow::Result;
use image::{GrayImage, Luma, RgbImage};
use imageproc::{distance_transform::Norm, morphology::dilate};
use koharu_ml::koharu_layout_highres::{Kind, Layout, Lettering, Mask, Typography};
use koharu_scene::{Bounds, LayerId, Op, PageId, Rect, Ref, Rgba, Transform};
use kurbo::{Affine, Point, Shape as _};
use serde_json::{Value, json};

use super::{
    outline::outlined,
    tree::{Node, Part, Tree},
};
use crate::stages::action;

/// What the parts are added from: the page's layout, each lettering's
/// typography, and the page's pixels, as the detector read them.
pub(super) struct Found<'a> {
    pub(super) layout: &'a Layout,
    pub(super) typography: &'a [Option<Typography>],
    pub(super) pixels: &'a RgbImage,
}

/// Where a lettering found in an area goes: into `parent`, an existing
/// panel or balloon, or the page, at `index`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Place {
    pub(super) parent: Option<LayerId>,
    pub(super) index: usize,
}

/// The ops adding the layout's parts at the top of the page, before the
/// layers left there, with each block's mask kept out of `kept`, the blocks
/// the page keeps, and the labels of the panels and balloons added.
pub(super) fn additions(
    found: &Found,
    page: PageId,
    model: &str,
    kept: &[[f32; 4]],
) -> Result<(Vec<Op>, Labels)> {
    let tree = Tree::new(found.layout);
    let mut adding = Adding::new(found, page, model, None, kept);
    adding.add(&tree.top, None)?;
    Ok((adding.ops, adding.labels_of))
}

/// Whether a kept block's source box holds lettering `bbox`'s centre: that
/// lettering is the block's, and isn't added again.
pub(super) fn claimed([x0, y0, x1, y1]: [f32; 4], kept: &[[f32; 4]]) -> bool {
    let (x, y) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    kept.iter().any(|&[left, top, right, bottom]| {
        (left..=right).contains(&x) && (top..=bottom).contains(&y)
    })
}

/// The label each detected panel and balloon was added under, by its index
/// in the layout.
#[derive(Default)]
pub(super) struct Labels {
    panels: HashMap<usize, String>,
    bubbles: HashMap<usize, String>,
}

impl Labels {
    /// The added balloon whose outline holds `point`, else the added panel
    /// whose box does.
    pub(super) fn holding(&self, layout: &Layout, point: Point) -> Option<String> {
        let bubble = self
            .bubbles
            .iter()
            .find(|(at, _)| layout.bubbles[**at].outline.contains(point));
        let panel = || {
            self.panels.iter().find(|(at, _)| {
                let [x0, y0, x1, y1] = layout.panels[**at].bbox.map(f64::from);
                kurbo::Rect::new(x0, y0, x1, y1).contains(point)
            })
        };
        bubble.or_else(panel).map(|(_, label)| label.clone())
    }
}

/// The ops adding an area's lettering, which has no panel or balloon of its
/// own: each block at its place, its mask kept out of `kept`, the blocks the
/// page keeps.
pub(super) fn placed(
    found: &Found,
    page: PageId,
    model: &str,
    places: &[Place],
    kept: &[[f32; 4]],
) -> Result<Vec<Op>> {
    let tree = Tree::new(found.layout);
    let mut adding = Adding::new(found, page, model, Some(places), kept);
    adding.add(&tree.top, None)?;
    Ok(adding.ops)
}

struct Adding<'a> {
    layout: &'a Layout,
    typography: &'a [Option<Typography>],
    pixels: &'a RgbImage,
    page: PageId,
    model: &'a str,
    /// Each lettering's place, by its index, when the layout is an area's.
    places: Option<&'a [Place]>,
    kept: &'a [[f32; 4]],
    ops: Vec<Op>,
    labels: usize,
    labels_of: Labels,
}

impl<'a> Adding<'a> {
    fn new(
        found: &Found<'a>,
        page: PageId,
        model: &'a str,
        places: Option<&'a [Place]>,
        kept: &'a [[f32; 4]],
    ) -> Self {
        Self {
            layout: found.layout,
            typography: found.typography,
            pixels: found.pixels,
            page,
            model,
            places,
            kept,
            ops: Vec::new(),
            labels: 0,
            labels_of: Labels::default(),
        }
    }

    fn add(&mut self, nodes: &[Node], parent: Option<&str>) -> Result<()> {
        // Lettering a kept block holds is skipped, so siblings count on.
        let mut next = 0;
        for node in nodes {
            if let Part::Lettering(at) = node.part
                && claimed(self.layout.lettering[at].bbox, self.kept)
            {
                continue;
            }
            let label = format!("d{}", self.labels);
            self.labels += 1;
            match node.part {
                Part::Panel(at) => _ = self.labels_of.panels.insert(at, format!("${label}")),
                Part::Bubble(at) => _ = self.labels_of.bubbles.insert(at, format!("${label}")),
                Part::Lettering(_) => {}
            }
            let (into, index) = match (parent, node.part, self.places) {
                (None, Part::Lettering(at), Some(places)) => (
                    places[at].parent.map(|parent| parent.to_string()),
                    places[at].index,
                ),
                _ => (parent.map(|parent| format!("${parent}")), next),
            };
            let (mut layer, score) = match node.part {
                Part::Panel(at) => {
                    let panel = &self.layout.panels[at];
                    let [x0, y0, x1, y1] = panel.bbox.map(f64::from);
                    let outline = kurbo::Rect::new(x0, y0, x1, y1).to_path(0.0).to_svg();
                    (json!({"kind": "panel", "outline": outline}), panel.score)
                }
                Part::Bubble(at) => {
                    let bubble = &self.layout.bubbles[at];
                    (
                        json!({"kind": "balloon", "outline": bubble.outline.to_svg()}),
                        bubble.score,
                    )
                }
                Part::Lettering(at) => {
                    let lettering = &self.layout.lettering[at];
                    (
                        block(lettering, self.typography[at].as_ref()),
                        lettering.score,
                    )
                }
            };
            layer["attributes"] = json!({
                "c2pa": {"detection": action("c2pa.created", self.model, None)},
                "koharu": {"detection": {"score": score}},
            });
            self.ops.push(serde_json::from_value(json!({
                "op": "add_layer",
                "id": format!("${label}"),
                "page": self.page,
                "parent": into,
                "index": index,
                "layer": layer,
            }))?);
            if let Part::Lettering(at) = node.part
                && let Some((rect, pixels)) = self.mask(at)
            {
                self.ops.push(Op::WritePixels {
                    layer: Ref::try_from(format!("${label}.mask")).map_err(anyhow::Error::msg)?,
                    rect,
                    pixels,
                });
            }
            self.add(&node.children, Some(&label))?;
            next += 1;
        }
        Ok(())
    }

    /// Lettering `at`'s mask, grown past its glyphs and over its outline,
    /// and the rectangle it covers.
    fn mask(&self, at: usize) -> Option<(Rect, Vec<u8>)> {
        let others: Vec<[f32; 4]> = self
            .layout
            .lettering
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != at)
            .map(|(_, other)| other.bbox)
            .chain(self.kept.iter().copied())
            .collect();
        let (rect, pixels) = grown(
            &self.layout.lettering[at].mask,
            self.layout.lettering[at].bbox,
            // Along its lines: a glyph's size, past either end.
            self.typography[at]
                .as_ref()
                .map_or([0.0; 2], |found| match found.vertical {
                    true => [0.0, found.size],
                    false => [found.size, 0.0],
                }),
            &others,
            self.layout.width,
            self.layout.height,
        )?;
        Some(outlined(self.pixels, rect, pixels, &others))
    }
}

/// How far a block's mask reaches past its glyph mask: the clean-up shows
/// only inside the mask, and slivers of letter outlines lie just outside the
/// glyphs.
const GROW: u32 = 3;

/// `mask` inside `bbox`, or within `reach` of it across and down but in none
/// of `others`, grown by `GROW` on a `width` by `height` page, and the
/// rectangle it covers; `None` when it's empty. RF-DETR's mask can spill onto
/// a neighbouring lettering, whose translation this block's clean-up would
/// then paint over: on Manga109 pages masks reached 70 pixels into a
/// neighbour's box, and one three glyphs past its own. The checkpoint learned
/// masks cut to their boxes, but a box can stop short of its lines' ends, as
/// one ended inside a trailing ellipsis, whose rest the inpainter then drew
/// back in; so a mask reaches a glyph past the ends, though not past the
/// sides, where a neighbour's lines stand, nor into another box.
fn grown(
    mask: &Mask,
    bbox: [f32; 4],
    reach: [f32; 2],
    others: &[[f32; 4]],
    width: u32,
    height: u32,
) -> Option<(Rect, Vec<u8>)> {
    if mask.width == 0 || mask.height == 0 {
        return None;
    }
    let within = |[left, top, right, bottom]: [f32; 4], [across, down]: [f32; 2], (x, y)| {
        (left - across..=right + across).contains(&x) && (top - down..=bottom + down).contains(&y)
    };
    let inside = |x: u32, y: u32| {
        let at = ((mask.x + x) as f32 + 0.5, (mask.y + y) as f32 + 0.5);
        within(bbox, [0.0; 2], at)
            || (within(bbox, reach, at) && !others.iter().any(|other| within(*other, [0.0; 2], at)))
    };
    let mut canvas = GrayImage::new(mask.width + 2 * GROW, mask.height + 2 * GROW);
    for (at, &value) in mask.pixels.iter().enumerate() {
        let (x, y) = (at as u32 % mask.width, at as u32 / mask.width);
        if inside(x, y) {
            canvas.put_pixel(x + GROW, y + GROW, Luma([value]));
        }
    }
    let canvas = dilate(&canvas, Norm::LInf, GROW as u8);
    // The canvas's corner on the page, and the part of it on the page.
    let (left, top) = (
        i64::from(mask.x) - i64::from(GROW),
        i64::from(mask.y) - i64::from(GROW),
    );
    let (x0, y0) = (left.max(0), top.max(0));
    let x1 = (left + i64::from(canvas.width())).min(i64::from(width));
    let y1 = (top + i64::from(canvas.height())).min(i64::from(height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let rect = Rect {
        x: x0 as i32,
        y: y0 as i32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    };
    let pixels = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .map(|(x, y)| canvas.get_pixel((x - left) as u32, (y - top) as u32).0[0])
        .collect();
    Some((rect, pixels))
}

/// A block whose source is the lettering: its box turned back by its tilt,
/// and the turn, in its direction, at its size, its ink as its fill and its
/// outline as its stroke.
fn block(lettering: &Lettering, typography: Option<&Typography>) -> Value {
    let [x0, y0, x1, y1] = lettering.bbox;
    let (vertical, tilt) = typography.map_or((y1 - y0 > x1 - x0, 0.0), |found| {
        (found.vertical, found.tilt)
    });
    let (bounds, transform) = turned(lettering.bbox, tilt);
    let opaque = |color: [u8; 3]| {
        let [red, green, blue] = snapped(color).map(|channel| f32::from(channel) / 255.0);
        Rgba([red, green, blue, 1.0])
    };
    let fill = typography.and_then(|found| found.ink).map(opaque);
    let outline = typography.and_then(|found| found.outline);
    let stroke = outline.map(|outline| opaque(outline.color));
    json!({
        "kind": "block",
        "role": match lettering.kind {
            Kind::Text => "text",
            Kind::Onomatopoeia => "sfx",
        },
        "source": {
            "frame": {"box": {"bounds": bounds}},
            "direction": if vertical { "vertical" } else { "horizontal" },
            "transform": transform,
            "style": {"chars": {
                "size": typography.map(|found| found.size),
                "fill": fill,
                "stroke": stroke,
                "stroke_width": outline.map(|outline| outline.width),
            }},
        },
    })
}

/// The most a colour's channels may differ to snap, and the luma at or under
/// which it snaps to black and at or over which to white, on 0 to 255.
const SNAP_SPREAD: u8 = 48;
const SNAP_DARK: u32 = 64;
const SNAP_LIGHT: u32 = 191;

/// A measured ink or outline colour, black or white when it's near either:
/// the measure averages anti-aliased and scanned pixels, which leaves black
/// ink a dark grey. Luma alone ranks saturated reds, blues, and purples as
/// dark, so only near-neutral colours snap; a chromatic one is deliberate.
fn snapped(color: [u8; 3]) -> [u8; 3] {
    let (Some(&high), Some(&low)) = (color.iter().max(), color.iter().min()) else {
        return color;
    };
    if high - low > SNAP_SPREAD {
        return color;
    }
    let luma =
        (u32::from(color[0]) * 54 + u32::from(color[1]) * 183 + u32::from(color[2]) * 19) / 256;
    if luma <= SNAP_DARK {
        [0; 3]
    } else if luma >= SNAP_LIGHT {
        [u8::MAX; 3]
    } else {
        color
    }
}

/// The box that, turned by `tilt` degrees clockwise about its centre, `bbox`
/// bounds, and that turn; `bbox` itself, unturned, when no box does.
fn turned(bbox: [f32; 4], tilt: f32) -> (Bounds, Transform) {
    let [x0, y0, x1, y1] = bbox.map(f64::from);
    let (width, height) = (x1 - x0, y1 - y0);
    let unturned = (
        Bounds {
            x: x0,
            y: y0,
            width,
            height,
        },
        Transform::default(),
    );
    let angle = f64::from(tilt).to_radians();
    let (sin, cos) = angle.sin_cos();
    // Tilts are within ±45°, where cos² − sin² is positive.
    let turn = cos * cos - sin * sin;
    let (inner_width, inner_height) = (
        (width * cos.abs() - height * sin.abs()) / turn,
        (height * cos.abs() - width * sin.abs()) / turn,
    );
    if tilt == 0.0 || turn <= f64::EPSILON || inner_width < 1.0 || inner_height < 1.0 {
        return unturned;
    }
    let centre = Point::new((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    (
        Bounds {
            x: centre.x - inner_width / 2.0,
            y: centre.y - inner_height / 2.0,
            width: inner_width,
            height: inner_height,
        },
        // Clockwise on the page, whose y runs down.
        Transform::from(Affine::rotate_about(angle, centre)),
    )
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;

    use super::*;

    #[test]
    fn a_mask_grows_past_its_glyphs_within_the_page() -> anyhow::Result<()> {
        // One pixel at the page's corner, and one in the middle.
        let corner = Mask {
            x: 0,
            y: 1,
            width: 1,
            height: 1,
            pixels: vec![255],
        };
        let (rect, pixels) =
            grown(&corner, [0.0, 0.0, 100.0, 100.0], [0.0; 2], &[], 100, 100).context("a mask")?;
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 4, 5));
        assert!(pixels.iter().all(|&value| value == 255));
        let middle = Mask {
            x: 50,
            y: 50,
            width: 1,
            height: 1,
            pixels: vec![255],
        };
        let (rect, pixels) =
            grown(&middle, [0.0, 0.0, 100.0, 100.0], [0.0; 2], &[], 100, 100).context("a mask")?;
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (47, 47, 7, 7));
        assert_eq!(pixels.iter().filter(|&&value| value == 255).count(), 49);
        Ok(())
    }

    #[test]
    fn a_mask_reaches_a_glyph_past_its_lines_ends_only() -> anyhow::Result<()> {
        // Three pixels on a row: in the box from 0 to 10, 4 past its right
        // edge, and 15 past it.
        let spilled = Mask {
            x: 5,
            y: 5,
            width: 21,
            height: 1,
            pixels: (0..21)
                .map(|at| if matches!(at, 0 | 9 | 20) { 255 } else { 0 })
                .collect(),
        };
        // The pixels each grows to on the row, reaching `reach` with
        // `others` beside the box.
        let covered = |reach: [f32; 2], others: &[[f32; 4]]| -> anyhow::Result<Vec<i32>> {
            let (rect, pixels) = grown(&spilled, [0.0, 0.0, 10.0, 10.0], reach, others, 100, 100)
                .context("a mask")?;
            Ok((0..rect.width as i32)
                .filter(|&x| pixels[(GROW * rect.width) as usize + x as usize] == 255)
                .map(|x| rect.x + x)
                .collect())
        };
        // Horizontal lines of glyphs 5 across end at the right edge: the
        // pixel 4 past it stays, each grown 3.
        let both: Vec<i32> = (2..=8).chain(11..=17).collect();
        assert_eq!(covered([5.0, 0.0], &[])?, both);
        // Vertical lines have their side there, where a neighbour's stand.
        let own: Vec<i32> = (2..=8).collect();
        assert_eq!(covered([0.0, 5.0], &[])?, own);
        // And no mask reaches into another lettering's box.
        assert_eq!(covered([5.0, 0.0], &[[12.0, 0.0, 30.0, 10.0]])?, own);
        Ok(())
    }

    #[test]
    fn only_near_neutral_colours_snap() {
        assert_eq!(snapped([20, 31, 24]), [0, 0, 0]);
        assert_eq!(snapped([40, 70, 40]), [0, 0, 0]);
        assert_eq!(snapped([230, 240, 250]), [255, 255, 255]);
        for chromatic in [[255, 0, 0], [181, 0, 255], [30, 40, 120], [20, 115, 235]] {
            assert_eq!(snapped(chromatic), chromatic);
        }
        assert_eq!(snapped([128, 128, 128]), [128, 128, 128]);
    }

    #[test]
    fn a_tilted_box_turns_back_to_the_box_it_bounds() {
        // A 100 by 20 box turned by 10° about (60, 40).
        let (sin, cos) = 10f64.to_radians().sin_cos();
        let (width, height) = (100.0 * cos + 20.0 * sin, 100.0 * sin + 20.0 * cos);
        let bbox = [
            60.0 - width / 2.0,
            40.0 - height / 2.0,
            60.0 + width / 2.0,
            40.0 + height / 2.0,
        ]
        .map(|value| value as f32);
        let (bounds, Transform([[a, b, _], [d, e, _]])) = turned(bbox, 10.0);
        assert!((bounds.width - 100.0).abs() < 0.01 && (bounds.height - 20.0).abs() < 0.01);
        assert!((a - cos).abs() < 1e-9 && (b + sin).abs() < 1e-9);
        assert!((d - sin).abs() < 1e-9 && (e - cos).abs() < 1e-9);
    }
}
