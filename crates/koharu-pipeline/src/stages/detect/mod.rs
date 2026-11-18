//! Detection: the page's panels, balloons, and blocks in reading order, in
//! place of the ones there, whoever made them, or an area's blocks in place
//! of the ones there. A block gets its source's frame, tilt, and direction,
//! and its mask.

mod area;
mod layers;
mod model;
mod outline;
mod tree;

use std::sync::Arc;

use image::RgbImage;
use koharu_ml::koharu_layout_highres::{Layout, Options, Typography};
use koharu_scene::{Content, Layer, Op};
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
    let read = crop.map(|crop| scan.crop_imm(crop.x as u32, crop.y as u32, crop.width, crop.height));
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
            None => {
                let found = Found {
                    layout: &layout,
                    typography: &typography,
                    pixels: &pixels,
                };
                Outcome::Page(layers::additions(&found, page, &name)?)
            }
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
                Outcome::Page(additions) => {
                    removals(&at.layers, &mut ops);
                    ops.extend(additions);
                }
                // Where an area's blocks go depends on the layers there now.
                Outcome::Area {
                    layout,
                    typography,
                    pixels,
                    work,
                    name,
                } => {
                    let boxes: Vec<[f32; 4]> =
                        layout.lettering.iter().map(|lettering| lettering.bbox).collect();
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

/// What a detection adds: a page's parts, or an area's lettering, which is
/// placed once the page's layers are known.
enum Outcome {
    Page(Vec<Op>),
    Area {
        layout: Layout,
        typography: Vec<Option<Typography>>,
        pixels: RgbImage,
        work: Arc<AreaWork>,
        name: String,
    },
}

/// Removes the page's panels, balloons, and blocks, each with what's inside
/// it, but a locked one, which the user keeps.
fn removals(layers: &[Layer], ops: &mut Vec<Op>) {
    for layer in layers.iter().filter(|layer| !layer.locked) {
        match layer.content {
            Content::Panel(_) | Content::Balloon(_) | Content::Block(_) => {
                ops.push(Op::RemoveLayer {
                    layer: layer.id.into(),
                })
            }
            _ => removals(&layer.children, ops),
        }
    }
}
