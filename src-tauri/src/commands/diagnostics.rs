// Shim: forwards to pollis_core::commands::diagnostics. Edit pollis-core, not here.

#[tauri::command]
pub fn get_diagnostics(app: tauri::AppHandle) -> String {
    pollis_core::commands::diagnostics::get_diagnostics(Some(app.package_info().version.to_string()))
}
