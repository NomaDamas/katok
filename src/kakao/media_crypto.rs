//! Pkv2 media decryption for KakaoTalk macOS image cache files.
//!
//! A Pkv2 file is `b"Pkv2" || iv[16] || AES-256-CBC-PKCS7(ciphertext)`.
//! The decrypted plaintext has a 256-byte wrapper before the image bytes.

use aes::Aes256;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

const PKV2_MAGIC: &[u8; 4] = b"Pkv2";
const IV_LEN: usize = 16;
const PLAINTEXT_HEADER_LEN: usize = 256;

type Aes256CbcDecryptor = cbc::Decryptor<Aes256>;

/// The key string fed to KakaoTalk's `mactalkAESDecrypt:` for image media.
pub fn media_key_string(log_id: i64) -> String {
    format!("#{log_id}%").chars().rev().collect()
}

/// Decrypt bytes after the 4-byte Pkv2 magic, returning wrapper + image bytes.
pub fn mactalk_aes_decrypt(data: &[u8], key_string: &str) -> Result<Vec<u8>> {
    if data.len() <= IV_LEN {
        return Err(Error::Kakao(
            "invalid Pkv2 payload: missing ciphertext".to_string(),
        ));
    }
    let (iv, ciphertext) = data.split_at(IV_LEN);
    if ciphertext.len() % IV_LEN != 0 {
        return Err(Error::Kakao(
            "invalid Pkv2 payload: ciphertext is not block-aligned".to_string(),
        ));
    }

    let aes_key = Sha256::digest(key_string.as_bytes());
    let mut buf = ciphertext.to_vec();
    let plaintext = Aes256CbcDecryptor::new_from_slices(&aes_key, iv)
        .map_err(|_| Error::Kakao("invalid Pkv2 AES key or IV".to_string()))?
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| Error::Kakao("invalid Pkv2 PKCS7 padding".to_string()))?;
    Ok(plaintext.to_vec())
}

/// Decrypt a raw `.img`/`.thm` Pkv2 file into image bytes.
pub fn decrypt_pkv2_image(file_bytes: &[u8], log_id: i64) -> Result<Vec<u8>> {
    let Some(payload) = file_bytes.strip_prefix(PKV2_MAGIC) else {
        return Err(Error::Kakao("not a Pkv2 file".to_string()));
    };
    let plaintext = mactalk_aes_decrypt(payload, &media_key_string(log_id))?;
    if plaintext.len() < PLAINTEXT_HEADER_LEN {
        return Err(Error::Kakao(
            "invalid Pkv2 plaintext: missing media wrapper".to_string(),
        ));
    }
    Ok(plaintext[PLAINTEXT_HEADER_LEN..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::{Digest as Sha1Digest, Sha1};

    const VECTOR_LOG_ID: i64 = 1_234_567_890_123;
    const VECTOR_IMAGE_HEX: &str = "ffd8ffe04b41544f4b2d504b56322d54455354ffd9";
    const VECTOR_IMAGE_SHA1: &str = "91ed9414d7eb34fe648db42be27a0b7847dc8c8e";
    // Generated from the Python reference algorithm with:
    // log_id=1234567890123, iv=00..0f, plaintext=(b"0123456789abcdef"*16)+image.
    const PYTHON_REFERENCE_PKV2_HEX: &str = concat!(
        "506b7632000102030405060708090a0b0c0d0e0f554b63056928134b57397f6a2e06f1f04",
        "faf2ce5a3905914af3afabf90b8605bc39e6f7ffe132a0bd65963bc6fdbc111d283724581",
        "b869f60e1c85fedaf14265380a50c41ab3efa9a46bade5e1bce7dc175f8fc5d06a29cc",
        "14bb8afbe382eb5bba3e676fd35b0c002fdf5621adedc2d344db8c97873ae4c62769b",
        "38524501062322c5258f86688e325f549a11696b3e68ed354979c4df585732c1d42b",
        "49afe3ac97b46997e39c43e9818cdd9870b7032d8da56cfe0663201a1daa321ad7",
        "a1ee6bbdb584d7b76ca562e05d26eeb3dd7b777c01c18e091bb177fef85bb1013c",
        "6b632c75112780f8f1b423dc5587e17ca1aacc3c8a585373fe2142cd299303fd1",
        "ec64340e58e9dabd4c6f1b2d5298eab53a925efb785f0eac9961d736046ba914fd"
    );

    fn bytes_from_hex(input: &str) -> Vec<u8> {
        assert_eq!(input.len() % 2, 0);
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).expect("hex is utf8");
                u8::from_str_radix(text, 16).expect("valid hex")
            })
            .collect()
    }

    fn sha1_hex(bytes: &[u8]) -> String {
        let digest = Sha1::digest(bytes);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    #[test]
    fn media_key_string_matches_reference_format() {
        assert_eq!(media_key_string(VECTOR_LOG_ID), "%3210987654321#");
    }

    #[test]
    fn decrypts_python_reference_pkv2_vector() {
        let file_bytes = bytes_from_hex(PYTHON_REFERENCE_PKV2_HEX);
        let image = decrypt_pkv2_image(&file_bytes, VECTOR_LOG_ID).expect("decrypt vector");

        assert_eq!(image, bytes_from_hex(VECTOR_IMAGE_HEX));
        assert_eq!(sha1_hex(&image), VECTOR_IMAGE_SHA1);
        assert_eq!(&image[..3], b"\xff\xd8\xff");
    }

    #[test]
    fn same_log_id_key_path_is_media_kind_agnostic() {
        let file_bytes = bytes_from_hex(PYTHON_REFERENCE_PKV2_HEX);

        let img = decrypt_pkv2_image(&file_bytes, VECTOR_LOG_ID).expect(".img decrypt");
        let thm = decrypt_pkv2_image(&file_bytes, VECTOR_LOG_ID).expect(".thm decrypt");

        assert_eq!(img, thm);
    }

    #[test]
    fn rejects_non_pkv2_files() {
        let err = decrypt_pkv2_image(b"nope", VECTOR_LOG_ID).expect_err("invalid magic");
        assert!(err.to_string().contains("not a Pkv2 file"));
    }

    #[test]
    fn rejects_unaligned_ciphertext() {
        let err = mactalk_aes_decrypt(&[0; IV_LEN + 1], &media_key_string(VECTOR_LOG_ID))
            .expect_err("unaligned ciphertext");
        assert!(err.to_string().contains("block-aligned"));
    }
}
