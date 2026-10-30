//! Шов «переходник ↔ файл по запросам байтов» (`<video src>`, сессия 03c).
//!
//! Тот же приём, что у [`super::stream_port`]: переходник видит писателя файла через
//! трейт [`FileWriterPort`] и берёт новый файл на каждое поколение-файл у
//! [`FileFactory`]. Настоящая реализация — `pushed_file` из `pushed-media-source`,
//! тесты подставляют запись вызовов.

use std::sync::Arc;

use media_prefetch::PrefetchConfig;
use pushed_media_source::{
    PushedFileError, PushedFileOpener, PushedFilePolicy, PushedFileRequests, PushedFileWriter,
    RangeBytesVerdict, RangeOutcome, RangeRequestId, RangeRequestIds, RangeRequestNotifier,
    pushed_file,
};

/// Сторона приёма файла так, как её видит переходник. Настоящая —
/// [`PushedFileWriter`].
pub(crate) trait FileWriterPort: Send {
    /// Рамка байтов запроса `id`, начинающаяся с `offset`.
    ///
    /// # Errors
    /// Нарушение порядка рамок, прежняя ошибка файла или файл уже запечатан.
    fn accept_range_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<RangeBytesVerdict, PushedFileError>;
    /// Итог запроса `id` (`range_fetch_finished`).
    ///
    /// # Errors
    /// Нарушение, прежняя ошибка файла или окончательный отказ источника.
    fn finish_request(
        &mut self,
        id: RangeRequestId,
        outcome: RangeOutcome,
    ) -> Result<(), PushedFileError>;
    /// «Больше байтов не будет» (сеанс кончился): полученное доигрывается.
    fn seal(&mut self);
}

impl FileWriterPort for PushedFileWriter {
    fn accept_range_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<RangeBytesVerdict, PushedFileError> {
        Self::accept_range_bytes(self, id, offset, payload)
    }

    fn finish_request(
        &mut self,
        id: RangeRequestId,
        outcome: RangeOutcome,
    ) -> Result<(), PushedFileError> {
        Self::finish_request(self, id, outcome)
    }

    fn seal(&mut self) {
        Self::seal(self);
    }
}

/// Только что созданный файл: писатель остаётся у переходника, остальное уходит
/// главному циклу в [`super::super::intake_notice::GenerationHandoff`].
pub(crate) struct StartedFile {
    /// Сторона приёма.
    pub(crate) writer: Box<dyn FileWriterPort>,
    /// Сторона открытия.
    pub(crate) opener: PushedFileOpener,
    /// Очередь запросов страницы (`fetch_range`).
    pub(crate) requests: PushedFileRequests,
    /// Окно предзагрузки файла: то же, что в его политике. Им главный цикл оборачивает
    /// открытый файл в фоновый демуксер — второго источника этого значения нет.
    pub(crate) prefetch: PrefetchConfig,
}

/// Откуда переходник берёт новый файл на каждое поколение-файл.
pub(crate) trait FileFactory: Send {
    /// Создаёт файл. `ids` — выдаватель номеров запросов, общий на весь сеанс:
    /// номера сквозные между поколениями-файлами, поэтому запоздавшая рамка прежнего
    /// файла узнаётся источником как устаревшая, а не как нарушение.
    fn start_file(&mut self, ids: RangeRequestIds) -> StartedFile;
}

/// Настоящая фабрика: `pushed_file(policy, ids, notifier)`. Политика (окно
/// предзагрузки и повторы из `[network]`) собрана вызывающим; `notifier` будит главный
/// цикл, когда появился запрос байтов, — переходник конфиг не читает и потоков не знает.
pub(crate) struct PushedFileFactory {
    /// Политика каждого нового файла.
    policy: PushedFilePolicy,
    /// Будильник главного цикла.
    notifier: Arc<dyn RangeRequestNotifier>,
}

impl PushedFileFactory {
    /// Фабрика с политикой `policy` и будильником `notifier`.
    pub(crate) fn new(policy: PushedFilePolicy, notifier: Arc<dyn RangeRequestNotifier>) -> Self {
        Self { policy, notifier }
    }
}

impl FileFactory for PushedFileFactory {
    fn start_file(&mut self, ids: RangeRequestIds) -> StartedFile {
        let (writer, opener, requests) = pushed_file(self.policy, ids, Arc::clone(&self.notifier));
        StartedFile {
            writer: Box::new(writer),
            opener,
            requests,
            prefetch: self.policy.prefetch(),
        }
    }
}

/// Фабрика для тестов, которым файл не нужен: настоящий источник с рекомендованной
/// политикой и будильником, который никого не будит.
#[cfg(test)]
pub(crate) fn unused_file_factory() -> Box<dyn FileFactory> {
    struct SilentNotifier;
    impl RangeRequestNotifier for SilentNotifier {
        fn range_requested(&self) {}
    }
    let policy = PushedFilePolicy::recommended().expect("recommended file policy");
    Box::new(PushedFileFactory::new(policy, Arc::new(SilentNotifier)))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pushed_media_source::RangeRetryPolicy;

    use super::*;

    struct Silent;
    impl RangeRequestNotifier for Silent {
        fn range_requested(&self) {}
    }

    /// Политика (то есть `[network]`) доходит до настоящего источника: окно предзагрузки
    /// уходит главному циклу вместе с файлом, а первый запрос к странице по размеру
    /// равен первому куску из политики, а не умолчанию.
    #[test]
    fn the_policy_window_reaches_the_file_and_sizes_its_first_request() {
        let prefetch = PrefetchConfig::new(8 * 1024, 16 * 1024, 64 * 1024).expect("prefetch");
        let policy = PushedFilePolicy::new(
            prefetch,
            RangeRetryPolicy::with_budget(Duration::from_secs(1)),
        )
        .expect("policy");
        let mut factory = PushedFileFactory::new(policy, Arc::new(Silent));

        let mut started = factory.start_file(RangeRequestIds::new());

        assert_eq!(started.prefetch, prefetch);
        let _progress = started.opener.poll_open();
        let deadline = Instant::now() + Duration::from_secs(10);
        let first = loop {
            if let Some(request) = started.requests.take_requests().first() {
                break *request;
            }
            assert!(Instant::now() < deadline, "the first request never came");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!((first.offset, first.length.get()), (0, 8 * 1024));
        assert_ne!(prefetch, PrefetchConfig::default());
    }
}
