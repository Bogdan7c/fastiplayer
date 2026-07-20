//! Durable locator eligibility, service-owner preflight и relative representation.

use std::fmt;
use std::path::{Path, PathBuf};

use playlist_core::{
    DurableReopenLocator, LocalLocator, PlaylistCompoundGroup, PlaylistItem, PlaylistLocator,
    SecretUrlLocator, ServiceDurableReopenPayload,
};
use url::Url;

use super::PlaylistExportFormat;

/// Absolute path будущего export document-а; filesystem access не выполняется.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistExportDocumentTarget {
    document_path: PathBuf,
}

impl PlaylistExportDocumentTarget {
    /// Валидирует local absolute target, необходимый reversible relative policy.
    pub fn local_file(
        document_path: impl Into<PathBuf>,
    ) -> Result<Self, PlaylistExportDocumentTargetError> {
        let document_path = document_path.into();
        if !document_path.is_absolute() {
            return Err(PlaylistExportDocumentTargetError::TargetMustBeAbsolute);
        }
        if document_path.parent().is_none() {
            return Err(PlaylistExportDocumentTargetError::TargetHasNoParent);
        }
        Ok(Self { document_path })
    }

    /// Возвращает exact target path только pure path/URI formatter-у.
    pub fn document_path(&self) -> &Path {
        self.document_path.as_path()
    }
}

impl fmt::Debug for PlaylistExportDocumentTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaylistExportDocumentTarget(<redacted>)")
    }
}

/// Invalid target не содержит raw path в formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportDocumentTargetError {
    /// Relative save target не даёт authoritative base.
    TargetMustBeAbsolute,
    /// Target без parent directory нельзя использовать как document base.
    TargetHasNoParent,
}

impl fmt::Display for PlaylistExportDocumentTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetMustBeAbsolute => {
                formatter.write_str("playlist export target обязан быть absolute")
            }
            Self::TargetHasNoParent => {
                formatter.write_str("playlist export target не имеет parent directory")
            }
        }
    }
}

impl std::error::Error for PlaylistExportDocumentTargetError {}

/// Aggregated classification будущего document payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportSecretClassification {
    /// Ни один exported durable locator не требует acknowledgement.
    NoSensitiveLocators,
    /// Document содержит один или несколько sensitive durable locators.
    SensitiveDurableLocators {
        /// Exact count помогает composed confirmation без раскрытия identities.
        locator_count: usize,
    },
}

impl PlaylistExportSecretClassification {
    /// Строит aggregated outcome из checked locator count.
    pub(super) const fn from_sensitive_count(locator_count: usize) -> Self {
        if locator_count == 0 {
            Self::NoSensitiveLocators
        } else {
            Self::SensitiveDurableLocators { locator_count }
        }
    }
}

/// Service-owner результат: только portable absolute HTTP(S) URL + classification.
#[derive(Clone, PartialEq, Eq)]
pub struct PortablePlaylistExportUrl {
    exact_url: String,
    secret_classification: PortableUrlSecretClassification,
}

impl PortablePlaylistExportUrl {
    /// Валидирует portable hierarchical URL, сохраняя exact caller identity.
    pub fn new(
        exact_url: impl Into<String>,
        secret_classification: PortableUrlSecretClassification,
    ) -> Result<Self, PortablePlaylistExportUrlError> {
        let exact_url = exact_url.into();
        if exact_url.chars().any(|character| character.is_control()) {
            return Err(PortablePlaylistExportUrlError::InvalidPortableUrl);
        }
        let parsed = Url::parse(&exact_url)
            .map_err(|_| PortablePlaylistExportUrlError::InvalidPortableUrl)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || parsed.cannot_be_a_base()
        {
            return Err(PortablePlaylistExportUrlError::InvalidPortableUrl);
        }
        Ok(Self {
            exact_url,
            secret_classification,
        })
    }

    /// Exact URL раскрывается только serializer-у после owner preflight.
    pub(super) fn expose_for_export(&self) -> &str {
        &self.exact_url
    }

    /// Возвращает owner-provided secret classification.
    pub const fn secret_classification(&self) -> PortableUrlSecretClassification {
        self.secret_classification
    }
}

impl fmt::Debug for PortablePlaylistExportUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PortablePlaylistExportUrl")
            .field("url", &"<redacted>")
            .field("secret_classification", &self.secret_classification)
            .finish()
    }
}

/// Per-URL owner classification до aggregated export classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortableUrlSecretClassification {
    /// URL разрешён без durable-locator acknowledgement.
    Public,
    /// Exact URL требует explicit acknowledgement до записи.
    SensitiveDurableIdentity,
}

/// Constructor failure не отражает raw URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortablePlaylistExportUrlError {
    /// Value не является portable absolute hierarchical HTTP(S) URL.
    InvalidPortableUrl,
}

impl fmt::Display for PortablePlaylistExportUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("portable export URL отклонён")
    }
}

impl std::error::Error for PortablePlaylistExportUrlError {}

/// Service/app adapter обязан preflight-ить direct и opaque service identities.
pub trait PlaylistExportLocatorPolicy: Send + Sync {
    /// Классифицирует exact durable URL тем же service owner-ом, который его admitted.
    fn preflight_url(
        &self,
        locator: &SecretUrlLocator,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection>;

    /// Превращает stable service payload только в owner-approved portable URL.
    fn preflight_service(
        &self,
        locator: &ServiceDurableReopenPayload,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection>;
}

/// Safe service-owned rejection vocabulary без arbitrary error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportLocatorRejection {
    /// Admitted URL service больше не доступен для export policy.
    ServiceOwnerUnavailable,
    /// Durable identity не имеет portable URL representation.
    NonPortableIdentity,
    /// Owner отклонил identity по своей current export policy.
    OwnerPolicyRejected,
}

impl fmt::Display for PlaylistExportLocatorRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceOwnerUnavailable => {
                formatter.write_str("владелец URL service недоступен для export preflight")
            }
            Self::NonPortableIdentity => {
                formatter.write_str("durable service identity не имеет portable URL")
            }
            Self::OwnerPolicyRejected => {
                formatter.write_str("владелец service отклонил durable export identity")
            }
        }
    }
}

/// Format-neutral typed ineligibility без raw locator/path/URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportIneligible {
    /// Native local path не является UTF-8 и запрещён S10.
    NonUtf8LocalPath,
    /// Foreign platform path нельзя интерпретировать на текущей OS.
    ForeignPlatformPath,
    /// Relative operational locator не имеет authoritative reopen base.
    RelativeLocalPath,
    /// Native path нельзя представить reversible file URI.
    UnrepresentableNativePath,
    /// Service owner не вернул portable URL.
    LocatorPolicy(PlaylistExportLocatorRejection),
}

impl fmt::Display for PlaylistExportIneligible {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8LocalPath => {
                formatter.write_str("local path не представим как strict UTF-8")
            }
            Self::ForeignPlatformPath => {
                formatter.write_str("foreign platform path не представим в export document")
            }
            Self::RelativeLocalPath => {
                formatter.write_str("relative local locator не имеет reversible export base")
            }
            Self::UnrepresentableNativePath => {
                formatter.write_str("native path нельзя обратимо представить в export document")
            }
            Self::LocatorPolicy(reason) => reason.fmt(formatter),
        }
    }
}

/// Checked locator text, Debug которого никогда не раскрывает identity.
pub(super) struct PreparedExportLocator {
    serialized: String,
    sensitive: bool,
}

impl PreparedExportLocator {
    /// Serializer получает только already-preflighted representation.
    pub(super) fn as_str(&self) -> &str {
        &self.serialized
    }

    /// Aggregator считает sensitive locators без повторного parsing.
    pub(super) const fn is_sensitive(&self) -> bool {
        self.sensitive
    }
}

impl fmt::Debug for PreparedExportLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedExportLocator")
            .field("serialized", &"<redacted>")
            .field("sensitive", &self.sensitive)
            .finish()
    }
}

/// Выбирает durable item locator; legacy operational locator остаётся fallback.
pub(super) fn preflight_item_locator(
    item: &PlaylistItem,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
    policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedExportLocator, PlaylistExportIneligible> {
    if let Some(payload) = item.durable_payload() {
        return preflight_durable_locator(payload.reopen_locator(), format, target, policy);
    }
    preflight_operational_locator(item.locator(), format, target, policy)
}

/// XSPF group root следует durable group payload либо legacy provenance locator.
pub(super) fn preflight_group_locator(
    group: &PlaylistCompoundGroup,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
    policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedExportLocator, PlaylistExportIneligible> {
    if let Some(payload) = group.durable_payload() {
        return preflight_durable_locator(payload.reopen_locator(), format, target, policy);
    }
    preflight_operational_locator(group.provenance_locator(), format, target, policy)
}

/// Разбирает closed durable locator variants без доступа к transient transport state.
fn preflight_durable_locator(
    locator: &DurableReopenLocator,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
    policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedExportLocator, PlaylistExportIneligible> {
    if let Some(local_locator) = locator.expose_local_for_reopen() {
        return preflight_local_locator(local_locator, format, target);
    }
    if let Some(url_locator) = locator.expose_url_for_reopen() {
        return policy_url(url_locator, policy);
    }
    let service_locator = locator
        .expose_service_payload_for_reopen()
        .expect("DurableReopenLocator является closed non-empty enum");
    policy
        .preflight_service(service_locator)
        .map(prepared_policy_url)
        .map_err(PlaylistExportIneligible::LocatorPolicy)
}

/// Legacy queue locator не получает скрытого service payload guessing.
fn preflight_operational_locator(
    locator: &PlaylistLocator,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
    policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedExportLocator, PlaylistExportIneligible> {
    match locator {
        PlaylistLocator::Local(local_locator) => {
            preflight_local_locator(local_locator, format, target)
        }
        PlaylistLocator::Url(url_locator) => policy_url(url_locator, policy),
    }
}

/// Применяет owner policy к direct durable URL.
fn policy_url(
    locator: &SecretUrlLocator,
    policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedExportLocator, PlaylistExportIneligible> {
    policy
        .preflight_url(locator)
        .map(prepared_policy_url)
        .map_err(PlaylistExportIneligible::LocatorPolicy)
}

/// Сужает public portable URL до serializer-only checked locator.
fn prepared_policy_url(portable_url: PortablePlaylistExportUrl) -> PreparedExportLocator {
    let sensitive = portable_url.secret_classification()
        == PortableUrlSecretClassification::SensitiveDurableIdentity;
    PreparedExportLocator {
        serialized: portable_url.expose_for_export().to_owned(),
        sensitive,
    }
}

/// Создаёт format-specific local representation только из exact native UTF-8 path.
fn preflight_local_locator(
    locator: &LocalLocator,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
) -> Result<PreparedExportLocator, PlaylistExportIneligible> {
    let native_path = locator
        .expose_native_path_for_persistence()
        .ok_or(PlaylistExportIneligible::ForeignPlatformPath)?;
    if !native_path.is_absolute() {
        return Err(PlaylistExportIneligible::RelativeLocalPath);
    }
    native_path
        .to_str()
        .ok_or(PlaylistExportIneligible::NonUtf8LocalPath)?;
    let file_url = Url::from_file_path(native_path)
        .map_err(|()| PlaylistExportIneligible::UnrepresentableNativePath)?;

    let serialized = match format {
        PlaylistExportFormat::M3u8 => reversible_m3u8_relative(native_path, target)
            .unwrap_or_else(|| file_url.as_str().to_owned()),
        PlaylistExportFormat::Xspf => reversible_xspf_relative(&file_url, target)
            .unwrap_or_else(|| file_url.as_str().to_owned()),
    };
    Ok(PreparedExportLocator {
        serialized,
        sensitive: false,
    })
}

/// M3U8 parser lexical join должен восстановить exact original `PathBuf`.
fn reversible_m3u8_relative(
    native_path: &Path,
    target: &PlaylistExportDocumentTarget,
) -> Option<String> {
    let parent = target.document_path().parent()?;
    let relative_path = native_path.strip_prefix(parent).ok()?;
    if relative_path.as_os_str().is_empty() || parent.join(relative_path) != native_path {
        return None;
    }
    let relative_text = relative_path.to_str()?;
    if relative_text.starts_with('#')
        || relative_text.contains('\r')
        || relative_text.contains('\n')
        || looks_like_uri_scheme(relative_text)
    {
        return None;
    }
    Some(relative_text.to_owned())
}

/// Не позволяет native filename `scheme:value` превратиться в URL при re-import.
fn looks_like_uri_scheme(candidate: &str) -> bool {
    let Some((scheme, _)) = candidate.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    let Some(first_character) = characters.next() else {
        return false;
    };
    first_character.is_ascii_alphabetic()
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// XSPF использует URL inverse: `base.join(relative) == exact file URL`.
fn reversible_xspf_relative(
    file_url: &Url,
    target: &PlaylistExportDocumentTarget,
) -> Option<String> {
    let document_url = Url::from_file_path(target.document_path()).ok()?;
    let relative = document_url.make_relative(file_url)?;
    if relative.is_empty() || document_url.join(&relative).ok().as_ref() != Some(file_url) {
        return None;
    }
    Some(relative)
}
