//! Эпизод: один демуксер на одном init-сегменте.
//!
//! Существующие демуксеры (`symphonia-demux`, `mpeg-ts-demux`) понимают ровно
//! один init-сегмент на поток. Поэтому новый init (качество), смена кодека или
//! смещения времени начинают новый эпизод: свежий демуксер получает init и
//! следующие медиасегменты. Так же устроены живые HLS и DASH.
//!
//! Демуксер эпизода работает в фоновом потоке `ProgressiveDemuxer`: он блокируется
//! на чтении трубы, а плеер видит `TemporarilyUnavailable` и не ждёт.

use std::num::NonZeroUsize;
use std::sync::Arc;

use demux_api::{DemuxInput, DemuxRegistry, ProgressiveDemuxer};
use media_core::{Demuxer, TrackInfo};
use source_core::CancellationToken;

use crate::container::PushedContainer;
use crate::intake::ChargedSegment;
use crate::pipe::{EpisodePipeAbandon, EpisodePipeWriter, episode_pipe};
use crate::policy::PushedSourcePolicy;
use crate::time::TimestampOffset;

/// Всё, что нужно для запуска эпизодов одного потока.
#[derive(Clone)]
pub(crate) struct EpisodeFactory {
    /// Реестр демуксеров (общий на поток).
    registry: Arc<DemuxRegistry>,
    /// Пределы.
    policy: PushedSourcePolicy,
}

/// Демуксер эпизода не запустился (причина — для диагностики).
///
/// Байты эпизода к этому моменту уже зачтены при приёме, поэтому потолка учёта
/// здесь нет: старт либо удался, либо не открылся демуксер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeStartError(pub(crate) String);

/// Что получает эпизод при старте.
pub(crate) struct EpisodeRecipe<'a> {
    /// Контейнер полосы.
    pub(crate) container: PushedContainer,
    /// Init-сегмент эпизода (у MPEG-TS его нет).
    pub(crate) initialization: Option<&'a ChargedSegment>,
    /// Первый медиасегмент эпизода.
    pub(crate) first_media: ChargedSegment,
    /// Смещение времени эпизода.
    pub(crate) offset: TimestampOffset,
}

impl std::fmt::Debug for EpisodeFactory {
    /// Реестр демуксеров не печатается.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpisodeFactory")
            .finish_non_exhaustive()
    }
}

impl EpisodeFactory {
    /// Фабрика эпизодов потока.
    pub(crate) fn new(registry: Arc<DemuxRegistry>, policy: PushedSourcePolicy) -> Self {
        Self { registry, policy }
    }

    /// Запускает эпизод: труба получает init и первый медиасегмент, демуксер
    /// открывается в своём потоке. Пишущая сторона остаётся у полосы.
    pub(crate) fn start(
        &self,
        recipe: EpisodeRecipe<'_>,
    ) -> Result<(EpisodePipeWriter, EpisodeHandle), EpisodeStartError> {
        let (writer, reader) = episode_pipe();
        let abandon = reader.abandon_handle();
        let mut opening_bytes = 0;
        for segment in recipe
            .initialization
            .into_iter()
            .cloned()
            .chain([recipe.first_media])
        {
            opening_bytes += segment.len();
            // Свежая труба ещё не выброшена: сегмент встанет в очередь.
            writer.push(segment);
        }
        let available = NonZeroUsize::new(opening_bytes)
            .ok_or_else(|| EpisodeStartError("an episode needs bytes".to_owned()))?;
        let open_budget = self
            .policy
            .episode_open_budget(available)
            .map_err(|error| EpisodeStartError(error.to_string()))?;
        let container_id = recipe
            .container
            .demux_container_id()
            .map_err(|error| EpisodeStartError(error.to_string()))?;
        let hints = recipe
            .container
            .demux_hints()
            .map_err(|error| EpisodeStartError(error.to_string()))?;

        let cancellation = CancellationToken::new();
        let registry = Arc::clone(&self.registry);
        let open_cancellation = cancellation.clone();
        let open_inner = move || {
            registry
                .open_required_container(
                    DemuxInput::byte_stream(Box::new(reader)),
                    hints,
                    open_budget,
                    open_cancellation,
                    container_id,
                )
                .map_err(anyhow::Error::new)
        };
        let demuxer = ProgressiveDemuxer::new_deferred(
            open_inner,
            cancellation.clone(),
            self.policy.episode_demux_limits(),
            self.policy.retry_hint(),
        )
        .map_err(|error| EpisodeStartError(error.to_string()))?;
        Ok((
            writer,
            EpisodeHandle {
                demuxer,
                offset: recipe.offset,
                abandon,
                cancellation,
            },
        ))
    }
}

/// Читающая сторона эпизода: у демуксера полосы на стороне плеера.
///
/// Уничтожение ручки выбрасывает эпизод: поток демуксера просыпается и выходит,
/// непрочитанные сегменты трубы отпускаются.
pub(crate) struct EpisodeHandle {
    /// Фоновый демуксер эпизода.
    demuxer: ProgressiveDemuxer,
    /// Смещение времени, действующее на весь эпизод.
    offset: TimestampOffset,
    /// Выброс трубы.
    abandon: EpisodePipeAbandon,
    /// Отмена открытия и чтения.
    cancellation: CancellationToken,
}

impl EpisodeHandle {
    /// Демуксер эпизода.
    pub(crate) fn demuxer_mut(&mut self) -> &mut ProgressiveDemuxer {
        &mut self.demuxer
    }

    /// Дорожки, которые уже опубликовал демуксер эпизода.
    pub(crate) fn tracks(&self) -> &[TrackInfo] {
        self.demuxer.tracks()
    }

    /// Смещение времени эпизода.
    pub(crate) const fn offset(&self) -> TimestampOffset {
        self.offset
    }
}

impl Drop for EpisodeHandle {
    /// Будит поток демуксера, заблокированный на чтении трубы, и отменяет его работу.
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.abandon.abandon();
    }
}

impl std::fmt::Debug for EpisodeHandle {
    /// Демуксер и труба во внешнюю диагностику не выводятся.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpisodeHandle")
            .field("offset", &self.offset)
            .finish_non_exhaustive()
    }
}
