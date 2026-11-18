//! Detection in an area the marquee or the lasso marked, for lettering the
//! page-wide pass missed: the detector reads the area's rectangle alone,
//! which it scales up as it scales a page, so small lettering there gets the
//! resolution a whole page would. The area's blocks take the place of the
//! ones there, as a page's take the page's, and go into the panel or balloon
//! they're in; the page's other layers stay.

use std::collections::HashMap;

use koharu_ml::koharu_layout_highres::Layout;
use koharu_scene::{Content, Layer, LayerId, Op, Rect};
use kurbo::{BezPath, Point, Shape as _};

use super::layers::Place;
use crate::Area;

/// The area's rectangle on a `width` by `height` page; `None` when it's off
/// the page.
pub(super) fn crop(area: &Area, width: u32, height: u32) -> Option<Rect> {
    let (x0, y0) = (area.rect.x.max(0), area.rect.y.max(0));
    let x1 = (i64::from(area.rect.x) + i64::from(area.rect.width)).min(i64::from(width));
    let y1 = (i64::from(area.rect.y) + i64::from(area.rect.height)).min(i64::from(height));
    (x1 > i64::from(x0) && y1 > i64::from(y0)).then(|| Rect {
        x: x0,
        y: y0,
        width: (x1 - i64::from(x0)) as u32,
        height: (y1 - i64::from(y0)) as u32,
    })
}

/// The lettering of `layout`, read on the `crop` of a `width` by `height`
/// page, on the page, those whose centre the area holds, with no panel or
/// balloon: the crop shows only part of either.
pub(super) fn within(layout: Layout, crop: Rect, area: &Area, width: u32, height: u32) -> Layout {
    let (dx, dy) = (crop.x as u32, crop.y as u32);
    let lettering = layout
        .lettering
        .into_iter()
        .map(|mut lettering| {
            let [x0, y0, x1, y1] = lettering.bbox;
            lettering.bbox = [x0 + dx as f32, y0 + dy as f32, x1 + dx as f32, y1 + dy as f32];
            lettering.mask.x += dx;
            lettering.mask.y += dy;
            lettering.bubble = None;
            lettering.panel = None;
            lettering.room = None;
            lettering
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
            *count += 1;
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
/// `removed`, unless they or a layer they're in is locked, and the boxes of
/// the rest into `kept`.
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
                if !locked && holds(area, centre(bbox)) {
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
            lettering: vec![lettering([0.0, 0.0, 10.0, 10.0]), lettering([40.0, 0.0, 50.0, 10.0])],
            bubbles: Vec::new(),
            panels: Vec::new(),
        };
        let layout = within(read, crop, &left_half(), 100, 100);
        assert_eq!((layout.width, layout.height), (100, 100));
        assert_eq!(layout.lettering.len(), 1);
        let kept = &layout.lettering[0];
        assert_eq!(kept.bbox, [10.0, 20.0, 20.0, 30.0]);
        assert_eq!((kept.mask.x, kept.mask.y), (10, 20));
        assert_eq!((kept.bubble, kept.panel), (None, None));
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
