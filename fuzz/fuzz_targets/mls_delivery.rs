//! Model-based fuzz target for MLS delivery (#1537) — the stateful target the
//! pure-function ones could never be: it drives REAL openmls state, through
//! pollis-core's own storage provider, seal (`try_mls_encrypt`), decryptor
//! (`MlsDecryptor`) and decrypt order (`into_sealing_order`), over a fuzzed
//! sequence of events, and asserts the zero-loss invariant of #1524.
//!
//! **Setup.** One group (`G`) with 2-4 channels, all sharing its one MLS group
//! as Pollis channels do; 2-3 devices to start, each its own local database.
//! A model DS holds the commit log and the envelopes, with the production
//! numbering (a per-channel `seq`, a group-wide `arrival`) and the #1041 epoch
//! gate: an envelope sealed at an epoch that is not the head is refused, and
//! its sender catches up and re-seals.
//!
//! **Fuzzed sequence.** Sends in any channel (an edit is the same envelope at
//! this layer), self-updates, device adds and removes, devices catching up
//! after any length of absence (the catch-up mirrors `ingest`: decrypt what is
//! sealed at the current epoch, then for every commit apply it and decrypt
//! what is sealed at the new epoch, each run in sealing order), and a process
//! killed between a decrypt and its row, or between a seal and its post.
//!
//! **Property.** After everyone comes back and drains: every message that
//! landed at epoch `e` is stored, with its text, on every device still a member
//! that joined at or before `e`; none is stored on a device that joined after
//! `e`; no outbox still holds a message. The only exempt losses are the three
//! in the root `CLAUDE.md` — here, only "sent before you joined" can arise.
//!
//! NEGATIVE CHECK (teeth): with `--cfg fuzz_mutant` the decrypt and its row are
//! not one transaction (the bug #1524 fixed): a kill after the decrypt leaves
//! the key spent and no row, the retry hits `SecretReuse`, and the message is
//! lost. Each case costs real post-quantum key generation (~12 runs/s under
//! ASan), too few for the fuzzer to rediscover a kill inside a 20 s teeth
//! budget, so `corpus/mls_delivery/send-then-die-reading` seeds the shortest
//! such sequence: one send, then a member killed while reading it.
#![no_main]

use std::collections::HashMap;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::messages::fuzz_surface::{into_sealing_order, rusqlite};
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
const MAX_DEVICES: usize = 5;
const MAX_OPS: usize = 24;

#[derive(Debug, Arbitrary)]
enum Op {
    /// `die`: the process is killed right after it seals.
    Send { dev: u8, channel: u8, die: bool },
    SelfUpdate { dev: u8 },
    Add { by: u8 },
    Remove { by: u8, target: u8 },
    /// `die`: the process is killed right after its first successful decrypt.
    CatchUp { dev: u8, die: bool },
}

#[derive(Debug, Arbitrary)]
struct Input {
    devices: u8,
    channels: u8,
    ops: Vec<Op>,
}

struct Envelope {
    id: String,
    channel: usize,
    seq: i64,
    arrival: i64,
    epoch: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Ds {
    /// `(epoch the commit was built on, commit bytes)`, in log order.
    commits: Vec<(u64, Vec<u8>)>,
    envelopes: Vec<Envelope>,
    next_seq: HashMap<usize, i64>,
}

impl Ds {
    fn head(&self) -> u64 {
        self.commits.len() as u64
    }
}

struct Device {
    conn: rusqlite::Connection,
    name: String,
    /// The epoch it joined at (its first epoch in the tree).
    joined: u64,
    /// Still in the tree.
    member: bool,
    /// Per channel: the highest `seq` this device has handled.
    cursor: HashMap<usize, i64>,
    /// Sends the process accepted and has not posted (the durable outbox).
    outbox: Vec<(String, usize, Vec<u8>)>,
    kill_after_decrypt: bool,
    kill_after_seal: bool,
}

/// A device's process died mid-operation.
struct Killed;

fn new_device(n: usize) -> (Device, SignatureKeyPair, CredentialWithKey) {
    let conn = rusqlite::Connection::open_in_memory().expect("open");
    pollis_core::db::local::apply_local_schema(&conn).expect("schema");
    conn.execute_batch("CREATE TABLE model_row (id TEXT PRIMARY KEY, text BLOB NOT NULL)")
        .expect("rows");
    let name = format!("d{n}");
    let sig = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).expect("keys");
    {
        let provider = PollisProvider::new(&conn);
        sig.store(provider.storage()).expect("store signer");
    }
    let cred = CredentialWithKey {
        credential: make_credential(&format!("u{}", n % 3), &name),
        signature_key: sig.public().into(),
    };
    let dev = Device {
        conn,
        name,
        joined: 0,
        member: true,
        cursor: HashMap::new(),
        outbox: Vec::new(),
        kill_after_decrypt: false,
        kill_after_seal: false,
    };
    (dev, sig, cred)
}

fn load(conn: &rusqlite::Connection) -> MlsGroup {
    let provider = PollisProvider::new(conn);
    MlsGroup::load(provider.storage(), &GroupId::from_slice(GROUP.as_bytes()))
        .expect("load")
        .expect("a group")
}

fn epoch_of(dev: &Device) -> u64 {
    load(&dev.conn).epoch().as_u64()
}

fn signer_of(conn: &rusqlite::Connection, group: &MlsGroup) -> SignatureKeyPair {
    let provider = PollisProvider::new(conn);
    let public = group.own_leaf_node().expect("own leaf").signature_key().as_slice().to_vec();
    SignatureKeyPair::read(provider.storage(), &public, CIPHERSUITE.signature_algorithm())
        .expect("signer")
}

fn has_row(dev: &Device, id: &str) -> bool {
    dev.conn
        .query_row("SELECT 1 FROM model_row WHERE id = ?1", [id], |_| Ok(()))
        .is_ok()
}

/// Open one envelope and store its row, as `ingest::decrypt_and_persist_one`
/// does: one savepoint over the key spend and the row.
fn open_one(dev: &mut Device, env: &Envelope) -> Result<(), Killed> {
    if has_row(dev, &env.id) {
        return Ok(());
    }
    #[cfg(not(fuzz_mutant))]
    dev.conn.execute_batch("SAVEPOINT one").expect("savepoint");
    let opened = MlsDecryptor::open(&dev.conn, GROUP)
        .expect("a group")
        .decrypt(&env.bytes);
    if let Ok((text, _sender)) = opened {
        if dev.kill_after_decrypt {
            dev.kill_after_decrypt = false;
            // The process dies: nothing after this runs, and SQLite rolls back
            // whatever the open transaction held.
            #[cfg(not(fuzz_mutant))]
            dev.conn
                .execute_batch("ROLLBACK TO one; RELEASE one")
                .expect("rollback");
            return Err(Killed);
        }
        dev.conn
            .execute(
                "INSERT INTO model_row (id, text) VALUES (?1, ?2)",
                rusqlite::params![env.id, text],
            )
            .expect("row");
    }
    #[cfg(not(fuzz_mutant))]
    dev.conn.execute_batch("RELEASE one").expect("release");
    Ok(())
}

/// Decrypt every fetched envelope sealed at `epoch`, in sealing order.
fn open_epoch(dev: &mut Device, fetched: &[Vec<&Envelope>], epoch: u64) -> Result<(), Killed> {
    let arrival: Vec<Vec<Option<i64>>> = fetched
        .iter()
        .map(|envs| envs.iter().map(|e| Some(e.arrival)).collect())
        .collect();
    let mut run: Vec<(usize, usize)> = Vec::new();
    for (ci, envs) in fetched.iter().enumerate() {
        for (ei, e) in envs.iter().enumerate() {
            if e.epoch == epoch {
                run.push((ci, ei));
            }
        }
    }
    into_sealing_order(&mut run, &arrival);
    for (ci, ei) in run {
        open_one(dev, fetched[ci][ei])?;
    }
    Ok(())
}

/// The interleaved catch-up (`ingest::catch_up_mls_group_interleaved`).
fn catch_up(dev: &mut Device, ds: &Ds, channels: usize) -> Result<(), Killed> {
    if !dev.member {
        return Ok(());
    }
    // Every channel's envelopes past its cursor, in the DS's (channel, seq)
    // listing order.
    let fetched: Vec<Vec<&Envelope>> = (0..channels)
        .map(|c| {
            let cursor = dev.cursor.get(&c).copied().unwrap_or(0);
            let mut envs: Vec<&Envelope> = ds
                .envelopes
                .iter()
                .filter(|e| e.channel == c && e.seq > cursor)
                .collect();
            envs.sort_by_key(|e| e.seq);
            envs
        })
        .collect();
    let mut epoch = epoch_of(dev);
    open_epoch(dev, &fetched, epoch)?;
    for (built_on, bytes) in &ds.commits {
        if *built_on < epoch {
            continue;
        }
        let provider = PollisProvider::new(&dev.conn);
        let mut group = load(&dev.conn);
        let msg = MlsMessageIn::tls_deserialize(&mut bytes.as_slice()).expect("commit");
        let processed = group
            .process_message(&provider, msg.try_into_protocol_message().expect("protocol"))
            .expect("process commit");
        if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
            group.merge_staged_commit(&provider, *staged).expect("merge");
        }
        if !group.is_active() {
            dev.member = false;
            return Ok(());
        }
        epoch = group.epoch().as_u64();
        drop(provider);
        open_epoch(dev, &fetched, epoch)?;
    }
    for (c, envs) in fetched.iter().enumerate() {
        if let Some(last) = envs.last() {
            dev.cursor.insert(c, last.seq);
        }
    }
    Ok(())
}

/// Post one queued message under the epoch gate, re-sealing until it lands.
fn deliver(
    dev: &mut Device,
    ds: &mut Ds,
    channels: usize,
    id: &str,
    channel: usize,
    text: &[u8],
    landed: &mut HashMap<String, (u64, Vec<u8>)>,
) -> Result<(), Killed> {
    loop {
        if !dev.member {
            // Removed with it unsent: the sender is told, and it was never
            // addressed to anyone.
            return Ok(());
        }
        let bytes = try_mls_encrypt(&dev.conn, GROUP, text).expect("seal");
        if dev.kill_after_seal {
            dev.kill_after_seal = false;
            return Err(Killed);
        }
        let (_, epoch) = envelope_lineage(&bytes).expect("lineage");
        if epoch == ds.head() {
            let seq = ds.next_seq.entry(channel).or_insert(0);
            *seq += 1;
            let seq = *seq;
            let arrival = ds.envelopes.len() as i64;
            ds.envelopes.push(Envelope {
                id: id.to_string(),
                channel,
                seq,
                arrival,
                epoch,
                bytes,
            });
            // The sender's own copy is its outbox row.
            dev.conn
                .execute(
                    "INSERT INTO model_row (id, text) VALUES (?1, ?2)",
                    rusqlite::params![id, text],
                )
                .expect("own row");
            landed.insert(id.to_string(), (epoch, text.to_vec()));
            return Ok(());
        }
        // Epoch-behind: catch up and re-seal (#1041).
        catch_up(dev, ds, channels)?;
    }
}

fn drain(
    dev: &mut Device,
    ds: &mut Ds,
    channels: usize,
    landed: &mut HashMap<String, (u64, Vec<u8>)>,
) -> Result<(), Killed> {
    while let Some((id, channel, text)) = dev.outbox.first().cloned() {
        deliver(dev, ds, channels, &id, channel, &text, landed)?;
        dev.outbox.remove(0);
    }
    Ok(())
}

fn commit(dev: &mut Device, ds: &mut Ds, build: impl FnOnce(&mut MlsGroup, &PollisProvider<'_>, &SignatureKeyPair) -> (MlsMessageOut, Option<MlsMessageOut>)) -> Option<MlsMessageOut> {
    let provider = PollisProvider::new(&dev.conn);
    let mut group = load(&dev.conn);
    let signer = signer_of(&dev.conn, &group);
    let built_on = group.epoch().as_u64();
    let (commit, welcome) = build(&mut group, &provider, &signer);
    group.merge_pending_commit(&provider).expect("merge own commit");
    ds.commits
        .push((built_on, commit.tls_serialize_detached().expect("commit bytes")));
    welcome
}

fuzz_target!(|input: Input| {
    let channels = 2 + (input.channels % 3) as usize;
    let start = 2 + (input.devices % 2) as usize;
    let mut ds = Ds::default();
    let mut landed: HashMap<String, (u64, Vec<u8>)> = HashMap::new();

    // The creator, then everyone else added one commit at a time.
    let (first, sig, cred) = new_device(0);
    {
        let provider = PollisProvider::new(&first.conn);
        MlsGroup::new_with_group_id(
            &provider,
            &sig,
            &create_config(),
            GroupId::from_slice(GROUP.as_bytes()),
            cred,
        )
        .expect("create");
    }
    let mut devices = vec![first];
    let add = |by: usize, devices: &mut Vec<Device>, ds: &mut Ds| {
        let (mut joiner, jsig, jcred) = new_device(devices.len());
        let kp = {
            let provider = PollisProvider::new(&joiner.conn);
            KeyPackage::builder()
                .build(CIPHERSUITE, &provider, &jsig, jcred)
                .expect("key package")
                .key_package()
                .clone()
        };
        let welcome = commit(&mut devices[by], ds, |g, p, s| {
            let (c, w, _) = g.add_members(p, s, &[kp]).expect("add");
            (c, Some(w))
        })
        .expect("welcome");
        let bytes = welcome.tls_serialize_detached().expect("welcome bytes");
        let MlsMessageBodyIn::Welcome(w) =
            MlsMessageIn::tls_deserialize(&mut bytes.as_slice()).expect("welcome").extract()
        else {
            panic!("not a welcome");
        };
        {
            let provider = PollisProvider::new(&joiner.conn);
            StagedWelcome::new_from_welcome(&provider, &join_config(), w, None)
                .expect("stage welcome")
                .into_group(&provider)
                .expect("join");
        }
        joiner.joined = ds.head();
        // It starts reading from now: nothing older is addressed to it.
        for c in 0..ds.next_seq.len().max(8) {
            joiner.cursor.insert(c, ds.next_seq.get(&c).copied().unwrap_or(0));
        }
        devices.push(joiner);
    };
    for _ in 1..start {
        add(0, &mut devices, &mut ds);
        // Keep the creator at head for the next add.
    }

    let mut next_msg = 0usize;
    for op in input.ops.into_iter().take(MAX_OPS) {
        let pick = |d: u8, devices: &Vec<Device>| -> Option<usize> {
            let members: Vec<usize> = (0..devices.len()).filter(|&i| devices[i].member).collect();
            (!members.is_empty()).then(|| members[d as usize % members.len()])
        };
        match op {
            Op::Send { dev, channel, die } => {
                let Some(d) = pick(dev, &devices) else { continue };
                let id = format!("m{next_msg}");
                next_msg += 1;
                let channel = channel as usize % channels;
                let text = format!("{id} from {} in {channel}", devices[d].name).into_bytes();
                devices[d].outbox.push((id, channel, text));
                devices[d].kill_after_seal = die;
                let _ = drain(&mut devices[d], &mut ds, channels, &mut landed);
                devices[d].kill_after_seal = false;
            }
            Op::SelfUpdate { dev } => {
                let Some(d) = pick(dev, &devices) else { continue };
                if catch_up(&mut devices[d], &ds, channels).is_err() || !devices[d].member {
                    continue;
                }
                commit(&mut devices[d], &mut ds, |g, p, s| {
                    let bundle = g
                        .commit_builder()
                        .force_self_update(true)
                        .load_psks(p.storage())
                        .expect("psks")
                        .build(p.rand(), p.crypto(), s, |_| true)
                        .expect("build")
                        .stage_commit(p)
                        .expect("stage");
                    let (c, _, _) = bundle.into_messages();
                    (c, None)
                });
            }
            Op::Add { by } => {
                if devices.len() >= MAX_DEVICES {
                    continue;
                }
                let Some(d) = pick(by, &devices) else { continue };
                if catch_up(&mut devices[d], &ds, channels).is_err() || !devices[d].member {
                    continue;
                }
                add(d, &mut devices, &mut ds);
            }
            Op::Remove { by, target } => {
                let Some(d) = pick(by, &devices) else { continue };
                let Some(t) = pick(target, &devices) else { continue };
                if t == d || catch_up(&mut devices[d], &ds, channels).is_err() || !devices[d].member {
                    continue;
                }
                let target_name = devices[t].name.clone();
                commit(&mut devices[d], &mut ds, |g, p, s| {
                    let leaf = g
                        .members()
                        .find(|m| {
                            pollis_core::commands::mls::parse_credential_device_id(&m.credential)
                                .as_deref()
                                == Some(target_name.as_str())
                        })
                        .expect("target leaf")
                        .index;
                    let (c, _, _) = g.remove_members(p, s, &[leaf]).expect("remove");
                    (c, None)
                });
                // Its removal is final in this model; a later return is a new device.
                devices[t].member = false;
            }
            Op::CatchUp { dev, die } => {
                let Some(d) = pick(dev, &devices) else { continue };
                if drain(&mut devices[d], &mut ds, channels, &mut landed).is_ok() {
                    devices[d].kill_after_decrypt = die;
                    let _ = catch_up(&mut devices[d], &ds, channels);
                    devices[d].kill_after_decrypt = false;
                }
            }
        }
    }

    // Everyone comes back: drains, then reads, until nothing moves.
    for dev in devices.iter_mut() {
        dev.kill_after_decrypt = false;
        dev.kill_after_seal = false;
    }
    for _ in 0..3 {
        for d in 0..devices.len() {
            if !devices[d].member {
                continue;
            }
            drain(&mut devices[d], &mut ds, channels, &mut landed).unwrap_or_else(|_| unreachable!());
            catch_up(&mut devices[d], &ds, channels).unwrap_or_else(|_| unreachable!());
        }
    }

    for dev in devices.iter().filter(|d| d.member) {
        assert!(dev.outbox.is_empty(), "{} still holds an unsent message", dev.name);
        for (id, (epoch, text)) in &landed {
            let stored: Option<Vec<u8>> = dev
                .conn
                .query_row("SELECT text FROM model_row WHERE id = ?1", [id], |r| r.get(0))
                .ok();
            if dev.joined <= *epoch {
                assert_eq!(
                    stored.as_deref(),
                    Some(text.as_slice()),
                    "{} lost {id} (landed at epoch {epoch}, joined at {})",
                    dev.name,
                    dev.joined
                );
            } else {
                assert!(stored.is_none(), "{} read {id}, sealed before it joined", dev.name);
            }
        }
    }
});
