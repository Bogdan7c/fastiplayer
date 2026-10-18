//! Владелец доступа к subtitle rendition-ам HLS: список дорожек, playlist и байты сегментов.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hls_playlist_core::{HlsParseRequest, HlsParserLimits, HlsPlaylist, parse_hls_playlist};
use source_core::{CancellationToken, HttpRequestTarget};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication,
};
use web_media_transport_api::SourceGeneration;

use super::error::HlsSubtitleFetchError;
use super::playlist::{HlsSubtitlePlaylistSnapshot, HlsSubtitleSegmentRef};
use super::timeline::PlaylistTimeline;
use crate::HlsSubtitleRenditionDescriptor;

/// Счётчик экземпляров: каждая ссылка помнит, каким транспортом выдана.
static NEXT_TRANSPORT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// Индекс rendition-а внутри одного транспорта (порядок как в master playlist).
///
/// Не является постоянной идентичностью: после reopen/refresh master индексы выдаются заново.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HlsSubtitleRenditionId(u32);

impl HlsSubtitleRenditionId {
    /// Порядковый номер rendition-а в списке `renditions()`.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Безопасное описание дорожки: только метаданные, никаких адресов.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsSubtitleRenditionInfo {
    /// Идентификатор для `load_playlist`.
    pub id: HlsSubtitleRenditionId,
    /// `GROUP-ID` из master playlist.
    pub group_id: Box<str>,
    /// `NAME`.
    pub name: Box<str>,
    /// `LANGUAGE`, если задан.
    pub language: Option<Box<str>>,
    /// `FORCED=YES`.
    pub forced: bool,
    /// `DEFAULT=YES`.
    pub default: bool,
    /// `AUTOSELECT=YES`.
    pub autoselect: bool,
    /// `CHARACTERISTICS`, если заданы.
    pub characteristics: Option<Box<str>>,
}

/// Верхний предел размера одного сегмента субтитров; значение приходит от вызывающего (config).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlsSubtitleSegmentByteLimit(NonZeroUsize);

impl HlsSubtitleSegmentByteLimit {
    /// Создаёт лимит из ненулевого числа байт.
    #[must_use]
    pub const fn from_bytes(maximum_bytes: NonZeroUsize) -> Self {
        Self(maximum_bytes)
    }

    /// Лимит в байтах.
    #[must_use]
    pub const fn bytes(self) -> NonZeroUsize {
        self.0
    }
}

/// Доступ к subtitle rendition-ам выбранной группы открытого HLS-источника.
///
/// HTTP-контекст - вспомогательный вид (`for_auxiliary_resources`) контекста открытия: те же
/// сессия, cookies, redirect- и retry-политика,
/// и тот же токен отмены источника. Поэтому транспорт живёт, пока жив источник, и отменяется
/// вместе с ним; отдельного клиента/кэша/политики он не создаёт. Токен вызова (`cancellation`)
/// проверяется до и после сетевой операции; прервать уже идущий запрос может только отмена
/// источника или встроенные таймауты HTTP-контекста.
#[derive(Clone)]
pub struct HlsSubtitleTransport {
    renditions: Box<[HlsSubtitleRenditionDescriptor]>,
    /// Эффективный адрес master playlist: база для относительных ссылок rendition-ов.
    master_base: HttpRequestTarget,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    parser_limits: HlsParserLimits,
    instance: u64,
    /// Стабильные шкалы live-плейлистов по rendition-ам; общие для всех клонов транспорта.
    timelines: Arc<Mutex<HashMap<HlsSubtitleRenditionId, PlaylistTimeline>>>,
}

impl HlsSubtitleTransport {
    /// Собирает транспорт при открытии источника; сеть при этом не трогается.
    pub(crate) fn new(
        renditions: Vec<HlsSubtitleRenditionDescriptor>,
        master_base: HttpRequestTarget,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        parser_limits: HlsParserLimits,
    ) -> Self {
        Self {
            renditions: renditions.into_boxed_slice(),
            master_base,
            // Вспомогательный вид контекста: сбои субтитров не должны взводить VOD endpoint
            // recovery видео (наблюдатель expiry отсутствует, кэш отдельный).
            http: http.for_auxiliary_resources(),
            generation,
            parser_limits,
            instance: NEXT_TRANSPORT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            timelines: Arc::default(),
        }
    }

    /// Безопасный список дорожек субтитров (пустой, если master их не объявляет).
    #[must_use]
    pub fn renditions(&self) -> Vec<HlsSubtitleRenditionInfo> {
        self.renditions
            .iter()
            .enumerate()
            .map(|(index, descriptor)| HlsSubtitleRenditionInfo {
                // Число rendition-ов ограничено парсером; saturating исключает панику.
                id: HlsSubtitleRenditionId(u32::try_from(index).unwrap_or(u32::MAX)),
                group_id: descriptor.group_id().into(),
                name: descriptor.name().into(),
                language: descriptor.language().map(Into::into),
                forced: descriptor.is_forced(),
                default: descriptor.is_default(),
                autoselect: descriptor.is_autoselect(),
                characteristics: descriptor.characteristics().map(Into::into),
            })
            .collect()
    }

    /// Загружает и разбирает media playlist дорожки (лёгкий профиль).
    ///
    /// Для live повторный вызов возвращает свежий снимок; расписание перечитывания - забота
    /// вызывающего (подсказка: `target_duration` снимка). `start` сегментов лежит на стабильной
    /// шкале rendition-а (см. `timeline`), поэтому не меняется при скольжении окна.
    pub fn load_playlist(
        &self,
        id: HlsSubtitleRenditionId,
        cancellation: &CancellationToken,
    ) -> Result<HlsSubtitlePlaylistSnapshot, HlsSubtitleFetchError> {
        let descriptor = usize::try_from(id.0)
            .ok()
            .and_then(|index| self.renditions.get(index))
            .ok_or(HlsSubtitleFetchError::UnknownRendition)?;
        ensure_not_cancelled(cancellation)?;
        let target = self
            .master_base
            .resolve_reference(descriptor.reference_for_future_fetch())
            .map_err(|_| HlsSubtitleFetchError::InvalidReference)?;
        let request = AdaptiveResourceFetchRequest::full(
            self.generation,
            target.clone(),
            self.http
                .maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        )
        .with_secret_forwarding(self.http.resource_secret_forwarding_for(&target));
        let resource = self.http.fetch_resource_blocking(request)?;
        ensure_not_cancelled(cancellation)?;
        let playlist = parse_hls_playlist(HlsParseRequest {
            document_bytes: resource.bytes(),
            reference_base: Some(resource.final_target().expose_secret_for_request()),
            limits: self.parser_limits,
        })
        .map_err(|_| HlsSubtitleFetchError::MalformedPlaylist)?;
        // Master вместо media playlist - структурно битая дорожка субтитров.
        let HlsPlaylist::Media(media) = playlist else {
            return Err(HlsSubtitleFetchError::MalformedPlaylist);
        };
        let mut snapshot = HlsSubtitlePlaylistSnapshot::from_media_playlist(
            &media,
            resource.final_target(),
            self.instance,
        )?;
        // Закрепляем начала сегментов на стабильной шкале (сеть уже позади, замок короткий).
        snapshot.timeline_epoch = self
            .timelines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(id)
            .or_default()
            .anchor(&mut snapshot.segments);
        Ok(snapshot)
    }

    /// Скачивает байты одного сегмента, не больше `limit` (и не больше жёсткой границы контекста).
    pub fn fetch_segment(
        &self,
        segment: &HlsSubtitleSegmentRef,
        limit: HlsSubtitleSegmentByteLimit,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, HlsSubtitleFetchError> {
        if segment.transport_instance != self.instance {
            return Err(HlsSubtitleFetchError::Stale);
        }
        ensure_not_cancelled(cancellation)?;
        let transport_bound = self
            .http
            .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment);
        let effective_limit = limit.bytes().min(transport_bound);
        let limit_bytes = effective_limit.get();
        if let Some(range) = segment.byte_range
            && range.length() > effective_limit
        {
            return Err(HlsSubtitleFetchError::TooLarge { limit_bytes });
        }
        let query = AdaptiveResourceQueryApplication::MergeScopedAddition;
        let purpose = AdaptiveResourcePurpose::MediaSegment;
        let request = match segment.byte_range {
            Some(range) => AdaptiveResourceFetchRequest::range(
                self.generation,
                segment.target.clone(),
                range,
                effective_limit,
                purpose,
                query,
            ),
            None => AdaptiveResourceFetchRequest::full(
                self.generation,
                segment.target.clone(),
                effective_limit,
                purpose,
                query,
            ),
        }
        .with_secret_forwarding(self.http.resource_secret_forwarding_for(&segment.target));
        let resource = self
            .http
            .fetch_resource_blocking(request)
            .map_err(|error| {
                match HlsSubtitleFetchError::from(error) {
                    // Поле лимита заполняет владелец политики, а не низкоуровневый слой.
                    HlsSubtitleFetchError::TooLarge { .. } => {
                        HlsSubtitleFetchError::TooLarge { limit_bytes }
                    }
                    other => other,
                }
            })?;
        ensure_not_cancelled(cancellation)?;
        Ok(resource.into_bytes())
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), HlsSubtitleFetchError> {
    if cancellation.is_cancelled() {
        Err(HlsSubtitleFetchError::Cancelled)
    } else {
        Ok(())
    }
}

impl fmt::Debug for HlsSubtitleTransport {
    /// Адреса, query и HTTP-контекст не печатаются: только число дорожек и поколение.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsSubtitleTransport")
            .field("renditions", &self.renditions.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
