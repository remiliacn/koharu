use image::{DynamicImage, GrayImage, RgbImage, imageops};
use koharu_ml::{
    Canceller,
    flux3_klein::{self, Flux3Klein},
    lama::{self, Lama},
    qwen_image::{self, QwenImage},
    rorem_mixed::{self, RoremMixed, RoremMixedOptions},
};
use koharu_runtime::Device;
use serde::{Deserialize, Serialize};
use strum::{Display, VariantArray};

/// The inpainters, by id in the section and by name.
#[derive(
    Clone, Copy, Debug, Default, Display, Eq, Hash, PartialEq, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "kebab-case")]
#[derive(specta::Type)]
pub enum Inpainter {
    #[default]
    #[strum(to_string = "LaMa")]
    Lama,
    #[strum(to_string = "FLUX.3 Klein")]
    Flux3Klein,
    #[strum(to_string = "Qwen-Image 2.1 Turbo")]
    QwenImage,
    #[strum(to_string = "RORem")]
    RoremMixed,
}

/// An inpainter, loaded.
pub(super) type Model = Box<dyn Inpaint + Send>;

pub(super) trait Inpaint {
    /// `image` redrawn where `mask` is set, at its own size, after `prompt`,
    /// which may be blank.
    fn inference(
        &self,
        image: &RgbImage,
        mask: &GrayImage,
        prompt: &str,
    ) -> anyhow::Result<RgbImage>;

    /// The longest side of a window it redraws in one pass.
    fn pass(&self) -> u32 {
        u32::MAX
    }

    /// What stops a redraw under way, for a model that can be stopped.
    fn canceller(&self) -> Option<Canceller> {
        None
    }
}

impl Inpaint for Lama {
    fn inference(
        &self,
        image: &RgbImage,
        mask: &GrayImage,
        // It takes no prompt.
        _: &str,
    ) -> anyhow::Result<RgbImage> {
        Lama::inference(self, &DynamicImage::ImageRgb8(image.clone()), mask)
    }

    /// Past it LaMa crops around each masked region and runs once for each.
    fn pass(&self) -> u32 {
        lama::WHOLE
    }
}

impl Inpaint for Flux3Klein {
    fn canceller(&self) -> Option<Canceller> {
        Some(Flux3Klein::canceller(self))
    }

    fn inference(
        &self,
        image: &RgbImage,
        mask: &GrayImage,
        prompt: &str,
    ) -> anyhow::Result<RgbImage> {
        let options = flux3_klein::InpaintOptions::default();
        let redrawn = Flux3Klein::inpaint(
            self,
            prompt,
            &DynamicImage::ImageRgb8(image.clone()),
            None,
            &DynamicImage::ImageLuma8(mask.clone()),
            &options,
        )?;
        Ok(fitted(redrawn, image))
    }
}

impl Inpaint for QwenImage {
    fn canceller(&self) -> Option<Canceller> {
        Some(QwenImage::canceller(self))
    }

    fn inference(
        &self,
        image: &RgbImage,
        mask: &GrayImage,
        prompt: &str,
    ) -> anyhow::Result<RgbImage> {
        let options = qwen_image::InpaintOptions::default();
        let redrawn = QwenImage::inpaint(
            self,
            prompt,
            &DynamicImage::ImageRgb8(image.clone()),
            &DynamicImage::ImageLuma8(mask.clone()),
            &options,
        )?;
        Ok(fitted(redrawn, image))
    }
}

impl Inpaint for RoremMixed {
    fn canceller(&self) -> Option<Canceller> {
        Some(RoremMixed::canceller(self))
    }

    /// RORem is SDXL: it reads a prompt describing the picture, not the
    /// instruction the others take, so it keeps the prompts it was converted
    /// with, as the fork's port did.
    fn inference(&self, image: &RgbImage, mask: &GrayImage, _: &str) -> anyhow::Result<RgbImage> {
        RoremMixed::inference(
            self,
            &DynamicImage::ImageRgb8(image.clone()),
            mask,
            rorem_mixed::DEFAULT_PROMPT,
            rorem_mixed::DEFAULT_NEGATIVE_PROMPT,
            &RoremMixedOptions::default(),
        )
    }

    /// The fork's tiles: lettering up to 512 pixels with 128 around it,
    /// which RORem redraws at its 512 square.
    fn pass(&self) -> u32 {
        768
    }
}

/// `redrawn` back at `image`'s size: the diffusion models draw on their own
/// grid.
fn fitted(redrawn: RgbImage, image: &RgbImage) -> RgbImage {
    let (width, height) = image.dimensions();
    if redrawn.dimensions() == (width, height) {
        redrawn
    } else {
        imageops::resize(&redrawn, width, height, imageops::FilterType::Lanczos3)
    }
}

pub(super) async fn load(model: Inpainter, device: &'static Device) -> anyhow::Result<Model> {
    Ok(match model {
        Inpainter::Lama => Box::new(Lama::load(device).await?),
        Inpainter::Flux3Klein => Box::new(Flux3Klein::load(device).await?),
        Inpainter::RoremMixed => Box::new(RoremMixed::load(device).await?),
        Inpainter::QwenImage => Box::new(QwenImage::load(device).await?),
    })
}
