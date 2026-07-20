//! XSPF URI export eligibility без queue snapshot и serializer policy.

use std::fmt;

use playlist_core::DurableReopenLocator;
use url::Url;

/// Canonical URI value, пригодное для XML escaping будущим S10 serializer-ом.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct XspfExportLocation {
    /// URL serialization уже содержит required percent encoding.
    serialized_uri: String,
}

impl XspfExportLocation {
    /// Преобразует durable locator без lossy path conversion.
    pub fn from_durable_locator(
        locator: &DurableReopenLocator,
    ) -> Result<Self, XspfExportIneligible> {
        // Native local path кодируется только стандартным reversible file-URL boundary.
        if let Some(local_locator) = locator.expose_local_for_reopen() {
            let native_path = local_locator
                .expose_native_path_for_persistence()
                .ok_or(XspfExportIneligible::ForeignPlatformPath)?;
            let file_uri = Url::from_file_path(native_path)
                .map_err(|()| XspfExportIneligible::UnrepresentableNativePath)?;
            return Ok(Self {
                serialized_uri: file_uri.into(),
            });
        }

        // URL identity раскрывается только внутри explicit export adapter-а.
        if let Some(secret_url) = locator.expose_url_for_reopen() {
            let parsed_uri = Url::parse(secret_url.expose_secret_for_persistence())
                .map_err(|_| XspfExportIneligible::MalformedDurableUrl)?;
            return Ok(Self {
                serialized_uri: parsed_uri.into(),
            });
        }

        // Service payload не угадывается: portable URL обязан выдать service owner в S10.
        Err(XspfExportIneligible::ServiceOwnerPreflightRequired)
    }

    /// Возвращает canonical percent-encoded URI будущему XML serializer-у.
    pub fn as_uri(&self) -> &str {
        &self.serialized_uri
    }
}

impl fmt::Debug for XspfExportLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("XspfExportLocation(<redacted>)")
    }
}

/// Secret-safe typed причина невозможности представить locator в XSPF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XspfExportIneligible {
    /// Native path нельзя обратимо представить file URI на текущей платформе.
    UnrepresentableNativePath,
    /// Foreign path нельзя молча трактовать как native path.
    ForeignPlatformPath,
    /// Stored URL нарушает absolute URL grammar и не сериализуется догадкой.
    MalformedDurableUrl,
    /// Stable service payload требует owner-approved portable URL mapping.
    ServiceOwnerPreflightRequired,
}

impl fmt::Display for XspfExportIneligible {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrepresentableNativePath => {
                formatter.write_str("native path нельзя обратимо представить в XSPF URI")
            }
            Self::ForeignPlatformPath => {
                formatter.write_str("foreign platform path не представим в XSPF URI")
            }
            Self::MalformedDurableUrl => {
                formatter.write_str("durable URL нельзя представить в XSPF")
            }
            Self::ServiceOwnerPreflightRequired => {
                formatter.write_str("service locator требует portable export preflight")
            }
        }
    }
}

impl std::error::Error for XspfExportIneligible {}
