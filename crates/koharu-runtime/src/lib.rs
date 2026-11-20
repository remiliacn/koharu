//! Gets Koharu ready to run models on this machine's GPU.

mod device;
mod hardware;
mod huggingface;
mod packages;
mod store;
mod transfer;

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

pub use device::{Backend, Device, Memory};
pub use huggingface::Repository;
pub use transfer::{State, Transfer, transfers};

/// Where Koharu keeps its data when the user chose a folder for it in
/// Settings, rather than the system drive: `[storage]` in `config.toml`. A
/// fork addition. Nothing moves on a change; the next start uses the new
/// folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub folder: Option<PathBuf>,
}

impl koharu_config::Section for StorageConfig {
    const NAME: &'static str = "storage";
}

/// The folder the user chose for Koharu's data, as this run started, when
/// Koharu can write it; a folder it can't is left for the default, with a
/// warning.
pub fn chosen() -> Option<&'static Path> {
    static CHOSEN: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        let folder = match koharu_config::get::<StorageConfig>() {
            Ok(storage) => storage.folder?,
            Err(error) => {
                tracing::warn!("the storage section doesn't read: {error}");
                return None;
            }
        };
        match holds(&folder) {
            Ok(()) => Some(folder),
            Err(error) => {
                tracing::warn!(
                    "Koharu can't keep its data in {}: {error}",
                    folder.display()
                );
                None
            }
        }
    });
    CHOSEN.as_deref()
}

/// That Koharu can keep its data in `folder`: it's made if it's missing, and
/// Koharu can write a file in it.
pub fn holds(folder: &Path) -> std::io::Result<()> {
    fs_err::create_dir_all(folder)?;
    tempfile::tempfile_in(folder).map(drop)
}

/// Koharu's root: the folder the user chose for its data, or else the folder
/// setup put it in, in Windows release builds, where this user can write it,
/// which a per-machine install's, under Program Files, isn't. The store goes
/// in it, and the app's projects beside the store.
pub fn root() -> Option<&'static Path> {
    static ROOT: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        if let Some(folder) = chosen() {
            return Some(folder.to_owned());
        }
        if !cfg!(all(windows, not(debug_assertions))) {
            return None;
        }
        let executable = std::env::current_exe().expect("Koharu knows where its executable is");
        let folder = executable.parent().expect("an executable is in a folder");
        match tempfile::tempfile_in(folder) {
            Err(error) if error.kind() == ErrorKind::PermissionDenied => None,
            _ => Some(folder.to_owned()),
        }
    });
    ROOT.as_deref()
}

/// Where every model runs: the best GPU, once what it needs is downloaded and
/// loaded. Every caller shares that start, and a failed one is tried again on
/// the next call.
pub async fn device() -> anyhow::Result<&'static Device> {
    static DEVICE: OnceCell<Device> = OnceCell::const_new();
    DEVICE
        .get_or_try_init(|| async {
            let gpu = hardware::candidates()
                .into_iter()
                .next()
                .context("GPU is not available")?;
            let device = packages::start(&gpu)
                .await
                .with_context(|| format!("{} can't be used", gpu.description))?;
            tracing::info!("models run on {}", device.description);
            Ok(device)
        })
        .await
}
