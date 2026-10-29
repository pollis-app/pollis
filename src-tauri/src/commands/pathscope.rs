//! The renderer's only route to a file dialog: the `tauri-plugin-dialog`
//! picker driven from Rust, so the chosen path is recorded in [`PathScope`]
//! before the renderer sees it. Shapes match the plugin's own commands so
//! `frontend/src/bridge/dialog.ts` keeps its signature.

use std::path::PathBuf;

use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tauri_plugin_fs::FsExt;

use crate::error::Result;
use crate::pathscope::PathScope;

#[derive(Debug, Clone, Deserialize)]
pub struct DialogFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDialogOptions {
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub directory: bool,
    pub title: Option<String>,
    pub default_path: Option<String>,
    #[serde(default)]
    pub filters: Vec<DialogFilter>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDialogOptions {
    pub title: Option<String>,
    pub default_path: Option<String>,
    #[serde(default)]
    pub filters: Vec<DialogFilter>,
}

/// The one place a picker result becomes an allowed path.
fn grant<R: tauri::Runtime>(app: &AppHandle<R>, path: &PathBuf, directory: bool) {
    let scope = app.state::<PathScope>();
    if directory {
        scope.remember_dir(path);
    } else {
        scope.remember_file(path);
    }
    // The fs plugin keeps its own allowlist; the renderer's next read (an
    // image preview via `readFile`) needs it.
    if let Some(fs) = app.try_fs_scope() {
        let _ = if directory {
            fs.allow_directory(path, true)
        } else {
            fs.allow_file(path)
        };
    }
}

fn into_paths(picked: Vec<FilePath>) -> Vec<PathBuf> {
    picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect()
}

/// Open the OS file/folder picker. Returns the chosen paths (empty on cancel);
/// always a list so one code path records the grants.
#[tauri::command]
pub async fn pick_open_paths<R: tauri::Runtime>(
    app: AppHandle<R>,
    options: OpenDialogOptions,
) -> Result<Vec<String>> {
    let mut builder = app.dialog().file();
    if let Some(title) = &options.title {
        builder = builder.set_title(title);
    }
    if let Some(default_path) = &options.default_path {
        builder = builder.set_directory(default_path);
    }
    for filter in &options.filters {
        let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        builder = builder.add_filter(&filter.name, &extensions);
    }

    // Callback form, not `blocking_*`: those deadlock on the main thread, and
    // a command can't choose its thread.
    let (tx, rx) = tokio::sync::oneshot::channel();
    match (options.directory, options.multiple) {
        (true, true) => builder.pick_folders(move |r| {
            let _ = tx.send(r.unwrap_or_default());
        }),
        (true, false) => builder.pick_folder(move |r| {
            let _ = tx.send(r.into_iter().collect());
        }),
        (false, true) => builder.pick_files(move |r| {
            let _ = tx.send(r.unwrap_or_default());
        }),
        (false, false) => builder.pick_file(move |r| {
            let _ = tx.send(r.into_iter().collect());
        }),
    }
    let picked = rx.await.unwrap_or_default();

    let paths = into_paths(picked);
    for path in &paths {
        grant(&app, path, options.directory);
    }
    Ok(paths
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// Open the OS save panel. `None` on cancel.
#[tauri::command]
pub async fn pick_save_path<R: tauri::Runtime>(
    app: AppHandle<R>,
    options: SaveDialogOptions,
) -> Result<Option<String>> {
    let mut builder = app.dialog().file();
    if let Some(title) = &options.title {
        builder = builder.set_title(title);
    }
    if let Some(default_path) = &options.default_path {
        let path = std::path::Path::new(default_path);
        // Same split as Tauri's own save command: the name seeds the field,
        // the parent seeds the directory.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            builder = builder.set_file_name(name);
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                builder = builder.set_directory(parent);
            }
        }
    }
    for filter in &options.filters {
        let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        builder = builder.add_filter(&filter.name, &extensions);
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    builder.save_file(move |r| {
        let _ = tx.send(r);
    });
    let Some(picked) = rx.await.unwrap_or(None) else {
        return Ok(None);
    };
    let Ok(path) = picked.into_path() else {
        return Ok(None);
    };

    let scope = app.state::<PathScope>();
    scope.remember_save_target(&path);
    if let Some(fs) = app.try_fs_scope() {
        let _ = fs.allow_file(&path);
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}
