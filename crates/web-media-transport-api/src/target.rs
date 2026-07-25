//! Neutral request target: HTTP(S) либо progressive FTP(S).

use std::fmt;

use source_core::{FtpRequestTarget, FtpScheme, HttpRequestTarget, HttpScheme};

/// Exact admitted transport scheme capability для provider descriptor-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransportScheme {
    /// Progressive/adaptive HTTP(S).
    Http(HttpScheme),
    /// Progressive FTP(S).
    Ftp(FtpScheme),
}

/// Checked request target без смешивания HTTP и FTP policy semantics.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum TransportRequestTarget {
    /// Existing HTTP(S) target + origin/path policy evidence.
    Http(HttpRequestTarget),
    /// Progressive FTP(S) target + endpoint evidence.
    Ftp(FtpRequestTarget),
}

impl TransportRequestTarget {
    /// Оборачивает уже проверенный HTTP target.
    #[must_use]
    pub fn from_http(target: HttpRequestTarget) -> Self {
        Self::Http(target)
    }

    /// Оборачивает уже проверенный FTP target.
    #[must_use]
    pub fn from_ftp(target: FtpRequestTarget) -> Self {
        Self::Ftp(target)
    }

    /// Возвращает typed scheme capability без раскрытия locator-а.
    #[must_use]
    pub const fn scheme(&self) -> TransportScheme {
        match self {
            Self::Http(target) => TransportScheme::Http(target.scheme()),
            Self::Ftp(target) => TransportScheme::Ftp(target.scheme()),
        }
    }

    /// HTTP-only accessor для concrete HTTP providers и secret scope.
    #[must_use]
    pub const fn as_http(&self) -> Option<&HttpRequestTarget> {
        match self {
            Self::Http(target) => Some(target),
            Self::Ftp(_) => None,
        }
    }

    /// FTP-only accessor для concrete FTP providers.
    #[must_use]
    pub const fn as_ftp(&self) -> Option<&FtpRequestTarget> {
        match self {
            Self::Ftp(target) => Some(target),
            Self::Http(_) => None,
        }
    }
}

impl From<HttpRequestTarget> for TransportRequestTarget {
    fn from(target: HttpRequestTarget) -> Self {
        Self::Http(target)
    }
}

impl From<FtpRequestTarget> for TransportRequestTarget {
    fn from(target: FtpRequestTarget) -> Self {
        Self::Ftp(target)
    }
}

impl fmt::Debug for TransportRequestTarget {
    /// Nested targets уже secret-safe.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(target) => formatter.debug_tuple("Http").field(target).finish(),
            Self::Ftp(target) => formatter.debug_tuple("Ftp").field(target).finish(),
        }
    }
}

impl fmt::Display for TransportRequestTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(target) => target.fmt(formatter),
            Self::Ftp(target) => target.fmt(formatter),
        }
    }
}
