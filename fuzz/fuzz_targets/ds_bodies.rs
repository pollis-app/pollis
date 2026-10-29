//! Fuzz target for the DS request bodies (#1537) and `check_cursor_stamp`.
//!
//! Properties:
//!   * **No panic** parsing any bytes as any body.
//!   * **No silent misparse:** an accepted body round-trips to the same value.
//!   * **Admitted stamps sort where their instants belong** when compared as text.
//!
//! Teeth: `--cfg fuzz_mutant` admits any UTC RFC 3339, letting `Z` and `+00:00` mix.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use serde::{de::DeserializeOwned, Serialize};

#[derive(Debug, Arbitrary)]
enum Input {
    Body { kind: u8, bytes: Vec<u8> },
    Stamps(Stamp, Stamp),
}

/// A stamp spelled as real clients spell them, or any string.
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
