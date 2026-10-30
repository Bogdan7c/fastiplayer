//! Что поток приёма байтов (`browser-stream`) сообщает главному циклу.
//!
//! Переходник протокола ([`super::translator`]) превращает сообщения страницы и
//! байты в вызовы источника `pushed-media-source`, а всё, что требует решения
//! главного цикла (новый поток для плеера, отказ странице, закрытие сеанса,
//! намерения страницы), выдаёт уведомлениями [`IntakeNotice`]. Уведомления едут
//! через ящик ([`super::inbox::BrowserSessionInbox::post_intake_notices`]).
//!
//! Каждое уведомление несёт [`SessionRef`] (ключ + экземпляр): главный цикл
//! исполняет его, только если этот экземпляр ещё актуален — сеанс мог смениться,
//! пока уведомление ждало в ящике.

use std::fmt;

use browser_bridge_protocol::session::player::StreamRejection;
use browser_bridge_protocol::{
    MediaTime, PlaybackRate, SeekId, SessionCloseReason, StreamGeneration,
};
use browser_bridge_session::SessionRef;
use pushed_media_source::{PushedStreamMonitor, PushedStreamOpener};

/// Новое поколение (новый `MediaSource` страницы) — новый поток для плеера.
///
/// Писатель потока остаётся на потоке приёма; главному циклу достаются сторона
/// открытия (её опрашивают на стороне подготовки, `poll_open` не блокирует) и
/// монитор буфера (позиция плеера → «буферизовано до» и «полон» для страницы).
pub(crate) struct GenerationHandoff {
    /// Сеанс, которому принадлежит поколение.
    pub(crate) session: SessionRef,
    /// Номер поколения из `generation_started`.
    pub(crate) generation: StreamGeneration,
    /// Сторона открытия потока для плеера.
    pub(crate) opener: PushedStreamOpener,
    /// Монитор буфера потока (клонируется, работает с любого потока).
    pub(crate) monitor: PushedStreamMonitor,
}

impl fmt::Debug for GenerationHandoff {
    /// Сторона открытия не печатается: внутри неё демуксеры, а не данные для журнала.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationHandoff")
            .field("session", &self.session)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Чего хочет плеер страницы от «элемента» — главный цикл передаёт это плееру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageIntent {
    /// Страница сама перемоталась (`seek_started{page_seek}`): плеер идёт следом.
    FollowPageSeek(MediaTime),
    /// Страница вызвала `play()`.
    Play,
    /// Страница вызвала `pause()`.
    Pause,
    /// Страница поменяла скорость.
    SetRate(PlaybackRate),
}

/// Уведомление потока приёма главному циклу.
#[derive(Debug)]
pub(crate) enum IntakeNotice {
    /// Новое поколение: старый поток доигрывает принятое, новый ждёт открытия.
    /// В коробке: передача тяжёлая (сторона открытия), а уведомления в очереди
    /// ящика в основном лёгкие.
    NewGeneration(Box<GenerationHandoff>),
    /// В поток легли новые байты: стоит опросить сторону открытия и монитор.
    /// Ящик сливает такие уведомления в одно (это подсказка, а не событие).
    BytesAppended {
        /// Сеанс.
        session: SessionRef,
    },
    /// Намерение страницы.
    PageIntent {
        /// Сеанс.
        session: SessionRef,
        /// Чего хочет страница.
        intent: PageIntent,
    },
    /// Страница начала перемотку, о которой просил плеер (`seek_started{player_seek}`).
    PlayerSeekAcknowledged {
        /// Сеанс.
        session: SessionRef,
        /// Номер команды `seek` плеера.
        seek_id: SeekId,
        /// Куда перематывает страница.
        target: MediaTime,
    },
    /// Источник не будет играть это поколение: странице — `stream_rejected`.
    /// На поколение — не больше одного такого уведомления.
    Reject {
        /// Сеанс.
        session: SessionRef,
        /// Поколение.
        generation: StreamGeneration,
        /// Причина для страницы.
        reason: StreamRejection,
    },
    /// Сеанс надо закрыть этим кодом (`SessionControl::close_session` — только
    /// этот экземпляр, не тот, что успел его заменить).
    CloseSession {
        /// Сеанс.
        session: SessionRef,
        /// Код закрытия.
        reason: SessionCloseReason,
    },
}

impl IntakeNotice {
    /// Сеанс, к которому относится уведомление.
    pub(crate) fn session(&self) -> &SessionRef {
        match self {
            Self::NewGeneration(handoff) => &handoff.session,
            Self::BytesAppended { session }
            | Self::PageIntent { session, .. }
            | Self::PlayerSeekAcknowledged { session, .. }
            | Self::Reject { session, .. }
            | Self::CloseSession { session, .. } => session,
        }
    }

    /// Что делать с уведомлением, если ящик переполнен.
    ///
    /// - намерения «играть/пауза/скорость» — можно потерять: следующее намерение их
    ///   заменит, плеер в худшем случае не повторит одну паузу;
    /// - квитанция перемотки плеера — можно потерять: главный цикл её только пишет в
    ///   отладочный журнал, а страница может прислать её сколько угодно раз (её номер
    ///   не сверяется), и неотбрасываемой она раздувала бы ящик по воле страницы;
    /// - перемотка страницей — терять НЕЛЬЗЯ: источник о ней уже знает (переходник
    ///   сообщил ему в порядке кадров) и не попросит страницу перемотаться к этой
    ///   цели, так что без неё плеер остался бы на старом месте, а источник подавал бы
    ///   новое. Чтобы страница не раздувала ящик потоком перемоток, при переполнении
    ///   новая перемотка заменяет прежнюю неисполненную перемотку того же сеанса (цель
    ///   новее делает старую ненужной) — сверх ёмкости ящика её копий не больше одной
    ///   на сеанс;
    /// - новый поток, отказ, закрытие — единичные события, без них сеанс зависнет.
    pub(crate) const fn overflow_rule(&self) -> OverflowRule {
        match self {
            Self::PageIntent {
                intent: PageIntent::FollowPageSeek(_),
                ..
            } => OverflowRule::ReplacePendingPageSeek,
            Self::PageIntent { .. } | Self::PlayerSeekAcknowledged { .. } => {
                OverflowRule::Droppable
            }
            Self::NewGeneration(_)
            | Self::BytesAppended { .. }
            | Self::Reject { .. }
            | Self::CloseSession { .. } => OverflowRule::Keep,
        }
    }

    /// Это перемотка страницей сеанса `session`.
    pub(crate) fn is_page_seek_of(&self, session: &SessionRef) -> bool {
        matches!(
            self,
            Self::PageIntent {
                session: notice_session,
                intent: PageIntent::FollowPageSeek(_),
            } if notice_session == session
        )
    }
}

/// Как уведомление ложится в переполненный ящик ([`IntakeNotice::overflow_rule`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverflowRule {
    /// Ложится всегда.
    Keep,
    /// Теряется (с записью в журнал).
    Droppable,
    /// Ложится, вытесняя прежнюю неисполненную перемотку страницей того же сеанса.
    ReplacePendingPageSeek,
}
