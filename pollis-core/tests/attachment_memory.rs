//! What caching an attachment actually costs in memory (#915).
//!
//! `cache_encrypt` is the largest single allocation `pollis-core` makes. Before
//! #915 it took the plaintext by reference, asked AES-GCM for a fresh ciphertext
//! buffer, and then copied nonce+ciphertext into a THIRD buffer — so caching a
//! 100 MiB attachment held roughly 300 MiB at once: the decrypted bytes the
//! caller still owned, the ciphertext, and the concatenation. On a phone (this
//! crate ships to mobile via uniffi) that is the difference between caching a
//! video and being killed for it.
//!
//! Memory is only testable if it is measured, so this installs a counting global
//! allocator and asserts against the numbers. The assertions are in multiples of
//! the payload rather than absolute bytes, so they say something durable rather
//! than encoding today's constants.
//!
//! Own process (an integration test), which is what makes a global allocator
//! usable here at all — it must not perturb the rest of the suite.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes allocated since the last reset, and the peak live total.
///
/// `live` can legitimately go negative-ish across a reset (a buffer allocated
/// before the window is freed inside it), so it is saturating rather than
/// wrapping — an underflow would otherwise read as an enormous peak.
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size().min(LIVE.load(Ordering::Relaxed)), Ordering::Relaxed);
        unsafe { System.dealloc(p, layout) }
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let np = unsafe { System.realloc(p, layout, new_size) };
        if !np.is_null() && new_size > layout.size() {
            let grew = new_size - layout.size();
            ALLOCATED.fetch_add(grew, Ordering::Relaxed);
            let live = LIVE.fetch_add(grew, Ordering::Relaxed) + grew;
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        np
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn reset() {
    ALLOCATED.store(0, Ordering::Relaxed);
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Bytes newly allocated since [`reset`].
fn allocated() -> usize {
    ALLOCATED.load(Ordering::Relaxed)
}

const MIB: usize = 1024 * 1024;
const PAYLOAD: usize = 16 * MIB;

fn db_key() -> Vec<u8> {
    vec![9u8; 32]
}

/// The real path allocates essentially NOTHING to encrypt a cached attachment.
///
/// This models `get_media_url`: the plaintext arrives from `decrypt_chunked`,
/// which sizes its buffer from the CIPHERTEXT length and therefore hands back a
/// `Vec` that already carries spare capacity (one 16-byte AEAD tag per chunk).
/// `cache_encrypt` encrypts into that spare room in place and splices the nonce
/// within the same allocation.
///
/// Before #915 this call allocated about **two full payloads** — the ciphertext
/// AES-GCM returned, plus the nonce+ciphertext concatenation — on top of the
/// plaintext the caller was still holding.
#[test]
fn encrypting_a_cached_attachment_allocates_nothing_like_the_payload() {
    // Spare capacity exactly as `decrypt_chunked` leaves it.
    let mut plaintext = Vec::with_capacity(PAYLOAD + 4096);
    plaintext.resize(PAYLOAD, 7u8);

    reset();
    let out = pollis_core::commands::r2::cache_encrypt(plaintext, &db_key(), b"info")
        .expect("encrypt");
    let allocated = allocated();

    assert_eq!(out.len(), PAYLOAD + 12 + 16, "nonce + ciphertext + tag");
    assert!(
        allocated < PAYLOAD / 4,
        "cache_encrypt allocated {allocated} bytes for a {PAYLOAD}-byte payload; \
         it should reuse the caller's buffer, not allocate another copy of it"
    );
}

/// Even with NO spare capacity, it never needs more than one extra buffer.
///
/// The worst case is one reallocation to make room for the 28 bytes of nonce and
/// tag. That is still bounded by a single payload — the point being that three
/// simultaneous copies are gone, not that allocation is always zero.
#[test]
fn a_tight_buffer_costs_at_most_one_extra_copy_not_two() {
    let plaintext = vec![7u8; PAYLOAD];

    reset();
    let out =
        pollis_core::commands::r2::cache_encrypt(plaintext, &db_key(), b"info").expect("encrypt");
    let allocated = allocated();

    assert_eq!(out.len(), PAYLOAD + 12 + 16);
    assert!(
        allocated < PAYLOAD * 3 / 2,
        "cache_encrypt allocated {allocated} bytes for a {PAYLOAD}-byte payload; \
         the pre-#915 shape allocated two full payloads here (ciphertext + \
         concatenation) and that is what must not come back"
    );
}

/// Round trip, so the memory assertions above are not measuring broken crypto.
///
/// The on-disk layout is unchanged by #915 — `[12-byte nonce][ciphertext+tag]` —
/// which is what lets already-cached files stay readable across the upgrade.
#[test]
fn the_in_place_encryption_still_round_trips_and_keeps_its_layout() {
    let key = db_key();
    let plaintext: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

    let encrypted =
        pollis_core::commands::r2::cache_encrypt(plaintext.clone(), &key, b"info").expect("encrypt");
    assert_eq!(
        encrypted.len(),
        plaintext.len() + 12 + 16,
        "layout must stay [nonce][ciphertext+tag] or existing cache files stop decrypting"
    );

    let decrypted =
        pollis_core::commands::r2::cache_decrypt(&encrypted, &key, b"info").expect("decrypt");
    assert_eq!(decrypted, plaintext);

    // Wrong info string (the content hash) must not decrypt — the per-file key
    // derivation is what stops one cached object being served as another.
    assert!(pollis_core::commands::r2::cache_decrypt(&encrypted, &key, b"other").is_err());
}
