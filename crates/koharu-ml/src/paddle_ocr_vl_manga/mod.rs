//! PaddleOCR-VL-For-Manga crop recognition through llama.cpp.
//!
//! Manga fine-tune of the original PaddleOCR-VL:
//! https://huggingface.co/jzhang533/PaddleOCR-VL-For-Manga/tree/1e8aa5f1dd90cc86fe9137c9c0b26ebde613cfe8
//! Generation follows the base model card:
//! https://huggingface.co/PaddlePaddle/PaddleOCR-VL/tree/7fa00a8c55b735ba51ba49a9058f3f9c57a99a11

use anyhow::{Context, Result, ensure};
use image::DynamicImage;

use crate::{
    Device,
    llm::{GenerationOptions, Input, Llm, LoadOptions, MtmdOptions, media_marker},
};

const MAX_NEW_TOKENS: usize = 1024;

crate::model_repository!("LunarOilRig/PaddleOCR-VL-manga-GGUF" @ "c22aca138c97341f2cdd544ec64063cd10456b72" {
    MODEL = "PaddleOCR-VL-manga-Q4_K_M.gguf",
    PROJECTOR = "mmproj-Q8_0.gguf",
});

#[derive(Debug)]
pub struct PaddleOCRVLManga {
    model: Llm,
}

impl PaddleOCRVLManga {
    pub async fn load(device: Device) -> Result<Self> {
        let (model_path, projector_path) =
            tokio::try_join!(MODEL.resolve(), PROJECTOR.resolve())
                .context("failed to resolve PaddleOCR-VL-For-Manga files")?;
        let model = Llm::load_with_options(
            device,
            model_path,
            LoadOptions {
                mtmd: Some(MtmdOptions::new(projector_path)),
                ..LoadOptions::default()
            },
        )
        .await
        .context("failed to load PaddleOCR-VL-For-Manga")?;
        ensure!(
            model.capabilities().vision,
            "PaddleOCR-VL-For-Manga projector does not support images"
        );
        Ok(Self { model })
    }

    pub fn inference(&self, image: &DynamicImage) -> Result<String> {
        // The embedded chat template predates PaddleOCR-VL 1.6 and renders only
        // list-form message content, so the prompt is that template's output.
        let prompt = format!(
            "<|begin_of_sentence|>User: {}OCR:\nAssistant: ",
            media_marker()
        );
        let generation = self.model.inference(
            &Input::new(&prompt).with_image(image),
            &GenerationOptions {
                max_tokens: MAX_NEW_TOKENS,
                temperature: 0.0,
                repeat_penalty: 1.0,
                add_special: true,
                ..GenerationOptions::default()
            },
        )?;
        Ok(decode_xml_entities(&generation.text))
    }
}

/// Divergence from upstream, which returns the raw generation: this fine-tune
/// sometimes writes a literal `&` as `&amp;`, so the five XML entities are
/// decoded in a single pass and any other `&` passes through.
fn decode_xml_entities(text: &str) -> String {
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        match ENTITIES.iter().find(|(entity, _)| rest.starts_with(entity)) {
            Some((entity, character)) => {
                output.push(*character);
                rest = &rest[entity.len()..];
            }
            None => {
                output.push('&');
                rest = &rest[1..];
            }
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::decode_xml_entities;

    #[test]
    fn decodes_xml_entities_once() {
        assert_eq!(decode_xml_entities("Ｒ&amp;D"), "Ｒ&D");
        assert_eq!(decode_xml_entities("&lt;&quot;Hi&apos;&gt;"), "<\"Hi'>");
        assert_eq!(decode_xml_entities("&amp;lt;"), "&lt;");
    }

    #[test]
    fn keeps_other_ampersands() {
        assert_eq!(decode_xml_entities("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(decode_xml_entities("R&D &nbsp; &"), "R&D &nbsp; &");
        assert_eq!(decode_xml_entities("トム＆ジェリー"), "トム＆ジェリー");
    }
}
