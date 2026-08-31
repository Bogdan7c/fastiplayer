use std::fmt;
use std::sync::Arc;

use web_media_transport_api::TransportRequestTarget;

use super::DirectMediaExtension;

/// Проверенный direct-media locator с exact reopen/persistence identity.
#[derive(Clone, PartialEq, Eq)]
pub struct DirectMediaUrl {
    inner: Arc<DirectMediaUrlInner>,
}

#[derive(PartialEq, Eq)]
struct DirectMediaUrlInner {
    request_target: TransportRequestTarget,
    extension: DirectMediaExtension,
    safe_label: String,
    requires_sensitive_persistence_acknowledgement: bool,
}

impl DirectMediaUrl {
    pub(crate) fn new(
        request_target: TransportRequestTarget,
        extension: DirectMediaExtension,
        safe_label: String,
        requires_sensitive_persistence_acknowledgement: bool,
    ) -> Self {
        Self {
            inner: Arc::new(DirectMediaUrlInner {
                request_target,
                extension,
                safe_label,
                requires_sensitive_persistence_acknowledgement,
            }),
        }
    }

    /// Раскрывает exact identity только для реального media open.
    #[must_use]
    pub fn expose_secret_for_open(&self) -> &str {
        match &self.inner.request_target {
            TransportRequestTarget::Http(target) => target.expose_secret_for_request(),
            TransportRequestTarget::Ftp(target) => target.expose_secret_for_request(),
        }
    }

    /// Раскрывает exact identity только для persistence snapshot-а.
    #[must_use]
    pub fn expose_secret_for_persistence(&self) -> &str {
        self.expose_secret_for_open()
    }

    /// Возвращает checked physical target только app-owned transport runtime-у.
    #[must_use]
    pub fn request_target_for_open(&self) -> TransportRequestTarget {
        self.inner.request_target.clone()
    }

    /// Возвращает container extension, доказанный URL path-ом.
    #[must_use]
    pub fn extension(&self) -> &DirectMediaExtension {
        &self.inner.extension
    }

    /// Возвращает безопасный label без userinfo/path/query/fragment.
    #[must_use]
    pub fn safe_label(&self) -> &str {
        &self.inner.safe_label
    }

    /// Сообщает app policy, что exact reopenable identity содержит userinfo/query.
    ///
    /// Проверка выполняется владельцем parsed URL и не требует повторного раскрытия
    /// secret identity на app boundary.
    #[must_use]
    pub fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.inner.requires_sensitive_persistence_acknowledgement
    }
}

impl fmt::Debug for DirectMediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectMediaUrl")
            .field("safe_label", &self.inner.safe_label)
            .field("extension", &self.inner.extension)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DirectMediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.inner.safe_label)
    }
}
