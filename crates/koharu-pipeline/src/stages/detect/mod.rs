//! Detection: the page's panels, balloons, and blocks in reading order, in
//! place of the ones there, whoever made them, or, while the page has empty
//! text boxes drawn with the Type tool, a block in each in place of the
//! ones there. A block gets its source's frame, tilt, and direction,
//! and its mask. A locked block stays, and so does a translated one, which
//! holds the user's work: the fork keeps it where the old version reset it,
//! in the new balloon or panel holding it, and lettering it holds isn't
//! added again.

mod area;
mod layers;
mod model;
mod outline;
mod tree;

use image::{DynamicImage, RgbImage};
use koharu_ml::koharu_layout_highres::{Layout, Options, Typography};
use koharu_scene::{Content, Layer, LayerId, Op, PageId, Rect, Ref};
use kurbo::Point;
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use self::layers::Found;
pub use self::model::Detector;
use super::{Task, key};
use crate::{Area, PipelineConfig, Stage};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(specta::Type)]
pub struct DetectorConfig {
    pub model: Detector,
    /// Each class's lowest confidence a detection keeps, from 0 to 1, or the
    /// model's own.
    #[serde(deserialize_with = "threshold")]
    #[specta(type = Option<f32>)]
    pub text: Option<f32>,
    #[serde(deserialize_with = "threshold")]
    #[specta(type = Option<f32>)]
    pub onomatopoeia: Option<f32>,
    #[serde(deserialize_with = "threshold")]
    #[specta(type = Option<f32>)]
    pub bubble: Option<f32>,
    #[serde(deserialize_with = "threshold")]
    #[specta(type = Option<f32>)]
    pub panel: Option<f32>,
}

fn threshold<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f32>, D::Error> {
    let threshold = Option::<f32>::deserialize(deserializer)?;
    match threshold {
        Some(value) if !(0.0..=1.0).contains(&value) => Err(D::Error::custom(format!(
            "a detection threshold of {value} isn't between 0 and 1"
        ))),
        _ => Ok(threshold),
    }
}

pub(crate) async fn detect(task: &Task, config: &PipelineConfig) -> anyhow::Result<()> {
    let member = &task.members[0];
    let read = member.clone();
    let scan = tokio::task::spawn_blocking(move || {
        let _span =
            tracing::info_span!("read", stage = %Stage::Detect, page = read.index + 1).entered();
        read.scan()
    })
    .await??;
    let Some((project, scan)) = scan else {
        return Ok(());
    };
    let page = member.page;
    // Each drawn box, with what the detector reads of it.
    let drawn: Vec<(LayerId, Area, Rect, DynamicImage, [u32; 2])> =
        area::drawn(&project, page, scan.width(), scan.height())
            .into_iter()
            .filter_map(|(text, area)| {
                let crop = area::crop(&area, scan.width(), scan.height())?;
                let (canvas, at) = area::canvas(&scan, crop);
                Some((text, area, crop, canvas, at))
            })
            .collect();
    let key = key(Stage::Detect, config).expect("a detector is Koharu's");
    let config = config.detector;
    let options = Options {
        text: config.text,
        onomatopoeia: config.onomatopoeia,
        bubble: config.bubble,
        panel: config.panel,
    };
    let (scan, drawn, detected) = task
        .inference(&[member], move |models| {
            let detector = models.get(&key, |device| model::load(config.model, device))?;
            let detected = match drawn.is_empty() {
                true => vec![detector.inference(&scan, &options)?],
                false => drawn
                    .iter()
                    .map(|(.., canvas, _)| detector.inference(canvas, &options))
                    .collect::<anyhow::Result<_>>()?,
            };
            Ok((scan, drawn, detected))
        })
        .await?;
    let name = config.model.to_string();
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<Outcome> {
        let pixels = scan.to_rgb8();
        let (width, height) = pixels.dimensions();
        let _span = tracing::info_span!("layout").entered();
        if drawn.is_empty() {
            let layout = detected
                .into_iter()
                .next()
                .expect("a page is read once")
                .layout(&pixels);
            let typography = measured(&layout, &pixels);
            return Ok(Outcome::Page {
                layout,
                typography,
                pixels,
                name,
            });
        }
        let mut boxes = Vec::new();
        let mut lettering = Vec::new();
        for ((text, area, crop, canvas, at), detected) in drawn.into_iter().zip(detected) {
            let found = area::within(
                detected.layout(&canvas.to_rgb8()),
                crop,
                at,
                &area,
                width,
                height,
            );
            if let Some(merged) = area::merged(found.lettering, area.rect) {
                boxes.push((text, area));
                lettering.push(merged);
            }
        }
        let layout = Layout {
            width,
            height,
            lettering,
            bubbles: Vec::new(),
            panels: Vec::new(),
        };
        let typography = measured(&layout, &pixels);
        Ok(Outcome::Boxes {
            boxes,
            layout,
            typography,
            pixels,
            name,
        })
    })
    .await??;
    member
        .apply(Stage::Detect, move |project| {
            let Ok(at) = project.page(page) else {
                return Ok(Vec::new());
            };
            match found {
                Outcome::Page {
                    layout,
                    typography,
                    pixels,
                    name,
                } => {
                    let found = Found {
                        layout: &layout,
                        typography: &typography,
                        pixels: &pixels,
                    };
                    redetected(&at.layers, &found, page, &name)
                }
                Outcome::Boxes {
                    boxes,
                    layout,
                    typography,
                    pixels,
                    name,
                } => {
                    let found = Found {
                        layout: &layout,
                        typography: &typography,
                        pixels: &pixels,
                    };
                    area::adopted(&at.layers, &found, &boxes, page, &name)
                }
            }
        })
        .await
}

/// Each lettering's typography on the page.
fn measured(layout: &Layout, pixels: &RgbImage) -> Vec<Option<Typography>> {
    let _span = tracing::info_span!("typography").entered();
    layout
        .lettering
        .par_iter()
        .map(|lettering| lettering.typography(pixels))
        .collect()
}

/// What a detection found on a page or in its drawn boxes, which is placed
/// once the page's layers are known.
enum Outcome {
    Page {
        layout: Layout,
        typography: Vec<Option<Typography>>,
        pixels: RgbImage,
        name: String,
    },
    /// Each box that held lettering, and that lettering, in the same order.
    Boxes {
        boxes: Vec<(LayerId, Area)>,
        layout: Layout,
        typography: Vec<Option<Typography>>,
        pixels: RgbImage,
        name: String,
    },
}

/// The ops replacing a page's parts with `found`'s, leaving its locked and
/// translated blocks, the translated ones moved into the new balloon or
/// panel holding them.
fn redetected(
    layers: &[Layer],
    found: &Found,
    page: PageId,
    name: &str,
) -> anyhow::Result<Vec<Op>> {
    let mut ops = Vec::new();
    let mut stays = Vec::new();
    staying(layers, false, false, &mut stays);
    let moved: Vec<&Stay> = stays.iter().filter(|stay| stay.moves).collect();
    // Out of what's removed first, onto the page's top level.
    let nested = moved.iter().filter(|stay| stay.nested);
    for (at_end, stay) in nested.enumerate() {
        ops.push(Op::MoveLayer {
            layer: stay.layer.into(),
            parent: None,
            index: layers.len() + at_end,
        });
    }
    let kept: Vec<LayerId> = moved.iter().map(|stay| stay.layer).collect();
    removals(layers, &kept, &mut ops);
    let boxes: Vec<[f32; 4]> = stays.iter().map(|stay| stay.bbox).collect();
    let (additions, labels) = layers::additions(found, page, name, &boxes)?;
    ops.extend(additions);
    for stay in moved {
        if let Some(label) = labels.holding(found.layout, centre(stay.bbox)) {
            ops.push(Op::MoveLayer {
                layer: stay.layer.into(),
                parent: Some(Ref::try_from(label).map_err(anyhow::Error::msg)?),
                index: 0,
            });
        }
    }
    Ok(ops)
}

/// Removes the page's panels, balloons, and blocks, each with what's inside
/// it, but a locked one, which the user keeps, and the `kept` blocks, moved
/// out of them before. A panel or balloon holding a locked layer stays too,
/// as the scene can't remove it, with what else is in it removed.
fn removals(layers: &[Layer], kept: &[LayerId], ops: &mut Vec<Op>) {
    for layer in layers
        .iter()
        .filter(|layer| !layer.locked && !kept.contains(&layer.id))
    {
        match layer.content {
            Content::Panel(_) | Content::Balloon(_) if holds_locked(layer) => {
                removals(&layer.children, kept, ops)
            }
            Content::Panel(_) | Content::Balloon(_) | Content::Block(_) => {
                ops.push(Op::RemoveLayer {
                    layer: layer.id.into(),
                })
            }
            _ => removals(&layer.children, kept, ops),
        }
    }
}

fn holds_locked(layer: &Layer) -> bool {
    layer
        .children
        .iter()
        .any(|child| child.locked || holds_locked(child))
}

/// A block a page's detection leaves: its source's box, and whether it moves
/// into the new layers, as a translated one in nothing locked does; a locked
/// one stays where it is.
struct Stay {
    layer: LayerId,
    bbox: [f32; 4],
    moves: bool,
    /// Inside another layer, not on the page's top level.
    nested: bool,
}

/// Whether `layer` is a block with a translation, which detection keeps.
pub(super) fn translated(layer: &Layer) -> bool {
    matches!(&layer.content, Content::Block(block) if block
        .translation
        .as_ref()
        .is_some_and(|text| !text.content.text().trim().is_empty()))
}

/// The blocks among `layers` that detection leaves, `locked` when a layer
/// they're in is, and `nested` below the top level.
fn staying(layers: &[Layer], locked: bool, nested: bool, out: &mut Vec<Stay>) {
    for layer in layers {
        let locked = locked || layer.locked;
        if let Content::Block(block) = &layer.content
            && (locked || translated(layer))
            && let Some(bounds) = block.source.bounds()
        {
            out.push(Stay {
                layer: layer.id,
                bbox: [bounds.x0, bounds.y0, bounds.x1, bounds.y1].map(|value| value as f32),
                moves: !locked,
                nested,
            });
        }
        staying(&layer.children, locked, true, out);
    }
}

fn centre([x0, y0, x1, y1]: [f32; 4]) -> Point {
    Point::new(f64::from(x0 + x1) / 2.0, f64::from(y0 + y1) / 2.0)
}

#[cfg(test)]
mod tests {
    use koharu_ml::koharu_layout_highres::{Bubble, Kind, Lettering, Mask, Panel};
    use koharu_scene::{Created, Edit, Session};
    use kurbo::{BezPath, Shape as _};
    use serde_json::json;

    use super::*;

    fn mask(bbox: [f32; 4]) -> Mask {
        Mask {
            x: bbox[0] as u32,
            y: bbox[1] as u32,
            width: 1,
            height: 1,
            pixels: vec![255],
        }
    }

    fn lettering(bbox: [f32; 4], bubble: Option<usize>) -> Lettering {
        Lettering {
            kind: Kind::Text,
            score: 0.9,
            bbox,
            mask: mask(bbox),
            bubble,
            panel: Some(0),
            room: None,
        }
    }

    /// A page detected again keeps a translated block, moved into the new
    /// balloon holding it, and adds no block over its lettering; an
    /// untranslated block is replaced.
    #[test]
    fn a_translated_block_survives_detection_in_its_new_balloon() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut session = Session::create(dir.path().join("test.khr"))?;
        let block = |x: f64, translation: Option<&str>| {
            let mut block = json!({"kind": "block", "source": {
                "frame": {"box": {"bounds": {"x": x, "y": 40.0, "width": 10.0, "height": 10.0}}},
            }});
            if let Some(text) = translation {
                block["translation"] = json!({"content": text});
            }
            block
        };
        let ops = [
            json!({"op": "add_page", "id": "$p", "index": 0, "size": {"width": 100, "height": 100}}),
            json!({"op": "add_layer", "id": "$panel", "page": "$p", "index": 0,
                "layer": {"kind": "panel", "outline": "M0 0L100 0L100 100L0 100Z"}}),
            json!({"op": "add_layer", "id": "$balloon", "page": "$p", "parent": "$panel", "index": 0,
                "layer": {"kind": "balloon", "outline": "M0 0L50 0L50 100L0 100Z"}}),
            json!({"op": "add_layer", "page": "$p", "parent": "$balloon", "index": 0,
                "layer": block(10.0, Some("Hi"))}),
            json!({"op": "add_layer", "page": "$p", "parent": "$panel", "index": 1,
                "layer": block(70.0, None)}),
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
        let (translated, bare) = (id(3)?, id(4)?);
        let project = session.project();
        let page = project.pages().next().expect("a page");
        let outline = kurbo::Rect::new(0.0, 0.0, 50.0, 100.0).to_path(0.0);
        let layout = Layout {
            width: 100,
            height: 100,
            lettering: vec![
                lettering([10.0, 40.0, 20.0, 50.0], Some(0)),
                lettering([70.0, 40.0, 80.0, 50.0], None),
            ],
            bubbles: vec![Bubble {
                score: 0.9,
                bbox: [0.0, 0.0, 50.0, 100.0],
                mask: mask([0.0, 0.0, 1.0, 1.0]),
                outline: BezPath::from_svg(&outline.to_svg())?,
                lettering: vec![0],
                panel: Some(0),
            }],
            panels: vec![Panel {
                score: 0.9,
                bbox: [0.0, 0.0, 100.0, 100.0],
                mask: mask([0.0, 0.0, 1.0, 1.0]),
            }],
        };
        let typography = vec![None, None];
        let pixels = RgbImage::new(100, 100);
        let found = Found {
            layout: &layout,
            typography: &typography,
            pixels: &pixels,
        };
        let ops = redetected(&page.layers, &found, page.id, "test")?;
        session.apply(Edit {
            label: "detect".to_owned(),
            ops,
        })?;

        let project = session.project();
        let (_, parent) = project.parent(translated)?;
        let parent = parent.expect("the translated block is in a layer");
        assert!(matches!(parent.content, Content::Balloon(_)));
        assert!(project.layer(bare).is_err());
        let mut blocks = Vec::new();
        fn walk(layers: &[Layer], out: &mut Vec<LayerId>) {
            for layer in layers {
                if matches!(layer.content, Content::Block(_)) {
                    out.push(layer.id);
                }
                walk(&layer.children, out);
            }
        }
        walk(&project.pages().next().expect("a page").layers, &mut blocks);
        assert_eq!(blocks.len(), 2);
        assert!(blocks.contains(&translated));
        Ok(())
    }
}
