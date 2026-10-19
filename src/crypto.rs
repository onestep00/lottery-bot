use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::{rngs::OsRng, RngCore};
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPublicKey};

pub fn rsa_hex(text: &str, modulus: &str, exponent: &str) -> Result<String> {
    let n = BigUint::parse_bytes(modulus.as_bytes(), 16).context("RSA modulus 형식 오류")?;
    let e = BigUint::parse_bytes(exponent.as_bytes(), 16).context("RSA exponent 형식 오류")?;
    let key = RsaPublicKey::new(n, e)?;
    Ok(hex::encode(key.encrypt(
        &mut OsRng,
        Pkcs1v15Encrypt,
        text.as_bytes(),
    )?))
}
fn key(pass: &str, salt: &[u8]) -> [u8; 16] {
    let mut out = [0; 16];
    let pass: String = pass.chars().take(32).collect();
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(pass.as_bytes(), salt, 1000, &mut out);
    out
}
pub fn encrypt(plain: &str, pass: &str) -> String {
    let mut salt = [0; 32];
    let mut iv = [0; 16];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut iv);
    let encrypted = cbc::Encryptor::<aes::Aes128>::new(&key(pass, &salt).into(), &iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
    format!(
        "{}{}{}",
        hex::encode(salt),
        hex::encode(iv),
        STANDARD.encode(encrypted)
    )
}
pub fn decrypt(encrypted: &str, pass: &str) -> Result<String> {
    ensure!(
        encrypted.is_ascii() && encrypted.len() > 96,
        "연금복권 암호문 형식 오류"
    );
    let salt = hex::decode(&encrypted[..64])?;
    let iv = hex::decode(&encrypted[64..96])?;
    let data = STANDARD.decode(&encrypted[96..])?;
    let decrypted = cbc::Decryptor::<aes::Aes128>::new_from_slices(&key(pass, &salt), &iv)
        .map_err(|_| anyhow::anyhow!("AES 키 형식 오류"))?
        .decrypt_padded_vec_mut::<Pkcs7>(&data)
        .map_err(|_| anyhow::anyhow!("연금복권 복호화 실패: 세션/사이트 변경 확인"))?;
    String::from_utf8(decrypted).context("연금복권 응답 UTF-8 형식 오류")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_payload_roundtrip_and_bad_key() {
        let data = encrypt("ROUND=300&USER_ID=한글", "session-key-1234567890");
        assert_eq!(
            decrypt(&data, "session-key-1234567890").unwrap(),
            "ROUND=300&USER_ID=한글"
        );
        assert!(decrypt(&data, "wrong-key").is_err());
        assert!(decrypt("malformed", "key").is_err());
    }
    #[test]
    fn decrypts_original_python_protocol_fixture_with_short_session() {
        // Independently generated with the original PyCryptodome PBKDF2/AES implementation.
        let fixture = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f000102030405060708090a0b0c0d0e0fZtjI5Th40ogkMKdQ195juUvD0PIrsXDojeCM48JC8OCzgwinsNedZeRnl/fZziYQ";
        assert_eq!(
            decrypt(fixture, "short-session").unwrap(),
            "ROUND=338&SEL_NO=001234&BUY_CNT=5"
        );
    }
}
