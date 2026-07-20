//! Pure snapshot, locator preflight и serialization playlist export.
//!
//! Этот модуль намеренно не выполняет file I/O: S11 передаст готовые UTF-8 bytes
//! в общий atomic writer только после пользовательского подтверждения.

mod locator;
mod m3u8;
mod snapshot;
mod xspf;

use std::fmt;

use playlist_core::{CachedPlaylistMetadata, PlaylistEntry, PlaylistItem};

pub use locator::{
    PlaylistExportDocumentTarget, PlaylistExportDocumentTargetError, PlaylistExportIneligible,
    PlaylistExportLocatorPolicy, PlaylistExportLocatorRejection,
    PlaylistExportSecretClassification, PortablePlaylistExportUrl, PortablePlaylistExportUrlError,
    PortableUrlSecretClassification,
};
pub use snapshot::{PlaylistExportScope, PlaylistExportSnapshot, PlaylistExportSnapshotError};

use locator::{PreparedExportLocator, preflight_group_locator, preflight_item_locator};

/// Поддержанные pure serializer-ы S10.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportFormat {
    /// UTF-8 Extended M3U8 с обязательным `#EXTM3U`.
    M3u8,
    /// Namespace-aware XSPF version 1.
    Xspf,
}

/// Нефатальное свойство результата, которое caller обязан показать до записи.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportWarning {
    /// M3U8 сохраняет parts, но не может сохранить compound boundary.
    CompoundGroupingFlattened {
        /// Число flattened top-level compound groups.
        compound_group_count: usize,
    },
}

/// Полностью preflighted export plan без queue handle и transport material.
pub struct PreparedPlaylistExport {
    format: PlaylistExportFormat,
    tracks: Box<[PreparedExportTrack]>,
    groups: Box<[PreparedExportGroup]>,
    warnings: Box<[PlaylistExportWarning]>,
    secret_classification: PlaylistExportSecretClassification,
}

impl PreparedPlaylistExport {
    /// Возвращает выбранный document format.
    pub const fn format(&self) -> PlaylistExportFormat {
        self.format
    }

    /// Возвращает canonical flattened track count.
    pub const fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Возвращает format-specific warnings без их сокрытия внутри serializer-а.
    pub fn warnings(&self) -> &[PlaylistExportWarning] {
        &self.warnings
    }

    /// Возвращает aggregated secret classification для S11 confirmation/file mode.
    pub const fn secret_classification(&self) -> PlaylistExportSecretClassification {
        self.secret_classification
    }

    /// Сериализует уже проверенный plan в UTF-8 bytes без I/O.
    pub fn serialize(&self) -> SerializedPlaylistExport {
        let utf8_document = match self.format {
            PlaylistExportFormat::M3u8 => m3u8::serialize(self),
            PlaylistExportFormat::Xspf => xspf::serialize(self),
        };
        SerializedPlaylistExport {
            format: self.format,
            utf8_document,
        }
    }
}

impl fmt::Debug for PreparedPlaylistExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPlaylistExport")
            .field("format", &self.format)
            .field("track_count", &self.tracks.len())
            .field("group_count", &self.groups.len())
            .field("warnings", &self.warnings)
            .field("secret_classification", &self.secret_classification)
            .finish()
    }
}

/// Owned UTF-8 export payload, всё ещё не записанный на filesystem.
pub struct SerializedPlaylistExport {
    format: PlaylistExportFormat,
    utf8_document: String,
}

impl SerializedPlaylistExport {
    /// Возвращает document format для выбора extension/MIME adapter-ом.
    pub const fn format(&self) -> PlaylistExportFormat {
        self.format
    }

    /// Возвращает complete UTF-8 document bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.utf8_document.as_bytes()
    }

    /// Возвращает complete UTF-8 document text для pure roundtrip tests.
    pub fn as_str(&self) -> &str {
        &self.utf8_document
    }

    /// Передаёт owned bytes будущему writer job без дополнительного clone.
    pub fn into_bytes(self) -> Vec<u8> {
        self.utf8_document.into_bytes()
    }
}

impl fmt::Debug for SerializedPlaylistExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerializedPlaylistExport")
            .field("format", &self.format)
            .field("byte_count", &self.utf8_document.len())
            .finish()
    }
}

/// Secret-safe ошибка одного exact export subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaylistExportPreflightError {
    subject: PlaylistExportSubject,
    reason: PlaylistExportIneligible,
}

impl PlaylistExportPreflightError {
    /// Возвращает stable queue identity без locator payload.
    pub const fn subject(self) -> PlaylistExportSubject {
        self.subject
    }

    /// Возвращает typed safe reason.
    pub const fn reason(self) -> PlaylistExportIneligible {
        self.reason
    }
}

impl fmt::Display for PlaylistExportPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "playlist export preflight отклонён для {}: {}",
            self.subject, self.reason
        )
    }
}

impl std::error::Error for PlaylistExportPreflightError {}

/// Safe identity subject, для которого preflight не смог построить locator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportSubject {
    /// Конкретный playable item или subordinate part.
    Item(playlist_core::PlaylistItemId),
    /// Compound root locator, нужный XSPF extension.
    Compound(playlist_core::PlaylistCompoundGroupId),
}

impl fmt::Display for PlaylistExportSubject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Item(item_id) => write!(formatter, "item {item_id}"),
            Self::Compound(group_id) => write!(formatter, "compound group {group_id}"),
        }
    }
}

/// Строит format-specific immutable plan и выполняет все fallible checks заранее.
pub fn preflight_playlist_export(
    snapshot: &PlaylistExportSnapshot,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
    locator_policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedPlaylistExport, PlaylistExportPreflightError> {
    let mut tracks = Vec::with_capacity(snapshot.retained_item_count());
    let mut groups = Vec::new();
    let mut sensitive_locator_count = 0usize;
    let mut flattened_compound_group_count = 0usize;

    for entry in snapshot.entries() {
        match entry {
            PlaylistEntry::Single(item) => {
                let track = preflight_track(item, format, target, locator_policy)?;
                sensitive_locator_count += usize::from(track.locator.is_sensitive());
                tracks.push(track);
            }
            PlaylistEntry::Compound(group) => {
                let first_track = tracks.len() + 1;
                if format == PlaylistExportFormat::Xspf {
                    let root_locator =
                        preflight_group_locator(group, format, target, locator_policy).map_err(
                            |reason| PlaylistExportPreflightError {
                                subject: PlaylistExportSubject::Compound(group.group_id()),
                                reason,
                            },
                        )?;
                    sensitive_locator_count += usize::from(root_locator.is_sensitive());
                    groups.push(PreparedExportGroup {
                        first_track,
                        track_count: group.retained_part_count(),
                        root_locator,
                    });
                } else {
                    flattened_compound_group_count += 1;
                }

                for part in group.parts() {
                    let track = preflight_track(part.item(), format, target, locator_policy)?;
                    sensitive_locator_count += usize::from(track.locator.is_sensitive());
                    tracks.push(track);
                }
            }
        }
    }

    let warnings = if flattened_compound_group_count == 0 {
        Vec::new().into_boxed_slice()
    } else {
        vec![PlaylistExportWarning::CompoundGroupingFlattened {
            compound_group_count: flattened_compound_group_count,
        }]
        .into_boxed_slice()
    };

    Ok(PreparedPlaylistExport {
        format,
        tracks: tracks.into_boxed_slice(),
        groups: groups.into_boxed_slice(),
        warnings,
        secret_classification: PlaylistExportSecretClassification::from_sensitive_count(
            sensitive_locator_count,
        ),
    })
}

/// Preflight одного playable item сохраняет metadata только как display hint.
fn preflight_track(
    item: &PlaylistItem,
    format: PlaylistExportFormat,
    target: &PlaylistExportDocumentTarget,
    locator_policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedExportTrack, PlaylistExportPreflightError> {
    let locator =
        preflight_item_locator(item, format, target, locator_policy).map_err(|reason| {
            PlaylistExportPreflightError {
                subject: PlaylistExportSubject::Item(item.item_id()),
                reason,
            }
        })?;
    Ok(PreparedExportTrack {
        locator,
        metadata: item.cached_metadata().clone(),
    })
}

/// Internal flattened track record, недоступный caller-у для подмены preflight.
struct PreparedExportTrack {
    locator: PreparedExportLocator,
    metadata: CachedPlaylistMetadata,
}

/// Internal XSPF group range поверх flattened track list.
struct PreparedExportGroup {
    first_track: usize,
    track_count: usize,
    root_locator: PreparedExportLocator,
}

/// Выбирает metadata title и гарантирует непустой fallback.
fn export_title(metadata: &CachedPlaylistMetadata) -> &str {
    metadata
        .title()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| metadata.fallback_display_name())
}

/// Возвращает XSPF-compatible positive track number.
fn xspf_track_number(metadata: &CachedPlaylistMetadata) -> Option<u32> {
    let value = metadata.track_number()?.value();
    u32::try_from(value).ok().filter(|value| *value > 0)
}

/// Возвращает XSPF millisecond hint только при exact representability.
fn xspf_duration_milliseconds(metadata: &CachedPlaylistMetadata) -> Option<u64> {
    let milliseconds = metadata.duration()?.as_duration().as_millis();
    u64::try_from(milliseconds).ok()
}

/// Sanitizes text для line-based M3U8 без locator/directive injection.
fn sanitize_m3u8_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character == '\r' || character == '\n' || character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Проверяет XML 1.0 character production для metadata hints.
fn is_xml_1_0_character(character: char) -> bool {
    matches!(
        character as u32,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
    )
}

/// Экранирует XML text и заменяет недопустимые metadata controls безопасным пробелом.
fn push_xml_text(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            character if is_xml_1_0_character(character) => output.push(character),
            _ => output.push(' '),
        }
    }
}

/// Экранирует XML attribute поверх той же XML 1.0 character policy.
fn push_xml_attribute(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            character if is_xml_1_0_character(character) => output.push(character),
            _ => output.push(' '),
        }
    }
}
