//! WebView2 keeps no form history.
//!
//! WebView2 (Chromium) remembers what is typed into form fields so it can suggest it later.
//! Pollis has no use for that, and anything typed into the app belongs in the encrypted local
//! database or nowhere. macOS and Linux use WebKit, which keeps no form history, so this is
//! Windows-only.
//!
//! On every start: general autofill and password autosave off, then clear what an earlier
//! build already saved.

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Profile2, ICoreWebView2Settings4, ICoreWebView2_13,
    COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL,
    COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE,
};
use webview2_com::ClearBrowsingDataCompletedHandler;
use windows_core::Interface;

/// Turn form history off and clear any already saved. Runs on the main thread.
pub fn disable(webview: &tauri::webview::PlatformWebview) -> windows_core::Result<()> {
    // SAFETY: COM calls on the live WebView2 that `with_webview` hands us on its own thread.
    unsafe {
        let core = webview.controller().CoreWebView2()?;
        let settings: ICoreWebView2Settings4 = core.Settings()?.cast()?;
        settings.SetIsGeneralAutofillEnabled(false)?;
        settings.SetIsPasswordAutosaveEnabled(false)?;

        let profile: ICoreWebView2Profile2 = core.cast::<ICoreWebView2_13>()?.Profile()?.cast()?;
        let done = ClearBrowsingDataCompletedHandler::create(Box::new(|result| {
            if let Err(e) = result {
                pollis_core::diag!("[webview] clearing saved form data failed: {e}");
            }
            Ok(())
        }));
        profile.ClearBrowsingData(
            COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL
                | COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE,
            &done,
        )?;
    }
    Ok(())
}
