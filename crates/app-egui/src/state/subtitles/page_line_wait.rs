//! «Жду документ строки меню» (сессия 15a2): как выбор строки меню страницы превращается в
//! выбор зрителя, когда документ субтитров этой строки приходит.
//!
//! # Зачем
//! Строка меню CC — просьба к странице («включи немецкий»). Плеер сам документ не просит:
//! страница (YouTube) скачивает его и присылает обычным путём 13b как `page_download` со
//! `showing`. Чтобы пришедший документ не остался «просто дорожкой, которую зритель не
//! выбирал» (тогда `showing` страницы проигрывает прежнему выбору зрителя, а в память
//! расширения ничего не уходит), выбор строки ставит ожидание. Первый подходящий документ
//! после него считается выбором зрителя: субтитры включаются, дорожка запоминается на сеанс,
//! страница получает `subtitle_choice`.
//!
//! # Какой документ подходит
//! **С подписью выбранной строки.** Подпись строки меню и подпись скачанного документа пишет одна
//! функция экстеншена (`caption-label.ts`): «English», «English (auto-generated)», «German
//! (auto-translated)». Поэтому документ узнаётся по подписи, а не по порядку прихода: загрузки
//! страницы обгоняют друг друга (документ прежнего языка дошёл до владельца уже после выбора, зритель
//! быстро жмёт V), и «первый пришедший» стал бы выбором не той строки (живой прогон 15a2).
//! И только **новее запроса**: поступление склада больше того, что владелец видел в момент
//! выбора. Так документ, пришедший раньше (в том числе тот, что склад заново отдаёт новому
//! поколению медиа после рекламы), ожидание не закрывает. Замена той же дорожки (en → de →
//! en) получает новое поступление, поэтому подходит так же, как новая дорожка.
//! Документ обязан быть скачан страницей (`page_download`) и показываться на ней (`showing`):
//! остальные дорожки страницы выбором строки не становятся.
//!
//! # Когда ожидание снимается
//! Принято (выбор зрителя сделан), зритель выбрал другую дорожку или «выкл», запросил следующую
//! серию, начался новый сеанс моста (новый склад). Пока ждём, прежний показ не трогается.
//!
//! # Срок (решение владельца 25.11.2026: «окно ждёт и говорит»)
//! Если текст не пришёл за [`PageDocumentWaitPolicy::limit`], ожидание не висит вечно: владелец
//! зовёт [`PageDocumentWait::expire_if_due`]. Исходов два. «Текст не пришёл» — страница не
//! прислала НИ ОДНОГО скачанного документа новее запроса: состояние `TextNotArrived`, строка в
//! меню не отмечается выбранной, зрителю тост. «Пришёл другой документ» — документ приходил, но с
//! другой подписью: его покажет обычное правило «страница показывает», надпись «не пришли» была
//! бы враньём, поэтому только журнал. `TextNotArrived` снимают новый выбор строки, `cancel` и
//! приход любого нового скачанного документа. Команд странице по истечении срока не шлём.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::page_subtitles::{
    PageSubtitleArrival, PageSubtitleDisplay, PageSubtitleSource, PageSubtitleTrackNumber,
    ShelvedPageSubtitle,
};

/// Сколько ждать документ выбранной строки, прежде чем честно сказать зрителю «не пришёл».
///
/// Документ субтитров — килобайты, YouTube вживую отвечал за ~40 мс; 10 секунд — запас на
/// медленную сеть и на загруженную страницу. Дольше ждать нельзя: зритель видит тост
/// «выбрано» и пустой экран, а молчание дольше десяти секунд читается как «сломалось».
const DEFAULT_DOCUMENT_WAIT_LIMIT: Duration = Duration::from_secs(10);

/// Политика ожидания документа строки (подменяется в тестах; в конфиг не выносится).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageDocumentWaitPolicy {
    /// Сколько ждать документ после выбора строки.
    pub(super) limit: Duration,
}

impl Default for PageDocumentWaitPolicy {
    fn default() -> Self {
        Self {
            limit: DEFAULT_DOCUMENT_WAIT_LIMIT,
        }
    }
}

/// Видели ли за время ожидания хоть один документ, скачанный страницей, новее запроса.
///
/// Судим по ПРИХОДУ, а не по принятию: документ мог прийти с другой подписью (две родные
/// дорожки одного языка, известное ограничение 15a2) — тогда его покажет обычное правило
/// «страница показывает», и надпись «текст не пришёл» была бы ложью.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageDownloadsSeen {
    /// Ни одного.
    NoneYet,
    /// Хотя бы один.
    AtLeastOne,
}

/// Чем кончилась проверка срока ожидания ([`PageDocumentWait::expire_if_due`]).
///
/// Подписи строки в исходе нет намеренно: исход идёт в журнал, а подписи страницы в журнал не
/// попадают (инвариант 7 моста — текст страницы остаётся у зрителя).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageDocumentWaitExpiry {
    /// Не ждём или срок ещё не вышел: ничего не произошло.
    NotDue,
    /// Срок вышел, а страница не прислала ни одного скачанного документа: зрителю нужно
    /// сказать об этом, строка перестаёт выглядеть выбранной.
    TextNotArrived {
        /// Сколько ждали.
        waited: Duration,
    },
    /// Срок вышел, но страница присылала другой документ (не с подписью строки): субтитры
    /// покажет обычное правило страницы; зрителю ничего, только журнал.
    AnotherDocumentArrived {
        /// Сколько ждали.
        waited: Duration,
    },
}

/// Фаза ожидания.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum WaitPhase {
    /// Строку не выбирали (или ожидание снято).
    #[default]
    NotWaiting,
    /// Строку выбрали: подходит документ с её подписью и поступлением новее `newer_than`.
    Waiting {
        /// Самое позднее поступление, которое владелец видел в момент выбора (`None` — склад
        /// тогда был пуст: подойдёт любое).
        newer_than: Option<PageSubtitleArrival>,
        /// Подпись выбранной строки (без пробелов по краям): ею же экстеншен подписывает документ.
        line_label: Arc<str>,
        /// Когда строку выбрали.
        started_at: Instant,
        /// Момент, после которого ждать больше не будем.
        deadline: Instant,
        /// Приходили ли скачанные страницей документы новее запроса.
        downloads: PageDownloadsSeen,
    },
    /// Срок вышел, текст не пришёл: страница строку «выбрала», но показывать нечего.
    TextNotArrived {
        /// Подпись строки без документа.
        line_label: Arc<str>,
        /// Поступление запроса: любой скачанный документ новее него снимает состояние.
        newer_than: Option<PageSubtitleArrival>,
    },
}

/// Ждёт ли владелец документ выбранной строки; хозяин срока ожидания (см. описание модуля).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct PageDocumentWait {
    /// Сколько ждать.
    policy: PageDocumentWaitPolicy,
    /// Текущая фаза.
    phase: WaitPhase,
}

/// Поступление `arrival` новее запроса, отсчитанного от `newer_than`.
fn is_newer(arrival: PageSubtitleArrival, newer_than: Option<PageSubtitleArrival>) -> bool {
    newer_than.is_none_or(|seen| arrival > seen)
}

impl PageDocumentWait {
    /// Ожидание с другой политикой (тесты).
    #[cfg(test)]
    pub(super) fn with_policy(policy: PageDocumentWaitPolicy) -> Self {
        Self {
            policy,
            phase: WaitPhase::NotWaiting,
        }
    }

    /// Начать ждать документ строки с подписью `line_label`, новее `newer_than`; выбор сделан в
    /// момент `now` (повторный выбор заменяет прежнее ожидание и снимает «текст не пришёл»).
    pub(super) fn begin(
        &mut self,
        newer_than: Option<PageSubtitleArrival>,
        line_label: &str,
        now: Instant,
    ) {
        self.phase = WaitPhase::Waiting {
            newer_than,
            line_label: Arc::from(line_label.trim()),
            started_at: now,
            deadline: now + self.policy.limit,
            downloads: PageDownloadsSeen::NoneYet,
        };
    }

    /// Снять ожидание (и состояние «текст не пришёл»).
    pub(super) fn cancel(&mut self) {
        self.phase = WaitPhase::NotWaiting;
    }

    /// Ждём ли сейчас (проверяют тесты).
    #[cfg(test)]
    pub(super) fn is_waiting(&self) -> bool {
        matches!(self.phase, WaitPhase::Waiting { .. })
    }

    /// Момент, когда ожидание истечёт; `None` — не ждём (проверяют тесты: срок замечает обычный
    /// проход окна, отдельного будильника кадра у него нет).
    #[cfg(test)]
    pub(super) fn deadline(&self) -> Option<Instant> {
        match &self.phase {
            WaitPhase::Waiting { deadline, .. } => Some(*deadline),
            WaitPhase::NotWaiting | WaitPhase::TextNotArrived { .. } => None,
        }
    }

    /// Держим ли отметку строки с подписью `label` снятой: срок вышел, текст не пришёл.
    pub(super) fn withholds_line_mark(&self, label: &str) -> bool {
        matches!(
            &self.phase,
            WaitPhase::TextNotArrived { line_label, .. } if line_label.as_ref() == label.trim()
        )
    }

    /// Проверка срока в момент `now`. Исход «текст не пришёл» или «пришёл другой документ»
    /// отдаётся ровно один раз: ожидание при этом закончено.
    pub(super) fn expire_if_due(&mut self, now: Instant) -> PageDocumentWaitExpiry {
        let WaitPhase::Waiting {
            newer_than,
            line_label,
            started_at,
            deadline,
            downloads,
        } = &self.phase
        else {
            return PageDocumentWaitExpiry::NotDue;
        };
        if now < *deadline {
            return PageDocumentWaitExpiry::NotDue;
        }
        let line_label = Arc::clone(line_label);
        let waited = now.saturating_duration_since(*started_at);
        match downloads {
            PageDownloadsSeen::AtLeastOne => {
                self.phase = WaitPhase::NotWaiting;
                PageDocumentWaitExpiry::AnotherDocumentArrived { waited }
            }
            PageDownloadsSeen::NoneYet => {
                self.phase = WaitPhase::TextNotArrived {
                    line_label,
                    newer_than: *newer_than,
                };
                PageDocumentWaitExpiry::TextNotArrived { waited }
            }
        }
    }

    /// Наблюдает новые документы склада и отвечает на ожидание: дорожка страницы, которой
    /// отвечает ожидание, — последняя по поступлению из подходящих (см. описание модуля);
    /// `None` — не ждём или подходящих нет.
    ///
    /// Побочно запоминает, что страница прислала скачанный документ новее запроса (подпись и
    /// показ для этого не важны), а в состоянии «текст не пришёл» такой документ снимает его:
    /// отметка строки дальше решается обычным правилом. Ответом документ после срока не
    /// становится: ожидание уже снято.
    pub(super) fn answering_track(
        &mut self,
        fresh: &[&ShelvedPageSubtitle],
    ) -> Option<PageSubtitleTrackNumber> {
        let newer_downloads = |newer_than: Option<PageSubtitleArrival>| {
            fresh.iter().any(|entry| {
                entry.document.source == PageSubtitleSource::PageDownload
                    && is_newer(entry.arrival, newer_than)
            })
        };
        match &mut self.phase {
            WaitPhase::NotWaiting => None,
            WaitPhase::TextNotArrived { newer_than, .. } => {
                if newer_downloads(*newer_than) {
                    self.phase = WaitPhase::NotWaiting;
                }
                None
            }
            WaitPhase::Waiting {
                newer_than,
                line_label,
                downloads,
                ..
            } => {
                if newer_downloads(*newer_than) {
                    *downloads = PageDownloadsSeen::AtLeastOne;
                }
                fresh
                    .iter()
                    .filter(|entry| is_newer(entry.arrival, *newer_than))
                    .filter(|entry| entry.document.label.text() == Some(line_label.as_ref()))
                    .filter(|entry| entry.document.source == PageSubtitleSource::PageDownload)
                    .filter(|entry| entry.document.display == PageSubtitleDisplay::ShowingOnPage)
                    .max_by_key(|entry| entry.arrival)
                    .map(|entry| entry.document.track)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::page_subtitles::{
        PageSubtitleDocument, PageSubtitleFormatHint, PageSubtitleLabel, PageSubtitleLanguage,
    };

    /// Момент выбора строки в тестах (время передаётся параметром, как в проде).
    fn start() -> Instant {
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now)
    }

    /// Политика теста: предел в 10 секунд, чтобы тесты не зависели от значения по умолчанию.
    const LIMIT: Duration = Duration::from_secs(10);

    fn wait_with_limit() -> PageDocumentWait {
        PageDocumentWait::with_policy(PageDocumentWaitPolicy { limit: LIMIT })
    }

    fn labelled(mut entry: ShelvedPageSubtitle, label: &str) -> ShelvedPageSubtitle {
        Arc::get_mut(&mut entry.document)
            .expect("fresh document")
            .label = PageSubtitleLabel::Text(label.to_owned());
        entry
    }

    fn arrival(number: u32) -> PageSubtitleArrival {
        (0..number).fold(PageSubtitleArrival::FIRST, |current, _| current.next())
    }

    fn shelved(
        track: u32,
        arrival_number: u32,
        source: PageSubtitleSource,
        display: PageSubtitleDisplay,
    ) -> ShelvedPageSubtitle {
        ShelvedPageSubtitle {
            arrival: arrival(arrival_number),
            document: Arc::new(PageSubtitleDocument {
                track: PageSubtitleTrackNumber::new(track),
                language: PageSubtitleLanguage::NotNamed,
                label: PageSubtitleLabel::Text("German".to_owned()),
                format_hint: PageSubtitleFormatHint::WebVtt,
                display,
                source,
                bytes: Arc::from(b"WEBVTT".as_slice()),
            }),
        }
    }

    fn download(track: u32, arrival_number: u32) -> ShelvedPageSubtitle {
        shelved(
            track,
            arrival_number,
            PageSubtitleSource::PageDownload,
            PageSubtitleDisplay::ShowingOnPage,
        )
    }

    #[test]
    fn a_document_with_another_label_does_not_answer_the_line() {
        let mut wait = PageDocumentWait::default();
        wait.begin(None, " German ", start());
        let mut other = download(1, 1);
        Arc::get_mut(&mut other.document)
            .expect("fresh document")
            .label = PageSubtitleLabel::Text("French".to_owned());
        let german = download(2, 2);
        assert_eq!(wait.answering_track(&[&other]), None);
        assert_eq!(
            wait.answering_track(&[&other, &german]),
            Some(PageSubtitleTrackNumber::new(2)),
            "the label is compared without surrounding spaces"
        );
    }

    #[test]
    fn not_waiting_answers_nothing_even_for_a_fitting_document() {
        let entry = download(1, 5);
        assert_eq!(PageDocumentWait::default().answering_track(&[&entry]), None);
    }

    #[test]
    fn only_documents_newer_than_the_request_answer_it() {
        let mut wait = PageDocumentWait::default();
        wait.begin(Some(arrival(3)), "German", start());
        let old = download(1, 3);
        let new = download(2, 4);
        assert_eq!(
            wait.answering_track(&[&old]),
            None,
            "seen before the choice"
        );
        assert_eq!(
            wait.answering_track(&[&old, &new]),
            Some(PageSubtitleTrackNumber::new(2))
        );
    }

    #[test]
    fn an_empty_shelf_at_request_time_accepts_any_arrival() {
        let mut wait = PageDocumentWait::default();
        wait.begin(None, "German", start());
        assert_eq!(
            wait.answering_track(&[&download(7, 0)]),
            Some(PageSubtitleTrackNumber::new(7))
        );
    }

    #[test]
    fn only_shown_page_downloads_answer_and_the_latest_wins() {
        let mut wait = PageDocumentWait::default();
        wait.begin(None, "German", start());
        let element = shelved(
            1,
            1,
            PageSubtitleSource::PageElement,
            PageSubtitleDisplay::ShowingOnPage,
        );
        let hidden = shelved(
            2,
            2,
            PageSubtitleSource::PageDownload,
            PageSubtitleDisplay::NotShowingOnPage,
        );
        assert_eq!(wait.answering_track(&[&element, &hidden]), None);
        let first = download(3, 3);
        let second = download(4, 4);
        assert_eq!(
            wait.answering_track(&[&first, &element, &second]),
            Some(PageSubtitleTrackNumber::new(4)),
            "the latest fitting document wins"
        );
    }

    #[test]
    fn cancel_stops_waiting_and_begin_replaces_the_previous_request() {
        let mut wait = PageDocumentWait::default();
        wait.begin(Some(arrival(1)), "German", start());
        assert!(wait.is_waiting());
        wait.begin(Some(arrival(9)), "German", start());
        assert_eq!(wait.answering_track(&[&download(1, 5)]), None);
        wait.cancel();
        assert!(!wait.is_waiting());
        assert_eq!(wait.answering_track(&[&download(1, 20)]), None);
    }

    #[test]
    fn the_deadline_is_the_choice_moment_plus_the_limit_and_absent_outside_waiting() {
        let mut wait = wait_with_limit();
        assert_eq!(wait.deadline(), None);
        wait.begin(None, "German", start());
        assert_eq!(wait.deadline(), Some(start() + LIMIT));
        wait.cancel();
        assert_eq!(wait.deadline(), None);
    }

    #[test]
    fn the_default_limit_is_ten_seconds() {
        let mut wait = PageDocumentWait::default();
        wait.begin(None, "German", start());
        assert_eq!(wait.deadline(), Some(start() + Duration::from_secs(10)));
    }

    #[test]
    fn nothing_downloaded_by_the_deadline_reports_text_not_arrived_once_and_withholds_the_mark() {
        let mut wait = wait_with_limit();
        wait.begin(None, " German ", start());
        let before = start() + LIMIT - Duration::from_millis(1);
        assert_eq!(wait.expire_if_due(before), PageDocumentWaitExpiry::NotDue);
        assert!(!wait.withholds_line_mark("German"), "still waiting");

        let at_deadline = start() + LIMIT;
        assert_eq!(
            wait.expire_if_due(at_deadline),
            PageDocumentWaitExpiry::TextNotArrived { waited: LIMIT }
        );
        assert!(wait.withholds_line_mark("German"));
        assert!(
            wait.withholds_line_mark(" German "),
            "labels compare trimmed"
        );
        assert!(!wait.withholds_line_mark("French"), "only the chosen line");
        assert_eq!(wait.deadline(), None, "no more wake-ups");
        assert_eq!(
            wait.expire_if_due(at_deadline + LIMIT),
            PageDocumentWaitExpiry::NotDue,
            "reported exactly once"
        );
        assert!(
            wait.withholds_line_mark("German"),
            "the mark stays withheld"
        );
    }

    #[test]
    fn a_download_with_another_label_before_the_deadline_is_logged_but_not_announced() {
        let mut wait = wait_with_limit();
        wait.begin(Some(arrival(3)), "German", start());
        let french = labelled(download(1, 4), "French");
        assert_eq!(wait.answering_track(&[&french]), None);
        assert_eq!(
            wait.expire_if_due(start() + LIMIT),
            PageDocumentWaitExpiry::AnotherDocumentArrived { waited: LIMIT }
        );
        assert!(!wait.withholds_line_mark("German"), "the page shows it");
        assert_eq!(wait.deadline(), None);
    }

    #[test]
    fn documents_seen_before_the_request_or_not_downloaded_do_not_count_as_arrived() {
        let mut wait = wait_with_limit();
        wait.begin(Some(arrival(3)), "German", start());
        let old = labelled(download(1, 3), "French");
        let element = labelled(
            shelved(
                2,
                4,
                PageSubtitleSource::PageElement,
                PageSubtitleDisplay::ShowingOnPage,
            ),
            "French",
        );
        assert_eq!(wait.answering_track(&[&old, &element]), None);
        assert!(matches!(
            wait.expire_if_due(start() + LIMIT),
            PageDocumentWaitExpiry::TextNotArrived { .. }
        ));
    }

    #[test]
    fn the_line_document_before_the_deadline_answers_and_after_the_wait_ends_the_deadline_is_silent()
     {
        let mut wait = wait_with_limit();
        wait.begin(None, "German", start());
        assert_eq!(
            wait.answering_track(&[&download(2, 1)]),
            Some(PageSubtitleTrackNumber::new(2))
        );
        // Владелец принял ответ и снял ожидание (`note_track`).
        wait.cancel();
        assert_eq!(
            wait.expire_if_due(start() + LIMIT * 2),
            PageDocumentWaitExpiry::NotDue
        );
        assert!(!wait.withholds_line_mark("German"));
    }

    #[test]
    fn the_line_document_after_the_deadline_does_not_answer_and_releases_the_mark() {
        let mut wait = wait_with_limit();
        wait.begin(None, "German", start());
        let _ = wait.expire_if_due(start() + LIMIT);
        assert!(wait.withholds_line_mark("German"));
        assert_eq!(
            wait.answering_track(&[&download(2, 1)]),
            None,
            "the wait is already over: a late document is an ordinary page track"
        );
        assert!(
            !wait.withholds_line_mark("German"),
            "the mark follows the usual rule"
        );
        assert_eq!(
            wait.expire_if_due(start() + LIMIT * 3),
            PageDocumentWaitExpiry::NotDue
        );
    }

    #[test]
    fn a_late_old_arrival_does_not_release_the_withheld_mark() {
        let mut wait = wait_with_limit();
        wait.begin(Some(arrival(5)), "German", start());
        let _ = wait.expire_if_due(start() + LIMIT);
        // Документ, который склад заново отдаёт новому поколению: он старше запроса.
        assert_eq!(wait.answering_track(&[&download(1, 2)]), None);
        assert!(wait.withholds_line_mark("German"));
    }

    #[test]
    fn cancel_and_a_new_choice_both_clear_text_not_arrived() {
        let mut wait = wait_with_limit();
        wait.begin(None, "German", start());
        let _ = wait.expire_if_due(start() + LIMIT);
        wait.cancel();
        assert!(!wait.withholds_line_mark("German"));

        wait.begin(None, "German", start());
        let _ = wait.expire_if_due(start() + LIMIT);
        let second_choice = start() + LIMIT * 2;
        wait.begin(None, "French", second_choice);
        assert!(
            !wait.withholds_line_mark("German"),
            "a new choice clears it"
        );
        assert!(wait.is_waiting());
        assert_eq!(wait.deadline(), Some(second_choice + LIMIT));
    }
}
