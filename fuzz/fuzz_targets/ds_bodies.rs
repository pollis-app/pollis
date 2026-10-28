//! Fuzz target for the DS request bodies (#1537): the `pollis-api` wire types
//! the Delivery Service parses from every signed POST on the message path, and
//! the DS's admission check for the client-chosen stamps those bodies carry
//! (`pollis_delivery::messages::check_cursor_stamp`).
//!
//! Properties:
//!   * **No panic** parsing any bytes as any body.
//!   * **No silent misparse:** a body the DS accepts re-serializes to bytes that
//!     parse back to the same value — a round trip through the wire type is
//!     stable, so nothing the client sent is dropped or reinterpreted on the way
//!     into the handler.
//!   * **Admitted stamps sort where their instants belong** — the reason the
//!     check exists (`sent_at` and `last_fetched_at` are compared as text): for
//!     any two admitted stamps, an earlier instant is the smaller string.
//!
//! NEGATIVE CHECK (teeth): with `--cfg fuzz_mutant` stamps are admitted by a
//! lenient check (anything RFC 3339 in UTC), which lets a `Z`-suffixed stamp in
//! beside the `+00:00` ones and breaks the ordering.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use serde::{de::DeserializeOwned, Serialize};

#[derive(Debug, Arbitrary)]
enum Input {
    Body { kind: u8, bytes: Vec<u8> },
    Stamps(Stamp, Stamp),
}

/// A stamp built the way clients and old builds actually spell them — every
/// fraction width, `Z` / `+00:00` / other offsets, odd separators — or any
/// string at all.
#[derive(Debug, Arbitrary)]
enum Stamp {
    Shaped {
        /// Seconds into the hour before `now`.
        secs: u16,
        nanos: u32,
        width: u8,
        suffix: u8,
        separator: bool,
    },
    Raw(String),
}

impl Stamp {
    fn render(&self) -> String {
        match self {
            Stamp::Raw(s) => s.clone(),
            Stamp::Shaped {
                secs,
                nanos,
                width,
                suffix,
                separator,
            } => {
                let s = (*secs % 3600) as u32;
                let base = format!(
                    "2026-10-28{}11:{:02}:{:02}",
                    if *separator { 'T' } else { ' ' },
                    s / 60,
                    s % 60
                );
                let frac = match width % 5 {
                    0 => String::new(),
                    1 => format!(".{:03}", (nanos % 1_000_000_000) / 1_000_000),
                    2 => format!(".{:06}", (nanos % 1_000_000_000) / 1_000),
                    3 => format!(".{:09}", nanos % 1_000_000_000),
                    _ => format!(".{}", nanos % 10),
                };
                let suffix = ["+00:00", "Z", "-00:00", "+01:00"][(*suffix % 4) as usize];
                format!("{base}{frac}{suffix}")
            }
        }
    }
}

fn round_trip<T: Serialize + DeserializeOwned>(bytes: &[u8]) {
    let Ok(v) = serde_json::from_slice::<T>(bytes) else {
        return;
    };
    // Compared as JSON values, so a map's key order is not mistaken for a change.
    let once = serde_json::to_value(&v).expect("an accepted body serializes");
    let bytes = serde_json::to_vec(&once).expect("serializes");
    let again: T = serde_json::from_slice(&bytes).expect("an accepted body re-parses");
    let twice = serde_json::to_value(&again).expect("serializes");
    assert_eq!(once, twice, "a body changed on its way through the wire type");
}

fn admitted(stamp: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    #[cfg(fuzz_mutant)]
    {
        let _ = now;
        // BUG: any UTC RFC 3339 shape.
        return chrono::DateTime::parse_from_rfc3339(stamp)
            .is_ok_and(|p| p.offset().local_minus_utc() == 0);
    }
    #[cfg(not(fuzz_mutant))]
    pollis_delivery::messages::check_cursor_stamp(stamp, now).is_ok()
}

fuzz_target!(|input: Input| {
    match input {
        Input::Body { kind, bytes } => {
            use pollis_api::{commit, directory, messages, reads};
            match kind % 9 {
                0 => round_trip::<messages::SendMessageBody>(&bytes),
                1 => round_trip::<messages::EditMessageBody>(&bytes),
                2 => round_trip::<messages::DeleteMessageBody>(&bytes),
                3 => round_trip::<messages::WatermarkBody>(&bytes),
                4 => round_trip::<messages::ReactionBody>(&bytes),
                5 => round_trip::<commit::SubmitBody>(&bytes),
                6 => round_trip::<directory::CatchUpBody>(&bytes),
                7 => round_trip::<reads::ConversationStateBody>(&bytes),
                _ => round_trip::<directory::CatchUpResponse>(&bytes),
            }
        }
        Input::Stamps(a, b) => {
            let (a, b) = (a.render(), b.render());
            let now = chrono::DateTime::parse_from_rfc3339("2026-10-28T12:00:00+00:00")
                .expect("now")
                .to_utc();
            if !(admitted(&a, now) && admitted(&b, now)) {
                return;
            }
            let pa = chrono::DateTime::parse_from_rfc3339(&a).expect("admitted parses");
            let pb = chrono::DateTime::parse_from_rfc3339(&b).expect("admitted parses");
            if pa < pb {
                assert!(a < b, "{a} sorts after {b}, a later instant");
            }
            if pb < pa {
                assert!(b < a, "{b} sorts after {a}, a later instant");
            }
        }
    }
});
