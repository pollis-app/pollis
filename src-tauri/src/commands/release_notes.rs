//! The newest desktop release's notes, for the Update page (#1916).
//!
//! The notes are the `notes` field of the updater feed (`update-<bundle type>.json`), written by
//! the release workflow from that version's `CHANGELOG.md` entry. This asks the updater plugin
//! itself for the feed, so the endpoints, the per-bundle file, the HTTP client and the overlay
//! plan are the ones every update check already uses: no second URL, no second client.
//!
//! Desktop-only, so it lives here rather than in pollis-core: the feed is picked by Tauri's
//! bundle type, which only the shell knows, and any pollis-core change moves the mobile OTA
//! fingerprint.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri_plugin_updater::UpdaterExt;

use pollis_core::commands::update::UpdateCheckPlan;
use pollis_core::state::AppState;

/// Upper bound on the one feed request the Update page makes.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest notes handed to the renderer; the feed is unsigned text from the CDN.
const MAX_NOTES_CHARS: usize = 8_000;
/// Longest version string accepted from the feed.
const MAX_VERSION_CHARS: usize = 64;

/// Mirrored by `ReleaseNotes` in `frontend/src/types/index.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseNotes {
    /// The feed's version, without a leading `v`.
    pub version: String,
    /// The changelog entry as written: Markdown-style text, never HTML to the renderer.
    pub notes: String,
}

/// The version and notes an updater feed carries, or `None` when it has no usable notes.
pub fn parse_release_notes(feed: &serde_json::Value) -> Option<ReleaseNotes> {
    let version = feed.get("version")?.as_str()?.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let version_ok = !version.is_empty()
        && version.len() <= MAX_VERSION_CHARS
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if !version_ok {
        return None;
    }

    let raw = feed.get("notes")?.as_str()?;
    // Line breaks and tabs are layout; every other control character is dropped.
    let cleaned: String = raw
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let mut notes = cleaned.trim().to_string();
    if notes.is_empty() {
        return None;
    }
    if notes.chars().count() > MAX_NOTES_CHARS {
        let capped: String = notes.chars().take(MAX_NOTES_CHARS).collect();
        // End on a whole line where there is one, so a cut never leaves half a word.
        notes = match capped.rfind('\n') {
            Some(end) => capped[..end].trim_end().to_string(),
            None => capped,
        };
    }

    Some(ReleaseNotes {
        version: version.to_string(),
        notes,
    })
}

async fn fetch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    plan: UpdateCheckPlan,
) -> std::result::Result<Option<ReleaseNotes>, String> {
    let mut builder = app
        .updater_builder()
        // Every version counts as an update here: the page shows the newest release's notes
        // when that release is the one running, and a plain check returns nothing then.
        .version_comparator(|_, _| true)
        .timeout(FETCH_TIMEOUT);
    match plan {
        UpdateCheckPlan::Direct => {}
        UpdateCheckPlan::Proxy { url } => {
            builder = builder.proxy(url.parse().map_err(|e| format!("bad proxy url: {e}"))?);
        }
        // Strict overlay with no circuit: the update check is skipped, so this is too.
        UpdateCheckPlan::Blocked { .. } => return Ok(None),
    }
    let updater = builder.build().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.and_then(|u| parse_release_notes(&u.raw_json)))
}

/// One feed request; never retried. Every failure is "no notes": the page then has no section.
#[tauri::command]
pub async fn get_latest_release_notes<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, Arc<AppState>>,
) -> pollis_core::error::Result<Option<ReleaseNotes>> {
    let plan = pollis_core::commands::update::get_update_check_plan(&state).await?;
    match fetch(&app, plan).await {
        Ok(notes) => Ok(notes),
        Err(e) => {
            pollis_core::diag!("[update] release notes unavailable: {e}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A feed shaped like the one `desktop-release.yml` publishes.
    fn feed(version: &str, notes: serde_json::Value) -> serde_json::Value {
        json!({
            "version": version,
            "pub_date": "2026-11-14T12:00:00Z",
            "notes": notes,
            "platforms": {
                "darwin-aarch64": { "url": "https://cdn.pollis.com/releases/v1.19.12/Pollis.app.tar.gz", "signature": "sig" }
            }
        })
    }

    #[test]
    fn a_feed_with_notes_gives_its_version_and_text() {
        let got = parse_release_notes(&feed(
            "1.19.12",
            json!("- **Release notes** on the Update page.\n- A fix."),
        ));
        assert_eq!(
            got,
            Some(ReleaseNotes {
                version: "1.19.12".into(),
                notes: "- **Release notes** on the Update page.\n- A fix.".into(),
            })
        );
    }

    #[test]
    fn a_feed_without_notes_gives_nothing() {
        // Every feed published before notes existed, and any release without a changelog entry.
        let mut bare = feed("1.19.11", json!(null));
        bare.as_object_mut().unwrap().remove("notes");
        assert_eq!(parse_release_notes(&bare), None);
        assert_eq!(parse_release_notes(&feed("1.19.11", json!(null))), None);
        assert_eq!(parse_release_notes(&feed("1.19.11", json!(""))), None);
        assert_eq!(
            parse_release_notes(&feed("1.19.11", json!("  \n\t\n"))),
            None
        );
        assert_eq!(
            parse_release_notes(&feed("1.19.11", json!(["- a list, not text"]))),
            None
        );
        assert_eq!(parse_release_notes(&json!("not an object")), None);
    }

    #[test]
    fn the_version_loses_its_v_and_must_look_like_one() {
        let got = parse_release_notes(&feed("v1.20.0-rc.1", json!("- x"))).unwrap();
        assert_eq!(got.version, "1.20.0-rc.1");
        for bad in [
            "",
            "v",
            "latest",
            "1.2.3 <b>",
            "1.2.3\n4",
            &"1".repeat(MAX_VERSION_CHARS + 1),
        ] {
            assert_eq!(
                parse_release_notes(&feed(bad, json!("- x"))),
                None,
                "{bad:?}"
            );
        }
        let mut no_version = feed("1.0.0", json!("- x"));
        no_version.as_object_mut().unwrap().remove("version");
        assert_eq!(parse_release_notes(&no_version), None);
    }

    #[test]
    fn notes_are_trimmed_and_stripped_of_control_characters() {
        let got = parse_release_notes(&feed(
            "1.19.12",
            json!("\r\n- one\r\n- two\u{0}\u{1b}[31m\n\n"),
        ))
        .unwrap();
        assert_eq!(got.notes, "- one\n- two[31m");
    }

    #[test]
    fn markup_stays_text() {
        // Nothing is interpreted here; the renderer shows it as text.
        let got =
            parse_release_notes(&feed("1.19.12", json!("- <img src=x onerror=alert(1)>"))).unwrap();
        assert_eq!(got.notes, "- <img src=x onerror=alert(1)>");
    }

    #[test]
    fn oversized_notes_are_cut_at_a_line_end() {
        let line = "- a line of release notes\n";
        let long = line.repeat(MAX_NOTES_CHARS / line.len() + 50);
        let got = parse_release_notes(&feed("1.19.12", json!(long))).unwrap();
        assert!(got.notes.chars().count() <= MAX_NOTES_CHARS);
        assert!(got.notes.ends_with("- a line of release notes"));

        // One unbroken run has no line to end on; it is cut at the limit, on a character.
        let got =
            parse_release_notes(&feed("1.19.12", json!("é".repeat(MAX_NOTES_CHARS + 10)))).unwrap();
        assert_eq!(got.notes.chars().count(), MAX_NOTES_CHARS);
    }
}
