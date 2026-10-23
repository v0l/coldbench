//! Encryption of wallet files at rest, with a password.
//!
//! The password goes through Argon2id to a 256 bit key, and the file is sealed with
//! ChaCha20-Poly1305. The header (magic, Argon2 cost, salt) is authenticated, so changing the
//! cost to something weaker is caught. A fresh nonce is used on every save.
//!
//! ```text
//! "CBENC\0\0\x01"  8 bytes   magic and version
//! m_kib t p        3 x u32 LE
//! salt            16 bytes
//! nonce           12 bytes
//! ciphertext + tag
//! ```

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

const MAGIC: &[u8; 8] = b"CBENC\0\0\x01";
const HEADER: usize = 8 + 12 + 16;
const NONCE: usize = 12;

/// How hard the password is made to work. Stored in the file, so it can be raised later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cost {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Cost {
    /// 64 MiB and three passes: a few tenths of a second here, a real price for a guesser.
    pub const DEFAULT: Cost = Cost { m_kib: 64 * 1024, t: 3, p: 1 };
    /// What an opened file may ask for, so a hostile file cannot make the app allocate gigabytes.
    const MAX: Cost = Cost { m_kib: 1024 * 1024, t: 16, p: 16 };
}

/// A key worked out from a password, kept while the wallet is open so saving needs no password.
#[derive(Clone, ZeroizeOnDrop)]
pub struct Key {
    key: [u8; 32],
    #[zeroize(skip)]
    salt: [u8; 16],
    #[zeroize(skip)]
    cost: Cost,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Key(..)")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Not our format, or cut short.
    Damaged,
    /// The password is wrong, or the file was changed.
    Wrong,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Damaged => write!(f, "this is not a wallet file that can be decrypted"),
            Error::Wrong => write!(f, "wrong password"),
        }
    }
}

pub fn is_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

fn work(password: &str, salt: &[u8; 16], cost: Cost) -> Result<[u8; 32], Error> {
    let params = Params::new(cost.m_kib, cost.t, cost.p, Some(32)).map_err(|_| Error::Damaged)?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|_| Error::Damaged)?;
    Ok(out)
}

impl Key {
    /// A key for a new password, with a salt of its own.
    pub fn new(password: &str) -> Key {
        Key::with_cost(password, Cost::DEFAULT)
    }

    pub fn with_cost(password: &str, cost: Cost) -> Key {
        let salt: [u8; 16] = Generate::generate();
        let key = work(password, &salt, cost).expect("the built in cost is valid");
        Key { key, salt, cost }
    }

    /// Whether `password` is the one this key came from.
    pub fn is(&self, password: &str) -> bool {
        let Ok(mut k) = work(password, &self.salt, self.cost) else { return false };
        // Compared without stopping at the first difference.
        let same = k.iter().zip(&self.key).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
        k.zeroize();
        same
    }

    fn header(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(HEADER);
        h.extend_from_slice(MAGIC);
        for n in [self.cost.m_kib, self.cost.t, self.cost.p] {
            h.extend_from_slice(&n.to_le_bytes());
        }
        h.extend_from_slice(&self.salt);
        h
    }

    /// `plain` as a file's bytes.
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let nonce = Nonce::generate();
        let head = self.header();
        let cipher = ChaCha20Poly1305::new_from_slice(&self.key).expect("a 32 byte key");
        let ct = cipher
            .encrypt(&nonce, Payload { msg: plain, aad: &head })
            .expect("encrypting in memory cannot fail");
        let mut out = head;
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }
}

/// The contents of an encrypted file, and the key that opens it, for saving again.
pub fn open(password: &str, file: &[u8]) -> Result<(Vec<u8>, Key), Error> {
    if !is_encrypted(file) || file.len() < HEADER + NONCE + 16 {
        return Err(Error::Damaged);
    }
    let word = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().expect("four bytes"));
    let cost = Cost { m_kib: word(8), t: word(12), p: word(16) };
    if cost.m_kib > Cost::MAX.m_kib || cost.t > Cost::MAX.t || cost.p > Cost::MAX.p {
        return Err(Error::Damaged);
    }
    let salt: [u8; 16] = file[20..36].try_into().expect("sixteen bytes");
    let key = Key { key: work(password, &salt, cost)?, salt, cost };
    let (head, rest) = file.split_at(HEADER);
    let (nonce, ct) = rest.split_at(NONCE);
    let cipher = ChaCha20Poly1305::new_from_slice(&key.key).expect("a 32 byte key");
    let nonce = Nonce::try_from(nonce).map_err(|_| Error::Damaged)?;
    let plain = cipher
        .decrypt(&nonce, Payload { msg: ct, aad: head })
        .map_err(|_| Error::Wrong)?;
    Ok((plain, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEAP: Cost = Cost { m_kib: 64, t: 1, p: 1 };

    #[test]
    fn it_round_trips_and_hides_the_text() {
        let key = Key::with_cost("correct horse", CHEAP);
        let file = key.seal(b"{\"xpub\":\"xpub6secret\"}");
        assert!(is_encrypted(&file));
        assert!(!file.windows(6).any(|w| w == b"secret"));
        let (plain, again) = open("correct horse", &file).unwrap();
        assert_eq!(plain, b"{\"xpub\":\"xpub6secret\"}");
        // The key that comes back seals files that open with the same password.
        let (plain, _) = open("correct horse", &again.seal(b"more")).unwrap();
        assert_eq!(plain, b"more");
    }

    #[test]
    fn a_wrong_password_or_a_changed_file_is_refused() {
        let key = Key::with_cost("right", CHEAP);
        let file = key.seal(b"data");
        assert_eq!(open("wrong", &file).unwrap_err(), Error::Wrong);
        assert_eq!(open("", &file).unwrap_err(), Error::Wrong);
        for at in [9, 20, 40, file.len() - 1] {
            let mut bad = file.clone();
            bad[at] ^= 1;
            assert!(open("right", &bad).is_err(), "byte {at}");
        }
        assert_eq!(open("right", &file[..30]).unwrap_err(), Error::Damaged);
        assert_eq!(open("right", b"{\"meta\":{}}").unwrap_err(), Error::Damaged);
    }

    #[test]
    fn two_saves_differ_and_a_greedy_cost_is_refused() {
        let key = Key::with_cost("pw", CHEAP);
        assert_ne!(key.seal(b"same"), key.seal(b"same"));
        let mut file = key.seal(b"x");
        file[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(open("pw", &file).unwrap_err(), Error::Damaged);
    }

    #[test]
    fn a_key_knows_its_password() {
        let key = Key::with_cost("pw", CHEAP);
        assert!(key.is("pw"));
        assert!(!key.is("pW"));
    }
}

#[cfg(test)]
mod vector {
    use super::*;

    /// A file made once, with the cheapest cost. If a crate upgrade changes what Argon2id or
    /// ChaCha20-Poly1305 produce, this stops opening and wallets saved before it would too.
    const FILE: &str = "4342454e4300000140000000010000000100000035d6eddcd2d38cf74a6813d003420908d61308ec0e68978cd5b676909a78e6509c16cecf9da3080ed174997b9760d4b8e40a9f62b0faec7d";

    #[test]
    fn a_file_sealed_before_still_opens() {
        let bytes: Vec<u8> = (0..FILE.len()).step_by(2).map(|i| u8::from_str_radix(&FILE[i..i + 2], 16).unwrap()).collect();
        let (plain, _) = open("vector password", &bytes).unwrap();
        assert_eq!(plain, b"known answer");
        assert_eq!(open("another", &bytes).unwrap_err(), Error::Wrong);
    }
}
