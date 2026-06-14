//! Native port of KakaoTalk's macOS key derivation.
//!
//! These functions are a 1:1 port of the proven reference algorithm
//! (`secure_key`, `database_name`, `hashed_device_uuid` from `kakaotalk_mac.py`,
//! which itself mirrors kakaocli's Swift `KeyDerivation`). All string slicing is
//! by byte offset, which is safe here because every input is ASCII (the
//! IOPlatformUUID is hex + dashes, and `user_id` is base-10 digits).

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use hmac::Hmac;
use sha1::Sha1;
use sha2::{Digest, Sha256};

const PBKDF2_ROUNDS: u32 = 100_000;
const PBKDF2_DKLEN: usize = 128;

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8]) -> [u8; PBKDF2_DKLEN] {
    let mut out = [0u8; PBKDF2_DKLEN];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(password, salt, PBKDF2_ROUNDS, &mut out)
        .expect("pbkdf2 output length is valid");
    out
}

/// `base64_standard( sha1(uuid)[20] ++ sha256(uuid)[32] )`.
pub fn hashed_device_uuid(uuid: &str) -> String {
    let uuid_bytes = uuid.as_bytes();
    let mut combined = Vec::with_capacity(20 + 32);
    combined.extend_from_slice(&Sha1::digest(uuid_bytes));
    combined.extend_from_slice(&Sha256::digest(uuid_bytes));
    BASE64_STANDARD.encode(combined)
}

fn reverse_ascii(input: &str) -> String {
    input.chars().rev().collect()
}

/// The 256-lowercase-hex SQLCipher passphrase for `(user_id, uuid)`.
pub fn secure_key(user_id: i64, uuid: &str) -> String {
    let hashed = hashed_device_uuid(uuid);
    let parts = [
        "A",
        &hashed,
        "|",
        "F",
        &uuid[0..5],
        "H",
        &user_id.to_string(),
        "|",
        &uuid[7..],
    ];
    let joined = parts.join("F");
    let hawawa = reverse_ascii(&joined);
    let salt_start = (uuid.len() as f64 * 0.3) as usize;
    let salt = &uuid[salt_start..];
    let derived = pbkdf2_sha256(hawawa.as_bytes(), salt.as_bytes());
    hex_lower(&derived)
}

/// The 78-hex DB filename for `(user_id, uuid)`.
pub fn database_name(user_id: i64, uuid: &str) -> String {
    let reversed_uuid = reverse_ascii(uuid);
    let parts = [
        ".",
        "F",
        &user_id.to_string(),
        "A",
        "F",
        &reversed_uuid,
        ".",
        "|",
    ];
    let hawawa = parts.join(".");
    let salt = reverse_ascii(&hashed_device_uuid(uuid));
    let derived = pbkdf2_sha256(hawawa.as_bytes(), salt.as_bytes());
    let hex = hex_lower(&derived);
    hex[28..28 + 78].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORACLE_UUID: &str = "42C34717-27C3-538C-81E4-8B568287C7A0";
    const ORACLE_USER_ID: i64 = 240_061_982;

    #[test]
    fn database_name_matches_known_oracle() {
        // Empirically verified filename for this (uuid, user_id) on the
        // reference machine. A perfect oracle for the PBKDF2 / string port.
        assert_eq!(
            database_name(ORACLE_USER_ID, ORACLE_UUID),
            "3080037d7a3b71fbe90b9492c50faf90eb3a8d708baec8ec3f18346bf53568cf84c0251259f2a6"
        );
    }

    #[test]
    fn secure_key_is_256_lowercase_hex() {
        let key = secure_key(ORACLE_USER_ID, ORACLE_UUID);
        assert_eq!(key.len(), 256);
        assert!(key
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }

    #[test]
    fn hashed_device_uuid_is_base64_of_52_bytes() {
        let hashed = hashed_device_uuid(ORACLE_UUID);
        let decoded = BASE64_STANDARD.decode(&hashed).expect("valid base64");
        assert_eq!(decoded.len(), 20 + 32);
    }
}
