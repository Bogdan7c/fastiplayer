use std::fmt;

use hls_playlist_core::{
    ExactReference, HlsKeyDeclaration, HlsKeyFormat, HlsKeyMethod, InitializationMap,
};
use zeroize::Zeroizing;

/// Ровно один AES-128 key по RFC 8216, автоматически обнуляемый при уничтожении.
pub struct SecretAes128Key(Zeroizing<[u8; 16]>);

impl SecretAes128Key {
    /// Exact RFC 8216 AES-128 key length.
    pub const BYTE_LENGTH: usize = 16;

    /// Проверяет, что загруженный key-файл содержит ровно 16 bytes.
    pub fn from_key_file_bytes(bytes: &[u8]) -> Result<Self, HlsKeyStateError> {
        if bytes.len() != Self::BYTE_LENGTH {
            return Err(HlsKeyStateError::InvalidKeyLength);
        }
        let mut key = Zeroizing::new([0u8; 16]);
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    /// Декодирует extractor override, содержащий ровно 16 bytes в hexadecimal-виде.
    pub fn from_hex(hex: &str) -> Result<Self, ExtractorAesOverrideError> {
        let hex = without_prefix(hex);
        if hex.len() != 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ExtractorAesOverrideError::InvalidKeyHex);
        }
        Ok(Self(decode_left_padded_hex(hex)?))
    }

    pub(crate) fn as_array(&self) -> &[u8; 16] {
        &self.0
    }
}

impl Clone for SecretAes128Key {
    fn clone(&self) -> Self {
        let mut cloned = Zeroizing::new([0u8; 16]);
        cloned.copy_from_slice(self.0.as_ref());
        Self(cloned)
    }
}

impl fmt::Debug for SecretAes128Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretAes128Key([REDACTED; 16])")
    }
}

/// 128-bit initialization vector по RFC 8216.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes128InitializationVector([u8; 16]);

impl Aes128InitializationVector {
    /// Создаёт IV из explicit bytes manifest/override.
    pub const fn explicit(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Выводит big-endian IV из media sequence с заполнением нулями слева.
    pub fn from_media_sequence(media_sequence: u64) -> Self {
        let mut bytes = [0u8; 16];
        bytes[8..].copy_from_slice(&media_sequence.to_be_bytes());
        Self(bytes)
    }

    /// Разбирает extractor hex и дополняет его нулями слева до 16 bytes.
    pub fn from_hex(hex: &str) -> Result<Self, ExtractorAesOverrideError> {
        let hex = without_prefix(hex);
        if hex.is_empty() || hex.len() > 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ExtractorAesOverrideError::InvalidIvHex);
        }
        Ok(Self(*decode_left_padded_hex(hex)?))
    }

    pub(crate) const fn as_array(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Активный key либо уже inline, либо должен быть загружен по exact reference.
#[derive(Clone, Debug)]
pub enum Aes128KeySource {
    Inline(SecretAes128Key),
    ManifestReference(ExactReference),
    ExtractorReplacement(ExtractorKeyUri),
}

/// Exact extractor key URI с безопасным redacted-форматированием.
#[derive(Clone, PartialEq, Eq)]
pub struct ExtractorKeyUri(Box<str>);

impl ExtractorKeyUri {
    /// Открывает replacement только будущей границе URI resolution/request composition.
    pub fn expose_for_resolution(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExtractorKeyUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractorKeyUri")
            .field("utf8_bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Extractor overrides применяются только при активном `METHOD=AES-128`.
#[derive(Clone, Debug, Default)]
pub struct ExtractorAesOverride {
    replacement_key_uri: Option<ExtractorKeyUri>,
    inline_key: Option<SecretAes128Key>,
    explicit_iv: Option<Aes128InitializationVector>,
}

impl ExtractorAesOverride {
    /// Создаёт validated override values без HTTP- или service-зависимостей.
    pub fn new(
        replacement_key_uri: Option<&str>,
        inline_key_hex: Option<&str>,
        explicit_iv_hex: Option<&str>,
    ) -> Result<Self, ExtractorAesOverrideError> {
        Ok(Self {
            replacement_key_uri: replacement_key_uri.map(|uri| ExtractorKeyUri(uri.into())),
            inline_key: inline_key_hex.map(SecretAes128Key::from_hex).transpose()?,
            explicit_iv: explicit_iv_hex
                .map(Aes128InitializationVector::from_hex)
                .transpose()?,
        })
    }
}

/// Недопустимые значения extractor override.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtractorAesOverrideError {
    #[error("extractor AES key hex не кодирует exact 16-byte key")]
    InvalidKeyHex,
    #[error("extractor AES IV hex не помещается в 16 bytes")]
    InvalidIvHex,
}

/// Активное AES-128 состояние после применения одной декларации `EXT-X-KEY`.
#[derive(Clone, Debug)]
pub struct ActiveAes128Key {
    source: Aes128KeySource,
    explicit_iv: Option<Aes128InitializationVector>,
}

impl ActiveAes128Key {
    /// Намерение источника key для будущей fetch/cache-композиции.
    pub const fn source(&self) -> &Aes128KeySource {
        &self.source
    }

    /// Выбирает explicit IV либо выводит его из media sequence.
    pub fn iv_for_media_segment(&self, media_sequence: u64) -> Aes128InitializationVector {
        self.explicit_iv
            .unwrap_or_else(|| Aes128InitializationVector::from_media_sequence(media_sequence))
    }

    /// Зашифрованные initialization sections требуют explicit IV по RFC 8216.
    pub fn iv_for_initialization_map(
        &self,
    ) -> Result<Aes128InitializationVector, HlsKeyStateError> {
        self.explicit_iv
            .ok_or(HlsKeyStateError::EncryptedMapRequiresExplicitIv)
    }
}

/// Владелец состояния ротации `EXT-X-KEY`/`NONE`.
#[derive(Clone, Debug, Default)]
pub struct HlsKeyState {
    active: Option<ActiveAes128Key>,
}

impl HlsKeyState {
    /// Атомарно применяет декларацию; ошибка сохраняет прежнее состояние без изменений.
    pub fn apply(
        &mut self,
        declaration: &HlsKeyDeclaration,
        extractor_override: Option<&ExtractorAesOverride>,
    ) -> Result<(), HlsKeyStateError> {
        let replacement = build_active_key(declaration, extractor_override)?;
        self.active = replacement;
        Ok(())
    }

    /// Текущее AES-состояние, отсутствующее после `METHOD=NONE`.
    pub const fn active(&self) -> Option<&ActiveAes128Key> {
        self.active.as_ref()
    }

    /// Восстанавливает key, зафиксированный MAP, независимо от последующей ротации сегментов.
    pub fn active_for_initialization_map(
        initialization_map: &InitializationMap,
        extractor_override: Option<&ExtractorAesOverride>,
    ) -> Result<Option<ActiveAes128Key>, HlsKeyStateError> {
        let Some(declaration) = initialization_map.key.as_ref() else {
            return Ok(None);
        };
        let active = build_active_key(declaration, extractor_override)?
            .ok_or(HlsKeyStateError::MissingActiveKey)?;
        active.iv_for_initialization_map()?;
        Ok(Some(active))
    }
}

/// Типизированные profile/key-state ошибки до network/player mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsKeyStateError {
    #[error("HLS key file должен содержать ровно 16 bytes")]
    InvalidKeyLength,
    #[error("HLS encryption method не поддерживается")]
    UnsupportedMethod,
    #[error("HLS KEYFORMAT не равен identity")]
    UnsupportedKeyFormat,
    #[error("AES-128 declaration не содержит key URI")]
    MissingKeyUri,
    #[error("encrypted EXT-X-MAP требует explicit IV")]
    EncryptedMapRequiresExplicitIv,
    #[error("initialization map заявляет encryption без active key")]
    MissingActiveKey,
}

fn build_active_key(
    declaration: &HlsKeyDeclaration,
    extractor_override: Option<&ExtractorAesOverride>,
) -> Result<Option<ActiveAes128Key>, HlsKeyStateError> {
    match declaration.method {
        HlsKeyMethod::None => return Ok(None),
        HlsKeyMethod::Aes128 => {}
        HlsKeyMethod::SampleAes | HlsKeyMethod::Other(_) => {
            return Err(HlsKeyStateError::UnsupportedMethod);
        }
    }
    if !matches!(
        declaration.key_format,
        HlsKeyFormat::ImplicitIdentity | HlsKeyFormat::Identity
    ) {
        return Err(HlsKeyStateError::UnsupportedKeyFormat);
    }
    let source = if let Some(key) = extractor_override.and_then(|value| value.inline_key.as_ref()) {
        Aes128KeySource::Inline(key.clone())
    } else if let Some(uri) =
        extractor_override.and_then(|value| value.replacement_key_uri.as_ref())
    {
        Aes128KeySource::ExtractorReplacement(uri.clone())
    } else {
        Aes128KeySource::ManifestReference(
            declaration
                .uri
                .clone()
                .ok_or(HlsKeyStateError::MissingKeyUri)?,
        )
    };
    let explicit_iv = extractor_override
        .and_then(|value| value.explicit_iv)
        .or(declaration
            .explicit_iv
            .map(Aes128InitializationVector::explicit));
    Ok(Some(ActiveAes128Key {
        source,
        explicit_iv,
    }))
}

fn decode_left_padded_hex(hex: &str) -> Result<Zeroizing<[u8; 16]>, ExtractorAesOverrideError> {
    let mut normalized = String::with_capacity(hex.len() + 1);
    if hex.len() % 2 == 1 {
        normalized.push('0');
    }
    normalized.push_str(hex);
    let mut decoded = Zeroizing::new([0u8; 16]);
    let start = 16 - normalized.len() / 2;
    for (index, pair) in normalized.as_bytes().chunks_exact(2).enumerate() {
        let text =
            std::str::from_utf8(pair).map_err(|_| ExtractorAesOverrideError::InvalidIvHex)?;
        decoded[start + index] =
            u8::from_str_radix(text, 16).map_err(|_| ExtractorAesOverrideError::InvalidIvHex)?;
    }
    Ok(decoded)
}

fn without_prefix(value: &str) -> &str {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value)
}
