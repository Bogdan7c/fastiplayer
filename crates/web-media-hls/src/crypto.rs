use aes::Aes128;
use cbc::{
    Decryptor,
    cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7},
};
use zeroize::Zeroizing;

use crate::{Aes128InitializationVector, SecretAes128Key};

/// Владелец secret plaintext; `Debug` намеренно отсутствует.
pub struct DecryptedBytes(Zeroizing<Vec<u8>>);

impl DecryptedBytes {
    /// Открывает plaintext только будущей границе передачи в demux.
    pub fn expose_for_demux(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for DecryptedBytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecryptedBytes")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Безопасная таксономия AES-ошибок без key/IV/ciphertext material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Aes128CbcDecryptError {
    #[error("AES-128-CBC ciphertext пуст или не кратен 16-byte block")]
    InvalidCiphertextLength,
    #[error("AES-128-CBC plaintext имеет invalid PKCS#7 padding")]
    InvalidPkcs7Padding,
}

/// Project-owned граница AES-128-CBC/PKCS#7 decrypt по RFC 8216.
pub fn decrypt_aes128_cbc_pkcs7(
    ciphertext: &[u8],
    key: &SecretAes128Key,
    iv: Aes128InitializationVector,
) -> Result<DecryptedBytes, Aes128CbcDecryptError> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return Err(Aes128CbcDecryptError::InvalidCiphertextLength);
    }
    let mut plaintext = Zeroizing::new(ciphertext.to_vec());
    let plaintext_length = Decryptor::<Aes128>::new(key.as_array().into(), iv.as_array().into())
        .decrypt_padded::<Pkcs7>(&mut plaintext)
        .map_err(|_| Aes128CbcDecryptError::InvalidPkcs7Padding)?
        .len();
    plaintext.truncate(plaintext_length);
    Ok(DecryptedBytes(plaintext))
}
