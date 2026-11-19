//! The TypeScript SDK's manifest checks (`sdk/ts/app/src/manifest.ts`)
//! mirror the runtime's. Both suites read the same fixtures,
//! `sdk/ts/app/test/fixtures/manifests/{valid,invalid}/*.json`; this one
//! holds the runtime's parser to them, `manifest.test.ts` the SDK's.

use std::path::PathBuf;

use pollis_app_manifest::Manifest;

fn fixtures(kind: &str) -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sdk/ts/app/test/fixtures/manifests")
        .join(kind);
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures in {}", dir.display());
    out
}

#[test]
fn the_runtime_accepts_every_valid_fixture() {
    for (name, bytes) in fixtures("valid") {
        if let Err(e) = Manifest::from_json(&bytes) {
            panic!("{name}: refused: {e}");
        }
    }
}

#[test]
fn the_runtime_refuses_every_invalid_fixture() {
    for (name, bytes) in fixtures("invalid") {
        assert!(Manifest::from_json(&bytes).is_err(), "{name}: accepted");
    }
}
