//! Транспорт зрителя: действие человека в окне ведёт и плеер, и страницу (04a).
//!
//! Окно браузерного режима не командует плееру само: кнопки, таймлайн и хоткеи дают
//! намерение зрителя ([`ViewerTransportIntent`], одна точка — `AppState::send_transport_command`),
//! окно отдаёт его сюда. Здесь — единственный владелец плеера и исходящих странице:
//!
//! ```text
//!  намерение зрителя ─► SessionMedia::on_viewer_transport
//!       ├─ воля страницы (`page_wants_play`, `page_rate`) = воля зрителя
//!       │  (её читают загрузка, согласование после установки и доигрывание)
//!       ├─ плеер: сразу (play / pause / скорость с сохранением тона / точная перемотка)
//!       └─ странице: play / pause / set_rate (надёжно, `PageOutbound`)
//! ```
//!
//! Перемотка зрителя странице отдельно НЕ шлётся: плеер перематывается точно, а
//! источник сам решает, нужна ли странице перемотка (цель на полке — нет; вне полки —
//! просьба `seek{seek_id}` через тот же `request_page_seek`, страница подтвердит
//! `seek_started{player_seek}`). Так одно действие = не больше одного `seek` странице.
//! Исключение — поколение, которое ещё не отдано плееру: цель становится его стартом
//! (ворота 04d, [`super::generation_placement`]), а страница о цели не знает, поэтому
//! просьба уходит оттуда — тоже одна и только если полка цели не держит; источник
//! помечает цель, и установка поколения вторую просьбу не ставит. Если страница потом
//! сама вернулась в другое место, клик повторяется ей один раз после показа нового
//! поколения ([`super::viewer_click_claim`], решение 149) — тогда на клик два `seek`.
//!
//! **Правило эха** ([`ViewerWill`]): страница, исполнив нашу команду, присылает
//! `page_playback{play|pause|set_rate}`. Это подтверждение, а не новая воля: плеер уже
//! в этом состоянии, второй команды он не получает. А запоздалое противоположное
//! (зритель нажал паузу, а от страницы ещё летит прежний `play`) — устаревшее: воля
//! зрителя главнее, пока страница её не подтвердила или не прошёл срок
//! [`VIEWER_WILL_CONFIRMATION_WINDOW`]. После подтверждения или срока страница снова
//! ведёт сама (реклама на паузе, автопереход и т. п.).
//!
//! **Без страницы** (доигрывание после ухода вкладки, 04b): плеером зритель управляет
//! как раньше, странице не уходит ничего. Перемотка — только по скачанному: просить
//! страницу о байтах некому, и перемотка вне полки ждала бы их вечно (плеер стоял бы в
//! «перемотке»). Поэтому цель вне скачанного (и любая цель у файла `<video src>` и эфира,
//! где скачанное не известно — `DownloadedRange::Unknown`) отклоняется: кадр остаётся,
//! где был.

use std::time::{Duration, Instant};

use browser_bridge_protocol::PlaybackRate as PagePlaybackRate;
use tracing::{debug, info};

use super::SessionMedia;
use super::generation_placement::PendingAim;
use super::viewer_click_claim::ViewerClickClaim;
use crate::browser_session::outbound::clock_rate;
use crate::viewer_transport::ViewerTransportIntent;

/// Сколько воля зрителя ждёт подтверждения страницы, прежде чем страница снова ведёт.
///
/// Команда идёт по loopback за миллисекунды, плеер страницы отвечает событием за
/// десятки миллисекунд даже в фоновой вкладке (таймеры душатся, события — нет,
/// `results/00c.md`). Две секунды — с большим запасом на тяжёлую страницу и при этом
/// коротко: страница, которая подтверждение не шлёт вовсе, через 2 с снова может
/// поставить паузу сама (реклама). Это правило протокола, а не настройка пользователя.
pub(crate) const VIEWER_WILL_CONFIRMATION_WINDOW: Duration = Duration::from_secs(2);

/// «Играть» или «пауза» — воля зрителя или страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlayWill {
    /// Играть.
    Play,
    /// Пауза.
    Pause,
}

impl PlayWill {
    const fn wants_play(self) -> bool {
        matches!(self, Self::Play)
    }
}

/// Что делать с сообщением страницы о play/pause/скорости.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageEcho {
    /// Неподтверждённой воли зрителя нет: страница ведёт, плеер следует.
    Follow,
    /// Страница подтвердила волю зрителя: плеер уже такой, команды нет.
    ConfirmsViewer,
    /// Противоречит неподтверждённой воле зрителя: устаревшее, не исполняется.
    StaleAgainstViewer,
}

/// Воля зрителя, которую страница ещё не подтвердила.
#[derive(Debug, Clone, Copy)]
struct Unconfirmed<T> {
    value: T,
    until: Instant,
}

/// Неподтверждённые воли зрителя (см. «правило эха» в описании модуля).
#[derive(Debug, Default)]
pub(super) struct ViewerWill {
    play: Option<Unconfirmed<PlayWill>>,
    rate: Option<Unconfirmed<PagePlaybackRate>>,
    /// Клик во время смены поколения против самовольного возврата страницы (149).
    click: ViewerClickClaim,
}

impl ViewerWill {
    fn note_play(&mut self, value: PlayWill, now: Instant) {
        self.play = Some(Unconfirmed {
            value,
            until: now + VIEWER_WILL_CONFIRMATION_WINDOW,
        });
    }

    fn note_rate(&mut self, value: PagePlaybackRate, now: Instant) {
        self.rate = Some(Unconfirmed {
            value,
            until: now + VIEWER_WILL_CONFIRMATION_WINDOW,
        });
    }

    /// Сверка play/pause страницы с волей зрителя; подтверждение снимает волю.
    pub(super) fn judge_page_play(&mut self, page: PlayWill) -> PageEcho {
        judge(&mut self.play, page)
    }

    /// Сверка скорости страницы с волей зрителя (в тысячных долях протокола — без
    /// погрешностей дробей); подтверждение снимает волю.
    pub(super) fn judge_page_rate(&mut self, page: PagePlaybackRate) -> PageEcho {
        judge(&mut self.rate, page)
    }

    /// Претензия клика зрителя во время смены поколения
    /// ([`super::viewer_click_claim`]).
    pub(super) fn click_claim(&mut self) -> &mut ViewerClickClaim {
        &mut self.click
    }

    /// Срок вышел — страница снова ведёт. Зовётся каждый проход медиа-части.
    pub(super) fn expire(&mut self, now: Instant) {
        self.click.expire(now);
        if self.play.is_some_and(|will| will.until <= now) {
            self.play = None;
        }
        if self.rate.is_some_and(|will| will.until <= now) {
            self.rate = None;
        }
    }

    /// Новая страница: воля, сказанная прежней, к ней не относится.
    pub(super) fn forget(&mut self) {
        *self = Self::default();
    }
}

fn judge<T: PartialEq + Copy>(will: &mut Option<Unconfirmed<T>>, page: T) -> PageEcho {
    match will {
        None => PageEcho::Follow,
        Some(unconfirmed) if unconfirmed.value == page => {
            *will = None;
            PageEcho::ConfirmsViewer
        }
        Some(_) => PageEcho::StaleAgainstViewer,
    }
}

/// Куда попала перемотка загруженного поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoadedSeek {
    /// Плеер показывает поколение: перемотан, «играть/пауза» восстановлены следом.
    Commanded,
    /// Плеер ещё ставит поколение: перемотка исполнится при показе (последняя побеждает).
    DeferredUntilShown,
    /// Загруженного поколения нет.
    NothingLoaded,
}

impl SessionMedia {
    /// Действие зрителя в окне: воля страницы, плеер и команда странице.
    pub(crate) fn on_viewer_transport(&mut self, intent: ViewerTransportIntent, now: Instant) {
        // Страницы нет (доигрываем после её ухода) — команды ей некому слать; плеером
        // зритель по-прежнему управляет.
        let page_listens = self.drain.is_none() && self.current_session.is_some();
        match intent {
            ViewerTransportIntent::Play | ViewerTransportIntent::Pause => {
                let will = if intent == ViewerTransportIntent::Play {
                    PlayWill::Play
                } else {
                    PlayWill::Pause
                };
                if will == PlayWill::Play && self.is_draining() {
                    // Решение 130 («магия возврата вкладки», сессия 11): здесь зритель
                    // нажал «играть» после ухода вкладки. В 04b — запасной путь:
                    // доигрываем скачанное, потом сообщение и закрытие окна. В 11 здесь
                    // же экстеншен откроет страницу заново на T.
                    debug!("Viewer resumed the playback after the tab was gone");
                }
                self.page_wants_play = will.wants_play();
                self.viewer_will.note_play(will, now);
                // Нет загруженного — воля всё равно запомнена: загрузка начнётся так.
                self.apply_page_play_state();
                if page_listens {
                    match will {
                        PlayWill::Play => self.outbound.request_page_play(),
                        PlayWill::Pause => self.outbound.request_page_pause(),
                    }
                }
            }
            ViewerTransportIntent::SetRate(rate) => {
                self.page_rate = rate;
                let page_rate = clock_rate(rate);
                self.viewer_will.note_rate(page_rate, now);
                self.command("viewer rate", |playback| playback.set_rate(rate));
                if page_listens {
                    self.outbound.request_page_rate(page_rate);
                }
            }
            ViewerTransportIntent::Seek(target) => self.viewer_seek(target, page_listens),
        }
    }

    /// Перемотка зрителя (см. описание модуля: странице — не больше одного `seek`).
    fn viewer_seek(&mut self, target: Duration, page_listens: bool) {
        // Новое поколение ещё не отдано плееру: цель — его старт (ворота 04d), прежнее
        // видео не двигаем; странице — не больше одной просьбы.
        let aim = self.aim_pending_for_viewer(target, page_listens);
        // Клик во время смены поколения главнее самовольного возврата страницы (149).
        self.note_viewer_click(target, aim);
        if aim == PendingAim::AimedPending {
            return;
        }
        if self.is_draining() && !self.loaded_generation_holds(target) {
            // Страницы нет: байтов вне скачанного не будет (правило — в описании модуля).
            info!("Viewer seek outside of what was downloaded ignored: the page is gone");
            return;
        }
        if self.seek_loaded_generation(target) == LoadedSeek::NothingLoaded {
            debug!("Viewer seek before the stream opened; nothing to move");
        }
    }

    /// Скачано ли у загруженного поколения то, что нужно для перемотки к `target`.
    fn loaded_generation_holds(&self, target: Duration) -> bool {
        self.loaded
            .as_ref()
            .is_some_and(|loaded| loaded.feed.downloaded_range().covers(target))
    }

    /// Перемотка загруженного поколения — общая для перемотки страницей и зрителем.
    ///
    /// Пока плеер ставит поколение, команда досталась бы прежнему медиа: она ждёт показа
    /// (`seek_when_shown`, правило 03e). После перемотки плеер приводится к
    /// «играть/пауза» страницы (перемотка после `Ended` кончается паузой).
    pub(super) fn seek_loaded_generation(&mut self, target: Duration) -> LoadedSeek {
        let Some(loaded) = self.loaded.as_mut() else {
            return LoadedSeek::NothingLoaded;
        };
        if !loaded.page_state_reconciled {
            loaded.seek_when_shown = Some(target);
            return LoadedSeek::DeferredUntilShown;
        }
        self.command("seek", |playback| playback.seek(target));
        self.apply_page_play_state();
        LoadedSeek::Commanded
    }

    /// Страница сказала «играть/пауза»: следовать, принять как подтверждение воли
    /// зрителя (без команды плееру) или отбросить как устаревшее.
    pub(super) fn follow_page_play(&mut self, page: PlayWill) {
        match self.viewer_will.judge_page_play(page) {
            PageEcho::Follow => {
                self.page_wants_play = page.wants_play();
                self.apply_page_play_state();
            }
            echo @ (PageEcho::ConfirmsViewer | PageEcho::StaleAgainstViewer) => {
                debug!(?echo, ?page, "Page play state not applied to the player");
            }
        }
    }

    /// Страница сменила скорость: то же правило эха, что у «играть/пауза».
    pub(super) fn follow_page_rate(&mut self, page: PagePlaybackRate) {
        match self.viewer_will.judge_page_rate(page) {
            PageEcho::Follow => {
                let rate = super::player_rate(page);
                self.page_rate = rate;
                self.command("set rate", |playback| playback.set_rate(rate));
            }
            echo @ (PageEcho::ConfirmsViewer | PageEcho::StaleAgainstViewer) => {
                debug!(?echo, "Page rate not applied to the player");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(permille: u32) -> PagePlaybackRate {
        PagePlaybackRate::from_permille(permille).expect("rate")
    }

    #[test]
    fn without_a_viewer_will_the_page_leads() {
        let mut will = ViewerWill::default();
        assert_eq!(will.judge_page_play(PlayWill::Pause), PageEcho::Follow);
        assert_eq!(will.judge_page_rate(rate(1_500)), PageEcho::Follow);
    }

    #[test]
    fn the_echo_confirms_once_and_then_the_page_leads_again() {
        let now = Instant::now();
        let mut will = ViewerWill::default();
        will.note_play(PlayWill::Pause, now);
        assert_eq!(
            will.judge_page_play(PlayWill::Play),
            PageEcho::StaleAgainstViewer
        );
        assert_eq!(
            will.judge_page_play(PlayWill::Pause),
            PageEcho::ConfirmsViewer
        );
        assert_eq!(will.judge_page_play(PlayWill::Play), PageEcho::Follow);
    }

    #[test]
    fn an_unconfirmed_will_expires_after_the_window() {
        let now = Instant::now();
        let mut will = ViewerWill::default();
        will.note_rate(rate(2_000), now);
        will.expire(now + VIEWER_WILL_CONFIRMATION_WINDOW - Duration::from_millis(1));
        assert_eq!(
            will.judge_page_rate(rate(1_000)),
            PageEcho::StaleAgainstViewer
        );
        will.expire(now + VIEWER_WILL_CONFIRMATION_WINDOW);
        assert_eq!(will.judge_page_rate(rate(1_000)), PageEcho::Follow);
    }
}
