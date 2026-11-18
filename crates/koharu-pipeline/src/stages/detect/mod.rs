//! Detection: the page's panels, balloons, and blocks in reading order, in
//! place of the ones there, whoever made them, or an area's blocks in place
//! of the ones there. A block gets its source's frame, tilt, and direction,
//! and its mask. A locked block stays, and so does a translated one, which
//! holds the user's work: the fork keeps it where the old version reset it,
//! in the new balloon or panel holding it, and lettering it holds isn't
//! added again.

mod area;
mod layers;
mod model;
mod outline;
mod tree;

use std::sync::Arc;

use image::RgbImage;
use koharu_ml::koharu_layout_highres::{Layout, Options, Typography};
use koharu_scene::{Content, Layer, LayerId, Op, PageId, Ref};
use kurbo::Point;
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use self::layers::Found;
pub use self::model::Detector;
use super::{AreaWork, Task, key};
use crate::{PipelineConfig, Stage};

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

pub(crate) async fn detect(
    task: &Task,
    config: &PipelineConfig,
    area: Option<Arc<AreaWork>>,
) -> anyhow::Result<()> {
    let member = &task.members[0];
    let read = member.clone();
    let scan = tokio::task::spawn_blocking(move || {
        let _span =
            tracing::info_span!("read", stage = %Stage::Detect, page = read.index + 1).entered();
        read.scan()
    })
    .await??;
    let Some((_, scan)) = scan else {
        return Ok(());
    };
    let crop = match &area {
        Some(work) => match area::crop(&work.area, scan.width(), scan.height()) {
            Some(crop) => Some(crop),
            None => return Ok(()),
        },
        None => None,
    };
    let key = key(Stage::Detect, config).expect("a detector is Koharu's");
    let config = config.detector;
    let options = Options {
        text: config.text,
        onomatopoeia: config.onomatopoeia,
        bubble: config.bubble,
        panel: config.panel,
    };
    let read =
        crop.map(|crop| scan.crop_imm(crop.x as u32, crop.y as u32, crop.width, crop.height));
    let (scan, read, detected) = task
        .inference(&[member], move |models| {
            let detector = models.get(&key, |device| model::load(config.model, device))?;
            let detected = detector.inference(read.as_ref().unwrap_or(&scan), &options)?;
            Ok((scan, read, detected))
        })
        .await?;
    let page = member.page;
    let name = config.model.to_string();
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<Outcome> {
        let pixels = scan.to_rgb8();
        let layout = {
            let _span = tracing::info_span!("layout").entered();
            match (read, crop, &area) {
                (Some(read), Some(crop), Some(work)) => area::within(
                    detected.layout(&read.to_rgb8()),
                    crop,
                    &work.area,
                    pixels.width(),
                    pixels.height(),
                ),
                _ => detected.layout(&pixels),
            }
        };
        let typography: Vec<Option<Typography>> = {
            let _span = tracing::info_span!("typography").entered();
            layout
                .lettering
                .par_iter()
                .map(|lettering| lettering.typography(&pixels))
                .collect()
        };
        Ok(match area {
            None => Outcome::Page {
                layout,
                typography,
                pixels,
                name,
            },
            Some(work) => Outcome::Area {
                layout,
                typography,
                pixels,
                work,
                name,
            },
        })
    })
    .await??;
    member
        .apply(Stage::Detect, move |project| {
            let Ok(at) = project.page(page) else {
                return Ok(Vec::new());
            };
            let mut ops = Vec::new();
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
                    ops.extend(redetected(&at.layers, &found, page, &name)?);
                }
                // Where an area's blocks go depends on the layers there now.
                Outcome::Area {
                    layout,
                    typography,
                    pixels,
                    work,
                    name,
                } => {
                    let boxes: Vec<[f32; 4]> = layout
                        .lettering
                        .iter()
                        .map(|lettering| lettering.bbox)
                        .collect();
                    let (removed, places, kept) = area::changes(&at.layers, &work.area, &boxes);
                    let found = Found {
                        layout: &layout,
                        typography: &typography,
                        pixels: &pixels,
                    };
                    ops.extend(removed);
                    ops.extend(layers::placed(&found, page, &name, &places, &kept)?);
                }
            }
            Ok(ops)
        })
        .await
}

/// What a detection found on a page or in an area, which is placed once the
/// page's layers are known.
enum Outcome {
    Page {
        layout: Layout,
        typography: Vec<Option<Typography>>,
        pixels: RgbImage,
        name: String,
    },
    Area {
        layout: Layout,
        typography: Vec<Option<Typography>>,
        pixels: RgbImage,
        work: Arc<AreaWork>,
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
