//! Нейтральные payload-контракты импорта, не зависящие от parser/service/I/O.

use std::fmt;
use std::num::NonZeroU32;

use crate::{LocalLocator, SecretUrlLocator};

/// Текущая версия opaque service payload для повторного открытия.
pub const CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION: u16 = 1;
/// Максимальная длина нейтрального service owner identity в UTF-8 bytes.
pub const MAX_DURABLE_REOPEN_SERVICE_OWNER_BYTES: usize = 64;
/// Максимальный размер opaque versioned service payload.
pub const MAX_DURABLE_REOPEN_SERVICE_PAYLOAD_BYTES: usize = 8 * 1024;
/// Максимальное число ancillary track hints у одного import item.
pub const MAX_PLAYLIST_ANCILLARY_TRACK_HINTS: usize = 32;
/// Максимальная длина stable ancillary identity.
pub const MAX_PLAYLIST_ANCILLARY_IDENTITY_BYTES: usize = 256;
/// Максимальная длина нормализованного language hint.
pub const MAX_PLAYLIST_ANCILLARY_LANGUAGE_BYTES: usize = 64;
/// Максимальная длина display name ancillary track.
pub const MAX_PLAYLIST_ANCILLARY_DISPLAY_NAME_BYTES: usize = 512;
/// Максимальная длина service-owned format identity.
pub const MAX_PLAYLIST_ANCILLARY_FORMAT_IDENTITY_BYTES: usize = 256;

/// Поддержанная версия service-owned durable reopen payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DurableReopenPayloadVersion(u16);

impl DurableReopenPayloadVersion {
    /// Текущая версия payload contract.
    pub const CURRENT: Self = Self(CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION);

    /// Валидирует persisted/service version до принятия opaque payload.
    pub fn from_version_value(version: u16) -> Result<Self, DurableReopenLocatorBuildError> {
        if version != CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION {
            return Err(DurableReopenLocatorBuildError::UnknownPayloadVersion {
                provided: version,
                supported: CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
            });
        }

        Ok(Self(version))
    }

    /// Возвращает version number для будущего persistence DTO.
    pub const fn expose_value_for_persistence(self) -> u16 {
        self.0
    }
}

/// Семантический источник service material до durable admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceReopenMaterialKind {
    /// Stable webpage URL/identity extracted child-а.
    StableWebpageIdentity,
    /// Stable original source URL/identity extracted child-а.
    StableOriginalIdentity,
    /// Stable extractor-owned identity, достаточная для повторного resolve.
    StableExtractorIdentity,
    /// Transient `formats[].url`.
    FormatUrl,
    /// Transient manifest URL.
    ManifestUrl,
    /// Transient fragment URL.
    FragmentUrl,
    /// Transient encryption key URL.
    KeyUrl,
    /// Signed media endpoint с ограниченным lifetime.
    SignedEndpoint,
    /// Request headers.
    Headers,
    /// Cookie material.
    Cookies,
    /// Authorization либо session material.
    AuthorizationOrSession,
}

impl ServiceReopenMaterialKind {
    /// Доказывает, что material category разрешена durable contract-ом.
    const fn is_stable_identity(self) -> bool {
        matches!(
            self,
            Self::StableWebpageIdentity
                | Self::StableOriginalIdentity
                | Self::StableExtractorIdentity
        )
    }
}

/// Exact durable identity, достаточная для будущего reopen/resolve.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum DurableReopenLocator {
    /// Exact local path identity без lossy UTF-8 conversion.
    Local(LocalLocator),
    /// Exact acknowledged URL identity с redacted formatting.
    Url(SecretUrlLocator),
    /// Bounded versioned service-owned stable child identity.
    ServicePayload(ServiceDurableReopenPayload),
}

impl DurableReopenLocator {
    /// Сохраняет exact native/foreign local identity.
    pub fn local(locator: LocalLocator) -> Self {
        Self::Local(locator)
    }

    /// Сохраняет exact acknowledged URL identity.
    pub fn url(locator: SecretUrlLocator) -> Self {
        Self::Url(locator)
    }

    /// Принимает только явно классифицированную stable service identity.
    pub fn from_service_payload(
        service_owner: impl Into<String>,
        payload_version: u16,
        material_kind: ServiceReopenMaterialKind,
        payload: impl Into<Vec<u8>>,
    ) -> Result<Self, DurableReopenLocatorBuildError> {
        if !material_kind.is_stable_identity() {
            return Err(DurableReopenLocatorBuildError::EphemeralTransportMaterial {
                material_kind,
            });
        }

        let service_owner = service_owner.into();
        validate_service_owner(&service_owner)?;
        let payload_version = DurableReopenPayloadVersion::from_version_value(payload_version)?;
        let payload = payload.into();

        if payload.is_empty() {
            return Err(DurableReopenLocatorBuildError::EmptyServicePayload);
        }
        if payload.len() > MAX_DURABLE_REOPEN_SERVICE_PAYLOAD_BYTES {
            return Err(
                DurableReopenLocatorBuildError::ServicePayloadLimitExceeded {
                    provided_bytes: payload.len(),
                    maximum_bytes: MAX_DURABLE_REOPEN_SERVICE_PAYLOAD_BYTES,
                },
            );
        }

        Ok(Self::ServicePayload(ServiceDurableReopenPayload {
            service_owner,
            payload_version,
            material_kind,
            payload: payload.into_boxed_slice(),
        }))
    }

    /// Возвращает local locator только для explicit open/persistence mapping.
    pub const fn expose_local_for_reopen(&self) -> Option<&LocalLocator> {
        match self {
            Self::Local(locator) => Some(locator),
            Self::Url(_) | Self::ServicePayload(_) => None,
        }
    }

    /// Возвращает secret URL только для explicit open/persistence mapping.
    pub const fn expose_url_for_reopen(&self) -> Option<&SecretUrlLocator> {
        match self {
            Self::Url(locator) => Some(locator),
            Self::Local(_) | Self::ServicePayload(_) => None,
        }
    }

    /// Возвращает service payload только для owner-approved reopen/persistence mapping.
    pub const fn expose_service_payload_for_reopen(&self) -> Option<&ServiceDurableReopenPayload> {
        match self {
            Self::ServicePayload(payload) => Some(payload),
            Self::Local(_) | Self::Url(_) => None,
        }
    }
}

impl fmt::Debug for DurableReopenLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(_) => formatter.write_str("DurableReopenLocator::Local(<redacted-path>)"),
            Self::Url(locator) => formatter
                .debug_tuple("DurableReopenLocator::Url")
                .field(locator)
                .finish(),
            Self::ServicePayload(payload) => formatter
                .debug_struct("DurableReopenLocator::ServicePayload")
                .field("service_owner", &payload.service_owner)
                .field("payload_version", &payload.payload_version)
                .field("material_kind", &payload.material_kind)
                .field("payload", &"<redacted-service-payload>")
                .finish(),
        }
    }
}

impl fmt::Display for DurableReopenLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(_) => formatter.write_str("<local-media>"),
            Self::Url(locator) => fmt::Display::fmt(locator, formatter),
            Self::ServicePayload(payload) => {
                write!(formatter, "<service-media:{}>", payload.service_owner)
            }
        }
    }
}

/// Bounded opaque service payload, доступный только через intent-named accessor.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ServiceDurableReopenPayload {
    service_owner: String,
    payload_version: DurableReopenPayloadVersion,
    material_kind: ServiceReopenMaterialKind,
    payload: Box<[u8]>,
}

impl ServiceDurableReopenPayload {
    /// Возвращает validated нейтральный service owner discriminator.
    pub fn service_owner(&self) -> &str {
        &self.service_owner
    }

    /// Возвращает поддержанную payload version.
    pub const fn payload_version(&self) -> DurableReopenPayloadVersion {
        self.payload_version
    }

    /// Возвращает stable service identity category.
    pub const fn material_kind(&self) -> ServiceReopenMaterialKind {
        self.material_kind
    }

    /// Раскрывает opaque bytes только владельцу reopen/persistence adapter-а.
    pub fn expose_payload_for_reopen(&self) -> &[u8] {
        &self.payload
    }
}

impl fmt::Debug for ServiceDurableReopenPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceDurableReopenPayload")
            .field("service_owner", &self.service_owner)
            .field("payload_version", &self.payload_version)
            .field("material_kind", &self.material_kind)
            .field("payload", &"<redacted-service-payload>")
            .finish()
    }
}

/// Ошибка durable locator admission без raw locator/payload в formatting.
#[derive(Clone, PartialEq, Eq)]
pub enum DurableReopenLocatorBuildError {
    /// Service owner identity пуста.
    EmptyServiceOwner,
    /// Service owner содержит символы вне bounded neutral grammar.
    InvalidServiceOwner,
    /// Service owner превышает named byte bound.
    ServiceOwnerLimitExceeded {
        /// Фактический размер UTF-8.
        provided_bytes: usize,
        /// Максимально разрешённый размер.
        maximum_bytes: usize,
    },
    /// Payload version неизвестна текущему domain contract.
    UnknownPayloadVersion {
        /// Полученная версия.
        provided: u16,
        /// Единственная поддержанная версия.
        supported: u16,
    },
    /// Stable service payload пуст.
    EmptyServicePayload,
    /// Stable service payload превышает named byte bound.
    ServicePayloadLimitExceeded {
        /// Фактический размер payload.
        provided_bytes: usize,
        /// Максимально разрешённый размер.
        maximum_bytes: usize,
    },
    /// Material относится к ephemeral transport/request state.
    EphemeralTransportMaterial {
        /// Безопасная категория отвергнутого материала.
        material_kind: ServiceReopenMaterialKind,
    },
}

impl fmt::Debug for DurableReopenLocatorBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for DurableReopenLocatorBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyServiceOwner => formatter.write_str("service owner must not be empty"),
            Self::InvalidServiceOwner => {
                formatter.write_str("service owner contains unsupported characters")
            }
            Self::ServiceOwnerLimitExceeded {
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "service owner is {provided_bytes} bytes; maximum is {maximum_bytes}"
            ),
            Self::UnknownPayloadVersion {
                provided,
                supported,
            } => write!(
                formatter,
                "service reopen payload version {provided} is unsupported; supported version is {supported}"
            ),
            Self::EmptyServicePayload => {
                formatter.write_str("service reopen payload must not be empty")
            }
            Self::ServicePayloadLimitExceeded {
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "service reopen payload is {provided_bytes} bytes; maximum is {maximum_bytes}"
            ),
            Self::EphemeralTransportMaterial { material_kind } => write!(
                formatter,
                "ephemeral transport material {material_kind:?} cannot become a durable reopen locator"
            ),
        }
    }
}

impl std::error::Error for DurableReopenLocatorBuildError {}

/// Способ появления ancillary track в source descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistAncillaryTrackSelectionKind {
    /// Track явно опубликован как пользовательский/manual.
    Manual,
    /// Track сгенерирован либо выбран автоматически.
    Automatic,
}

/// Где находится ancillary track без transport-specific request state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaylistAncillaryTrackOrigin {
    /// Track встроен в основной media source.
    Embedded,
    /// Track имеет отдельную durable reopen identity.
    External(DurableReopenLocator),
}

/// Bounded subtitle/ancillary groundwork без playback behavior.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistAncillaryTrackHint {
    semantic_identity: String,
    language: Option<String>,
    display_name: Option<String>,
    selection_kind: PlaylistAncillaryTrackSelectionKind,
    origin: PlaylistAncillaryTrackOrigin,
    service_format_identity: Option<String>,
}

impl PlaylistAncillaryTrackHint {
    /// Создаёт hint и валидирует все caller-controlled text bounds.
    pub fn new(
        semantic_identity: impl Into<String>,
        language: Option<String>,
        display_name: Option<String>,
        selection_kind: PlaylistAncillaryTrackSelectionKind,
        origin: PlaylistAncillaryTrackOrigin,
        service_format_identity: Option<String>,
    ) -> Result<Self, PlaylistPayloadBuildError> {
        let semantic_identity = semantic_identity.into();
        validate_required_text(
            &semantic_identity,
            PlaylistPayloadTextField::AncillarySemanticIdentity,
            MAX_PLAYLIST_ANCILLARY_IDENTITY_BYTES,
        )?;
        validate_optional_text(
            language.as_deref(),
            PlaylistPayloadTextField::AncillaryLanguage,
            MAX_PLAYLIST_ANCILLARY_LANGUAGE_BYTES,
        )?;
        validate_optional_text(
            display_name.as_deref(),
            PlaylistPayloadTextField::AncillaryDisplayName,
            MAX_PLAYLIST_ANCILLARY_DISPLAY_NAME_BYTES,
        )?;
        validate_optional_text(
            service_format_identity.as_deref(),
            PlaylistPayloadTextField::AncillaryServiceFormatIdentity,
            MAX_PLAYLIST_ANCILLARY_FORMAT_IDENTITY_BYTES,
        )?;

        Ok(Self {
            semantic_identity,
            language,
            display_name,
            selection_kind,
            origin,
            service_format_identity,
        })
    }

    /// Возвращает stable semantic identity.
    pub fn semantic_identity(&self) -> &str {
        &self.semantic_identity
    }

    /// Возвращает optional normalized language.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Возвращает optional display name.
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// Возвращает manual/automatic classification.
    pub const fn selection_kind(&self) -> PlaylistAncillaryTrackSelectionKind {
        self.selection_kind
    }

    /// Возвращает embedded/external durable origin.
    pub const fn origin(&self) -> &PlaylistAncillaryTrackOrigin {
        &self.origin
    }

    /// Возвращает optional bounded service format identity.
    pub fn service_format_identity(&self) -> Option<&str> {
        self.service_format_identity.as_deref()
    }
}

impl fmt::Debug for PlaylistAncillaryTrackHint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistAncillaryTrackHint")
            .field("semantic_identity", &"<redacted-identity>")
            .field("language", &self.language)
            .field("display_name", &self.display_name)
            .field("selection_kind", &self.selection_kind)
            .field("origin", &self.origin)
            .field(
                "service_format_identity",
                &self
                    .service_format_identity
                    .as_ref()
                    .map(|_| "<redacted-identity>"),
            )
            .finish()
    }
}

/// Нейтральный формат/import owner, создавший draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistImportSourceKind {
    /// Generic M3U document.
    M3u,
    /// UTF-8 M3U8 document.
    M3u8,
    /// XSPF v1 document.
    Xspf,
    /// Поддержанный CUE AUDIO subset.
    Cue,
    /// Versioned service topology payload.
    Service,
}

/// Durable provenance одного imported single либо compound root.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistImportProvenance {
    root_locator: DurableReopenLocator,
    source_kind: PlaylistImportSourceKind,
    source_ordinal: Option<NonZeroU32>,
}

impl PlaylistImportProvenance {
    /// Создаёт provenance без parser/service dependency.
    pub fn new(
        root_locator: DurableReopenLocator,
        source_kind: PlaylistImportSourceKind,
        source_ordinal: Option<NonZeroU32>,
    ) -> Self {
        Self {
            root_locator,
            source_kind,
            source_ordinal,
        }
    }

    /// Возвращает durable root document/service identity.
    pub const fn root_locator(&self) -> &DurableReopenLocator {
        &self.root_locator
    }

    /// Возвращает нейтральный import owner.
    pub const fn source_kind(&self) -> PlaylistImportSourceKind {
        self.source_kind
    }

    /// Возвращает one-based ordinal, если source format его доказал.
    pub const fn source_ordinal(&self) -> Option<NonZeroU32> {
        self.source_ordinal
    }
}

impl fmt::Debug for PlaylistImportProvenance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistImportProvenance")
            .field("root_locator", &self.root_locator)
            .field("source_kind", &self.source_kind)
            .field("source_ordinal", &self.source_ordinal)
            .finish()
    }
}

/// Caller-controlled text field для typed bounded diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistPayloadTextField {
    /// Stable ancillary semantic identity.
    AncillarySemanticIdentity,
    /// Ancillary language.
    AncillaryLanguage,
    /// Ancillary display name.
    AncillaryDisplayName,
    /// Service format identity.
    AncillaryServiceFormatIdentity,
}

/// Ошибка bounded neutral payload без echo исходного текста.
#[derive(Clone, PartialEq, Eq)]
pub enum PlaylistPayloadBuildError {
    /// Обязательное поле пусто.
    EmptyText {
        /// Безопасное имя поля.
        field: PlaylistPayloadTextField,
    },
    /// Поле превышает named UTF-8 byte bound.
    TextLimitExceeded {
        /// Безопасное имя поля.
        field: PlaylistPayloadTextField,
        /// Фактическая длина.
        provided_bytes: usize,
        /// Максимальная длина.
        maximum_bytes: usize,
    },
    /// Ancillary list превышает item-level bound.
    AncillaryTrackLimitExceeded {
        /// Фактическое число hints.
        provided: usize,
        /// Максимальное число hints.
        maximum: usize,
    },
}

impl fmt::Debug for PlaylistPayloadBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for PlaylistPayloadBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText { field } => write!(formatter, "{field:?} must not be empty"),
            Self::TextLimitExceeded {
                field,
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "{field:?} is {provided_bytes} bytes; maximum is {maximum_bytes}"
            ),
            Self::AncillaryTrackLimitExceeded { provided, maximum } => write!(
                formatter,
                "ancillary track count is {provided}; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for PlaylistPayloadBuildError {}

/// Валидирует item-level ancillary bound без silent truncation.
pub(crate) fn validate_ancillary_track_count(
    provided: usize,
) -> Result<(), PlaylistPayloadBuildError> {
    if provided > MAX_PLAYLIST_ANCILLARY_TRACK_HINTS {
        return Err(PlaylistPayloadBuildError::AncillaryTrackLimitExceeded {
            provided,
            maximum: MAX_PLAYLIST_ANCILLARY_TRACK_HINTS,
        });
    }

    Ok(())
}

fn validate_service_owner(owner: &str) -> Result<(), DurableReopenLocatorBuildError> {
    if owner.is_empty() {
        return Err(DurableReopenLocatorBuildError::EmptyServiceOwner);
    }
    if owner.len() > MAX_DURABLE_REOPEN_SERVICE_OWNER_BYTES {
        return Err(DurableReopenLocatorBuildError::ServiceOwnerLimitExceeded {
            provided_bytes: owner.len(),
            maximum_bytes: MAX_DURABLE_REOPEN_SERVICE_OWNER_BYTES,
        });
    }
    if !owner
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte))
    {
        return Err(DurableReopenLocatorBuildError::InvalidServiceOwner);
    }

    Ok(())
}

fn validate_required_text(
    text: &str,
    field: PlaylistPayloadTextField,
    maximum_bytes: usize,
) -> Result<(), PlaylistPayloadBuildError> {
    if text.is_empty() {
        return Err(PlaylistPayloadBuildError::EmptyText { field });
    }

    validate_text_bound(text, field, maximum_bytes)
}

fn validate_optional_text(
    text: Option<&str>,
    field: PlaylistPayloadTextField,
    maximum_bytes: usize,
) -> Result<(), PlaylistPayloadBuildError> {
    if let Some(text) = text {
        validate_required_text(text, field, maximum_bytes)?;
    }

    Ok(())
}

fn validate_text_bound(
    text: &str,
    field: PlaylistPayloadTextField,
    maximum_bytes: usize,
) -> Result<(), PlaylistPayloadBuildError> {
    if text.len() > maximum_bytes {
        return Err(PlaylistPayloadBuildError::TextLimitExceeded {
            field,
            provided_bytes: text.len(),
            maximum_bytes,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests;
