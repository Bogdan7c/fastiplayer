//! Probe метаданных локальных файлов для плейлиста (соседи папки, ручное
//! добавление, обновление видимых строк, сортировка по метаданным).
//!
//! Почему probe живёт здесь, а не в `playlist-discovery`: формат файла обязан
//! определяться тем же набором container factory, что и при воспроизведении
//! (`open_local_demuxer_from_source`). Раньше плейлист пробовал только Symphonia,
//! поэтому сырые `.ac3`/`.eac3`/`.ec3` и `.ts` оставались без длительности, а
//! Symphonia писала в лог `ERROR probe reached EOF` на каждый такой файл. Теперь
//! выбор формата один — по содержимому, через общий local registry; второго
//! детектора форматов нет.
//!
//! `playlist-discovery` остаётся нейтральным (без config/player зависимостей) и
//! получает этот adapter через свой порт [`DiscoveryProbe`].

use std::io;
use std::path::Path;

use demux_api::{DemuxFactoryOpenError, DemuxOpenError, DemuxProbeRejection};
use fastiplayer_config::PlayerDemuxConfig;
use media_core::{Demuxer, MediaDuration};
use playlist_discovery::{
    DiscoveryProbe, LocalMediaFingerprint, ProbeOneLocalMediaError, ProbedLocalMedia,
    classify_local_media_tracks, read_local_media_fingerprint,
};
use source_core::{CancellationToken, LocalFileSource, SourceError};

use super::{AttachmentReadLimits, LocalDemuxOpenError, open_local_demuxer_from_source};

/// Лимиты вложений для probe метаданных: ни одного байта вложений не читаем.
///
/// Плейлисту нужны только дорожки, длительность и теги. Шрифты MKV (до 100 МиБ
/// при воспроизведении) здесь были бы чистой тратой I/O на каждый соседний файл:
/// вложение больше лимита demuxer пропускает, не читая содержимое.
const METADATA_PROBE_ATTACHMENT_LIMITS: AttachmentReadLimits = AttachmentReadLimits::DEFAULT
    .with_max_bytes_per_attachment(0)
    .with_max_total_bytes(0);

/// Adapter порта [`DiscoveryProbe`] поверх production local demux registry.
///
/// Каждый probe открывает собственный demuxer (тот же путь, что playback open),
/// снимает дорожки/длительность/теги и сразу закрывает его — пакеты не читаются.
#[derive(Debug, Clone, Default)]
pub struct LocalRegistryDiscoveryProbe {
    /// Параметры demux для open-а. Единственное поле config-а —
    /// лимит битых пакетов подряд — влияет только на чтение пакетов, которого
    /// probe не делает; поэтому берём валидный default config-а, а не значение
    /// пользователя, и не тянем config в долгоживущий executor.
    demux_config: PlayerDemuxConfig,
}

impl LocalRegistryDiscoveryProbe {
    /// Создаёт probe с default-параметрами demux (см. поле `demux_config`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl DiscoveryProbe for LocalRegistryDiscoveryProbe {
    fn read_fingerprint(
        &self,
        locator: &Path,
        cancellation: &CancellationToken,
    ) -> Result<LocalMediaFingerprint, ProbeOneLocalMediaError> {
        // Проверка кэша не открывает demuxer: владелец этой семантики — discovery crate.
        read_local_media_fingerprint(locator, cancellation)
    }

    fn probe(
        &self,
        locator: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
        probe_local_media_with_registry(locator, &self.demux_config, cancellation)
    }
}

/// Открывает один файл через общий local registry и строит запись плейлиста.
fn probe_local_media_with_registry(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
    cancellation: &CancellationToken,
) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
    ensure_not_cancelled(cancellation)?;
    // Отдельного exists-check нет: open — единственная авторитетная операция
    // и сохраняет точный `io::ErrorKind` (нет файла, нет прав).
    let local_source = LocalFileSource::open(path).map_err(map_source_open_error)?;
    // Fingerprint снят с того же handle-а, который читает demuxer.
    let opened_snapshot = local_source.metadata_snapshot();
    let fingerprint =
        LocalMediaFingerprint::new(opened_snapshot.file_size_bytes, opened_snapshot.modified_at);

    let extension_hint = path.extension().and_then(|extension| extension.to_str());
    let demuxer = open_local_demuxer_from_source(
        local_source,
        extension_hint,
        demux_config,
        METADATA_PROBE_ATTACHMENT_LIMITS,
        cancellation.clone(),
    )
    .map_err(map_local_demux_open_error)?;
    ensure_not_cancelled(cancellation)?;

    build_probed_record(path, demuxer.as_ref(), fingerprint)
}

/// Переводит снимок открытого demuxer-а в неизменяемую запись discovery.
fn build_probed_record(
    path: &Path,
    demuxer: &dyn Demuxer,
    fingerprint: LocalMediaFingerprint,
) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
    // Та же классификация, что у explicit playback open (D75): одна vocabulary.
    let media_kind = classify_local_media_tracks(demuxer.tracks())?;
    let duration = demuxer.duration().map(MediaDuration::from_duration);
    let metadata = demuxer.media_metadata().unwrap_or_default().tags;
    Ok(ProbedLocalMedia::new(
        display_filename(path),
        media_kind,
        duration,
        metadata,
        fingerprint,
    ))
}

/// Ошибка открытия файла: I/O остаётся I/O, отмена — отменой.
fn map_source_open_error(error: SourceError) -> ProbeOneLocalMediaError {
    match error {
        SourceError::Cancelled => ProbeOneLocalMediaError::Cancelled,
        SourceError::LocalIo { source, .. } => ProbeOneLocalMediaError::IoFailure(source),
        // Других ошибок `LocalFileSource::open` не возвращает; если появятся —
        // не теряем их, а сообщаем как I/O с исходным текстом.
        other => ProbeOneLocalMediaError::IoFailure(io::Error::other(other.to_string())),
    }
}

/// Сохраняет различие «формат не наш» / «файл битый» / «I/O» / «отмена».
///
/// «Не распознан ни одним factory» — это `UnsupportedContainer` (обычный файл не
/// того типа, не ошибка приложения); всё, что factory узнал, но не смог открыть, —
/// `ProbeFailure`.
fn map_local_demux_open_error(error: LocalDemuxOpenError) -> ProbeOneLocalMediaError {
    if error.is_cancelled() {
        return ProbeOneLocalMediaError::Cancelled;
    }
    match error {
        LocalDemuxOpenError::RegistrySetup(setup_error) => ProbeOneLocalMediaError::ProbeFailure {
            reason: format!("local demux registry unavailable: {setup_error:#}"),
        },
        LocalDemuxOpenError::Open(open_error) => map_demux_open_error(open_error),
    }
}

fn map_demux_open_error(error: DemuxOpenError) -> ProbeOneLocalMediaError {
    match error {
        DemuxOpenError::NoMatch => ProbeOneLocalMediaError::UnsupportedContainer {
            reason: error.to_string(),
        },
        DemuxOpenError::ProbeRejected(rejection) => map_probe_rejection(rejection),
        DemuxOpenError::FactoryRejected {
            source: DemuxFactoryOpenError::Cancelled,
            ..
        } => ProbeOneLocalMediaError::Cancelled,
        DemuxOpenError::AmbiguousMatch { .. }
        | DemuxOpenError::UnexpectedContainer { .. }
        | DemuxOpenError::FactoryRejected { .. } => ProbeOneLocalMediaError::ProbeFailure {
            reason: format!("{:#}", anyhow::Error::new(error)),
        },
    }
}

fn map_probe_rejection(rejection: DemuxProbeRejection) -> ProbeOneLocalMediaError {
    match rejection {
        DemuxProbeRejection::Cancelled => ProbeOneLocalMediaError::Cancelled,
        // Local source всегда seekable byte source; отказ по форме входа значит,
        // что формат локально не поддерживается.
        DemuxProbeRejection::UnsupportedInput { .. } => {
            ProbeOneLocalMediaError::UnsupportedContainer {
                reason: rejection.to_string(),
            }
        }
        // Чтение байтов для sniff-а не удалось: это I/O, а не формат.
        DemuxProbeRejection::InputFailure { reason } => {
            ProbeOneLocalMediaError::IoFailure(io::Error::other(reason))
        }
        DemuxProbeRejection::Truncated { .. }
        | DemuxProbeRejection::DeadlineExceeded { .. }
        | DemuxProbeRejection::Malformed { .. } => ProbeOneLocalMediaError::ProbeFailure {
            reason: rejection.to_string(),
        },
    }
}

/// Имя файла без пути к папке (правило UX: в UI не показываем полный путь).
fn display_filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), ProbeOneLocalMediaError> {
    if cancellation.is_cancelled() {
        Err(ProbeOneLocalMediaError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
