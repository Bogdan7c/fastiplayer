//! Пределы и сроки источника. Каждое число — с объяснением «почему так».
//!
//! Здесь нет пользовательских настроек: это границы безопасности против чужих
//! байтов и параметры существующих компонентов (`ProgressiveDemuxer`,
//! `CompositeAvDemuxer`). «Сколько держать вперёд» — настройка 03b.

use std::num::NonZeroUsize;
use std::time::Duration;

use demux_api::{
    CompositeComponentLeadPolicy, CompositeComponentLeadPolicyError, DemuxSniffBudget,
    DemuxSniffBudgetError, ProgressiveDemuxBufferLimits,
};
use media_core::{DemuxRetryHint, DemuxRetryHintError};

use crate::framing::FramingLimits;

/// Init-сегмент: реальные — единицы КиБ; 4 МиБ хватает `moov` с длинными таблицами.
const MAX_INITIALIZATION_BYTES: usize = 4 * 1024 * 1024;
/// Медиасегмент: DASH 4K, 5 с × ~40 Мбит/с ≈ 25 МиБ; 32 МиБ — с запасом.
const MAX_MEDIA_SEGMENT_BYTES: usize = 32 * 1024 * 1024;
/// Принято, но не прочитано демуксерами: ~2 минуты 1080p YouTube (~4 Мбит/с) ≈ 60 МиБ
/// на дорожку; 256 МиБ — жёсткий потолок памяти потока, а не цель буфера.
const MAX_HELD_STREAM_BYTES: usize = 256 * 1024 * 1024;
/// Полос потока: столько SourceBuffer допускает протокол моста (на эталоне 1–2).
const MAX_LANES: usize = 8;
/// Запущенных, но ещё не забранных плеером эпизодов на полосу. У каждого — свой
/// поток демуксера и очереди; честная страница меняет init или смещение редко, а
/// плеер забирает эпизоды по мере показа. Больше — страница плодит эпизоды.
const MAX_PENDING_EPISODES_PER_LANE: usize = 4;
/// Очередь готовых событий одного эпизода — как у живого HLS (`media-source-open`).
const EPISODE_EVENT_QUEUE: usize = 256;
/// Байты пакетов в очереди одного эпизода — как у живого HLS.
const EPISODE_ENCODED_QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// Срок открытия демуксера эпизода: байты init уже на месте, разбор занимает миллисекунды.
const EPISODE_OPEN_DEADLINE: Duration = Duration::from_secs(2);
/// Как скоро плеер переспрашивает, если байтов ещё нет — как у живого HLS.
const RETRY_AFTER: Duration = Duration::from_millis(10);
/// Насколько видео может уйти вперёд звука (и наоборот) при раздельных полосах —
/// как у живого HLS: плееры страниц кормят звук и видео почти вровень.
const COMPOSITE_MAX_TIMESTAMP_LEAD: Duration = Duration::from_secs(3);
/// Самый большой пакет, ждущий пару из другой полосы — как у живого HLS.
const COMPOSITE_MAX_PENDING_PACKET_BYTES: usize = 4 * 1024 * 1024;

/// Ошибка сборки пределов: число вне допустимого диапазона компонента.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushedSourcePolicyError {
    /// Подсказка повтора вне пределов `DemuxRetryHint`.
    #[error(transparent)]
    RetryHint(#[from] DemuxRetryHintError),
    /// Политика ожидания раздельных полос вне пределов `CompositeAvDemuxer`.
    #[error(transparent)]
    CompositeLead(#[from] CompositeComponentLeadPolicyError),
    /// Срок открытия нулевой.
    #[error(transparent)]
    OpenDeadline(#[from] DemuxSniffBudgetError),
}

/// Пределы и сроки одного потока.
#[derive(Debug, Clone, Copy)]
pub struct PushedSourcePolicy {
    /// Пределы размера сегментов для сканера.
    framing: FramingLimits,
    /// Потолок принятых, но не прочитанных байтов потока.
    max_held_stream_bytes: NonZeroUsize,
    /// Потолок числа полос.
    max_lanes: NonZeroUsize,
    /// Потолок ждущих эпизодов на полосу.
    max_pending_episodes_per_lane: NonZeroUsize,
    /// Очереди фонового демуксера эпизода.
    episode_demux_limits: ProgressiveDemuxBufferLimits,
    /// Срок открытия демуксера эпизода.
    episode_open_deadline: Duration,
    /// Подсказка плееру «спроси снова через …».
    retry_hint: DemuxRetryHint,
    /// Ожидание пары при раздельных полосах звука и видео.
    composite_lead_policy: CompositeComponentLeadPolicy,
}

impl PushedSourcePolicy {
    /// Пределы для потока из браузера (значения и причины — в константах модуля).
    pub fn recommended() -> Result<Self, PushedSourcePolicyError> {
        let composite_lead_policy = CompositeComponentLeadPolicy::single_pending_packet(
            COMPOSITE_MAX_TIMESTAMP_LEAD,
            const { NonZeroUsize::new(COMPOSITE_MAX_PENDING_PACKET_BYTES).expect("non-zero") },
        )?;
        // Проверяем срок так же, как это сделает реестр при открытии.
        DemuxSniffBudget::new(NonZeroUsize::MIN, NonZeroUsize::MIN, EPISODE_OPEN_DEADLINE)?;
        Ok(Self {
            framing: FramingLimits {
                max_initialization_bytes: MAX_INITIALIZATION_BYTES,
                max_media_bytes: MAX_MEDIA_SEGMENT_BYTES,
            },
            max_held_stream_bytes: const { NonZeroUsize::new(MAX_HELD_STREAM_BYTES).expect("non-zero") },
            max_lanes: const { NonZeroUsize::new(MAX_LANES).expect("non-zero") },
            max_pending_episodes_per_lane: const {
                NonZeroUsize::new(MAX_PENDING_EPISODES_PER_LANE).expect("non-zero")
            },
            episode_demux_limits: ProgressiveDemuxBufferLimits::new(
                const { NonZeroUsize::new(EPISODE_EVENT_QUEUE).expect("non-zero") },
                const { NonZeroUsize::new(EPISODE_ENCODED_QUEUE_BYTES).expect("non-zero") },
            ),
            episode_open_deadline: EPISODE_OPEN_DEADLINE,
            retry_hint: DemuxRetryHint::new(RETRY_AFTER)?,
            composite_lead_policy,
        })
    }

    /// Тот же набор с другим потолком удерживаемых байтов потока.
    #[must_use]
    pub fn with_max_held_stream_bytes(mut self, max_held_stream_bytes: NonZeroUsize) -> Self {
        self.max_held_stream_bytes = max_held_stream_bytes;
        self
    }

    /// Тот же набор с другим пределом одного медиасегмента.
    #[must_use]
    pub fn with_max_media_segment_bytes(mut self, max_media_segment_bytes: NonZeroUsize) -> Self {
        self.framing.max_media_bytes = max_media_segment_bytes.get();
        self
    }

    /// Пределы сканера.
    pub(crate) const fn framing_limits(&self) -> FramingLimits {
        self.framing
    }

    /// Потолок принятых, но не прочитанных байтов.
    pub(crate) const fn max_held_stream_bytes(&self) -> usize {
        self.max_held_stream_bytes.get()
    }

    /// Потолок числа полос.
    pub(crate) const fn max_lanes(&self) -> usize {
        self.max_lanes.get()
    }

    /// Потолок ждущих эпизодов на полосу.
    pub(crate) const fn max_pending_episodes_per_lane(&self) -> usize {
        self.max_pending_episodes_per_lane.get()
    }

    /// Очереди фонового демуксера эпизода.
    pub(crate) const fn episode_demux_limits(&self) -> ProgressiveDemuxBufferLimits {
        self.episode_demux_limits
    }

    /// Бюджет открытия эпизода: читать ровно столько байтов, сколько уже лежит в трубе.
    ///
    /// Реестр при открытии читает префикс до заполнения бюджета. Если попросить больше,
    /// чем пришло, поток демуксера ждал бы следующих байтов страницы ещё до разбора.
    pub(crate) fn episode_open_budget(
        &self,
        available_bytes: NonZeroUsize,
    ) -> Result<DemuxSniffBudget, DemuxSniffBudgetError> {
        DemuxSniffBudget::new(
            available_bytes,
            NonZeroUsize::MIN,
            self.episode_open_deadline,
        )
    }

    /// Подсказка повтора для плеера.
    pub(crate) const fn retry_hint(&self) -> DemuxRetryHint {
        self.retry_hint
    }

    /// Политика ожидания пары раздельных полос.
    pub(crate) const fn composite_lead_policy(&self) -> CompositeComponentLeadPolicy {
        self.composite_lead_policy
    }
}
