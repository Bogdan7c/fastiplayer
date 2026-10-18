//! Клонируемый дескриптор загрузки субтитров одного открытого видео.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use service_ytdlp::{YtDlpSubtitleFormat, YtDlpSubtitleInventory, YtDlpSubtitleTrack};
use source_core::{CancellationToken, HttpSourceSession, SourceRuntimeConfig};

use super::http::{FormatFailure, RequestFailure, get_body};
use super::refresh::{SubtitleSourceRefresher, YtDlpSubtitleRefresher};
use super::types::{
    FetchedWebSubtitle, WebSubtitleFetchError, WebSubtitleFetchLimits, WebSubtitleTrack,
    WebSubtitleTrackId,
};

/// Общее состояние всех клонов дескриптора.
struct HandleState {
    /// Текущий инвентарь с подписанными ссылками; заменяется после обновления.
    inventory: Mutex<Arc<YtDlpSubtitleInventory>>,
    /// Откуда брать свежие ссылки.
    refresher: Arc<dyn SubtitleSourceRefresher>,
    /// Сетевые настройки для HTTP-сессии.
    source_config: SourceRuntimeConfig,
}

/// Загрузка субтитров конкретного видео. Секреты внутри, Debug их не показывает.
///
/// `fetch` блокирующий: приложение обязано вызывать его с фонового потока.
#[derive(Clone)]
pub struct WebSubtitleFetchHandle {
    state: Arc<HandleState>,
}

impl WebSubtitleFetchHandle {
    /// Собирает дескриптор из уже извлечённого инвентаря.
    fn new(
        inventory: YtDlpSubtitleInventory,
        refresher: Arc<dyn SubtitleSourceRefresher>,
        source_config: SourceRuntimeConfig,
    ) -> Self {
        Self {
            state: Arc::new(HandleState {
                inventory: Mutex::new(Arc::new(inventory)),
                refresher,
                source_config,
            }),
        }
    }

    /// Дескриптор для видео, открытого через yt-dlp: ссылки из его снимка,
    /// обновление — повторным запуском того же extractor-а.
    pub(crate) fn for_yt_dlp_extraction(
        inventory: YtDlpSubtitleInventory,
        refresher: YtDlpSubtitleRefresher,
        source_config: SourceRuntimeConfig,
    ) -> Self {
        Self::new(inventory, Arc::new(refresher), source_config)
    }

    /// Безопасный список дорожек (без ссылок) для меню.
    #[must_use]
    pub fn tracks(&self) -> Vec<WebSubtitleTrack> {
        self.current_inventory()
            .tracks()
            .iter()
            .filter_map(WebSubtitleTrack::from_service_track)
            .collect()
    }

    /// Скачивает дорожку: сначала srv3, при провале самого формата — WebVTT.
    ///
    /// * 429 и сетевые ошибки к запасному формату не ведут (лимит тот же);
    /// * при протухшей ссылке ровно один раз заново спрашивает yt-dlp;
    /// * никаких фоновых повторов: повтор — только новым вызовом по клику.
    pub fn fetch(
        &self,
        track: &WebSubtitleTrackId,
        limits: WebSubtitleFetchLimits,
        cancellation: &CancellationToken,
    ) -> Result<FetchedWebSubtitle, WebSubtitleFetchError> {
        if cancellation.is_cancelled() {
            return Err(WebSubtitleFetchError::Cancelled);
        }
        let session = HttpSourceSession::new(&self.state.source_config)
            .map_err(|_| WebSubtitleFetchError::NetworkUnavailable)?;
        let inventory = self.current_inventory();
        match fetch_from_inventory(&session, &inventory, track, limits, cancellation) {
            Err(WebSubtitleFetchError::LinkExpired) => {}
            outcome => return outcome,
        }
        // Единственное повторное извлечение на один вызов `fetch`.
        let refreshed = Arc::new(self.state.refresher.refresh(cancellation)?);
        *self
            .state
            .inventory
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::clone(&refreshed);
        fetch_from_inventory(&session, &refreshed, track, limits, cancellation).map_err(|error| {
            // Дорожка могла пропасть из свежего ответа: это «не найдено», а не «неизвестная».
            if error == WebSubtitleFetchError::UnknownTrack {
                WebSubtitleFetchError::NotFound
            } else {
                error
            }
        })
    }

    fn current_inventory(&self) -> Arc<YtDlpSubtitleInventory> {
        Arc::clone(
            &self
                .state
                .inventory
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

impl fmt::Debug for WebSubtitleFetchHandle {
    /// Только число дорожек: ссылки и подписи не раскрываются.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebSubtitleFetchHandle")
            .field("track_count", &self.current_inventory().tracks().len())
            .finish_non_exhaustive()
    }
}

/// Равенство по identity общего состояния: два клона равны, разные видео — нет.
impl PartialEq for WebSubtitleFetchHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for WebSubtitleFetchHandle {}

/// Ищет дорожку в инвентаре и перебирает её форматы.
fn fetch_from_inventory(
    session: &HttpSourceSession,
    inventory: &YtDlpSubtitleInventory,
    track_id: &WebSubtitleTrackId,
    limits: WebSubtitleFetchLimits,
    cancellation: &CancellationToken,
) -> Result<FetchedWebSubtitle, WebSubtitleFetchError> {
    let track = inventory
        .find_track(track_id.kind(), track_id.language())
        .ok_or(WebSubtitleFetchError::UnknownTrack)?;
    fetch_track_formats(session, track, limits, cancellation)
}

fn fetch_track_formats(
    session: &HttpSourceSession,
    track: &YtDlpSubtitleTrack,
    limits: WebSubtitleFetchLimits,
    cancellation: &CancellationToken,
) -> Result<FetchedWebSubtitle, WebSubtitleFetchError> {
    let now = SystemTime::now();
    let mut failures = Vec::new();
    for source in track.sources() {
        if cancellation.is_cancelled() {
            return Err(WebSubtitleFetchError::Cancelled);
        }
        // Подпись уже протухла по локальным часам: сеть тратить незачем.
        if source
            .expires_at()
            .is_some_and(|expires_at| expires_at <= now)
        {
            return Err(WebSubtitleFetchError::LinkExpired);
        }
        let outcome = get_body(session, source.target_for_request(), limits, cancellation)
            .and_then(|bytes| {
                validate_body(source.format(), &bytes)
                    .map(|()| bytes)
                    .map_err(RequestFailure::Format)
            });
        match outcome {
            Ok(bytes) => {
                return Ok(FetchedWebSubtitle::new(bytes, source.format().into()));
            }
            Err(RequestFailure::Format(failure)) => failures.push(failure),
            Err(RequestFailure::Fatal(error)) => return Err(error),
        }
    }
    Err(error_after_all_formats_failed(&failures))
}

/// Итоговая ошибка, когда провалились все форматы дорожки.
fn error_after_all_formats_failed(failures: &[FormatFailure]) -> WebSubtitleFetchError {
    if failures.contains(&FormatFailure::Gone) {
        // 410 у формата чаще всего значит протухшую подпись: пусть сработает обновление.
        return WebSubtitleFetchError::LinkExpired;
    }
    match failures.last() {
        Some(FormatFailure::NotFound) | None => WebSubtitleFetchError::NotFound,
        Some(FormatFailure::EmptyBody | FormatFailure::WrongBody | FormatFailure::Gone) => {
            WebSubtitleFetchError::InvalidResponse
        }
    }
}

/// Грубая проверка, что тело похоже на заявленный формат.
fn validate_body(format: YtDlpSubtitleFormat, bytes: &[u8]) -> Result<(), FormatFailure> {
    const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
    /// Сколько байтов начала смотрим в поисках корневого элемента srv3.
    const SRV3_ROOT_SEARCH_BYTES: usize = 512;
    /// Начало корневого элемента srv3.
    const SRV3_ROOT_TAG: &[u8] = b"<timedtext";

    let without_bom = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    let trimmed = without_bom.trim_ascii_start();
    if trimmed.is_empty() {
        return Err(FormatFailure::EmptyBody);
    }
    let looks_valid = match format {
        YtDlpSubtitleFormat::Srv3 => {
            let head = &trimmed[..trimmed.len().min(SRV3_ROOT_SEARCH_BYTES)];
            trimmed.starts_with(b"<")
                && head
                    .windows(SRV3_ROOT_TAG.len())
                    .any(|window| window == SRV3_ROOT_TAG)
        }
        YtDlpSubtitleFormat::WebVtt => trimmed.starts_with(b"WEBVTT"),
    };
    if looks_valid {
        Ok(())
    } else {
        Err(FormatFailure::WrongBody)
    }
}
