// Shim: forwards to pollis_core::commands::diagnostics. Edit pollis-core, not here.

#[tauri::command]
pub fn get_diagnostics<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> String {
    pollis_core::commands::diagnostics::get_diagnostics(Some(app.package_info().version.to_string()))
}
