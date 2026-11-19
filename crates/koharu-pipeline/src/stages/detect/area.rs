//! Detection in an area the marquee or the lasso marked, for lettering the
//! page-wide pass missed: the detector reads the area's rectangle alone,
//! which it scales up as it scales a page, so small lettering there gets the
//! resolution a whole page would. The area's blocks take the place of the
//! ones there, as a page's take the page's, and go into the panel or balloon
//! they're in; the page's other layers stay.

use std::collections::HashMap;

use image::{DynamicImage, Rgb, RgbImage, imageops};
use koharu_ml::koharu_layout_highres::{Layout, Mask};
use koharu_scene::{Content, Layer, LayerId, Op, Rect};
use kurbo::{BezPath, Point, Shape as _};

use super::{
    layers::{Place, claimed},
    translated,
};
use crate::Area;

/// What the detector reads past an area on every side: a quarter of its
/// longer side, at least 32 pixels, as the fork's drawn-box detection read.
/// A tight marquee leaves lettering without its balloon or panel around it,
/// and on 36 letterings the detector found 20 read alone and 29 with this.
fn context(rect: Rect) -> u32 {
    ((rect.width.max(rect.height) as f32 * 0.25).ceil() as u32).max(32)
}

/// The smallest side of the white canvas a read is centred on. The detector
/// scales what it reads up to its input, so a small area's lettering grew
/// far past any page's; on a canvas this size the same 36 letterings were
/// found 34 times.
const CANVAS: u32 = 640;

/// The area's rectangle on a `width` by `height` page with the context around
/// it, cut to the page; `None` when it's off the page.
pub(super) fn crop(area: &Area, width: u32, height: u32) -> Option<Rect> {
    let margin = i64::from(context(area.rect));
    let (x0, y0) = (
        (i64::from(area.rect.x) - margin).max(0),
        (i64::from(area.rect.y) - margin).max(0),
    );
    let x1 = (i64::from(area.rect.x) + i64::from(area.rect.width) + margin).min(i64::from(width));
    let y1 = (i64::from(area.rect.y) + i64::from(area.rect.height) + margin).min(i64::from(height));
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0 as i32,
        y: y0 as i32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    })
}

/// `crop` of `scan` centred on a white canvas at least `CANVAS` on a side,
/// and where the crop's corner lies on it.
pub(super) fn canvas(scan: &DynamicImage, crop: Rect) -> (DynamicImage, [u32; 2]) {
    let read = scan.crop_imm(crop.x as u32, crop.y as u32, crop.width, crop.height);
    let side = CANVAS.max(crop.width).max(crop.height);
    let at = [(side - crop.width) / 2, (side - crop.height) / 2];
    let mut canvas = RgbImage::from_pixel(side, side, Rgb([u8::MAX; 3]));
    imageops::overlay(
        &mut canvas,
        &read.to_rgb8(),
        i64::from(at[0]),
        i64::from(at[1]),
    );
    (DynamicImage::ImageRgb8(canvas), at)
}

/// The lettering of `layout`, read on a canvas holding the `crop` of a
/// `width` by `height` page at `at`, on the page: its mask cut to the crop,
/// as the canvas past it is blank, and only those whose centre the area
/// holds, with no panel or balloon, which the crop shows only part of.
pub(super) fn within(
    layout: Layout,
    crop: Rect,
    at: [u32; 2],
    area: &Area,
    width: u32,
    height: u32,
) -> Layout {
    let (dx, dy) = (
        i64::from(crop.x) - i64::from(at[0]),
        i64::from(crop.y) - i64::from(at[1]),
    );
    let lettering = layout
        .lettering
        .into_iter()
        .filter_map(|mut lettering| {
            let [x0, y0, x1, y1] = lettering.bbox;
            lettering.bbox = [
                x0 + dx as f32,
                y0 + dy as f32,
                x1 + dx as f32,
                y1 + dy as f32,
            ];
            lettering.mask = cut(&lettering.mask, dx, dy, crop)?;
            lettering.bubble = None;
            lettering.panel = None;
            lettering.room = None;
            Some(lettering)
        })
        .filter(|lettering| holds(area, centre(lettering.bbox)))
        .collect();
    Layout {
        width,
        height,
        lettering,
        bubbles: Vec::new(),
        panels: Vec::new(),
    }
}

/// `mask`, moved by (`dx`, `dy`) onto the page and cut to `crop`; `None` when
/// none of it lies there.
fn cut(mask: &Mask, dx: i64, dy: i64, crop: Rect) -> Option<Mask> {
    let left = (i64::from(mask.x) + dx).max(i64::from(crop.x));
    let top = (i64::from(mask.y) + dy).max(i64::from(crop.y));
    let right = (i64::from(mask.x) + dx + i64::from(mask.width))
        .min(i64::from(crop.x) + i64::from(crop.width));
    let bottom = (i64::from(mask.y) + dy + i64::from(mask.height))
        .min(i64::from(crop.y) + i64::from(crop.height));
    if right <= left || bottom <= top {
        return None;
    }
    let pixels: Vec<u8> = (top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (own_x, own_y) = (x - dx - i64::from(mask.x), y - dy - i64::from(mask.y));
            mask.pixels[(own_y * i64::from(mask.width) + own_x) as usize]
        })
        .collect();
    pixels.iter().any(|&value| value != 0).then(|| Mask {
        x: left as u32,
        y: top as u32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
        pixels,
    })
}

/// What an area's detection changes on a page with `layers`: the unlocked
/// blocks whose centre the area holds go, and each of the `found` lettering
/// boxes, in reading order, goes at the end of the innermost unlocked panel
/// or balloon holding its centre, else of the page. Also the boxes of the
/// blocks that stay, which the new masks keep out of.
pub(super) fn changes(
    layers: &[Layer],
    area: &Area,
    found: &[[f32; 4]],
) -> (Vec<Op>, Vec<Place>, Vec<[f32; 4]>) {
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    blocks(layers, area, false, &mut removed, &mut kept);
    // How many children each holder has once the removals are done.
    let mut counts = HashMap::<Option<LayerId>, usize>::new();
    let places = found
        .iter()
        .map(|&bbox| {
            let (parent, children) = holder(layers, centre(bbox), None);
            let count = counts.entry(parent).or_insert_with(|| {
                children
                    .iter()
                    .filter(|layer| !removed.contains(&layer.id))
                    .count()
            });
            let place = Place {
                parent,
                index: *count,
            };
            // A kept block's lettering isn't added again, so takes no place.
            if !claimed(bbox, &kept) {
                *count += 1;
            }
            place
        })
        .collect();
    let ops = removed
        .into_iter()
        .map(|layer| Op::RemoveLayer {
            layer: layer.into(),
        })
        .collect();
    (ops, places, kept)
}

/// The blocks among `layers` whose source's centre `area` holds into
/// `removed`, unless they or a layer they're in is locked or they're
/// translated, and the boxes of the rest into `kept`.
fn blocks(
    layers: &[Layer],
    area: &Area,
    locked: bool,
    removed: &mut Vec<LayerId>,
    kept: &mut Vec<[f32; 4]>,
) {
    for layer in layers {
        let locked = locked || layer.locked;
        match &layer.content {
            Content::Block(block) => {
                let Some(bounds) = block.source.bounds() else {
                    continue;
                };
                let bbox = [bounds.x0, bounds.y0, bounds.x1, bounds.y1].map(|value| value as f32);
                if !locked && !translated(layer) && holds(area, centre(bbox)) {
                    removed.push(layer.id);
                } else {
                    kept.push(bbox);
                }
            }
            _ => blocks(&layer.children, area, locked, removed, kept),
        }
    }
}

/// The innermost unlocked panel or balloon among `layers` holding `point`,
/// and its children; `parent` and `layers` when none does.
fn holder<'l>(
    layers: &'l [Layer],
    point: Point,
    parent: Option<LayerId>,
) -> (Option<LayerId>, &'l [Layer]) {
    for layer in layers.iter().filter(|layer| !layer.locked) {
        let outline = match &layer.content {
            Content::Panel(panel) => panel.outline.as_str(),
            Content::Balloon(balloon) => balloon.outline.as_str(),
            _ => continue,
        };
        if BezPath::from_svg(outline).is_ok_and(|outline| outline.contains(point)) {
            return holder(&layer.children, point, Some(layer.id));
        }
    }
    (parent, layers)
}

fn centre([x0, y0, x1, y1]: [f32; 4]) -> Point {
    Point::new(f64::from(x0 + x1) / 2.0, f64::from(y0 + y1) / 2.0)
}

/// Whether the area is painted at `point`.
fn holds(area: &Area, point: Point) -> bool {
    let (x, y) = (
        point.x.floor() as i64 - i64::from(area.rect.x),
        point.y.floor() as i64 - i64::from(area.rect.y),
    );
    (0..i64::from(area.rect.width)).contains(&x)
        && (0..i64::from(area.rect.height)).contains(&y)
        && area.pixels[(y * i64::from(area.rect.width) + x) as usize] != 0
}

#[cfg(test)]
mod tests {
    use koharu_ml::koharu_layout_highres::{Kind, Lettering, Mask};
    use koharu_scene::{Created, Edit, Session};
    use serde_json::json;

    use super::*;

    /// The left half of a 100 by 100 page.
    fn left_half() -> Area {
        Area {
            rect: Rect {
                x: 0,
                y: 0,
                width: 50,
                height: 100,
            },
            pixels: vec![255; 50 * 100],
        }
    }

    fn lettering(bbox: [f32; 4]) -> Lettering {
        Lettering {
            kind: Kind::Text,
            score: 0.9,
            bbox,
            mask: Mask {
                x: bbox[0] as u32,
                y: bbox[1] as u32,
                width: 1,
                height: 1,
                pixels: vec![255],
            },
            bubble: Some(0),
            panel: Some(0),
            room: None,
        }
    }

    #[test]
    fn an_areas_lettering_moves_onto_the_page_and_keeps_to_the_area() {
        let crop = Rect {
            x: 10,
            y: 20,
            width: 60,
            height: 60,
        };
        let read = Layout {
            width: 60,
            height: 60,
            // Centred 15 and 55 across the page, the area's left half
            // holding only the first.
            lettering: vec![
                lettering([0.0, 0.0, 10.0, 10.0]),
                lettering([40.0, 0.0, 50.0, 10.0]),
            ],
            bubbles: Vec::new(),
            panels: Vec::new(),
        };
        let layout = within(read, crop, [0, 0], &left_half(), 100, 100);
        assert_eq!((layout.width, layout.height), (100, 100));
        assert_eq!(layout.lettering.len(), 1);
        let kept = &layout.lettering[0];
        assert_eq!(kept.bbox, [10.0, 20.0, 20.0, 30.0]);
        assert_eq!((kept.mask.x, kept.mask.y), (10, 20));
        assert_eq!((kept.bubble, kept.panel), (None, None));
    }

    /// An area is read with context around it, centred on a white canvas,
    /// and what's found comes back onto the page cut to the crop.
    #[test]
    fn an_area_is_read_with_context_on_a_canvas_and_found_back_on_the_page() {
        let area = Area {
            rect: Rect {
                x: 100,
                y: 100,
                width: 40,
                height: 80,
            },
            pixels: vec![255; 40 * 80],
        };
        let crop = crop(&area, 1000, 1000).expect("on the page");
        assert_eq!(
            crop,
            Rect {
                x: 68,
                y: 68,
                width: 104,
                height: 144,
            }
        );
        let scan = DynamicImage::ImageRgb8(RgbImage::from_pixel(1000, 1000, Rgb([0; 3])));
        let (canvas, at) = canvas(&scan, crop);
        assert_eq!((canvas.width(), canvas.height()), (640, 640));
        assert_eq!(at, [268, 248]);
        assert_eq!(canvas.to_rgb8().get_pixel(0, 0).0, [255; 3]);
        assert_eq!(canvas.to_rgb8().get_pixel(268, 248).0, [0; 3]);
        // A lettering at the area's middle, its mask reaching two pixels onto
        // the blank canvas left of the crop.
        let mut found = lettering([300.0, 300.0, 320.0, 330.0]);
        found.mask = Mask {
            x: 266,
            y: 300,
            width: 60,
            height: 1,
            pixels: vec![255; 60],
        };
        let read = Layout {
            width: 640,
            height: 640,
            lettering: vec![found],
            bubbles: Vec::new(),
            panels: Vec::new(),
        };
        let layout = within(read, crop, at, &area, 1000, 1000);
        let kept = &layout.lettering[0];
        assert_eq!(kept.bbox, [100.0, 120.0, 120.0, 150.0]);
        assert_eq!((kept.mask.x, kept.mask.y, kept.mask.width), (68, 120, 58));
    }

    #[test]
    fn an_area_replaces_its_unlocked_blocks_and_files_new_ones_in_their_balloon()
    -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut session = Session::create(dir.path().join("test.khr"))?;
        let block = |x: f64, locked: bool| {
            json!({"kind": "block", "locked": locked, "source": {
                "frame": {"box": {"bounds": {"x": x, "y": 40.0, "width": 10.0, "height": 10.0}}},
            }})
        };
        let ops = [
            json!({"op": "add_page", "id": "$p", "index": 0, "size": {"width": 100, "height": 100}}),
            json!({"op": "add_layer", "id": "$panel", "page": "$p", "index": 0,
                "layer": {"kind": "panel", "outline": "M0 0L100 0L100 100L0 100Z"}}),
            json!({"op": "add_layer", "id": "$balloon", "page": "$p", "parent": "$panel", "index": 0,
                "layer": {"kind": "balloon", "outline": "M0 0L50 0L50 100L0 100Z"}}),
            json!({"op": "add_layer", "page": "$p", "parent": "$balloon", "index": 0, "layer": block(10.0, false)}),
            json!({"op": "add_layer", "page": "$p", "parent": "$balloon", "index": 1, "layer": block(30.0, true)}),
            json!({"op": "add_layer", "page": "$p", "parent": "$panel", "index": 1, "layer": block(70.0, false)}),
        ];
        let change = session.apply(Edit {
            label: "test".to_owned(),
            ops: ops
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()?,
        })?;
        let id = |at: usize| match change.created.get(at) {
            Some(Created::Layer { id, .. }) => Ok(*id),
            _ => anyhow::bail!("no layer {at}"),
        };
        let (balloon, unlocked) = (id(2)?, id(3)?);
        let project = session.project();
        let page = project.pages().next().expect("a page");

        let (ops, places, kept) = changes(&page.layers, &left_half(), &[[20.0, 60.0, 30.0, 70.0]]);

        let removed: Vec<_> = ops
            .iter()
            .map(|op| match op {
                Op::RemoveLayer { layer } => layer.uuid(),
                _ => None,
            })
            .collect();
        assert_eq!(removed, [Some(unlocked.0)]);
        assert_eq!(places.len(), 1);
        assert_eq!(places[0].parent, Some(balloon));
        // After the locked block, which stays.
        assert_eq!(places[0].index, 1);
        assert_eq!(kept, [[30.0, 40.0, 40.0, 50.0], [70.0, 40.0, 80.0, 50.0]]);
        Ok(())
    }
}
