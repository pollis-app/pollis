/*
** The slice of the LibTomCrypt API that SQLCipher's LibTomCrypt crypto
** provider actually calls — and nothing else.
**
** `vendor/sqlite3.c` is the stock SQLCipher amalgamation. Compiled with
** -DSQLCIPHER_CRYPTO_LIBTOMCRYPT it selects `crypto_libtomcrypt.c`, whose only
** contact with the outside world is `#include <tomcrypt.h>` plus the ~20
** symbols declared below. This header, and the Rust implementations in
** `src/abi.rs` that back it, are that contact surface. The amalgamation itself
** is byte-identical to upstream (see vendor/PROVENANCE.md) — there is no patch
** to audit, only this file.
**
** It is deliberately NOT libtomcrypt. Every primitive resolves to the same
** RustCrypto crates the rest of pollis already uses (`aes`, `sha1`, `sha2`,
** `hmac`, `pbkdf2`), so Windows gains SQLCipher without dragging a second
** libcrypto into an image that already contains libwebrtc's BoringSSL. The
** ciphertext is stock SQLCipher 4: AES-256-CBC pages, HMAC-SHA512 page MACs,
** PBKDF2-HMAC-SHA512 key derivation.
**
** The types below are opaque handles owned by the Rust side. Their layouts are
** defined here and mirrored by `#[repr(C)]` structs in `src/abi.rs`; nothing
** else reads them, so they need not resemble libtomcrypt's.
*/
#ifndef POLLIS_TOMCRYPT_H
#define POLLIS_TOMCRYPT_H

#define CRYPT_OK 0
#define CRYPT_ERROR 1

/*
** Reported by `PRAGMA cipher_provider_version`. `PRAGMA cipher_provider` still
** answers "libtomcrypt" — that string is baked into the upstream provider we
** deliberately did not patch — so this is the field that names what is really
** underneath.
*/
#define SCRYPT "pollis-rustcrypto-1"

/* Registry entries. Only the fields SQLCipher reads are present. */
struct ltc_hash_descriptor {
  const char *name;
  unsigned long hashsize;
};

struct ltc_cipher_descriptor {
  const char *name;
  int min_key_length;
  int max_key_length;
  int block_length;
};

struct ltc_prng_descriptor {
  const char *name;
};

extern const struct ltc_hash_descriptor hash_descriptor[];
extern const struct ltc_cipher_descriptor cipher_descriptor[];

extern const struct ltc_hash_descriptor sha1_desc;
extern const struct ltc_hash_descriptor sha256_desc;
extern const struct ltc_hash_descriptor sha512_desc;
extern const struct ltc_cipher_descriptor rijndael_desc;
extern const struct ltc_prng_descriptor fortuna_desc;

/* Opaque handles; the payload is a Rust-owned allocation. */
typedef struct {
  void *state;
} hmac_state;

typedef struct {
  void *state;
} symmetric_CBC;

typedef struct {
  int started;
} prng_state;

int register_prng(const struct ltc_prng_descriptor *prng);
int register_cipher(const struct ltc_cipher_descriptor *cipher);
int register_hash(const struct ltc_hash_descriptor *hash);
int find_cipher(const char *name);
int find_hash(const char *name);

/*
** SQLCipher's provider treats these as a Fortuna CSPRNG it seeds itself. Here
** they are a thin front for the operating system's CSPRNG: entropy pushed in is
** accepted and discarded, and every read comes from the OS. That is strictly
** stronger than the userspace Fortuna the name implies, and it means
** `sqlcipher_ltc_activate`'s seeding dance cannot leave the generator weak.
*/
int fortuna_start(prng_state *prng);
int fortuna_add_entropy(const unsigned char *in, unsigned long inlen, prng_state *prng);
int fortuna_ready(prng_state *prng);
unsigned long fortuna_read(unsigned char *out, unsigned long outlen, prng_state *prng);
int fortuna_done(prng_state *prng);

int hmac_init(hmac_state *hmac, int hash, const unsigned char *key, unsigned long keylen);
int hmac_process(hmac_state *hmac, const unsigned char *in, unsigned long inlen);
int hmac_done(hmac_state *hmac, unsigned char *out, unsigned long *outlen);

int pkcs_5_alg2(const unsigned char *password, unsigned long password_len,
                const unsigned char *salt, unsigned long salt_len,
                int iteration_count, int hash_idx,
                unsigned char *out, unsigned long *outlen);

int cbc_start(int cipher, const unsigned char *IV, const unsigned char *key,
              int keylen, int num_rounds, symmetric_CBC *cbc);
int cbc_encrypt(const unsigned char *pt, unsigned char *ct, unsigned long len, symmetric_CBC *cbc);
int cbc_decrypt(const unsigned char *ct, unsigned char *pt, unsigned long len, symmetric_CBC *cbc);
int cbc_done(symmetric_CBC *cbc);

#endif /* POLLIS_TOMCRYPT_H */
