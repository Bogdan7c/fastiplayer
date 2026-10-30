//! Пределы и сроки источника. Каждое число — с объяснением «почему так».
//!
//! Почти всё здесь — границы безопасности против чужих байтов и параметры
//! существующих компонентов (`ProgressiveDemuxer`, `CompositeAvDemuxer`).
//! Единственная пользовательская настройка — окно буфера ([`BufferWindow`]):
//! приложение берёт его из config.toml и передаёт через
//! [`PushedSourcePolicy::with_buffer_window`].

use std::num::NonZeroUsize;
use std::time::Duration;

use demux_api::{
    CompositeComponentLeadPolicy, CompositeComponentLeadPolicyError, DemuxSniffBudget,
    DemuxSniffBudgetError, ProgressiveDemuxBufferLimits,
};
use media_core::{DemuxRetryHint, DemuxRetryHintError};

use crate::buffer_window::BufferWindow;
use crate::framing::FramingLimits;
use crate::shelf::ShelfRules;
use crate::stream_buffer::StreamBufferRules;

/// Init-сегмент: реальные — единицы КиБ; 4 МиБ хватает `moov` с длинными таблицами.
const MAX_INITIALIZATION_BYTES: usize = 4 * 1024 * 1024;
/// Медиасегмент: DASH 4K, 5 с × ~40 Мбит/с ≈ 25 МиБ; 32 МиБ — с запасом.
const MAX_MEDIA_SEGMENT_BYTES: usize = 32 * 1024 * 1024;
/// Всё, что держит поток (полка + трубы + init): окно 2,5 минуты 1080p YouTube
/// (~4 Мбит/с) ≈ 75 МиБ на дорожку; 256 МиБ — жёсткий потолок памяти потока, а не
/// цель буфера. Цель задаёт окно ([`BUFFER_AHEAD`], [`BUFFER_BEHIND`]).
const MAX_HELD_STREAM_BYTES: usize = 256 * 1024 * 1024;
/// С какой доли жёсткого потолка удерживаемых байтов источник говорит странице
/// «полон», даже если запас вперёд в секундах меньше окна. Окно в секундах при
/// большом битрейте больше потолка (120 + 30 с × 20 Мбит/с ≈ 360 МиБ > 256 МиБ):
/// без этого честная страница, слушающая «полон», упёрлась бы в потолок (ошибка
/// потока). 75 % оставляют четверть потолка (64 МиБ) на сегменты, которые
/// страница уже несла, — это больше самого большого медиасегмента (32 МиБ).
const BUFFER_FULL_AT_CEILING_PERCENT: usize = 75;
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
/// Запас вперёд, после которого источник говорит странице «полон»: около двух
/// минут, как держат сами плееры страниц (YouTube в фоне — 118 с, Tortuga — 120 с).
/// Умолчание; пользовательское значение приходит из config.toml.
const BUFFER_AHEAD: Duration = Duration::from_secs(120);
/// Сколько показанного держать позади позиции: перемотка на полминуты назад —
/// без новой загрузки. Умолчание; пользовательское значение — из config.toml.
const BUFFER_BEHIND: Duration = Duration::from_secs(30);
/// Наложение соседних сегментов, которое ещё не считается наложением (и не
/// выбрасывает старый сегмент с полки). Соседние фрагменты fMP4 стыкуются точно
/// (разметка времени округляет обе границы одинаково), а у кластеров WebM с
/// Opus граница звука может разойтись с началом следующего кластера на
/// миллисекунды округления `TimecodeScale`. 10 мс — с запасом на округление и
/// меньше половины кадра Opus (20 мс): настоящий повтор сегмента всегда больше.
const SHELF_OVERLAP_TOLERANCE: Duration = Duration::from_millis(10);
/// Наибольшая дыра между сегментами, через которую полка всё ещё считает их
/// одним непрерывным куском: сегмент подаётся демуксеру, только если начинается
/// не дальше этого от конца поданного. Дыры у честных плееров страниц — только
/// округление (единицы мс); 100 мс — меньше трёх кадров 30 к/с, и плеер их
/// перешагивает без заметного разрыва, а настоящий пропуск фрагмента (≥ 0,2 с)
/// остаётся дырой, на которой плеер ждёт.
const SHELF_MAX_CONTIGUOUS_GAP: Duration = Duration::from_millis(100);
/// Насколько сегмент WebM с «открытым» концом (длительность последнего кадра не
/// записана, и мерить не по чему — у дорожки один кадр в кластере) может
/// тянуться за начало этого кадра: следующий сегмент, начавшийся не дальше,
/// считается продолжением. 2 с — кадр слайд-шоу 0,5 к/с; при обычных 24–60 к/с
/// конец оценивается по шагу кадров и это правило не нужно.
const SHELF_OPEN_TAIL_REACH: Duration = Duration::from_secs(2);
/// Сколько после перемотки не верить позиции плеера, далёкой от цели: позиция,
/// снятая приложением до перемотки, приходит за миллисекунды после неё; 1 с — с
/// большим запасом на это и не замораживает буфер, если приложение сообщает
/// позицию редко.
const STALE_POSITION_WINDOW: Duration = Duration::from_secs(1);
/// Насколько цель перемотки плеера может отличаться от цели, к которой страница
/// перематывается сама (`seek_started{page_seek}` → `note_page_seek`), чтобы
/// считаться той же перемоткой и не слать странице запрос. Приложение передаёт
/// плееру ту же цель, но плеер вправе её округлить или прижать к шкале; 250 мс —
/// с большим запасом на это и всё ещё меньше любой осмысленной ручной перемотки
/// «в другое место» (шаг клавиш — секунды).
const PAGE_SEEK_MATCH_TOLERANCE: Duration = Duration::from_millis(250);

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
    /// Окно буфера с нулевым запасом вперёд: источник всегда был бы «полон».
    #[error("the buffer window must keep more than zero seconds ahead")]
    ZeroBufferAhead,
}

/// Пределы и сроки одного потока.
#[derive(Debug, Clone, Copy)]
pub struct PushedSourcePolicy {
    /// Пределы размера сегментов для сканера.
    framing: FramingLimits,
    /// Потолок всех байтов, которые держит поток.
    max_held_stream_bytes: NonZeroUsize,
    /// Доля потолка (в процентах, 1–100), с которой источник говорит «полон».
    buffer_full_at_ceiling_percent: usize,
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
    /// Окно буфера (пользовательская настройка).
    buffer_window: BufferWindow,
    /// Допуск наложения соседних сегментов полки.
    shelf_overlap_tolerance: Duration,
    /// Наибольшая дыра внутри непрерывного куска полки.
    shelf_max_contiguous_gap: Duration,
    /// Допуск совпадения цели плеера с целью перемотки страницы.
    page_seek_match_tolerance: Duration,
    /// Насколько сегмент с открытым концом тянется за свой точный конец.
    shelf_open_tail_reach: Duration,
    /// Сколько после перемотки не верить позиции, далёкой от цели.
    stale_position_window: Duration,
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
            buffer_full_at_ceiling_percent: BUFFER_FULL_AT_CEILING_PERCENT,
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
            buffer_window: BufferWindow {
                ahead: BUFFER_AHEAD,
                behind: BUFFER_BEHIND,
            },
            shelf_overlap_tolerance: SHELF_OVERLAP_TOLERANCE,
            shelf_max_contiguous_gap: SHELF_MAX_CONTIGUOUS_GAP,
            page_seek_match_tolerance: PAGE_SEEK_MATCH_TOLERANCE,
            shelf_open_tail_reach: SHELF_OPEN_TAIL_REACH,
            stale_position_window: STALE_POSITION_WINDOW,
        })
    }

    /// Тот же набор с другим окном буфера.
    ///
    /// Умолчание [`Self::recommended`] — 120 с вперёд и 30 с назад; приложение
    /// подставляет сюда значения пользователя из config.toml
    /// (`[browser_session] buffer_ahead_s`, `buffer_behind_s`).
    pub fn with_buffer_window(
        mut self,
        window: BufferWindow,
    ) -> Result<Self, PushedSourcePolicyError> {
        if window.ahead.is_zero() {
            return Err(PushedSourcePolicyError::ZeroBufferAhead);
        }
        self.buffer_window = window;
        Ok(self)
    }

    /// Действующее окно буфера.
    #[must_use]
    pub const fn buffer_window(&self) -> BufferWindow {
        self.buffer_window
    }

    /// Тот же набор с другим потолком удерживаемых байтов потока. Порог «полон»
    /// по байтам ([`Self::buffer_full_at_held_bytes`]) — та же доля нового потолка.
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

    /// С скольких удерживаемых байтов источник говорит «полон»: доля
    /// жёсткого потолка (`BUFFER_FULL_AT_CEILING_PERCENT`, 75 %; не меньше одного байта).
    #[must_use]
    pub fn buffer_full_at_held_bytes(&self) -> usize {
        let ceiling = self.max_held_stream_bytes.get();
        // Делим до умножения: без переполнения при любом потолке; точность до
        // сотой доли потолка здесь не нужна.
        (ceiling / 100)
            .saturating_mul(self.buffer_full_at_ceiling_percent)
            .max(1)
    }

    /// Пределы сканера.
    pub(crate) const fn framing_limits(&self) -> FramingLimits {
        self.framing
    }

    /// Потолок всех байтов, которые держит поток (полка, трубы, init-сегменты).
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

    /// Допуск совпадения цели перемотки плеера с целью перемотки страницы.
    #[must_use]
    pub const fn page_seek_match_tolerance(&self) -> Duration {
        self.page_seek_match_tolerance
    }

    /// Правила буфера потока: полка (окно, допуск наложения, наибольшая дыра,
    /// открытый хвост), допуск совпадения перемоток, порог «полон» в байтах и
    /// окно недоверия к позициям после перемотки.
    pub(crate) fn stream_buffer_rules(&self) -> StreamBufferRules {
        StreamBufferRules {
            shelf: ShelfRules::new(
                self.buffer_window,
                self.shelf_overlap_tolerance,
                self.shelf_max_contiguous_gap,
                self.shelf_open_tail_reach,
            ),
            page_seek_match_tolerance: self.page_seek_match_tolerance,
            full_at_held_bytes: self.buffer_full_at_held_bytes(),
            stale_position_window: self.stale_position_window,
        }
    }
}
