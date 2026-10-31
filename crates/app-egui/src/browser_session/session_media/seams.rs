//! Швы медиа-части: плеер и закрытие сеанса так, как их видит [`super::SessionMedia`].
//! Настоящие — [`BrowserPlayback`] и `SessionControl`; тесты подставляют подделки.

use std::time::{Duration, Instant};

use browser_bridge_protocol::{PlayerSessionMessage, SessionCloseReason};
use browser_bridge_session::{
    ActiveFarewellOutcome, CloseOutcome, FarewellError, ReplacedFarewellOutcome, SessionControl,
    SessionRef,
};
use player_core::{PlaybackRate as PlayerPlaybackRate, PreparedMedia};

use crate::browser_session::playback::{
    BrowserPlayback, BrowserPlaybackCommandError, LoadStart, MediaLoadTicket, PlaybackObservation,
};
use crate::downloaded_range::DownloadedRange;

/// Плеер так, как его видит медиа-часть. Настоящие — безоконный [`BrowserPlayback`]
/// (тесты) и «пульт» окна браузерного режима (`window::remote::WindowRemote`, 04a).
pub(crate) trait MediaPlayback {
    /// Загрузить новое медиа: играть сразу или встать на паузу — по `start`.
    /// Квитанция связывает загрузку с наблюдениями, когда плеер её установит.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError>;
    /// Играть.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError>;
    /// Пауза.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError>;
    /// Скорость.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn set_rate(&mut self, rate: PlayerPlaybackRate) -> Result<(), BrowserPlaybackCommandError>;
    /// Точная перемотка.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError>;
    /// Что скачано у показанного медиа — для живого перетаскивания по таймлайну окна
    /// (зовётся каждый проход; `Unknown` — ничего не известно).
    fn offer_downloaded_range(&mut self, range: DownloadedRange);
    /// Шаг плеера: события, кадры, звук, журнал.
    fn pump(&mut self, now: Instant) -> PlaybackObservation;
    /// Итоговая строка журнала и остановка плеера.
    fn finish(self: Box<Self>);
}

impl MediaPlayback for BrowserPlayback {
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError> {
        Self::load(self, prepared, start)
    }

    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        Self::play(self)
    }

    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        Self::pause(self)
    }

    fn set_rate(&mut self, rate: PlayerPlaybackRate) -> Result<(), BrowserPlaybackCommandError> {
        Self::set_rate(self, rate)
    }

    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError> {
        Self::seek(self, target)
    }

    /// У безоконного плеера нет таймлайна: о скачанном спрашивать некому.
    fn offer_downloaded_range(&mut self, _range: DownloadedRange) {}

    fn pump(&mut self, now: Instant) -> PlaybackObservation {
        Self::pump(self, now)
    }

    fn finish(self: Box<Self>) {
        let _summary = Self::finish(*self);
    }
}

/// Кто закрывает сеанс: по просьбе потока приёма, при закрытии окна и при замене
/// вкладки (прощание, решение 127). Настоящий — `SessionControl`.
pub(crate) trait SessionCloser {
    /// Закрывает ровно этот экземпляр (не тот, что успел его заменить).
    fn close_session(&self, session: &SessionRef, reason: SessionCloseReason) -> CloseOutcome;
    /// Закрывает активный экземпляр, отдав странице последнее сообщение перед кадром
    /// закрытия (зритель закрыл окно: `return_to_page` → 4000).
    ///
    /// # Errors
    /// Сообщение не по протоколу — сеанс при этом НЕ закрыт.
    fn close_session_with_farewell(
        &self,
        session: &SessionRef,
        farewell: &PlayerSessionMessage,
        reason: SessionCloseReason,
    ) -> Result<ActiveFarewellOutcome, FarewellError>;
    /// Последнее сообщение заменённой вкладке, пока она в «коридоре прощания».
    ///
    /// # Errors
    /// Сообщение не по протоколу — коридор закроется по своему сроку.
    fn bid_farewell_to_replaced(
        &self,
        replaced: &SessionRef,
        farewell: &PlayerSessionMessage,
    ) -> Result<ReplacedFarewellOutcome, FarewellError>;
}

impl SessionCloser for SessionControl {
    fn close_session(&self, session: &SessionRef, reason: SessionCloseReason) -> CloseOutcome {
        Self::close_session(self, session, reason)
    }

    fn close_session_with_farewell(
        &self,
        session: &SessionRef,
        farewell: &PlayerSessionMessage,
        reason: SessionCloseReason,
    ) -> Result<ActiveFarewellOutcome, FarewellError> {
        Self::close_session_with_farewell(self, session, farewell, reason)
    }

    fn bid_farewell_to_replaced(
        &self,
        replaced: &SessionRef,
        farewell: &PlayerSessionMessage,
    ) -> Result<ReplacedFarewellOutcome, FarewellError> {
        Self::bid_farewell_to_replaced(self, replaced, farewell)
    }
}
