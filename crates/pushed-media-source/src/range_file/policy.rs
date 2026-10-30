//! Пределы и сроки «файла по запросам байтов». Каждое число — с объяснением.
//!
//! Настройки сети приходят от приложения (`PrefetchConfig` из `[network]`, бюджет
//! повторов), всё остальное — границы безопасности против враждебной страницы.

use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use demux_api::{DemuxSniffBudget, DemuxSniffBudgetError};
use media_prefetch::PrefetchConfig;

use super::retry::RangeRetryPolicy;

/// Наибольший запрос диапазона, байт. Это **договор протокола моста** (запрос
/// `fetch_range` не длиннее 16 МиБ), а не импорт протокола: crate о протоколе не
/// знает. 16 МиБ — потолок памяти одного запроса; страница, присылающая больше,
/// нарушает договор.
pub const MAX_RANGE_REQUEST_BYTES: u32 = 16 * 1024 * 1024;

/// Срок «подсмотра» начала файла для определения формата: байты в запросе уже
/// просили у страницы, ждём лишь её ответ; 15 с — как таймаут чтения сети.
const DEFAULT_SNIFF_DEADLINE: Duration = Duration::from_secs(15);
/// Самое большое оглавление MP4, которое источник читает целиком ради проверки
/// шифрования: у фильмов на часы `moov` — единицы МиБ; 32 МиБ — с большим запасом.
const DEFAULT_MAX_MOVIE_BYTES: u32 = 32 * 1024 * 1024;

/// Ошибка сборки политики.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushedFilePolicyError {
    /// Запрос длиннее договора протокола.
    #[error("a range request may not exceed {MAX_RANGE_REQUEST_BYTES} bytes, got {requested}")]
    RequestTooLarge {
        /// Запрошенный предел.
        requested: u32,
    },
    /// Первый кусок (для определения формата) не помещается в один запрос.
    #[error(
        "the initial prefetch chunk ({initial_chunk_bytes} bytes) must fit into one range \
         request ({max_request_bytes} bytes)"
    )]
    InitialChunkTooLarge {
        /// Первый кусок prefetch.
        initial_chunk_bytes: u64,
        /// Предел запроса.
        max_request_bytes: u32,
    },
    /// Срок определения формата нулевой.
    #[error(transparent)]
    SniffDeadline(#[from] DemuxSniffBudgetError),
}

/// Пределы и сроки одного файла.
#[derive(Debug, Clone, Copy)]
pub struct PushedFilePolicy {
    /// Окно предзагрузки (то же, что у обычного progressive HTTP).
    prefetch: PrefetchConfig,
    /// Самый длинный запрос к странице.
    max_request_bytes: NonZeroU32,
    /// Повторы после сетевых сбоев.
    retry: RangeRetryPolicy,
    /// Срок определения формата.
    sniff_deadline: Duration,
    /// Самое большое оглавление MP4, читаемое целиком.
    max_movie_bytes: u32,
}

impl PushedFilePolicy {
    /// Политика из окна предзагрузки: запрос — один кусок prefetch, но не больше
    /// договора протокола.
    ///
    /// # Errors
    /// Первый кусок prefetch не помещается в запрос (кусок больше 16 МиБ).
    pub fn new(
        prefetch: PrefetchConfig,
        retry: RangeRetryPolicy,
    ) -> Result<Self, PushedFilePolicyError> {
        let chunk = u32::try_from(prefetch.chunk_bytes())
            .unwrap_or(MAX_RANGE_REQUEST_BYTES)
            .min(MAX_RANGE_REQUEST_BYTES);
        let max_request_bytes = NonZeroU32::new(chunk).unwrap_or(NonZeroU32::MIN);
        Self {
            prefetch,
            max_request_bytes,
            retry,
            sniff_deadline: DEFAULT_SNIFF_DEADLINE,
            max_movie_bytes: DEFAULT_MAX_MOVIE_BYTES,
        }
        .validated()
    }

    /// Умолчания для приложения без своих настроек: окно по умолчанию `media-prefetch`,
    /// повторы на минуту.
    ///
    /// # Errors
    /// Невозможна при умолчаниях; тип ошибки общий с [`Self::new`].
    pub fn recommended() -> Result<Self, PushedFilePolicyError> {
        Self::new(
            PrefetchConfig::default(),
            RangeRetryPolicy::with_budget(Duration::from_secs(60)),
        )
    }

    /// Свой предел запроса (не больше [`MAX_RANGE_REQUEST_BYTES`]).
    ///
    /// # Errors
    /// Предел больше договора протокола или меньше первого куска prefetch.
    pub fn with_max_request_bytes(
        mut self,
        max_request_bytes: NonZeroU32,
    ) -> Result<Self, PushedFilePolicyError> {
        self.max_request_bytes = max_request_bytes;
        self.validated()
    }

    /// Свой срок определения формата.
    ///
    /// # Errors
    /// Нулевой срок.
    pub fn with_sniff_deadline(
        mut self,
        deadline: Duration,
    ) -> Result<Self, PushedFilePolicyError> {
        self.sniff_deadline = deadline;
        self.validated()
    }

    /// Свой предел оглавления MP4.
    #[must_use]
    pub const fn with_max_movie_bytes(mut self, max_movie_bytes: u32) -> Self {
        self.max_movie_bytes = max_movie_bytes;
        self
    }

    /// Проверка инвариантов политики.
    fn validated(self) -> Result<Self, PushedFilePolicyError> {
        let max = self.max_request_bytes.get();
        if max > MAX_RANGE_REQUEST_BYTES {
            return Err(PushedFilePolicyError::RequestTooLarge { requested: max });
        }
        if self.prefetch.initial_chunk_bytes() > u64::from(max) {
            return Err(PushedFilePolicyError::InitialChunkTooLarge {
                initial_chunk_bytes: self.prefetch.initial_chunk_bytes(),
                max_request_bytes: max,
            });
        }
        // Нулевой срок отвергает сам бюджет; проверяем заранее, а не при открытии.
        self.sniff_budget().map(|_| self)
    }

    /// Окно предзагрузки: то же, что приложение даёт обычному progressive-файлу.
    #[must_use]
    pub const fn prefetch(&self) -> PrefetchConfig {
        self.prefetch
    }

    /// Самый длинный запрос к странице.
    #[must_use]
    pub const fn max_request_bytes(&self) -> NonZeroU32 {
        self.max_request_bytes
    }

    /// Политика повторов.
    #[must_use]
    pub const fn retry(&self) -> RangeRetryPolicy {
        self.retry
    }

    /// Первый запрос открытия: смещение 0, длина — начальный кусок prefetch.
    pub(super) fn initial_request_bytes(&self) -> u32 {
        // Проверено в `validated`: помещается в один запрос.
        u32::try_from(self.prefetch.initial_chunk_bytes()).unwrap_or(MAX_RANGE_REQUEST_BYTES)
    }

    /// Предел оглавления MP4.
    pub(super) const fn max_movie_bytes(&self) -> u32 {
        self.max_movie_bytes
    }

    /// Бюджет определения формата: читаем не больше первого куска.
    pub(super) fn sniff_budget(&self) -> Result<DemuxSniffBudget, PushedFilePolicyError> {
        let max_bytes = usize::try_from(self.prefetch.initial_chunk_bytes())
            .ok()
            .and_then(NonZeroUsize::new)
            .unwrap_or(NonZeroUsize::MIN);
        Ok(DemuxSniffBudget::new(
            max_bytes,
            NonZeroUsize::MIN,
            self.sniff_deadline,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefetch(initial: u64, chunk: u64) -> PrefetchConfig {
        PrefetchConfig::new(initial, chunk, chunk * 4).expect("prefetch config")
    }

    #[test]
    fn request_is_one_prefetch_chunk_capped_by_the_protocol_contract() {
        let small = PushedFilePolicy::new(prefetch(1024, 64 * 1024), RangeRetryPolicy::disabled())
            .expect("policy");
        assert_eq!(small.max_request_bytes().get(), 64 * 1024);
        assert_eq!(small.initial_request_bytes(), 1024);

        let huge = PushedFilePolicy::new(
            prefetch(1024, 64 * 1024 * 1024),
            RangeRetryPolicy::disabled(),
        )
        .expect("policy");
        assert_eq!(huge.max_request_bytes().get(), MAX_RANGE_REQUEST_BYTES);
    }

    #[test]
    fn request_limit_above_the_contract_is_rejected_with_a_clear_error() {
        let policy = PushedFilePolicy::recommended().expect("policy");
        let too_large = NonZeroU32::new(MAX_RANGE_REQUEST_BYTES + 1).expect("non-zero");
        assert_eq!(
            policy.with_max_request_bytes(too_large).err(),
            Some(PushedFilePolicyError::RequestTooLarge {
                requested: MAX_RANGE_REQUEST_BYTES + 1
            })
        );
    }

    #[test]
    fn initial_chunk_larger_than_a_request_is_rejected() {
        let policy = PushedFilePolicy::recommended().expect("policy");
        let tiny = NonZeroU32::new(1024).expect("non-zero");
        assert!(matches!(
            policy.with_max_request_bytes(tiny),
            Err(PushedFilePolicyError::InitialChunkTooLarge { .. })
        ));
    }

    #[test]
    fn zero_sniff_deadline_is_rejected() {
        let policy = PushedFilePolicy::recommended().expect("policy");
        assert!(matches!(
            policy.with_sniff_deadline(Duration::ZERO),
            Err(PushedFilePolicyError::SniffDeadline(_))
        ));
    }
}
