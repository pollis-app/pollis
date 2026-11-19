//! The public helpers that need no sandbox.

use pollis_app::{bail, Error, MessageRef, Webhook};

#[test]
fn the_wit_is_the_canonical_pollis_app_wit() {
    let root = env!("CARGO_MANIFEST_DIR");
    let copy = std::fs::read_to_string(format!("{root}/wit/app.wit")).unwrap();
    let canonical = std::fs::read_to_string(format!("{root}/../../../pollis-app-wit/wit/app.wit")).unwrap();
    assert_eq!(copy, canonical, "copy pollis-app-wit/wit/app.wit to sdk/rust/pollis-app/wit/");
}

#[test]
fn question_mark_converts_any_standard_error() {
    fn parse() -> pollis_app::Result<u8> {
        Ok("not a number".parse::<u8>()?)
    }
    let error = parse().unwrap_err();
    assert_eq!(error.to_string(), "invalid digit found in string");
    assert!(!error.is_refused());
}

#[test]
fn bail_returns_a_formatted_error() {
    fn fails(id: u32) -> pollis_app::Result {
        bail!("no ticket {id}");
    }
    assert_eq!(fails(7).unwrap_err().message(), "no ticket 7");
    assert_eq!(Error::msg("plain").to_string(), "plain");
}

#[test]
fn a_webhook_reads_as_text_json_and_headers() {
    let webhook = Webhook {
        delivery_id: "d1".to_string(),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body: br#"{"n":7}"#.to_vec(),
    };
    assert_eq!(webhook.text(), r#"{"n":7}"#);
    assert_eq!(webhook.json::<serde_json::Value>().unwrap()["n"], 7);
    assert_eq!(webhook.header("content-type"), Some("application/json"));
    assert_eq!(webhook.header("x-missing"), None);
    assert!(webhook.json::<Vec<u8>>().is_err());
}

#[test]
fn a_message_is_addressed_by_value_reference_or_id() {
    fn id(target: impl MessageRef) -> String {
        target.message_id().to_string()
    }
    let owned = String::from("m1");
    assert_eq!(id("m1"), "m1");
    assert_eq!(id(&owned), "m1");
    assert_eq!(id(owned), "m1");
}
