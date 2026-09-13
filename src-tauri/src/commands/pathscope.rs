//! The renderer's only route to a file dialog.
//!
//! `plugin:dialog|open` and `plugin:dialog|save` used to be reachable straight
//! from the webview (`dialog:default` in `capabilities/default.json`). They are
//! not any more, and these two commands replace them: they drive the SAME
//! `tauri-plugin-dialog` picker, but from Rust, so the chosen path is recorded
//! in [`PathScope`] before it is handed to the renderer. That ordering is the
//! whole point — see `crate::pathscope` for why a "register this path for me"
//! command would have been worthless.
//!
//! The argument and return shapes match the plugin's own commands so
//! `frontend/src/bridge/dialog.ts` keeps the signature its call sites already
//! use.

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

/// Everything the renderer picked, plus the grant it now holds.
///
/// Recording happens here rather than in the caller so there is exactly one
/// place a picker result becomes an allowed path, and it is the place that
/// produced it.
fn grant<R: tauri::Runtime>(app: &AppHandle<R>, path: &PathBuf, directory: bool) {
    let scope = app.state::<PathScope>();
    if directory {
        scope.remember_dir(path);
    } else {
        scope.remember_file(path);
    }
    // The fs plugin keeps its own runtime allowlist, and `plugin:dialog|open`
    // used to extend it on our behalf. It no longer runs, so the read the
    // renderer does next (an image preview via `readFile`) needs the grant
    // made here instead.
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

/// Open the OS file/folder picker. Returns the chosen absolute paths, or an
/// empty vec if the user cancelled.
///
/// Always a list, even for a single pick: the renderer's `dialogOpen` already
/// normalizes `string | string[] | null`, and one shape here means one code
/// path recording the grants.
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

    // The callback form, not `blocking_*`: the blocking helpers deadlock when
    // they land on the main thread, and a tauri command has no say in which
    // thread it runs on.
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

/// Open the OS save panel. Returns the chosen absolute path, or `None` on
/// cancel.
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
        // Tauri's own save command splits a `defaultPath` the same way: a bare
        // file name seeds the field, a full path also seeds the directory.
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
