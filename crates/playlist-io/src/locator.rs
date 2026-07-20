use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistMediaKind,
    PlaylistSingleImportDraft, SecretUrlLocator,
};
use url::Url;

use crate::{M3uDocumentSource, M3uExtInfHint};

/// Успешно разрешённый generic locator и safe fallback label.
pub(crate) struct ResolvedGenericLocator {
    /// Exact durable local/network identity.
    durable_locator: DurableReopenLocator,
    /// Safe display fallback без secret URL path/query.
    fallback_display_name: String,
}

/// Recoverable locator failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocatorResolutionError {
    /// URI syntax либо base resolution malformed.
    Malformed,
    /// Opaque/non-network scheme не является draft.
    UnsupportedScheme,
}

/// Разрешает generic locator без I/O и scheme admission.
pub(crate) fn resolve_generic_locator(
    raw_locator: &str,
    source: &M3uDocumentSource,
) -> Result<ResolvedGenericLocator, LocatorResolutionError> {
    if raw_locator.is_empty() {
        return Err(LocatorResolutionError::Malformed);
    }

    if looks_like_uri_reference(raw_locator) {
        return resolve_uri_locator(raw_locator, source);
    }

    match source {
        M3uDocumentSource::Local { path } => {
            let candidate_path = Path::new(raw_locator);
            let resolved_path = if candidate_path.is_absolute() {
                candidate_path.to_path_buf()
            } else {
                local_document_parent(path).join(candidate_path)
            };
            Ok(local_resolution(resolved_path))
        }
        M3uDocumentSource::Network { parsed_uri, .. } => {
            let resolved_uri = parsed_uri
                .join(raw_locator)
                .map_err(|_| LocatorResolutionError::Malformed)?;
            network_resolution(resolved_uri, None)
        }
    }
}

/// Строит playlist-core draft из разрешённого locator и metadata hint.
pub(crate) fn build_import_draft(
    resolved: ResolvedGenericLocator,
    source: &M3uDocumentSource,
    source_kind: PlaylistImportSourceKind,
    source_ordinal: std::num::NonZeroU32,
    extinf_hint: Option<&M3uExtInfHint>,
) -> Result<PlaylistSingleImportDraft, playlist_core::PlaylistPayloadBuildError> {
    let mut cached_metadata =
        CachedPlaylistMetadata::new(resolved.fallback_display_name, PlaylistMediaKind::Unknown);

    if let Some(extinf_hint) = extinf_hint {
        cached_metadata = cached_metadata
            .with_duration(match extinf_hint.duration() {
                crate::M3uDurationHint::Known(duration) => Some(duration),
                crate::M3uDurationHint::Unknown => None,
            })
            .with_title(extinf_hint.display_title().map(ToOwned::to_owned));
    }

    let provenance =
        PlaylistImportProvenance::new(source.durable_root(), source_kind, Some(source_ordinal));

    PlaylistSingleImportDraft::new(
        resolved.durable_locator,
        cached_metadata,
        None,
        Vec::new(),
        provenance,
        PlaylistImportAvailability::Available,
    )
}

/// Переводит non-negative finite seconds в neutral duration.
pub(crate) fn duration_from_seconds(seconds: f64) -> Option<MediaDuration> {
    Duration::try_from_secs_f64(seconds)
        .ok()
        .map(MediaDuration::from_duration)
}

/// Отличает URI scheme от native relative path.
fn looks_like_uri_reference(candidate: &str) -> bool {
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

/// Разрешает absolute/relative URI branch.
fn resolve_uri_locator(
    raw_locator: &str,
    source: &M3uDocumentSource,
) -> Result<ResolvedGenericLocator, LocatorResolutionError> {
    let parsed_uri = match Url::parse(raw_locator) {
        Ok(parsed_uri) => parsed_uri,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            let base_uri = source
                .parsed_network_uri()
                .ok_or(LocatorResolutionError::Malformed)?;
            base_uri
                .join(raw_locator)
                .map_err(|_| LocatorResolutionError::Malformed)?
        }
        Err(_) => return Err(LocatorResolutionError::Malformed),
    };

    if parsed_uri.scheme() == "file" {
        return resolve_file_uri(parsed_uri);
    }

    network_resolution(parsed_uri, Some(raw_locator.to_owned()))
}

/// Принимает только hierarchical URI с authority; actual scheme admission остаётся app-owned.
fn network_resolution(
    parsed_uri: Url,
    exact_input_uri: Option<String>,
) -> Result<ResolvedGenericLocator, LocatorResolutionError> {
    if parsed_uri.cannot_be_a_base() || parsed_uri.host_str().is_none() {
        return Err(LocatorResolutionError::UnsupportedScheme);
    }

    let fallback_display_name = parsed_uri.host_str().unwrap_or("network media").to_owned();
    let reopenable_uri = exact_input_uri.unwrap_or_else(|| parsed_uri.into());
    let secret_locator = SecretUrlLocator::from_reopenable_url(reopenable_uri)
        .expect("parsed absolute URI is non-empty");
    Ok(ResolvedGenericLocator {
        durable_locator: DurableReopenLocator::url(secret_locator),
        fallback_display_name,
    })
}

/// Декодирует file URI только с empty/localhost authority и без query/fragment.
fn resolve_file_uri(mut parsed_uri: Url) -> Result<ResolvedGenericLocator, LocatorResolutionError> {
    if parsed_uri.query().is_some() || parsed_uri.fragment().is_some() {
        return Err(LocatorResolutionError::Malformed);
    }

    match parsed_uri.host_str() {
        None | Some("") => {}
        Some(host) if host.eq_ignore_ascii_case("localhost") => {
            parsed_uri
                .set_host(None)
                .map_err(|_| LocatorResolutionError::Malformed)?;
        }
        Some(_) => return Err(LocatorResolutionError::UnsupportedScheme),
    }

    let native_path = parsed_uri
        .to_file_path()
        .map_err(|()| LocatorResolutionError::Malformed)?;
    Ok(local_resolution(native_path))
}

/// Создаёт local resolution без lossy filename conversion.
fn local_resolution(native_path: PathBuf) -> ResolvedGenericLocator {
    ResolvedGenericLocator {
        durable_locator: DurableReopenLocator::local(LocalLocator::Native(native_path)),
        fallback_display_name: "Локальный медиафайл".to_owned(),
    }
}

/// Возвращает lexical parent local document-а.
fn local_document_parent(document_path: &Path) -> &Path {
    document_path.parent().unwrap_or_else(|| Path::new(""))
}
