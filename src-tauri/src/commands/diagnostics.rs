// Shim: forwards to pollis_core::commands::diagnostics. Edit pollis-core, not here.

#[tauri::command]
pub fn get_diagnostics<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> String {
    pollis_core::commands::diagnostics::get_diagnostics(Some(app.package_info().version.to_string()))
}

// Desktop-only, so it lives here rather than in pollis-core: any pollis-core change moves the
// mobile OTA fingerprint, which would keep a JS-only diagnostics fix off the phones already out.
// `path` must be a location picked in the save panel (`crate::pathscope`).
#[tauri::command]
pub fn save_diagnostics<R: tauri::Runtime>(path: String, app: tauri::AppHandle<R>, scope: tauri::State<'_, crate::pathscope::PathScope>) -> pollis_core::error::Result<()> {
    scope.require(std::path::Path::new(&path), "save_diagnostics")?;
    let report = pollis_core::commands::diagnostics::get_diagnostics(Some(app.package_info().version.to_string()));
    std::fs::write(&path, report)
        .map_err(|e| pollis_core::error::Error::Other(anyhow::anyhow!("could not write the diagnostics file: {e}")))
}
