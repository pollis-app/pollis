//! Fuzz target for the decrypt path's parsing (#1537): `MlsDecryptor::decrypt`
//! and `envelope_lineage`, fed envelope bytes that arrive from the DS and are
//! therefore untrusted — the TLS framing, the MLS header, the sender data and
//! the AEAD all parse attacker-chosen bytes.
//!
//! A real two-member group is stood up once (pollis-core's provider, suite and
//! configurations) and a handful of genuine envelopes are sealed into it. Each
//! input picks one, mutates it (flipped bytes, a truncation, appended junk) or
//! replaces it outright, and decrypts it on the receiving member inside a
//! savepoint that is rolled back, so no input spends a key another one needs.
//!
//! Properties:
//!   * **No panic**, whatever the bytes.
//!   * **Failure is an answer:** a decrypt that does not open returns a
//!     `DecryptFailure`.
//!   * **No forged acceptance:** a decrypt that opens yields exactly the
//!     plaintext of the genuine envelope it was cut from, and only when that
//!     envelope's bytes are intact (trailing bytes past a complete message are
//!     ignored by the TLS reader, which is harmless: same message).
//!
//! NEGATIVE CHECK (teeth): with `--cfg fuzz_mutant` the target "decrypts" by
//! header — anything whose MLS header parses at the group's position is taken
//! as the genuine message — the shortcut a lineage pre-check must never become.
#![no_main]

use std::sync::{Mutex, OnceLock};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::messages::fuzz_surface::rusqlite;
use pollis_core::commands::mls::fuzz_surface::openmls::prelude::tls_codec::{
    Deserialize as _, Serialize as _,
};
use pollis_core::commands::mls::fuzz_surface::openmls::prelude::*;
use pollis_core::commands::mls::fuzz_surface::openmls_basic_credential::SignatureKeyPair;
use pollis_core::commands::mls::fuzz_surface::{
    create_config, join_config, make_credential, MlsDecryptor, PollisProvider, CIPHERSUITE,
};
use pollis_core::commands::mls::{envelope_lineage, try_mls_encrypt};

const GROUP: &str = "G";

struct World {
    receiver: rusqlite::Connection,
    /// `(envelope bytes, its plaintext)`.
    genuine: Vec<(Vec<u8>, Vec<u8>)>,
}

fn device(name: &str) -> (rusqlite::Connection, SignatureKeyPair, CredentialWithKey) {
    let conn = rusqlite::Connection::open_in_memory().expect("open");
    pollis_core::db::local::apply_local_schema(&conn).expect("schema");
    let sig = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).expect("keys");
    sig.store(PollisProvider::new(&conn).storage()).expect("store signer");
    let cred = CredentialWithKey {
        credential: make_credential(name, name),
        signature_key: sig.to_public_vec().into(),
    };
    (conn, sig, cred)
}

fn world() -> &'static Mutex<World> {
    static WORLD: OnceLock<Mutex<World>> = OnceLock::new();
    WORLD.get_or_init(|| {
        let (sender, ssig, scred) = device("alice");
        let (receiver, rsig, rcred) = device("bob");
        let sp = PollisProvider::new(&sender);
        let mut group = MlsGroup::new_with_group_id(
            &sp,
            &ssig,
            &create_config(),
            GroupId::from_slice(GROUP.as_bytes()),
            scred,
        )
        .expect("create");
        let kp = KeyPackage::builder()
            .build(CIPHERSUITE, &PollisProvider::new(&receiver), &rsig, rcred)
            .expect("key package")
            .key_package()
            .clone();
        let (_commit, welcome, _) = group.add_members(&sp, &ssig, &[kp]).expect("add");
        group.merge_pending_commit(&sp).expect("merge");
        let bytes = welcome.tls_serialize_detached().expect("welcome");
        let MlsMessageBodyIn::Welcome(w) =
            MlsMessageIn::tls_deserialize(&mut bytes.as_slice()).expect("welcome").extract()
        else {
            panic!("not a welcome");
        };
        let rp = PollisProvider::new(&receiver);
        StagedWelcome::new_from_welcome(&rp, &join_config(), w, None)
            .expect("stage")
            .into_group(&rp)
            .expect("join");
        let genuine = (0..4)
            .map(|i| {
                let text = format!("genuine {i}").into_bytes();
                (try_mls_encrypt(&sender, GROUP, &text).expect("seal"), text)
            })
            .collect();
        Mutex::new(World { receiver, genuine })
    })
}

#[derive(Debug, Arbitrary)]
enum Input {
    Mutated {
        pick: u8,
        flips: Vec<(u16, u8)>,
        truncate: Option<u16>,
        append: Vec<u8>,
    },
    Raw(Vec<u8>),
}

fn decrypt(
    conn: &rusqlite::Connection,
    bytes: &[u8],
    genuine: &[(Vec<u8>, Vec<u8>)],
) -> Result<Vec<u8>, ()> {
    #[cfg(fuzz_mutant)]
    {
        // BUG: trust the header.
        if envelope_lineage(bytes) == envelope_lineage(&genuine[0].0) {
            return Ok(genuine[0].1.clone());
        }
    }
    let _ = genuine;
    let mut dec = MlsDecryptor::open(conn, GROUP).expect("a group");
    dec.decrypt(bytes).map(|(text, _)| text).map_err(|_| ())
}

fuzz_target!(|input: Input| {
    let guard = world().lock().unwrap_or_else(|e| e.into_inner());
    let (bytes, origin) = match input {
        Input::Mutated {
            pick,
            flips,
            truncate,
            append,
        } => {
            let i = pick as usize % guard.genuine.len();
            let mut b = guard.genuine[i].0.clone();
            for (at, x) in flips.into_iter().take(8) {
                let at = at as usize % b.len();
                b[at] ^= x;
            }
            if let Some(t) = truncate {
                b.truncate(t as usize % (b.len() + 1));
            }
            b.extend(append.into_iter().take(64));
            (b, Some(i))
        }
        Input::Raw(b) => (b, None),
    };
    let _ = envelope_lineage(&bytes);

    let conn = &guard.receiver;
    conn.execute_batch("SAVEPOINT probe").expect("savepoint");
    let opened = decrypt(conn, &bytes, &guard.genuine);
    conn.execute_batch("ROLLBACK TO probe; RELEASE probe").expect("rollback");

    if let Ok(text) = opened {
        let intact = |(g, _): &(Vec<u8>, Vec<u8>)| bytes.len() >= g.len() && bytes[..g.len()] == g[..];
        let source = guard
            .genuine
            .iter()
            .find(|g| intact(g))
            .unwrap_or_else(|| panic!("bytes cut from {origin:?} opened without being a genuine envelope"));
        assert_eq!(text, source.1, "an envelope opened as a different message");
    }
});
