//! PKCE (RFC 7636, `S256` only) and the random `state` / `nonce` values
//! a sign-in needs. RFC 9700 makes PKCE mandatory for public clients.

use sha2::{Digest, Sha256};

/// Code verifier and its `S256` challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl Pkce {
    /// Fresh pair: a 256-bit verifier (43 base64url characters).
    #[must_use]
    pub fn generate() -> Self {
        Self::from_verifier(random_token(32))
    }

    /// Pair for a given verifier (tests, RFC vectors).
    #[must_use]
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }

    /// `code_verifier` for the token request. Keep it secret.
    #[must_use]
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// `code_challenge` for the authorization request.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// `code_challenge_method`.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        "S256"
    }
}

/// Random URL-safe token of `bytes` random bytes (`state`, `nonce`).
///
/// # Panics
/// When the OS random source fails.
#[must_use]
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0_u8; bytes];
    fill_random(&mut buf);
    base64url(&buf)
}

/// Random `u64` (session ids).
#[cfg(feature = "desktop")]
#[must_use]
pub(crate) fn random_u64() -> u64 {
    let mut bytes = [0_u8; 8];
    fill_random(&mut bytes);
    u64::from_ne_bytes(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn fill_random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("invariant: the OS random source is available");
}

#[cfg(target_arch = "wasm32")]
fn fill_random(_buf: &mut [u8]) {
    unimplemented!("web auth runs in the page's own flow on wasm");
}

/// Base64url without padding (RFC 4648 §5).
#[must_use]
pub fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let symbols = chunk.len() + 1;
        for i in 0..symbols {
            let index = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(pkce.method(), "S256");
    }

    #[test]
    fn base64url_matches_rfc4648() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn generated_values_are_fresh_and_sized() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier(), b.verifier());
        assert_eq!(a.verifier().len(), 43);
        assert_eq!(random_token(16).len(), 22);
    }
}
