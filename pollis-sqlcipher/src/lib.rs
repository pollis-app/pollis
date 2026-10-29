//! SQLCipher for Windows, with a crypto provider that is not OpenSSL.
//!
//! Static OpenSSL collides with libwebrtc's BoringSSL under MSVC (#991, #992), so [`abi`] backs the libtomcrypt provider with RustCrypto.

pub mod abi;
