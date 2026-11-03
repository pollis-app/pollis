// Shim: forwards to pollis_core::commands::diagnostics. Edit pollis-core, not here.

#[tauri::command]
pub fn get_diagnostics<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> String {
    pollis_core::commands::diagnostics::get_diagnostics(Some(app.package_info().version.to_string()))
}

// `path` must be a location picked in the save panel (`crate::pathscope`).
#[tauri::command]
pub fn save_diagnostics<R: tauri::Runtime>(path: String, app: tauri::AppHandle<R>, scope: tauri::State<'_, crate::pathscope::PathScope>) -> pollis_core::error::Result<()> {
    scope.require(std::path::Path::new(&path), "save_diagnostics")?;
    pollis_core::commands::diagnostics::save_diagnostics(path, Some(app.package_info().version.to_string()))
}
