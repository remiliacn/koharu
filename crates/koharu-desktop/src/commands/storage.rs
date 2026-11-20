//! Where Koharu keeps its data: its models and GPU runtime, projects, browser
//! profile, and logs, in a folder the user chooses off the system drive. A
//! fork addition. A change takes effect on the next start and moves nothing:
//! the old folder stays for the user to delete.

use anyhow::Context as _;
use koharu_runtime::StorageConfig;
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;

#[derive(Debug, Serialize, Type)]
pub struct DataFolder {
    /// Where this run keeps Koharu's data.
    pub current: String,
    /// The folder Settings chose, which the next start uses; `None` for the
    /// default.
    pub chosen: Option<String>,
    /// Whether the next start keeps it elsewhere than this run does.
    pub restart: bool,
}

fn data_folder(app: &AppHandle<CefRuntime>) -> anyhow::Result<DataFolder> {
    let current = match koharu_runtime::root() {
        Some(folder) => folder.to_owned(),
        None => dirs::data_local_dir()
            .context("the user's local data folder is unknown")?
            .join(&app.config().identifier),
    };
    let chosen = koharu_config::get::<StorageConfig>()?.folder;
    Ok(DataFolder {
        current: current.display().to_string(),
        restart: chosen.as_deref() != koharu_runtime::chosen(),
        chosen: chosen.map(|folder| folder.display().to_string()),
    })
}

#[tauri::command]
#[specta::specta]
pub fn get_data_folder(app: AppHandle<CefRuntime>) -> Result<DataFolder> {
    Ok(data_folder(&app)?)
}

/// Picks a folder for Koharu's data and keeps it for the next start, once
/// Koharu can write in it; a cancelled pick changes nothing.
#[tauri::command]
#[specta::specta]
pub async fn choose_data_folder(
    app: AppHandle<CefRuntime>,
    window: Window<CefRuntime>,
) -> Result<DataFolder> {
    let picked = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|picked| picked.path().to_owned());
    if let Some(folder) = picked {
        koharu_runtime::holds(&folder)
            .with_context(|| format!("Koharu can't keep its data in {}", folder.display()))?;
        koharu_config::update(|storage: &mut StorageConfig| storage.folder = Some(folder))?;
    }
    Ok(data_folder(&app)?)
}

/// Puts Koharu's data back in the default folder from the next start.
#[tauri::command]
#[specta::specta]
pub fn reset_data_folder(app: AppHandle<CefRuntime>) -> Result<DataFolder> {
    koharu_config::update(|storage: &mut StorageConfig| storage.folder = None)?;
    Ok(data_folder(&app)?)
}
