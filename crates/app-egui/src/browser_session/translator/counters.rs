//! Счётчики переходника (для журнала и тестов): только числа, без данных страницы.
//!
//! Вынесены из `translator.rs`, когда к счётчикам файла добавились счётчики ресурсов
//! HLS (10a1): центральный модуль переводчика держится в пределе 800 строк.

/// Счётчики переходника (для журнала и тестов).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranslatorCounters {
    /// Вход старого экземпляра или старого поколения.
    pub(crate) stale_inputs: u64,
    /// Вход поколения, от которого уже отказались.
    pub(crate) rejected_generation_inputs: u64,
    /// Вход экземпляра, который уже просили закрыть.
    pub(crate) closing_instance_inputs: u64,
    /// Сообщения, которые переходник не исполняет (`attach`, если бы он дошёл мимо сервера).
    pub(crate) ignored_messages: u64,
    /// Слова страницы о вариантах (меню, навигация, подсказка субтитров), отданные главному
    /// циклу (14a).
    pub(crate) page_variant_words: u64,
    /// Слова страницы о смене серии на той же странице (`awaiting_next_episode`,
    /// `page_title_changed`), отданные главному циклу (16a).
    pub(crate) episode_change_words: u64,
    /// `tab_gone` страницы: переходник его только отмечает — сеанс закрывает сервер
    /// (4002), а доигрывание полученного ведёт главный цикл.
    pub(crate) tab_gone_messages: u64,
    /// Рамки файла `<video src>`, принятые источником.
    pub(crate) range_frames_accepted: u64,
    /// Байты в принятых рамках файла.
    pub(crate) range_bytes_accepted: u64,
    /// Рамки файла, не нужные больше: запрос отменён перемоткой или заменён повтором,
    /// рамка прежнего файла (источник отбросил их молча) либо пришла, когда файла нет,
    /// но номера в этом сеансе уже выдавались.
    pub(crate) range_frames_stale: u64,
    /// Рамки файла, отвергнутые без нарушения: поколение уже отвергнуто либо файл
    /// запечатан (сеанс кончился).
    pub(crate) range_frames_rejected: u64,
    /// Куски, выброшенные источником: плеер уже закрыл поток.
    pub(crate) discarded_after_consumer_closed: u64,
    /// Разбор входа упал с паникой (дефект): экземпляр закрыт.
    pub(crate) internal_failures: u64,
    /// Документы субтитров страницы, собранные целиком и отданные складу.
    pub(crate) page_subtitle_documents: u64,
    /// Отказы дорожкам субтитров страницы (кусок не по предложению, лишняя дорожка).
    pub(crate) page_subtitle_refusals: u64,
    /// Ресурсы HLS-поколений (рамки и итоги очереди ресурсов).
    pub(crate) resources: HlsResourceCounters,
}

impl TranslatorCounters {
    /// Сообщение страницы, которое переходник не исполняет: отметка в журнале и счётчике.
    pub(crate) fn note_ignored_message(&mut self) {
        tracing::debug!("Page message is not consumed by the stream translator");
        self.ignored_messages += 1;
    }
}

/// Счётчики ресурсов HLS (для журнала и тестов; только числа).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsResourceCounters {
    /// Рамки ресурсов, принятые очередью.
    pub(crate) frames_accepted: u64,
    /// Байты в принятых рамках.
    pub(crate) bytes_accepted: u64,
    /// Рамки и итоги брошенных движком или замолчавших запросов (очередь отбросила молча).
    pub(crate) frames_stale: u64,
}
