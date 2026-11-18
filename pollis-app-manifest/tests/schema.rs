//! The published JSON schema and the Rust manifest type describe the same shape.

use std::collections::BTreeSet;

use pollis_app_manifest::{ActionKind, EventKind, Manifest, TriggerKind};
use serde_json::Value;

fn schema() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pollis-app-wit/pollis-app.schema.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn full_manifest() -> Value {
    serde_json::to_value(
        Manifest::from_json(
            br##"{"$schema":"x","name":"a","version":"1","description":"d","triggers":["webhook","schedule","commands"],"schedules":[{"name":"s","every":60,"offset":1}],
                "read":{"channels":["*"],"history":1,"events":["created"]},
                "actions":["post"],"state":{"quota":1},"egress":["a.example"],
                "commands":[{"name":"t","description":"d","arg_hint":"<x>","request":false}]}"##,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn schema_properties_match_the_rust_type() {
    let schema = schema();
    let m = full_manifest();
    assert_eq!(keys(&schema["properties"]), keys(&m));
    assert_eq!(
        keys(&schema["properties"]["read"]["properties"]),
        keys(&m["read"])
    );
    assert_eq!(
        keys(&schema["properties"]["state"]["properties"]),
        keys(&m["state"])
    );
    assert_eq!(
        keys(&schema["properties"]["schedules"]["items"]["properties"]),
        keys(&m["schedules"][0])
    );
    // `request: false` is left out when serialized; the schema lists it.
    let mut command = keys(&m["commands"][0]);
    command.insert("request".to_string());
    assert_eq!(
        keys(&schema["properties"]["commands"]["items"]["properties"]),
        command
    );
}

/// The schema's command limits are the Rust ones (#2018).
#[test]
fn schema_command_limits_match_the_rust_ones() {
    use pollis_app_manifest::commands::*;
    let schema = schema();
    let c = &schema["properties"]["commands"];
    assert_eq!(c["maxItems"], MAX_COMMANDS);
    assert_eq!(
        c["items"]["properties"]["name"]["pattern"],
        format!("^[a-z][a-z0-9_-]{{0,{}}}$", MAX_COMMAND_NAME_LEN - 1)
    );
    assert_eq!(c["items"]["properties"]["description"]["maxLength"], MAX_COMMAND_DESCRIPTION_LEN);
    assert_eq!(c["items"]["properties"]["arg_hint"]["maxLength"], MAX_COMMAND_ARG_HINT_LEN);
}

#[test]
fn schema_enums_match_the_rust_enums() {
    let schema = schema();
    let enum_of = |ptr: &str| -> BTreeSet<String> {
        schema
            .pointer(ptr)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    for (ptr, field) in [
        ("/properties/triggers/items/enum", "triggers"),
        ("/properties/actions/items/enum", "actions"),
        ("/properties/read/properties/events/items/enum", "events"),
    ] {
        for value in enum_of(ptr) {
            let json = match field {
                "events" => {
                    format!(r#"{{"name":"a","version":"1","read":{{"events":["{value}"]}}}}"#)
                }
                // The schedule trigger comes with its schedules.
                _ if value == "schedule" => {
                    r#"{"name":"a","version":"1","triggers":["schedule"],"schedules":[{"name":"s","every":60}]}"#
                        .to_string()
                }
                // So does the commands trigger, with its commands.
                _ if value == "commands" => {
                    r#"{"name":"a","version":"1","triggers":["commands"],"commands":[{"name":"c","description":"d"}]}"#
                        .to_string()
                }
                _ => format!(r#"{{"name":"a","version":"1","{field}":["{value}"]}}"#),
            };
            assert!(
                Manifest::from_json(json.as_bytes()).is_ok(),
                "schema allows {field} {value:?} but Rust refuses it"
            );
        }
    }
    // And the other direction: every Rust variant is in the schema.
    let names = |v: Vec<&str>| v.into_iter().map(String::from).collect::<BTreeSet<_>>();
    assert_eq!(
        enum_of("/properties/triggers/items/enum"),
        names(TriggerKind::ALL.iter().map(|t| t.as_str()).collect())
    );
    assert_eq!(
        enum_of("/properties/actions/items/enum"),
        names(ActionKind::ALL.iter().map(|a| a.as_str()).collect())
    );
    assert_eq!(
        enum_of("/properties/read/properties/events/items/enum"),
        names(EventKind::ALL.iter().map(|e| e.as_str()).collect())
    );
}
