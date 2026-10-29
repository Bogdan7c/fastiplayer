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

use bytes::Bytes;
use demux_api::{DemuxInput, DemuxRegistry, ProgressiveDemuxer};
use media_core::{Demuxer, TrackInfo};
use source_core::CancellationToken;

use crate::container::PushedContainer;
use crate::intake::{IntakeLedger, IntakeLimitExceeded};
use crate::pipe::{EpisodePipeAbandon, EpisodePipeWriter, PipeWrite, episode_pipe};
use crate::policy::PushedSourcePolicy;
use crate::time::TimestampOffset;

/// Всё, что нужно для запуска эпизодов одного потока.
#[derive(Clone)]
pub(crate) struct EpisodeFactory {
    /// Реестр демуксеров (общий на поток).
    registry: Arc<DemuxRegistry>,
    /// Общий учёт удерживаемых байтов.
    ledger: Arc<IntakeLedger>,
    /// Пределы.
    policy: PushedSourcePolicy,
}

/// Почему эпизод не стартовал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EpisodeStartError {
    /// Потолок удерживаемых байтов превышен.
    IntakeLimitExceeded(IntakeLimitExceeded),
    /// Демуксер не запустился.
    Demuxer(String),
}

/// Что получает эпизод при старте.
pub(crate) struct EpisodeRecipe<'a> {
    /// Контейнер полосы.
    pub(crate) container: PushedContainer,
    /// Текущий init-сегмент (у MPEG-TS его нет).
    pub(crate) initialization: Option<&'a Bytes>,
    /// Первый медиасегмент эпизода.
    pub(crate) first_media: Bytes,
    /// Смещение времени эпизода.
    pub(crate) offset: TimestampOffset,
}

impl std::fmt::Debug for EpisodeFactory {
    /// Реестр демуксеров не печатается: в диагностике достаточно учёта байтов.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpisodeFactory")
            .field("held_bytes", &self.ledger.held_bytes())
            .finish_non_exhaustive()
    }
}

impl EpisodeFactory {
    /// Фабрика эпизодов потока.
    pub(crate) fn new(
        registry: Arc<DemuxRegistry>,
        ledger: Arc<IntakeLedger>,
        policy: PushedSourcePolicy,
    ) -> Self {
        Self {
            registry,
            ledger,
            policy,
        }
    }

    /// Общий учёт удерживаемых байтов.
    pub(crate) fn ledger(&self) -> &IntakeLedger {
        &self.ledger
    }

    /// Запускает эпизод: труба получает init и первый медиасегмент, демуксер
    /// открывается в своём потоке. Пишущая сторона остаётся у полосы.
    pub(crate) fn start(
        &self,
        recipe: EpisodeRecipe<'_>,
    ) -> Result<(EpisodePipeWriter, EpisodeHandle), EpisodeStartError> {
        let (writer, reader) = episode_pipe(Arc::clone(&self.ledger));
        let abandon = reader.abandon_handle();
        let mut opening_bytes = 0;
        for segment in recipe
            .initialization
            .into_iter()
            .cloned()
            .chain([recipe.first_media])
        {
            opening_bytes += segment.len();
            self.push(&writer, segment)
                .map_err(EpisodeStartError::IntakeLimitExceeded)?;
        }
        let available = NonZeroUsize::new(opening_bytes)
            .ok_or_else(|| EpisodeStartError::Demuxer("an episode needs bytes".to_owned()))?;
        let open_budget = self
            .policy
            .episode_open_budget(available)
            .map_err(|error| EpisodeStartError::Demuxer(error.to_string()))?;
        let container_id = recipe
            .container
            .demux_container_id()
            .map_err(|error| EpisodeStartError::Demuxer(error.to_string()))?;
        let hints = recipe
            .container
            .demux_hints()
            .map_err(|error| EpisodeStartError::Demuxer(error.to_string()))?;

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
        .map_err(|error| EpisodeStartError::Demuxer(error.to_string()))?;
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

    /// Ставит сегмент в трубу эпизода, зачтя его в общий учёт.
    pub(crate) fn push(
        &self,
        writer: &EpisodePipeWriter,
        segment: Bytes,
    ) -> Result<PipeWrite, IntakeLimitExceeded> {
        self.ledger.try_reserve(segment.len())?;
        Ok(writer.push_reserved(segment))
    }
}

/// Читающая сторона эпизода: у демуксера полосы на стороне плеера.
///
/// Уничтожение ручки выбрасывает эпизод: поток демуксера просыпается и выходит,
/// непрочитанные байты уходят из общего учёта.
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
