//! Serde-neutral locator vocabulary с обратимыми native/foreign identities.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Платформа, на которой была создана foreign path identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum ForeignPathPlatform {
    /// Linux path semantics.
    Linux,
    /// macOS path semantics.
    MacOs,
    /// Windows path semantics.
    Windows,
    /// Будущая или неизвестная платформа, имя которой сохраняется дословно.
    Other(String),
}

impl fmt::Debug for ForeignPathPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Linux => formatter.write_str("ForeignPathPlatform::Linux"),
            Self::MacOs => formatter.write_str("ForeignPathPlatform::MacOs"),
            Self::Windows => formatter.write_str("ForeignPathPlatform::Windows"),
            Self::Other(_) => formatter.write_str("ForeignPathPlatform::Other(<redacted>)"),
        }
    }
}

impl fmt::Display for ForeignPathPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Linux => formatter.write_str("linux"),
            Self::MacOs => formatter.write_str("macos"),
            Self::Windows => formatter.write_str("windows"),
            Self::Other(_) => formatter.write_str("other-platform"),
        }
    }
}

/// Точное представление path units чужой платформы.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum ForeignPathEncoding {
    /// Валидный UTF-8, который всё равно не считается platform-neutral.
    Utf8(String),
    /// Сырые byte units Unix-подобной платформы.
    Bytes(Vec<u8>),
    /// Сырые wide units Windows, включая unpaired surrogate values.
    Wide(Vec<u16>),
    /// Неизвестный encoding с обратимо сохранёнными units.
    Opaque {
        /// Имя encoding из более нового persistence schema.
        encoding_name: String,
        /// Нормализованные unsigned units неизвестного encoding.
        raw_units: Vec<u32>,
    },
}

impl fmt::Debug for ForeignPathEncoding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utf8(_) => formatter.write_str("ForeignPathEncoding::Utf8(<redacted>)"),
            Self::Bytes(units) => formatter
                .debug_tuple("ForeignPathEncoding::Bytes")
                .field(&format_args!("{} units", units.len()))
                .finish(),
            Self::Wide(units) => formatter
                .debug_tuple("ForeignPathEncoding::Wide")
                .field(&format_args!("{} units", units.len()))
                .finish(),
            Self::Opaque { raw_units, .. } => formatter
                .debug_tuple("ForeignPathEncoding::Opaque")
                .field(&format_args!("{} units", raw_units.len()))
                .finish(),
        }
    }
}

/// Нативно неоткрываемая, но полностью обратимая path identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ForeignPlatformPath {
    platform: ForeignPathPlatform,
    encoding: ForeignPathEncoding,
}

impl ForeignPlatformPath {
    /// Создаёт foreign locator без попытки lossy conversion в текущий `PathBuf`.
    pub fn new(platform: ForeignPathPlatform, encoding: ForeignPathEncoding) -> Self {
        Self { platform, encoding }
    }

    /// Возвращает origin platform для persistence mapping.
    pub fn platform_for_persistence(&self) -> &ForeignPathPlatform {
        &self.platform
    }

    /// Возвращает exact foreign units для persistence mapping.
    pub fn encoding_for_persistence(&self) -> &ForeignPathEncoding {
        &self.encoding
    }
}

impl fmt::Debug for ForeignPlatformPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForeignPlatformPath")
            .field("platform", &self.platform)
            .field("encoding", &self.encoding)
            .finish()
    }
}

impl fmt::Display for ForeignPlatformPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "foreign {} path", self.platform)
    }
}

/// Локальный locator: нативный path либо обратимая foreign identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum LocalLocator {
    /// Exact native `PathBuf`; identity никогда не строится через UTF-8 lossification.
    Native(PathBuf),
    /// Path чужой/неизвестной платформы, который нельзя открывать как native.
    Foreign(ForeignPlatformPath),
}

impl LocalLocator {
    /// Даёт native path только boundary, который действительно открывает источник.
    pub fn expose_native_path_for_open(&self) -> Option<&Path> {
        match self {
            Self::Native(path) => Some(path.as_path()),
            Self::Foreign(_) => None,
        }
    }

    /// Даёт exact native path persistence adapter-у без lossy conversion.
    pub fn expose_native_path_for_persistence(&self) -> Option<&Path> {
        match self {
            Self::Native(path) => Some(path.as_path()),
            Self::Foreign(_) => None,
        }
    }

    /// Даёт foreign representation persistence adapter-у для exact roundtrip.
    pub fn expose_foreign_path_for_persistence(&self) -> Option<&ForeignPlatformPath> {
        match self {
            Self::Native(_) => None,
            Self::Foreign(path) => Some(path),
        }
    }
}

impl fmt::Debug for LocalLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(_) => formatter.write_str("LocalLocator::Native(<redacted-path>)"),
            Self::Foreign(path) => formatter
                .debug_tuple("LocalLocator::Foreign")
                .field(path)
                .finish(),
        }
    }
}

impl fmt::Display for LocalLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(path) => {
                let safe_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_else(|| "<local-path>".into());
                formatter.write_str(&safe_name)
            }
            Self::Foreign(path) => fmt::Display::fmt(path, formatter),
        }
    }
}

/// URL identity, raw значение которой нельзя случайно вывести форматированием.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretUrlLocator {
    reopenable_url: String,
}

impl SecretUrlLocator {
    /// Принимает уже классифицированный app/service URL и запрещает пустую identity.
    pub fn from_reopenable_url(
        reopenable_url: impl Into<String>,
    ) -> Result<Self, PlaylistLocatorBuildError> {
        let reopenable_url = reopenable_url.into();

        if reopenable_url.is_empty() {
            return Err(PlaylistLocatorBuildError::EmptyUrlIdentity);
        }

        Ok(Self { reopenable_url })
    }

    /// Явно раскрывает secret-bearing identity только media-open boundary.
    pub fn expose_secret_for_open(&self) -> &str {
        &self.reopenable_url
    }

    /// Явно раскрывает secret-bearing identity только persistence boundary.
    pub fn expose_secret_for_persistence(&self) -> &str {
        &self.reopenable_url
    }

    /// Строит безопасную подпись без userinfo, query, fragment и path payload.
    pub fn redacted_label(&self) -> String {
        redact_url_identity(&self.reopenable_url)
    }
}

impl Hash for SecretUrlLocator {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.reopenable_url.hash(state);
    }
}

impl fmt::Debug for SecretUrlLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SecretUrlLocator")
            .field(&self.redacted_label())
            .finish()
    }
}

impl fmt::Display for SecretUrlLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.redacted_label())
    }
}

/// Persisted/reopenable source identity одного playlist item.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum PlaylistLocator {
    /// Локальный native или foreign path.
    Local(LocalLocator),
    /// Secret-bearing URL с redacted formatting boundary.
    Url(SecretUrlLocator),
}

impl PlaylistLocator {
    /// Возвращает local locator без попытки URL/path coercion.
    pub fn as_local(&self) -> Option<&LocalLocator> {
        match self {
            Self::Local(locator) => Some(locator),
            Self::Url(_) => None,
        }
    }

    /// Возвращает secret URL wrapper, но не раскрывает raw строку автоматически.
    pub fn as_secret_url(&self) -> Option<&SecretUrlLocator> {
        match self {
            Self::Local(_) => None,
            Self::Url(locator) => Some(locator),
        }
    }
}

impl fmt::Debug for PlaylistLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(locator) => formatter
                .debug_tuple("PlaylistLocator::Local")
                .field(locator)
                .finish(),
            Self::Url(locator) => formatter
                .debug_tuple("PlaylistLocator::Url")
                .field(locator)
                .finish(),
        }
    }
}

impl fmt::Display for PlaylistLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(locator) => fmt::Display::fmt(locator, formatter),
            Self::Url(locator) => fmt::Display::fmt(locator, formatter),
        }
    }
}

/// Ошибка построения locator, не содержащая raw URL/path в formatting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlaylistLocatorBuildError {
    /// Reopen identity не может быть пустой.
    EmptyUrlIdentity,
}

impl fmt::Debug for PlaylistLocatorBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaylistLocatorBuildError::EmptyUrlIdentity")
    }
}

impl fmt::Display for PlaylistLocatorBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("URL locator не может быть пустым")
    }
}

impl std::error::Error for PlaylistLocatorBuildError {}

/// Консервативно оставляет только scheme и host, скрывая все credential payloads.
fn redact_url_identity(raw_url: &str) -> String {
    let Some(scheme_separator) = raw_url.find("://") else {
        return "url:<redacted>".to_owned();
    };
    let scheme_end = scheme_separator + 3;
    let suffix = &raw_url[scheme_end..];
    let authority_end = suffix.find(['/', '?', '#']).unwrap_or(suffix.len());
    let authority = &suffix[..authority_end];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);

    if host.is_empty() {
        return "url:<redacted>".to_owned();
    }

    let scheme = &raw_url[..scheme_end];
    let has_hidden_suffix = authority_end < suffix.len();

    if has_hidden_suffix {
        format!("{scheme}{host}/<redacted>")
    } else {
        format!("{scheme}{host}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_url_formatting_is_redacted_but_explicit_exposure_is_exact() {
        // Этот URL одновременно содержит userinfo, query и fragment secrets.
        let raw_url = "https://alice:password@example.com/media/file.mp4?token=secret#private";
        let locator = SecretUrlLocator::from_reopenable_url(raw_url).expect("valid locator");

        // Обычное форматирование не должно содержать ни одного secret fragment.
        let debug_text = format!("{locator:?}");
        let display_text = locator.to_string();
        for secret in [
            "alice", "password", "token", "secret", "private", "file.mp4",
        ] {
            assert!(!debug_text.contains(secret));
            assert!(!display_text.contains(secret));
        }

        // Intent-named boundaries сохраняют точную reopen/persistence identity.
        assert_eq!(locator.expose_secret_for_open(), raw_url);
        assert_eq!(locator.expose_secret_for_persistence(), raw_url);
    }

    #[test]
    fn foreign_bytes_and_wide_units_are_preserved_exactly() {
        // Byte и wide variants не проходят через String/PathBuf conversion.
        let byte_path = ForeignPlatformPath::new(
            ForeignPathPlatform::Linux,
            ForeignPathEncoding::Bytes(vec![0xff, b'/', b'a']),
        );
        let wide_path = ForeignPlatformPath::new(
            ForeignPathPlatform::Windows,
            ForeignPathEncoding::Wide(vec![0xd800, b'X' as u16]),
        );

        assert_eq!(
            byte_path.encoding_for_persistence(),
            &ForeignPathEncoding::Bytes(vec![0xff, b'/', b'a'])
        );
        assert_eq!(
            wide_path.encoding_for_persistence(),
            &ForeignPathEncoding::Wide(vec![0xd800, b'X' as u16])
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_non_utf_path_keeps_exact_os_bytes() {
        // Unix-only adapter создаёт invalid UTF-8 OsString для identity test.
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let original_bytes = vec![b'/', b'm', b'e', b'd', b'i', b'a', b'/', 0xff];
        let locator =
            LocalLocator::Native(PathBuf::from(OsString::from_vec(original_bytes.clone())));
        let restored_bytes = locator
            .expose_native_path_for_persistence()
            .expect("native path")
            .as_os_str()
            .as_bytes();

        assert_eq!(restored_bytes, original_bytes);
    }
}
