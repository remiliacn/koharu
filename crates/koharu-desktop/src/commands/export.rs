//! Export: every page as PNG or PSD files in a folder, or as a CBZ of PNGs.

use std::{
    collections::HashSet,
    io::{Cursor, Write as _},
    path::Path,
    sync::Arc,
};

use anyhow::Context as _;
use fs_err::File;
use image::ImageFormat;
use koharu_psd::Labels;
use koharu_renderer::Frame;
use koharu_scene::PageId;
use serde::Deserialize;
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::app::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Png,
    Psd,
    Cbz,
}

/// Picks where the open project's pages go, and writes them there: `pages`,
/// or every page when it's empty. `labels` names a PSD's layers the scene
/// names nothing, in the interface's language.
#[tauri::command]
#[specta::specta]
pub async fn export(
    app: State<'_, Arc<App>>,
    window: Window<CefRuntime>,
    format: ExportFormat,
    labels: Labels,
    pages: Vec<PageId>,
) -> Result<()> {
    let name = app
        .project
        .lock()
        .as_ref()
        .map(|project| project.name.clone())
        .context("no project is open")?;
    let dialog = rfd::AsyncFileDialog::new().set_parent(&window);
    let destination = match format {
        ExportFormat::Png | ExportFormat::Psd => dialog.pick_folder().await,
        ExportFormat::Cbz => {
            dialog
                .set_file_name(format!("{name}.cbz"))
                .add_filter("CBZ", &["cbz"])
                .save_file()
                .await
        }
    };
    let Some(destination) = destination.map(|picked| picked.path().to_owned()) else {
        return Ok(());
    };
    let app = Arc::clone(&app);
    Ok(
        tokio::task::spawn_blocking(move || write(&app, format, &labels, &pages, &destination))
            .await??,
    )
}

/// Writes `only` the pages it names, or every page when it names none, each
/// under the name `stems` gives it among all the project's pages, so a page
/// exported alone keeps the name a whole export gives it.
fn write(
    app: &App,
    format: ExportFormat,
    labels: &Labels,
    only: &[PageId],
    destination: &Path,
) -> anyhow::Result<()> {
    let session = app.session()?;
    let pages: Vec<(PageId, String)> = session
        .lock()
        .project()
        .pages()
        .map(|page| (page.id, page.name.clone()))
        .collect();
    let names: Vec<&str> = pages.iter().map(|(_, name)| name.as_str()).collect();
    let stems = stems(&names, matches!(format, ExportFormat::Cbz));
    let mut archive = match format {
        ExportFormat::Cbz => Some(::zip::ZipWriter::new(File::create(destination)?)),
        ExportFormat::Png | ExportFormat::Psd => None,
    };
    let chosen = pages
        .iter()
        .zip(&stems)
        .filter(|((page, _), _)| only.is_empty() || only.contains(page));
    for ((page, _), stem) in chosen {
        let page = *page;
        let frame = app.renderer.render(&session.lock(), page)?;
        match (format, &mut archive) {
            (ExportFormat::Psd, _) => {
                let psd = koharu_psd::export(&session.lock(), &app.renderer, page, labels)?;
                fs_err::write(destination.join(format!("{stem}.psd")), psd)?;
            }
            (_, Some(archive)) => {
                // Already-compressed PNGs gain nothing from deflate.
                let options = ::zip::write::SimpleFileOptions::default()
                    .compression_method(::zip::CompressionMethod::Stored);
                archive.start_file(format!("{stem}.png"), options)?;
                archive.write_all(&png(&frame)?)?;
            }
            (_, None) => {
                fs_err::write(destination.join(format!("{stem}.png")), png(&frame)?)?;
            }
        }
    }
    if let Some(archive) = archive {
        archive.finish()?;
    }
    Ok(())
}

/// Each page's file name without its extension: the name of the file it was
/// made from, as `cover`, in characters every filesystem takes, or its number
/// when it has none. A CBZ's names start with the number, `0001_cover`, as a
/// reader orders its pages by name. A name already taken, ignoring case as
/// Windows does, gets ` (2)`, ` (3)`, and so on, as Explorer's copies do.
fn stems(names: &[&str], numbered: bool) -> Vec<String> {
    let digits = names.len().to_string().len().max(4);
    let mut taken = HashSet::new();
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let number = index + 1;
            let name = name
                .trim()
                .trim_end_matches(|character: char| character == '.' || character.is_whitespace());
            let name = name
                .rsplit_once('.')
                .map_or(name, |(stem, _)| stem)
                .replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "_");
            let stem = match (name.is_empty(), numbered) {
                (true, _) => format!("{number:0digits$}"),
                (false, true) => format!("{number:0digits$}_{name}"),
                (false, false) => name,
            };
            (1..)
                .map(|copy| match copy {
                    1 => stem.clone(),
                    _ => format!("{stem} ({copy})"),
                })
                .find(|candidate| taken.insert(candidate.to_lowercase()))
                .expect("some copy number is free")
        })
        .collect()
}

fn png(frame: &Frame) -> anyhow::Result<Vec<u8>> {
    let raster = frame.rasterize()?;
    let mut png = Cursor::new(Vec::new());
    raster.image.write_to(&mut png, ImageFormat::Png)?;
    Ok(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::stems;

    #[test]
    fn a_page_is_exported_under_its_files_name() {
        let pages = &["cover.png", "chap 1 p3.jpeg", "a:b?.webp", ""];
        assert_eq!(stems(pages, false), ["cover", "chap 1 p3", "a_b_", "0004"]);
    }

    #[test]
    fn a_cbz_numbers_its_pages_for_a_readers_order() {
        let pages = &["cover.png", ""];
        assert_eq!(stems(pages, true), ["0001_cover", "0002"]);
    }

    #[test]
    fn a_name_taken_already_gets_a_copy_number() {
        let pages = &["page.png", "Page.jpg", "page.webp", "page (2).png"];
        assert_eq!(
            stems(pages, false),
            ["page", "Page (2)", "page (3)", "page (2) (2)"]
        );
    }
}
