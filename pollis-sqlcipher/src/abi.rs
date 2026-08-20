//! The C ABI declared by `include/tomcrypt.h`, implemented over RustCrypto.
//!
//! Every function here is called only by SQLCipher's LibTomCrypt crypto
//! provider, compiled into `vendor/sqlite3.c`.
//!
//! The symbols are `#[no_mangle]` on every target, and the amalgamation is
//! compiled on every target, even though Windows is the only one that ships
//! them: it means `tests/sqlcipher.rs` can drive a real encrypted database
//! through this provider on Linux and macOS too. A mistake in here therefore
//! surfaces in seconds on the ordinary gate rather than only in the Windows
//! job. Nothing outside this crate links these names — `pollis-core` depends on
//! `pollis-sqlcipher` under `cfg(windows)` only — so generic names like
//! `hmac_init` cannot reach an image that has its own.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_ulong, c_void};

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

pub const CRYPT_OK: c_int = 0;
pub const CRYPT_ERROR: c_int = 1;

/// AES block size, and therefore the SQLCipher page IV size.
const AES_BLOCK: usize = 16;

// ── Registry ─────────────────────────────────────────────────────────────────
//
// SQLCipher looks primitives up by name and then indexes `hash_descriptor` /
// `cipher_descriptor` with what it got back. Our registries are fixed at
// compile time — `register_*` has nothing to do — so the indices are constants
// and a lookup can never fail for an algorithm we support.

const HASH_SHA1: c_int = 0;
const HASH_SHA256: c_int = 1;
const HASH_SHA512: c_int = 2;
const CIPHER_RIJNDAEL: c_int = 0;

#[repr(C)]
pub struct HashDescriptor {
    pub name: *const c_char,
    pub hashsize: c_ulong,
}

// SAFETY: the only non-`Sync` field is a pointer into a `'static` byte string
// literal. Nothing ever writes through it.
unsafe impl Sync for HashDescriptor {}

#[repr(C)]
pub struct CipherDescriptor {
    pub name: *const c_char,
    pub min_key_length: c_int,
    pub max_key_length: c_int,
    pub block_length: c_int,
}

// SAFETY: as for `HashDescriptor`.
unsafe impl Sync for CipherDescriptor {}

#[repr(C)]
pub struct PrngDescriptor {
    pub name: *const c_char,
}

// SAFETY: as for `HashDescriptor`.
unsafe impl Sync for PrngDescriptor {}

static NAME_SHA1: &[u8] = b"sha1\0";
static NAME_SHA256: &[u8] = b"sha256\0";
static NAME_SHA512: &[u8] = b"sha512\0";
static NAME_RIJNDAEL: &[u8] = b"rijndael\0";
static NAME_FORTUNA: &[u8] = b"fortuna\0";

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static hash_descriptor: [HashDescriptor; 3] = [
    HashDescriptor { name: NAME_SHA1.as_ptr() as *const c_char, hashsize: 20 },
    HashDescriptor { name: NAME_SHA256.as_ptr() as *const c_char, hashsize: 32 },
    HashDescriptor { name: NAME_SHA512.as_ptr() as *const c_char, hashsize: 64 },
];

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static cipher_descriptor: [CipherDescriptor; 1] = [CipherDescriptor {
    name: NAME_RIJNDAEL.as_ptr() as *const c_char,
    min_key_length: 16,
    // SQLCipher derives its page key length from this field, so it is what
    // makes the cipher AES-256 rather than AES-128.
    max_key_length: 32,
    block_length: AES_BLOCK as c_int,
}];

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static sha1_desc: HashDescriptor =
    HashDescriptor { name: NAME_SHA1.as_ptr() as *const c_char, hashsize: 20 };

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static sha256_desc: HashDescriptor =
    HashDescriptor { name: NAME_SHA256.as_ptr() as *const c_char, hashsize: 32 };

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static sha512_desc: HashDescriptor =
    HashDescriptor { name: NAME_SHA512.as_ptr() as *const c_char, hashsize: 64 };

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static rijndael_desc: CipherDescriptor = CipherDescriptor {
    name: NAME_RIJNDAEL.as_ptr() as *const c_char,
    min_key_length: 16,
    max_key_length: 32,
    block_length: AES_BLOCK as c_int,
};

#[allow(non_upper_case_globals)]
#[no_mangle]
pub static fortuna_desc: PrngDescriptor =
    PrngDescriptor { name: NAME_FORTUNA.as_ptr() as *const c_char };

/// # Safety
/// `name` must be a valid NUL-terminated C string.
unsafe fn lookup(name: *const c_char, table: &[(&[u8], c_int)]) -> c_int {
    if name.is_null() {
        return -1;
    }
    let want = CStr::from_ptr(name).to_bytes();
    for (candidate, index) in table {
        if &candidate[..candidate.len() - 1] == want {
            return *index;
        }
    }
    -1
}

/// # Safety
/// `name` must be a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn find_hash(name: *const c_char) -> c_int {
    lookup(
        name,
        &[(NAME_SHA1, HASH_SHA1), (NAME_SHA256, HASH_SHA256), (NAME_SHA512, HASH_SHA512)],
    )
}

/// # Safety
/// `name` must be a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn find_cipher(name: *const c_char) -> c_int {
    lookup(name, &[(NAME_RIJNDAEL, CIPHER_RIJNDAEL)])
}

/// # Safety
/// `desc` must point at one of the descriptors declared above.
#[no_mangle]
pub unsafe extern "C" fn register_hash(desc: *const HashDescriptor) -> c_int {
    if desc.is_null() {
        return -1;
    }
    find_hash((*desc).name)
}

/// # Safety
/// `desc` must point at one of the descriptors declared above.
#[no_mangle]
pub unsafe extern "C" fn register_cipher(desc: *const CipherDescriptor) -> c_int {
    if desc.is_null() {
        return -1;
    }
    find_cipher((*desc).name)
}

/// # Safety
/// `desc` must point at `fortuna_desc`.
#[no_mangle]
pub unsafe extern "C" fn register_prng(desc: *const PrngDescriptor) -> c_int {
    if desc.is_null() {
        return -1;
    }
    0
}

// ── Randomness ───────────────────────────────────────────────────────────────
//
// SQLCipher's provider believes it is driving a Fortuna instance it seeds from
// `sqlite3_randomness`. It is not: every read is the operating system CSPRNG
// (`getrandom` / `BCryptGenRandom`), which cannot be left unseeded and is the
// same source `rand::rngs::OsRng` gives the rest of pollis. Entropy handed in is
// accepted and dropped — mixing it in could only lower the quality.

#[repr(C)]
pub struct PrngState {
    pub started: c_int,
}

/// # Safety
/// `prng` must point at a writable `PrngState`.
#[no_mangle]
pub unsafe extern "C" fn fortuna_start(prng: *mut PrngState) -> c_int {
    if prng.is_null() {
        return CRYPT_ERROR;
    }
    (*prng).started = 1;
    CRYPT_OK
}

/// # Safety
/// `prng` must point at a `PrngState`.
#[no_mangle]
pub unsafe extern "C" fn fortuna_add_entropy(
    _input: *const u8,
    _len: c_ulong,
    _prng: *mut PrngState,
) -> c_int {
    CRYPT_OK
}

/// # Safety
/// `prng` must point at a `PrngState`.
#[no_mangle]
pub unsafe extern "C" fn fortuna_ready(_prng: *mut PrngState) -> c_int {
    CRYPT_OK
}

/// # Safety
/// `out` must be writable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn fortuna_read(
    out: *mut u8,
    len: c_ulong,
    _prng: *mut PrngState,
) -> c_ulong {
    if out.is_null() || len == 0 {
        return 0;
    }
    let buf = std::slice::from_raw_parts_mut(out, len as usize);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, buf);
    len
}

/// # Safety
/// `prng` must point at a `PrngState`.
#[no_mangle]
pub unsafe extern "C" fn fortuna_done(prng: *mut PrngState) -> c_int {
    if !prng.is_null() {
        (*prng).started = 0;
    }
    CRYPT_OK
}

// ── HMAC ─────────────────────────────────────────────────────────────────────

enum HmacKind {
    Sha1(Box<Hmac<Sha1>>),
    Sha256(Box<Hmac<Sha256>>),
    Sha512(Box<Hmac<Sha512>>),
}

#[repr(C)]
pub struct HmacStateHandle {
    pub state: *mut c_void,
}

/// # Safety
/// `state` must point at a writable handle; `key` must be readable for
/// `keylen` bytes.
#[no_mangle]
pub unsafe extern "C" fn hmac_init(
    state: *mut HmacStateHandle,
    hash: c_int,
    key: *const u8,
    keylen: c_ulong,
) -> c_int {
    if state.is_null() || (key.is_null() && keylen != 0) {
        return CRYPT_ERROR;
    }
    let key = if keylen == 0 { &[][..] } else { std::slice::from_raw_parts(key, keylen as usize) };
    // `new_from_slice` on an HMAC accepts any key length, so these cannot fail.
    let kind = match hash {
        HASH_SHA1 => match <Hmac<Sha1> as Mac>::new_from_slice(key) {
            Ok(m) => HmacKind::Sha1(Box::new(m)),
            Err(_) => return CRYPT_ERROR,
        },
        HASH_SHA256 => match <Hmac<Sha256> as Mac>::new_from_slice(key) {
            Ok(m) => HmacKind::Sha256(Box::new(m)),
            Err(_) => return CRYPT_ERROR,
        },
        HASH_SHA512 => match <Hmac<Sha512> as Mac>::new_from_slice(key) {
            Ok(m) => HmacKind::Sha512(Box::new(m)),
            Err(_) => return CRYPT_ERROR,
        },
        _ => return CRYPT_ERROR,
    };
    (*state).state = Box::into_raw(Box::new(kind)) as *mut c_void;
    CRYPT_OK
}

/// # Safety
/// `state` must be a handle initialised by [`hmac_init`] and not yet finished;
/// `input` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn hmac_process(
    state: *mut HmacStateHandle,
    input: *const u8,
    len: c_ulong,
) -> c_int {
    if state.is_null() || (*state).state.is_null() {
        return CRYPT_ERROR;
    }
    if len == 0 {
        return CRYPT_OK;
    }
    if input.is_null() {
        return CRYPT_ERROR;
    }
    let data = std::slice::from_raw_parts(input, len as usize);
    let kind = &mut *((*state).state as *mut HmacKind);
    match kind {
        HmacKind::Sha1(m) => m.update(data),
        HmacKind::Sha256(m) => m.update(data),
        HmacKind::Sha512(m) => m.update(data),
    }
    CRYPT_OK
}

/// # Safety
/// `state` must be a handle initialised by [`hmac_init`]; `out` must be
/// writable for `*outlen` bytes. The handle is consumed either way.
#[no_mangle]
pub unsafe extern "C" fn hmac_done(
    state: *mut HmacStateHandle,
    out: *mut u8,
    outlen: *mut c_ulong,
) -> c_int {
    if state.is_null() || (*state).state.is_null() || out.is_null() || outlen.is_null() {
        return CRYPT_ERROR;
    }
    let kind = Box::from_raw((*state).state as *mut HmacKind);
    (*state).state = std::ptr::null_mut();

    let digest: Vec<u8> = match *kind {
        HmacKind::Sha1(m) => m.finalize().into_bytes().to_vec(),
        HmacKind::Sha256(m) => m.finalize().into_bytes().to_vec(),
        HmacKind::Sha512(m) => m.finalize().into_bytes().to_vec(),
    };

    // A caller asking for fewer bytes than the digest holds would silently get a
    // truncated MAC, so refuse instead.
    if (*outlen as usize) < digest.len() {
        return CRYPT_ERROR;
    }
    std::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len());
    *outlen = digest.len() as c_ulong;
    CRYPT_OK
}

// ── PBKDF2 ───────────────────────────────────────────────────────────────────

/// # Safety
/// `password`/`salt` must be readable for their stated lengths and `out`
/// writable for `*outlen` bytes.
#[no_mangle]
pub unsafe extern "C" fn pkcs_5_alg2(
    password: *const u8,
    password_len: c_ulong,
    salt: *const u8,
    salt_len: c_ulong,
    iteration_count: c_int,
    hash_idx: c_int,
    out: *mut u8,
    outlen: *mut c_ulong,
) -> c_int {
    if out.is_null() || outlen.is_null() || iteration_count <= 0 {
        return CRYPT_ERROR;
    }
    if (password.is_null() && password_len != 0) || (salt.is_null() && salt_len != 0) {
        return CRYPT_ERROR;
    }
    let want = *outlen as usize;
    if want == 0 {
        return CRYPT_ERROR;
    }
    let password = if password_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(password, password_len as usize)
    };
    let salt =
        if salt_len == 0 { &[][..] } else { std::slice::from_raw_parts(salt, salt_len as usize) };

    let derived = std::slice::from_raw_parts_mut(out, want);
    let rounds = iteration_count as u32;
    match hash_idx {
        HASH_SHA1 => pbkdf2::pbkdf2::<Hmac<Sha1>>(password, salt, rounds, derived),
        HASH_SHA256 => pbkdf2::pbkdf2::<Hmac<Sha256>>(password, salt, rounds, derived),
        HASH_SHA512 => pbkdf2::pbkdf2::<Hmac<Sha512>>(password, salt, rounds, derived),
        _ => return CRYPT_ERROR,
    }
    CRYPT_OK
}

// ── AES-CBC ──────────────────────────────────────────────────────────────────

enum AesKey {
    A128(Box<aes::Aes128>),
    A192(Box<aes::Aes192>),
    A256(Box<aes::Aes256>),
}

struct CbcState {
    key: AesKey,
    iv: [u8; AES_BLOCK],
}

#[repr(C)]
pub struct SymmetricCbcHandle {
    pub state: *mut c_void,
}

/// # Safety
/// `iv` must be readable for 16 bytes, `key` for `keylen`, and `cbc` writable.
#[no_mangle]
pub unsafe extern "C" fn cbc_start(
    cipher: c_int,
    iv: *const u8,
    key: *const u8,
    keylen: c_int,
    _num_rounds: c_int,
    cbc: *mut SymmetricCbcHandle,
) -> c_int {
    if cbc.is_null() || iv.is_null() || key.is_null() || cipher != CIPHER_RIJNDAEL {
        return CRYPT_ERROR;
    }
    if keylen < 0 {
        return CRYPT_ERROR;
    }
    let key_bytes = std::slice::from_raw_parts(key, keylen as usize);
    let aes = match keylen {
        16 => AesKey::A128(Box::new(aes::Aes128::new(GenericArray::from_slice(key_bytes)))),
        24 => AesKey::A192(Box::new(aes::Aes192::new(GenericArray::from_slice(key_bytes)))),
        32 => AesKey::A256(Box::new(aes::Aes256::new(GenericArray::from_slice(key_bytes)))),
        _ => return CRYPT_ERROR,
    };
    let mut iv_bytes = [0u8; AES_BLOCK];
    std::ptr::copy_nonoverlapping(iv, iv_bytes.as_mut_ptr(), AES_BLOCK);
    (*cbc).state = Box::into_raw(Box::new(CbcState { key: aes, iv: iv_bytes })) as *mut c_void;
    CRYPT_OK
}

fn encrypt_block(key: &AesKey, block: &mut [u8; AES_BLOCK]) {
    let ga = GenericArray::from_mut_slice(block);
    match key {
        AesKey::A128(k) => k.encrypt_block(ga),
        AesKey::A192(k) => k.encrypt_block(ga),
        AesKey::A256(k) => k.encrypt_block(ga),
    }
}

fn decrypt_block(key: &AesKey, block: &mut [u8; AES_BLOCK]) {
    let ga = GenericArray::from_mut_slice(block);
    match key {
        AesKey::A128(k) => k.decrypt_block(ga),
        AesKey::A192(k) => k.decrypt_block(ga),
        AesKey::A256(k) => k.decrypt_block(ga),
    }
}

/// # Safety
/// `pt` must be readable and `ct` writable for `len` bytes; `cbc` must be a
/// handle from [`cbc_start`].
#[no_mangle]
pub unsafe extern "C" fn cbc_encrypt(
    pt: *const u8,
    ct: *mut u8,
    len: c_ulong,
    cbc: *mut SymmetricCbcHandle,
) -> c_int {
    if cbc.is_null() || (*cbc).state.is_null() || pt.is_null() || ct.is_null() {
        return CRYPT_ERROR;
    }
    let len = len as usize;
    if len % AES_BLOCK != 0 {
        return CRYPT_ERROR;
    }
    let state = &mut *((*cbc).state as *mut CbcState);
    let input = std::slice::from_raw_parts(pt, len);
    let output = std::slice::from_raw_parts_mut(ct, len);
    let mut block = [0u8; AES_BLOCK];
    for offset in (0..len).step_by(AES_BLOCK) {
        for (i, byte) in block.iter_mut().enumerate() {
            *byte = input[offset + i] ^ state.iv[i];
        }
        encrypt_block(&state.key, &mut block);
        output[offset..offset + AES_BLOCK].copy_from_slice(&block);
        state.iv = block;
    }
    CRYPT_OK
}

/// # Safety
/// `ct` must be readable and `pt` writable for `len` bytes; `cbc` must be a
/// handle from [`cbc_start`].
#[no_mangle]
pub unsafe extern "C" fn cbc_decrypt(
    ct: *const u8,
    pt: *mut u8,
    len: c_ulong,
    cbc: *mut SymmetricCbcHandle,
) -> c_int {
    if cbc.is_null() || (*cbc).state.is_null() || pt.is_null() || ct.is_null() {
        return CRYPT_ERROR;
    }
    let len = len as usize;
    if len % AES_BLOCK != 0 {
        return CRYPT_ERROR;
    }
    let state = &mut *((*cbc).state as *mut CbcState);
    let input = std::slice::from_raw_parts(ct, len);
    let output = std::slice::from_raw_parts_mut(pt, len);
    let mut block = [0u8; AES_BLOCK];
    let mut carry = [0u8; AES_BLOCK];
    for offset in (0..len).step_by(AES_BLOCK) {
        block.copy_from_slice(&input[offset..offset + AES_BLOCK]);
        carry.copy_from_slice(&block);
        decrypt_block(&state.key, &mut block);
        for (byte, mask) in block.iter_mut().zip(state.iv.iter()) {
            *byte ^= *mask;
        }
        output[offset..offset + AES_BLOCK].copy_from_slice(&block);
        state.iv = carry;
    }
    CRYPT_OK
}

/// # Safety
/// `cbc` must be a handle from [`cbc_start`]; it is consumed.
#[no_mangle]
pub unsafe extern "C" fn cbc_done(cbc: *mut SymmetricCbcHandle) -> c_int {
    if cbc.is_null() {
        return CRYPT_ERROR;
    }
    if !(*cbc).state.is_null() {
        drop(Box::from_raw((*cbc).state as *mut CbcState));
        (*cbc).state = std::ptr::null_mut();
    }
    CRYPT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn hmac(alg: c_int, key: &[u8], data: &[u8], out_len: usize) -> Vec<u8> {
        let mut handle = HmacStateHandle { state: std::ptr::null_mut() };
        let mut out = vec![0u8; out_len];
        let mut outlen = out_len as c_ulong;
        unsafe {
            assert_eq!(hmac_init(&mut handle, alg, key.as_ptr(), key.len() as c_ulong), CRYPT_OK);
            assert_eq!(
                hmac_process(&mut handle, data.as_ptr(), data.len() as c_ulong),
                CRYPT_OK
            );
            assert_eq!(hmac_done(&mut handle, out.as_mut_ptr(), &mut outlen), CRYPT_OK);
            assert!(handle.state.is_null(), "hmac_done must consume the handle");
        }
        assert_eq!(outlen as usize, out_len);
        out
    }

    fn pbkdf2_kat(alg: c_int, pass: &[u8], salt: &[u8], iters: c_int, dk_len: usize) -> Vec<u8> {
        let mut out = vec![0u8; dk_len];
        let mut outlen = dk_len as c_ulong;
        unsafe {
            assert_eq!(
                pkcs_5_alg2(
                    pass.as_ptr(),
                    pass.len() as c_ulong,
                    salt.as_ptr(),
                    salt.len() as c_ulong,
                    iters,
                    alg,
                    out.as_mut_ptr(),
                    &mut outlen,
                ),
                CRYPT_OK
            );
        }
        out
    }

    /// RFC 4231 §4.3 (SHA-256/512) and RFC 2202 §3 (SHA-1), test case 2.
    #[test]
    fn hmac_matches_the_published_vectors() {
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        assert_eq!(hex(&hmac(HASH_SHA1, key, data, 20)), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
        assert_eq!(
            hex(&hmac(HASH_SHA256, key, data, 32)),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac(HASH_SHA512, key, data, 64)),
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea25055\
             49758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
    }

    /// A MAC split across two `hmac_process` calls — which is exactly how
    /// SQLCipher MACs a page, feeding the page bytes and then the page number —
    /// must equal the MAC of the concatenation.
    #[test]
    fn hmac_is_streaming() {
        let key = b"Jefe";
        let split = {
            let mut handle = HmacStateHandle { state: std::ptr::null_mut() };
            let mut out = [0u8; 64];
            let mut outlen = 64 as c_ulong;
            unsafe {
                hmac_init(&mut handle, HASH_SHA512, key.as_ptr(), 4);
                hmac_process(&mut handle, b"what do ya ".as_ptr(), 11);
                hmac_process(&mut handle, b"want for nothing?".as_ptr(), 17);
                hmac_done(&mut handle, out.as_mut_ptr(), &mut outlen);
            }
            out.to_vec()
        };
        assert_eq!(split, hmac(HASH_SHA512, key, b"what do ya want for nothing?", 64));
    }

    /// RFC 6070 PBKDF2-HMAC-SHA1 vectors.
    #[test]
    fn pbkdf2_matches_rfc6070() {
        assert_eq!(
            hex(&pbkdf2_kat(HASH_SHA1, b"password", b"salt", 1, 20)),
            "0c60c80f961f0e71f3a9b524af6012062fe037a6"
        );
        assert_eq!(
            hex(&pbkdf2_kat(HASH_SHA1, b"password", b"salt", 2, 20)),
            "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957"
        );
        assert_eq!(
            hex(&pbkdf2_kat(HASH_SHA1, b"password", b"salt", 4096, 20)),
            "4b007901b765489abead49d926f721d065a429c1"
        );
    }

    /// The KDF SQLCipher 4 actually uses. Cross-checked against the same
    /// parameters run through OpenSSL's `PKCS5_PBKDF2_HMAC`, which is what the
    /// Linux build's provider calls — so a Windows database and a Linux one
    /// derive the same key from the same passphrase.
    #[test]
    fn pbkdf2_hmac_sha512_matches() {
        assert_eq!(
            hex(&pbkdf2_kat(HASH_SHA512, b"password", b"salt", 1, 64)),
            "867f70cf1ade02cff3752599a3a53dc4af34c7a669815ae5d513554e1c8cf252\
             c02d470a285a0501bad999bfe943c08f050235d7d68b1da55e63f73b60a57fce"
        );
        assert_eq!(
            hex(&pbkdf2_kat(HASH_SHA512, b"password", b"salt", 4096, 64)),
            "d197b1b33db0143e018b12f3d1d1479e6cdebdcc97c5c0f87f6902e072f457b5\
             143f30602641b3d55cd335988cb36b84376060ecd532e039b742a239434af2d5"
        );
    }

    /// NIST SP 800-38A §F.2.5/F.2.6, CBC-AES256.
    #[test]
    fn aes_256_cbc_matches_nist() {
        let key = unhex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let iv = unhex("000102030405060708090a0b0c0d0e0f");
        let plain = unhex(
            "6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710",
        );
        let expected = "f58c4c04d6e5f1ba779eabfb5f7bfbd6\
                        9cfc4e967edb808d679f777bc6702c7d\
                        39f23369a9d9bacfa530e26304231461\
                        b2eb05e2c39be9fcda6c19078c6a9d1b";

        let mut cipher_text = vec![0u8; plain.len()];
        let mut handle = SymmetricCbcHandle { state: std::ptr::null_mut() };
        unsafe {
            assert_eq!(
                cbc_start(CIPHER_RIJNDAEL, iv.as_ptr(), key.as_ptr(), 32, 0, &mut handle),
                CRYPT_OK
            );
            assert_eq!(
                cbc_encrypt(
                    plain.as_ptr(),
                    cipher_text.as_mut_ptr(),
                    plain.len() as c_ulong,
                    &mut handle
                ),
                CRYPT_OK
            );
            assert_eq!(cbc_done(&mut handle), CRYPT_OK);
            assert!(handle.state.is_null(), "cbc_done must consume the handle");
        }
        assert_eq!(hex(&cipher_text), expected);

        // And back again — a fresh context, as SQLCipher uses one per page.
        let mut round_trip = vec![0u8; plain.len()];
        let mut handle = SymmetricCbcHandle { state: std::ptr::null_mut() };
        unsafe {
            assert_eq!(
                cbc_start(CIPHER_RIJNDAEL, iv.as_ptr(), key.as_ptr(), 32, 0, &mut handle),
                CRYPT_OK
            );
            assert_eq!(
                cbc_decrypt(
                    cipher_text.as_ptr(),
                    round_trip.as_mut_ptr(),
                    cipher_text.len() as c_ulong,
                    &mut handle
                ),
                CRYPT_OK
            );
            assert_eq!(cbc_done(&mut handle), CRYPT_OK);
        }
        assert_eq!(round_trip, plain);
    }

    /// The registry SQLCipher indexes into. `max_key_length` is what makes the
    /// page cipher AES-**256**, and `block_length` is the page IV size — a wrong
    /// value here would silently weaken every database.
    #[test]
    fn registry_reports_aes_256_cbc() {
        unsafe {
            assert_eq!(find_cipher(NAME_RIJNDAEL.as_ptr() as *const c_char), CIPHER_RIJNDAEL);
            assert_eq!(find_hash(NAME_SHA512.as_ptr() as *const c_char), HASH_SHA512);
            assert_eq!(find_hash(c"md5".as_ptr()), -1);
            assert_eq!(register_cipher(&rijndael_desc), CIPHER_RIJNDAEL);
            assert_eq!(register_hash(&sha512_desc), HASH_SHA512);
            assert_eq!(register_prng(&fortuna_desc), 0);
        }
        assert_eq!(cipher_descriptor[CIPHER_RIJNDAEL as usize].max_key_length, 32);
        assert_eq!(cipher_descriptor[CIPHER_RIJNDAEL as usize].block_length, 16);
        assert_eq!(hash_descriptor[HASH_SHA512 as usize].hashsize, 64);
        assert_eq!(hash_descriptor[HASH_SHA256 as usize].hashsize, 32);
        assert_eq!(hash_descriptor[HASH_SHA1 as usize].hashsize, 20);
    }

    /// A truncated MAC would still round-trip inside SQLCipher while being a
    /// weaker MAC, so a too-small output buffer has to be an error rather than a
    /// short write.
    #[test]
    fn hmac_refuses_to_truncate() {
        let mut handle = HmacStateHandle { state: std::ptr::null_mut() };
        let mut out = [0u8; 32];
        let mut outlen = 32 as c_ulong;
        unsafe {
            assert_eq!(hmac_init(&mut handle, HASH_SHA512, b"k".as_ptr(), 1), CRYPT_OK);
            assert_eq!(hmac_process(&mut handle, b"data".as_ptr(), 4), CRYPT_OK);
            assert_eq!(
                hmac_done(&mut handle, out.as_mut_ptr(), &mut outlen),
                CRYPT_ERROR,
                "a 32-byte buffer cannot hold a SHA-512 MAC"
            );
        }
    }

    #[test]
    fn cbc_rejects_partial_blocks_and_unknown_ciphers() {
        let key = [7u8; 32];
        let iv = [0u8; 16];
        let mut handle = SymmetricCbcHandle { state: std::ptr::null_mut() };
        unsafe {
            assert_eq!(cbc_start(42, iv.as_ptr(), key.as_ptr(), 32, 0, &mut handle), CRYPT_ERROR);
            assert_eq!(cbc_start(CIPHER_RIJNDAEL, iv.as_ptr(), key.as_ptr(), 7, 0, &mut handle), CRYPT_ERROR);
            assert_eq!(
                cbc_start(CIPHER_RIJNDAEL, iv.as_ptr(), key.as_ptr(), 32, 0, &mut handle),
                CRYPT_OK
            );
            let src = [0u8; 17];
            let mut dst = [0u8; 17];
            assert_eq!(cbc_encrypt(src.as_ptr(), dst.as_mut_ptr(), 17, &mut handle), CRYPT_ERROR);
            assert_eq!(cbc_done(&mut handle), CRYPT_OK);
        }
    }

    #[test]
    fn fortuna_reads_from_the_os() {
        let mut prng = PrngState { started: 0 };
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        unsafe {
            assert_eq!(fortuna_start(&mut prng), CRYPT_OK);
            assert_eq!(fortuna_add_entropy(a.as_ptr(), 32, &mut prng), CRYPT_OK);
            assert_eq!(fortuna_ready(&mut prng), CRYPT_OK);
            assert_eq!(fortuna_read(a.as_mut_ptr(), 32, &mut prng), 32);
            assert_eq!(fortuna_read(b.as_mut_ptr(), 32, &mut prng), 32);
            assert_eq!(fortuna_done(&mut prng), CRYPT_OK);
        }
        assert_ne!(a, [0u8; 32], "the PRNG returned all zeroes");
        assert_ne!(a, b, "two reads returned identical bytes");
    }
}
