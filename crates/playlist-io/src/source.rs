use std::{fmt, path::PathBuf};

use playlist_core::{DurableReopenLocator, LocalLocator, SecretUrlLocator};
use url::Url;

/// Locator playlist-документа и общий authoritative base-resolution owner.
#[derive(Clone, PartialEq, Eq)]
pub enum PlaylistDocumentSource {
    /// Локальный exact native path.
    Local {
        /// Путь manifest/import document.
        path: PathBuf,
    },
    /// Exact secret-safe absolute hierarchical URI.
    Network {
        /// Raw identity сохраняется только для explicit reopen.
        exact_uri: String,
        /// Parsed base не публикуется и используется только для resolution.
        parsed_uri: Url,
    },
}

impl PlaylistDocumentSource {
    /// Создаёт local source без filesystem access.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local { path: path.into() }
    }

    /// Валидирует absolute hierarchical network base без fetch.
    pub fn network(exact_uri: impl Into<String>) -> Result<Self, PlaylistDocumentSourceError> {
        let exact_uri = exact_uri.into();
        let parsed_uri =
            Url::parse(&exact_uri).map_err(|_| PlaylistDocumentSourceError::InvalidNetworkUri)?;

        if parsed_uri.cannot_be_a_base() || parsed_uri.host_str().is_none() {
            return Err(PlaylistDocumentSourceError::InvalidNetworkUri);
        }
        if parsed_uri.scheme() == "file" {
            return Err(PlaylistDocumentSourceError::FileUriIsNotNetworkSource);
        }

        Ok(Self::Network {
            exact_uri,
            parsed_uri,
        })
    }

    /// Сообщает, получен ли playlist document по network URI.
    pub const fn is_network(&self) -> bool {
        matches!(self, Self::Network { .. })
    }

    /// Возвращает local path только explicit local-open/import owner-у.
    pub const fn expose_local_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Local { path } => Some(path),
            Self::Network { .. } => None,
        }
    }

    /// Возвращает exact network identity только explicit reopen owner-у.
    pub fn expose_network_uri(&self) -> Option<&str> {
        match self {
            Self::Network { exact_uri, .. } => Some(exact_uri),
            Self::Local { .. } => None,
        }
    }

    /// Возвращает parsed network base только внутри crate.
    pub(crate) const fn parsed_network_uri(&self) -> Option<&Url> {
        match self {
            Self::Network { parsed_uri, .. } => Some(parsed_uri),
            Self::Local { .. } => None,
        }
    }

    /// Строит durable root provenance без lossy conversion.
    pub(crate) fn durable_root(&self) -> DurableReopenLocator {
        match self {
            Self::Local { path } => DurableReopenLocator::local(LocalLocator::Native(path.clone())),
            Self::Network { exact_uri, .. } => {
                let secret_url = SecretUrlLocator::from_reopenable_url(exact_uri.clone())
                    .expect("validated non-empty network source");
                DurableReopenLocator::url(secret_url)
            }
        }
    }
}

impl fmt::Debug for PlaylistDocumentSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { .. } => formatter.write_str("PlaylistDocumentSource::Local(<redacted>)"),
            Self::Network { parsed_uri, .. } => formatter
                .debug_struct("PlaylistDocumentSource::Network")
                .field("host", &parsed_uri.host_str().unwrap_or("<unknown>"))
                .finish_non_exhaustive(),
        }
    }
}

/// Secret-safe ошибка source construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistDocumentSourceError {
    /// URI malformed, opaque или не имеет authority host.
    InvalidNetworkUri,
    /// `file:` должен входить через local source boundary.
    FileUriIsNotNetworkSource,
}

impl fmt::Display for PlaylistDocumentSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNetworkUri => {
                formatter.write_str("playlist source не является absolute network URI")
            }
            Self::FileUriIsNotNetworkSource => {
                formatter.write_str("file URI не является network playlist source")
            }
        }
    }
}

impl std::error::Error for PlaylistDocumentSourceError {}

/// Backward-compatible имя source boundary для M3U callers.
pub type M3uDocumentSource = PlaylistDocumentSource;

/// Backward-compatible имя ошибки M3U source construction.
pub type M3uDocumentSourceError = PlaylistDocumentSourceError;

/// Format-specific имя того же source boundary для XSPF callers.
pub type XspfDocumentSource = PlaylistDocumentSource;

/// Format-specific имя ошибки XSPF source construction.
pub type XspfDocumentSourceError = PlaylistDocumentSourceError;
