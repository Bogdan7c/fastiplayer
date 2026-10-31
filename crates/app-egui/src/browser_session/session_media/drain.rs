//! Доигрывание после ухода вкладки (решение 42, сессия 03c моста).
//!
//! Сеанс закончился концом, после которого зритель ещё смотрит скачанное (`tab_gone`,
//! пропажа экстеншена без возврата): процесс не выходит, пока плеер не доиграет
//! полученное. Машина «когда выходить» (`ProcessLifecycle`) сама сроков не знает и по
//! времени не будит — она ждёт ответа «доиграно» от медиа-части. Этот модуль и есть
//! ответ: чистые правила [`DrainWatch::evaluate`] поверх снимка [`DrainContent`], без
//! плеера и часов ОС, поэтому каждое правило проверяется без потоков.
//!
//! Сначала ждём, пока поток приёма разберёт всё, что экземпляр успел прислать, и
//! запечатает его ([`IntakeStance::Sealed`], уведомление `InstanceSealed`). Событие
//! конца сеанса идёт в ящик НАПРЯМУЮ и может обогнать данные (поколение, намерение
//! «пауза»), которые ещё лежат в очереди потока приёма: решать «нечего играть» или
//! «страница на паузе» до запечатывания значит выйти раньше, чем видео успело появиться.
//! До запечатывания действует только страховка по сроку (правило 6).
//!
//! Правила по порядку, когда экземпляр запечатан (каждое — свой [`DrainEnd`], в журнал
//! одной строкой):
//! 1. ничего не загружено и не открывается → сразу ([`DrainEnd::NothingToPlay`]);
//! 2. страница (или зритель окна) хочет паузу — по политике [`PausedDrain`]:
//!    - [`PausedDrain::ExitAtOnce`] (безоконный процесс, правило 103) → сразу
//!      ([`DrainEnd::PagePaused`]): нажать «играть» некому;
//!    - [`PausedDrain::WaitForViewer`] (окно, 04b) → [`DrainProgress::AwaitingViewer`]:
//!      человек мог просто отойти, окно ждёт его с последним кадром. Срок застоя на
//!      паузе НЕ идёт (позиция на паузе и должна стоять); «играть» зрителя продолжает
//!      доигрывание с новым отсчётом срока. В этой политике конец и отказ (правила 3–4)
//!      важнее паузы: смотреть дальше нечего.
//!
//!    Намерение читается в момент вопроса, уже после запечатывания: все намерения
//!    страницы к тому времени разобраны;
//! 3. плеер дошёл до конца → [`DrainEnd::PlayedToEnd`];
//! 4. плеер отказал по загруженному поколению → сразу ([`DrainEnd::PlayerFault`]);
//! 5. поколение ещё открывается → ждём: запечатанный файл или поток откроется или
//!    откажет;
//! 6. позиция не двигается дольше [`DrainPolicy::stall_timeout`] → [`DrainEnd::Stalled`]
//!    — страховка от вечного процесса, когда плеер «играет», но звук и кадры стоят.

use std::time::{Duration, Instant};

/// Сроки доигрывания. Каждое число — с объяснением (как у `BrowserSessionPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrainPolicy {
    /// Сколько позиция плеера может не двигаться, пока он «должен играть», прежде чем
    /// считать его застрявшим.
    pub(crate) stall_timeout: Duration,
    /// Как часто просыпаться, пока доигрываем. Часы странице не шлём (её нет), поэтому
    /// их тик цикл не будит: без периодического пробуждения конец воспроизведения,
    /// не принесший кадра, заметили бы поздно.
    pub(crate) poll_interval: Duration,
    /// Что делать, если в момент вопроса страница (или зритель) хочет паузу.
    pub(crate) paused: PausedDrain,
}

/// Пауза во время доигрывания: выйти сразу или ждать человека.
///
/// Различие процессов выражено типом, а не флагом: безоконному процессу некому нажать
/// «играть» (правило 103), окно показывает последний кадр и ждёт зрителя (04b, замена
/// правила 103 для окна).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PausedDrain {
    /// Пауза = конец доигрывания ([`DrainEnd::PagePaused`]).
    ExitAtOnce,
    /// Пауза = ждать зрителя ([`DrainProgress::AwaitingViewer`]), срок застоя стоит.
    WaitForViewer,
}

impl DrainPolicy {
    /// Умолчания для настоящего процесса с данным правилом паузы.
    ///
    /// - `stall_timeout` 15 с: дольше `SeekTimeout` плеера (10 с) и любой разумной
    ///   паузы декодера или диска. После запечатывания новых байтов не придёт, так что
    ///   настоящая остановка — это уже не ожидание данных, а поломка; 15 с — компромисс
    ///   между «зритель смотрит в пустой экран» и «процесс висит вечно».
    /// - `poll_interval` 250 мс: тот же шаг, что тик часов (`CLOCK_INTERVAL`), — конец
    ///   видео замечаем в пределах четверти секунды.
    pub(crate) const fn recommended(paused: PausedDrain) -> Self {
        Self {
            stall_timeout: Duration::from_secs(15),
            poll_interval: Duration::from_millis(250),
            paused,
        }
    }

    /// Та же политика с другим сроком застоя (тестовая переменная процесса,
    /// `super::super::drain_override`).
    pub(crate) const fn with_stall_timeout(self, stall_timeout: Duration) -> Self {
        Self {
            stall_timeout,
            ..self
        }
    }
}

/// Чем кончилось доигрывание.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainEnd {
    /// Нечего играть: поколение не загружено и не открывается.
    NothingToPlay,
    /// В момент конца сеанса страница хотела паузу.
    PagePaused,
    /// Плеер доиграл до конца.
    PlayedToEnd,
    /// Плеер отказал по загруженному поколению.
    PlayerFault,
    /// Позиция не двигалась дольше срока.
    Stalled,
}

impl DrainEnd {
    /// Стабильная метка для журнала (её читают e2e-тесты и разбор логов).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::NothingToPlay => "nothing_to_play",
            Self::PagePaused => "page_paused",
            Self::PlayedToEnd => "played_to_end",
            Self::PlayerFault => "player_fault",
            Self::Stalled => "stalled",
        }
    }
}

/// Как идёт доигрывание.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainProgress {
    /// Ещё играем; проснуться к этому сроку (раньше — по любому пробуждению цикла).
    Draining {
        /// Когда проснуться.
        wake_at: Instant,
    },
    /// Окно на паузе ждёт зрителя ([`PausedDrain::WaitForViewer`]): по времени будить
    /// незачем — продолжит действие человека (оно само будит цикл окна).
    AwaitingViewer,
    /// Закончили: процессу можно выходить.
    Drained(DrainEnd),
}

/// Чего страница хочет от воспроизведения (по её последним `play`/`pause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageStance {
    /// Играть (автозапуск — тоже «играть»).
    WantsPlay,
    /// Пауза.
    WantsPause,
}

/// Разобрал ли поток приёма всё, что прислал закончившийся экземпляр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntakeStance {
    /// Нет: в очереди потока приёма ещё могут лежать поколения и намерения страницы.
    Open,
    /// Да: больше от экземпляра ничего не придёт.
    Sealed,
}

/// Всё, что нужно знать правилам в момент вопроса «доиграно?».
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DrainSnapshot {
    /// Разобран ли вход экземпляра.
    pub(crate) intake: IntakeStance,
    /// Что держит медиа-часть.
    pub(crate) content: DrainContent,
    /// Чего хочет страница.
    pub(crate) page: PageStance,
}

/// Что медиа-часть держит в момент вопроса «доиграно?».
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DrainContent {
    /// Ничего: ни загруженного поколения, ни открывающегося.
    Nothing,
    /// Поколение открывается (запечатанный источник откроется или откажет).
    Opening,
    /// Поколение загружено в плеер.
    Loaded {
        /// Позиция плеера, если наблюдение показывает именно это поколение (иначе
        /// это ещё прежнее медиа и двигаться «нечему»).
        position: Option<Duration>,
        /// Плеер дошёл до конца именно этого поколения.
        ended: bool,
        /// Плеер отказал по этому поколению.
        faulted: bool,
    },
}

/// Наблюдатель за доигрыванием одного сеанса.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DrainWatch {
    /// Сроки.
    policy: DrainPolicy,
    /// Последнее увиденное положение и когда оно сдвинулось (`None` до первого вопроса).
    progress: Option<ProgressMark>,
}

/// Отметка «позиция была такой с этого момента».
#[derive(Debug, Clone, Copy)]
struct ProgressMark {
    /// Позиция плеера (`None` — поколение ещё не показано).
    position: Option<Duration>,
    /// С какого момента позиция не менялась.
    unchanged_since: Instant,
}

impl DrainWatch {
    /// Доигрывание начинается: сеанс только что закончился.
    pub(crate) const fn begin(policy: DrainPolicy) -> Self {
        Self {
            policy,
            progress: None,
        }
    }

    /// Ответ на «доиграно?» в момент `now`.
    pub(crate) fn evaluate(&mut self, snapshot: DrainSnapshot, now: Instant) -> DrainProgress {
        if snapshot.intake == IntakeStance::Sealed
            && let Some(end) = Self::immediate_end(snapshot, self.policy.paused)
        {
            return DrainProgress::Drained(end);
        }
        if snapshot.page == PageStance::WantsPause
            && self.policy.paused == PausedDrain::WaitForViewer
        {
            // Пауза — не застой: отметка сбрасывается, и после «играть» срок застоя
            // отсчитывается заново, с момента, когда позиция снова должна двигаться.
            self.progress = None;
            return DrainProgress::AwaitingViewer;
        }
        let position = match snapshot.content {
            DrainContent::Loaded { position, .. } => position,
            DrainContent::Nothing | DrainContent::Opening => None,
        };
        if self.stalled(position, now) {
            return DrainProgress::Drained(DrainEnd::Stalled);
        }
        DrainProgress::Draining {
            wake_at: now + self.policy.poll_interval,
        }
    }

    /// Правила 1–4: конец, не требующий времени. Пауза — конец только при
    /// [`PausedDrain::ExitAtOnce`]; при [`PausedDrain::WaitForViewer`] её решает
    /// [`Self::evaluate`] уже после конца и отказа.
    fn immediate_end(snapshot: DrainSnapshot, paused: PausedDrain) -> Option<DrainEnd> {
        if snapshot.content == DrainContent::Nothing {
            return Some(DrainEnd::NothingToPlay);
        }
        if snapshot.page == PageStance::WantsPause && paused == PausedDrain::ExitAtOnce {
            return Some(DrainEnd::PagePaused);
        }
        match snapshot.content {
            DrainContent::Loaded { faulted: true, .. } => Some(DrainEnd::PlayerFault),
            DrainContent::Loaded { ended: true, .. } => Some(DrainEnd::PlayedToEnd),
            _ => None,
        }
    }

    /// Правило 6: позиция стоит дольше срока.
    fn stalled(&mut self, position: Option<Duration>, now: Instant) -> bool {
        let mark = self.progress.get_or_insert(ProgressMark {
            position,
            unchanged_since: now,
        });
        if mark.position != position {
            *mark = ProgressMark {
                position,
                unchanged_since: now,
            };
        }
        now.saturating_duration_since(mark.unchanged_since) >= self.policy.stall_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: DrainPolicy = DrainPolicy {
        stall_timeout: Duration::from_secs(10),
        poll_interval: Duration::from_millis(250),
        paused: PausedDrain::ExitAtOnce,
    };

    /// Политика окна: пауза ждёт зрителя.
    const WINDOW_POLICY: DrainPolicy = DrainPolicy {
        paused: PausedDrain::WaitForViewer,
        ..POLICY
    };

    fn wants_play(content: DrainContent) -> DrainSnapshot {
        DrainSnapshot {
            intake: IntakeStance::Sealed,
            content,
            page: PageStance::WantsPlay,
        }
    }

    fn playing(position_s: u64) -> DrainSnapshot {
        wants_play(DrainContent::Loaded {
            position: Some(Duration::from_secs(position_s)),
            ended: false,
            faulted: false,
        })
    }

    fn drained(progress: DrainProgress) -> DrainEnd {
        match progress {
            DrainProgress::Drained(end) => end,
            other @ (DrainProgress::Draining { .. } | DrainProgress::AwaitingViewer) => {
                panic!("still draining: {other:?}")
            }
        }
    }

    fn paused(content: DrainContent) -> DrainSnapshot {
        DrainSnapshot {
            intake: IntakeStance::Sealed,
            content,
            page: PageStance::WantsPause,
        }
    }

    fn loaded_at(position_s: u64) -> DrainContent {
        DrainContent::Loaded {
            position: Some(Duration::from_secs(position_s)),
            ended: false,
            faulted: false,
        }
    }

    #[test]
    fn nothing_loaded_or_opening_ends_at_once() {
        let mut watch = DrainWatch::begin(POLICY);
        assert_eq!(
            drained(watch.evaluate(wants_play(DrainContent::Nothing), Instant::now())),
            DrainEnd::NothingToPlay
        );
    }

    #[test]
    fn a_page_that_wants_pause_ends_at_once_even_with_a_loaded_video() {
        let paused = |content| DrainSnapshot {
            intake: IntakeStance::Sealed,
            content,
            page: PageStance::WantsPause,
        };
        let mut watch = DrainWatch::begin(POLICY);
        let loaded = DrainContent::Loaded {
            position: Some(Duration::from_secs(3)),
            ended: false,
            faulted: false,
        };
        assert_eq!(
            drained(watch.evaluate(paused(loaded), Instant::now())),
            DrainEnd::PagePaused
        );
        let mut opening = DrainWatch::begin(POLICY);
        assert_eq!(
            drained(opening.evaluate(paused(DrainContent::Opening), Instant::now())),
            DrainEnd::PagePaused
        );
    }

    #[test]
    fn the_pause_is_read_at_the_moment_of_the_question() {
        // Пауза, дошедшая до цикла позже конца сеанса, всё равно останавливает ожидание.
        let mut watch = DrainWatch::begin(POLICY);
        let start = Instant::now();
        assert!(matches!(
            watch.evaluate(playing(1), start),
            DrainProgress::Draining { .. }
        ));
        let paused = DrainSnapshot {
            intake: IntakeStance::Sealed,
            content: DrainContent::Opening,
            page: PageStance::WantsPause,
        };
        assert_eq!(
            drained(watch.evaluate(paused, start + Duration::from_millis(100))),
            DrainEnd::PagePaused
        );
    }

    #[test]
    fn a_playing_video_keeps_the_process_alive_until_the_player_ends() {
        let mut watch = DrainWatch::begin(POLICY);
        let start = Instant::now();
        assert_eq!(
            watch.evaluate(playing(1), start),
            DrainProgress::Draining {
                wake_at: start + POLICY.poll_interval
            }
        );
        let ended = wants_play(DrainContent::Loaded {
            position: Some(Duration::from_secs(5)),
            ended: true,
            faulted: false,
        });
        assert_eq!(
            drained(watch.evaluate(ended, start + Duration::from_secs(1))),
            DrainEnd::PlayedToEnd
        );
    }

    #[test]
    fn a_player_fault_ends_at_once() {
        let mut watch = DrainWatch::begin(POLICY);
        let faulted = wants_play(DrainContent::Loaded {
            position: Some(Duration::ZERO),
            ended: false,
            faulted: true,
        });
        assert_eq!(
            drained(watch.evaluate(faulted, Instant::now())),
            DrainEnd::PlayerFault
        );
    }

    #[test]
    fn an_opening_generation_is_awaited_but_not_forever() {
        let mut watch = DrainWatch::begin(POLICY);
        let start = Instant::now();
        let opening = wants_play(DrainContent::Opening);
        assert!(matches!(
            watch.evaluate(opening, start),
            DrainProgress::Draining { .. }
        ));
        assert!(matches!(
            watch.evaluate(opening, start + Duration::from_secs(9)),
            DrainProgress::Draining { .. }
        ));
        assert_eq!(
            drained(watch.evaluate(opening, start + Duration::from_secs(10))),
            DrainEnd::Stalled
        );
    }

    #[test]
    fn a_moving_position_resets_the_stall_clock_and_a_frozen_one_ends_it() {
        let mut watch = DrainWatch::begin(POLICY);
        let start = Instant::now();
        assert!(matches!(
            watch.evaluate(playing(1), start),
            DrainProgress::Draining { .. }
        ));
        // Позиция сдвинулась на 9 с — часы сброшены, 18 с от старта это ещё не застой.
        assert!(matches!(
            watch.evaluate(playing(2), start + Duration::from_secs(9)),
            DrainProgress::Draining { .. }
        ));
        assert!(matches!(
            watch.evaluate(playing(2), start + Duration::from_secs(18)),
            DrainProgress::Draining { .. }
        ));
        assert_eq!(
            drained(watch.evaluate(playing(2), start + Duration::from_secs(19))),
            DrainEnd::Stalled
        );
    }

    /// Пока вход экземпляра не разобран, ни «нечего играть», ни «пауза», ни «доиграно»
    /// не решаются: данные ещё могут лежать в очереди. Решает только срок.
    #[test]
    fn nothing_is_decided_before_the_intake_is_sealed_except_by_the_deadline() {
        let open = |content, page| DrainSnapshot {
            intake: IntakeStance::Open,
            content,
            page,
        };
        let start = Instant::now();
        let mut watch = DrainWatch::begin(POLICY);
        for (content, page) in [
            (DrainContent::Nothing, PageStance::WantsPlay),
            (DrainContent::Nothing, PageStance::WantsPause),
            (DrainContent::Opening, PageStance::WantsPause),
            (
                DrainContent::Loaded {
                    position: Some(Duration::from_secs(5)),
                    ended: true,
                    faulted: true,
                },
                PageStance::WantsPlay,
            ),
        ] {
            assert!(
                matches!(
                    watch.evaluate(open(content, page), start),
                    DrainProgress::Draining { .. }
                ),
                "{content:?} {page:?}"
            );
        }
        let frozen = DrainContent::Loaded {
            position: Some(Duration::from_secs(5)),
            ended: true,
            faulted: true,
        };
        assert_eq!(
            drained(watch.evaluate(
                open(frozen, PageStance::WantsPlay),
                start + POLICY.stall_timeout
            )),
            DrainEnd::Stalled
        );
    }

    #[test]
    fn end_labels_are_stable() {
        let labels = [
            (DrainEnd::NothingToPlay, "nothing_to_play"),
            (DrainEnd::PagePaused, "page_paused"),
            (DrainEnd::PlayedToEnd, "played_to_end"),
            (DrainEnd::PlayerFault, "player_fault"),
            (DrainEnd::Stalled, "stalled"),
        ];
        for (end, label) in labels {
            assert_eq!(end.log_label(), label);
        }
    }

    // ---- окно: пауза ждёт зрителя (04b) ------------------------------------------

    #[test]
    fn in_the_window_a_paused_video_waits_for_the_viewer_instead_of_ending() {
        let mut watch = DrainWatch::begin(WINDOW_POLICY);
        let start = Instant::now();
        assert_eq!(
            watch.evaluate(paused(loaded_at(3)), start),
            DrainProgress::AwaitingViewer
        );
        // Открывающееся поколение на паузе тоже ждёт человека, а не выходит.
        assert_eq!(
            watch.evaluate(paused(DrainContent::Opening), start),
            DrainProgress::AwaitingViewer
        );
    }

    /// Пауза дольше срока застоя — не застой: позиция на паузе и должна стоять.
    #[test]
    fn the_stall_deadline_does_not_run_while_the_window_waits_on_pause() {
        let mut watch = DrainWatch::begin(WINDOW_POLICY);
        let start = Instant::now();
        for elapsed_s in [0, 5, 11, 60, 3_600] {
            assert_eq!(
                watch.evaluate(paused(loaded_at(3)), start + Duration::from_secs(elapsed_s)),
                DrainProgress::AwaitingViewer,
                "after {elapsed_s} s on pause"
            );
        }
    }

    /// «Играть» зрителя после долгой паузы: срок застоя считается с момента «играть»,
    /// а не с момента, когда позиция встала на паузе.
    #[test]
    fn play_after_a_long_pause_restarts_the_stall_clock() {
        let mut watch = DrainWatch::begin(WINDOW_POLICY);
        let start = Instant::now();
        assert!(matches!(
            watch.evaluate(playing(3), start),
            DrainProgress::Draining { .. }
        ));
        let resumed_at = start + Duration::from_secs(60);
        assert_eq!(
            watch.evaluate(paused(loaded_at(3)), start + Duration::from_secs(1)),
            DrainProgress::AwaitingViewer
        );
        assert!(matches!(
            watch.evaluate(playing(3), resumed_at),
            DrainProgress::Draining { .. }
        ));
        assert!(matches!(
            watch.evaluate(
                playing(3),
                resumed_at + POLICY.stall_timeout - Duration::from_millis(1)
            ),
            DrainProgress::Draining { .. }
        ));
        assert_eq!(
            drained(watch.evaluate(playing(3), resumed_at + POLICY.stall_timeout)),
            DrainEnd::Stalled
        );
    }

    /// В окне конец и отказ важнее паузы: смотреть дальше нечего — сообщение и закрытие.
    #[test]
    fn in_the_window_the_end_and_a_fault_win_over_the_pause() {
        let mut watch = DrainWatch::begin(WINDOW_POLICY);
        let now = Instant::now();
        let ended = DrainContent::Loaded {
            position: Some(Duration::from_secs(6)),
            ended: true,
            faulted: false,
        };
        assert_eq!(
            drained(watch.evaluate(paused(ended), now)),
            DrainEnd::PlayedToEnd
        );
        let faulted = DrainContent::Loaded {
            position: Some(Duration::ZERO),
            ended: false,
            faulted: true,
        };
        assert_eq!(
            drained(watch.evaluate(paused(faulted), now)),
            DrainEnd::PlayerFault
        );
        assert_eq!(
            drained(watch.evaluate(paused(DrainContent::Nothing), now)),
            DrainEnd::NothingToPlay
        );
    }

    /// Пока вход не запечатан, окно на паузе тоже ждёт зрителя (а не срок): данные ещё
    /// могут прийти, а человек — нажать «играть».
    #[test]
    fn before_the_seal_a_paused_window_waits_without_a_deadline() {
        let mut watch = DrainWatch::begin(WINDOW_POLICY);
        let start = Instant::now();
        let open_paused = DrainSnapshot {
            intake: IntakeStance::Open,
            content: DrainContent::Nothing,
            page: PageStance::WantsPause,
        };
        assert_eq!(
            watch.evaluate(open_paused, start + POLICY.stall_timeout * 3),
            DrainProgress::AwaitingViewer
        );
    }

    /// Безоконная политика не изменилась: пауза — конец сразу (правило 103).
    #[test]
    fn without_a_window_the_pause_still_ends_the_drain_at_once() {
        let mut watch = DrainWatch::begin(POLICY);
        assert_eq!(
            drained(watch.evaluate(paused(loaded_at(3)), Instant::now())),
            DrainEnd::PagePaused
        );
    }

    #[test]
    fn a_test_stall_override_changes_only_the_stall_timeout() {
        let shortened = DrainPolicy::recommended(PausedDrain::WaitForViewer)
            .with_stall_timeout(Duration::from_millis(700));
        assert_eq!(shortened.stall_timeout, Duration::from_millis(700));
        assert_eq!(
            shortened.poll_interval,
            DrainPolicy::recommended(PausedDrain::WaitForViewer).poll_interval
        );
        assert_eq!(shortened.paused, PausedDrain::WaitForViewer);
    }
}
