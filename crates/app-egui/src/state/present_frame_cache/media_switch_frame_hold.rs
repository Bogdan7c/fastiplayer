//! Кадр окна на смене медиа (решение владельца 380, сессия 16b3c моста).
//!
//! Окно браузерного режима меняет медиа на каждом новом поколении страницы (следующий ролик
//! ленты, смена качества). Обычный путь кэша кадра (`present_frame_cache`) на границе
//! открытия выбрасывает последний кадр прежнего медиа, и до первого кадра нового рендер
//! рисует пустое (чёрное) поле — «чёрная вспышка» между роликами.
//!
//! Этот модуль владеет двумя вещами одной смены медиа:
//! - **удержанием**: кадр, который кэш выбросил бы на «мягкой» причине (граница открытия,
//!   плеер ещё не отдал кадр, сменилось поколение рендера), уходит сюда и рисуется вместо
//!   пустого поля, пока не покажется первый кадр нового медиа — но не дольше срока из
//!   настроек. Обычное окно кадр не держит (`MediaSwitchFramePolicy::ClearImmediately`);
//! - **замером**: сколько кадров окна ушло на экран без видео между началом смены и первым
//!   кадром нового медиа и сколько миллисекунд это заняло. Одна строка журнала на смену
//!   (`report.rs`) — по ней судья стенда видит, была ли вспышка.
//!
//! Модуль ничего не знает о GPU: кадр — любой тип с [`SwitchFrame`] (поколение рендера и
//! «тот же ли это кадр»). Настоящий кадр — `RenderablePresentFrame` (`app_glue.rs`), в тестах —
//! подделка со счётчиком освобождений.
//!
//! Инварианты:
//! - удержание живёт только внутри начатой смены: без неё «мягкий» сброс кэша выбрасывает кадр,
//!   как раньше (обычное окно, перемотка, пауза и любые сбросы вне смены не меняются);
//! - удержанный кадр держит аренду кадра старого декодера (а через неё — весь отставленный
//!   декодер, `retain_retired_video_decoder_for_outstanding_leases`), поэтому он отпускается
//!   на первом кадре нового медиа, по сроку и на любой «жёсткой» причине — не позже;
//! - конец смены — показанный кадр ДРУГОГО поколения рендера, чем было у окна в начале смены
//!   (установка нового медиа всегда двигает поколение, `staged_media_install/commit.rs`), но не
//!   раньше, чем окно увидело обе границы открытия: запись установки (`record_installed_media`)
//!   и событие плеера `MediaOpened`. Обе приходят в окно позже, чем плеер может уже показать
//!   первый кадр нового медиа, и обе сбрасывают кэш кадра; закрой смена раньше — такой поздний
//!   сброс выбросил бы уже показанный кадр нового медиа мимо замера и дал пустое поле. Внутри
//!   смены поздний сброс кадр нового медиа оставляет в кэше (`app_glue.rs`). Метку источника не
//!   сравниваем: у браузерного потока она одна на все поколения.

use std::time::{Duration, Instant};

mod app_glue;
mod report;

#[cfg(test)]
mod tests;

/// Что делать с последним кадром прежнего медиа, пока новое ещё не показало свой кадр.
///
/// Выдаёт режим окна (`WindowMode::media_switch_frame_policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaSwitchFramePolicy {
    /// Кадр уходит сразу; до первого кадра нового медиа — пустое поле (обычное окно; окно
    /// браузерного режима, если удержание выключено настройкой).
    ClearImmediately,
    /// Кадр остаётся на экране до первого кадра нового медиа, но не дольше `max_hold`.
    HoldUntilFirstNewFrame {
        /// Предел удержания: дальше — пустое поле, как без удержания.
        max_hold: Duration,
    },
}

/// Кадр глазами смены медиа.
pub(crate) trait SwitchFrame {
    /// Поколение рендера, которому принадлежит кадр (у нового медиа — другое).
    fn render_generation(&self) -> u64;
    /// Тот же ли это кадр декодера (повтор, а не новый кадр).
    fn is_same_frame(&self, other: &Self) -> bool;
}

/// Что показывает кадр окна, который уходит на экран, — с точки зрения смены медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaSwitchFrameContent {
    /// Смены нет — учитывать нечего.
    NotSwitching,
    /// Кадр окна без видео: пустое (чёрное) поле.
    NoVideo,
    /// Удержанный кадр прежнего медиа вместо пустого поля.
    HeldPreviousFrame,
    /// Живой кадр прежнего медиа: новое ещё ставится, прежнее играет.
    PreviousMediaFrame,
    /// Кадр нового медиа: на нём смена кончается.
    NewMediaFrame {
        /// Поколение рендера нового медиа (для строки замера).
        render_generation: u64,
    },
}

/// Почему смена кончилась без кадра нового медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaSwitchAbandonCause {
    /// Плеер окна отказал ставить новое медиа сразу.
    LoadRefused,
    /// Установка нового медиа не удалась.
    LoadFailed,
    /// Плеер остановлен.
    PlayerStopped,
    /// Плеер перешёл в состояние ошибки.
    PlayerFailed,
    /// Плеер завершает работу.
    PlayerShutdown,
    /// Фатальная ошибка плеера.
    PlayerFatalError,
    /// Медиа снято без замены (точный release установленного медиа).
    MediaReleased,
    /// Полный сброс медиа плеера (Clear очереди).
    MediaReset,
    /// Новое медиа — без видео: кадра ждать бессмысленно.
    AudioOnlyMedia,
}

/// Почему удержание отпущено раньше первого кадра нового медиа (смена при этом идёт дальше).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldReleaseCause {
    /// Поверхность окна пересоздана: ресурсы кадра держать небезопасно.
    SurfaceLifecycleBreak,
    /// Рендер не смог нарисовать кадр.
    RenderFailure,
    /// Плеер сообщил ошибку рендера.
    WorkerRenderError,
    /// Живая смена класса декодера: показ ведёт заморозка смены бэкенда.
    BackendSwap,
    /// Рендер заменён или окно уничтожается: кадр чужого рендера рисовать нельзя.
    RendererReplaced,
}

/// Что стало с прежним кадром к концу смены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaSwitchHoldOutcome {
    /// Прежний кадр держался до конца смены.
    Held,
    /// Срок удержания вышел раньше кадра нового медиа.
    Expired,
    /// Удержание отпущено «жёсткой» причиной.
    Released(HoldReleaseCause),
    /// Политика окна не держит кадр: он ушёл сразу.
    Disabled,
    /// Держать было нечего: прежнего кадра не было (первое медиа окна, звук без видео).
    NoPreviousFrame,
}

/// Чем кончилась смена медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaSwitchEnd {
    /// Показан первый кадр нового медиа.
    FirstNewFrame {
        /// Сколько прошло от начала смены до показа этого кадра.
        after: Duration,
        /// Поколение рендера нового медиа.
        render_generation: u64,
    },
    /// Смена прервана: кадра нового медиа не будет.
    Abandoned(MediaSwitchAbandonCause),
}

/// Итог одной смены медиа — содержимое строки замера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaSwitchReport {
    /// Порядковый номер смены в окне (1, 2, …).
    pub(crate) switch_number: u64,
    /// Чем кончилась смена.
    pub(crate) end: MediaSwitchEnd,
    /// Что стало с прежним кадром.
    pub(crate) hold_outcome: MediaSwitchHoldOutcome,
    /// Кадров окна, ушедших на экран без видео (пустое поле), за смену.
    pub(crate) empty_frames: u64,
    /// Кадров окна, ушедших на экран с удержанным прежним кадром.
    pub(crate) held_frames: u64,
}

/// Судьба кадра, который выбросил кэш (`offer_discarded_frame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiscardedFrameFate {
    /// Кадр остался на экране до первого кадра нового медиа.
    HeldOnScreen,
    /// Смены нет — кадр отпущен, как раньше.
    DroppedOutsideSwitch,
    /// Политика окна не держит кадр — отпущен.
    DroppedByPolicy,
    /// Удержание этой смены уже кончилось (срок, жёсткая причина) — отпущен.
    DroppedAfterHoldEnded,
}

/// Итог начала смены (`begin_switch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaSwitchBegin {
    /// Началась новая смена с этим номером.
    Started {
        /// Номер смены.
        switch_number: u64,
    },
    /// Смена уже идёт (новое медиа заменило ещё не показанное): начало и замер прежние —
    /// зритель ждёт с первого начала.
    AlreadyInProgress {
        /// Номер идущей смены.
        switch_number: u64,
    },
}

/// Итог просьбы отпустить удержание (`release_hold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoldRelease {
    /// Удержанный кадр отпущен.
    Released,
    /// Отпускать нечего (смены нет или кадр не держится).
    NothingHeld,
}

/// Граница открытия нового медиа, которую окно должно увидеть до конца смены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenBoundary {
    /// Окно записало установку нового медиа (`record_installed_media`).
    InstallRecorded,
    /// Окно получило событие плеера `MediaOpened`.
    PlayerReportedOpened,
}

/// Итог проверки срока удержания (`expire_due_hold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoldExpiry {
    /// Срок вышел только что: кадр отпущен.
    Expired,
    /// Срок не вышел или кадр не держится.
    NotDue,
}

/// Место прежнего кадра в идущей смене.
enum HoldSlot<Frame> {
    /// Кэш ещё ничего не выбрасывал (или прежнего кадра не было).
    NothingOffered,
    /// Кадр выброшен, и политика его не держит.
    DroppedByPolicy,
    /// Кадр на экране до первого кадра нового медиа, но не позже `until`.
    Holding {
        /// Удержанный кадр (держит аренду кадра старого декодера).
        frame: Frame,
        /// Когда удержание кончается по сроку.
        until: Instant,
    },
    /// Срок вышел, кадр отпущен.
    Expired,
    /// Кадр отпущен «жёсткой» причиной.
    Released(HoldReleaseCause),
    /// Кадр отпущен, потому что обычный путь уже подготовил кадр нового медиа: экрану
    /// удержанный больше не нужен, а отставленный декодер прежнего медиа свободен сразу.
    HandedOverToNewFrame,
}

impl<Frame> HoldSlot<Frame> {
    /// Что стало с прежним кадром, если смена кончится сейчас.
    const fn outcome(&self) -> MediaSwitchHoldOutcome {
        match self {
            Self::NothingOffered => MediaSwitchHoldOutcome::NoPreviousFrame,
            Self::DroppedByPolicy => MediaSwitchHoldOutcome::Disabled,
            Self::Holding { .. } | Self::HandedOverToNewFrame => MediaSwitchHoldOutcome::Held,
            Self::Expired => MediaSwitchHoldOutcome::Expired,
            Self::Released(cause) => MediaSwitchHoldOutcome::Released(*cause),
        }
    }
}

/// Идущая смена медиа.
struct ActiveSwitch<Frame> {
    /// Номер смены (для строки замера).
    switch_number: u64,
    /// Политика окна на момент начала смены.
    policy: MediaSwitchFramePolicy,
    /// Когда окно начало ставить новое медиа.
    began_at: Instant,
    /// Поколение рендера окна в начале смены: кадр другого поколения — уже новое медиа.
    from_render_generation: u64,
    /// Прежний кадр.
    hold: HoldSlot<Frame>,
    /// Кадров без видео на экране за смену.
    empty_frames: u64,
    /// Кадров с удержанным прежним кадром на экране за смену.
    held_frames: u64,
    /// Окно записало установку нового медиа.
    install_recorded: bool,
    /// Окно получило событие плеера `MediaOpened`.
    player_reported_opened: bool,
    /// Первый показанный кадр нового медиа: через сколько от начала и какого поколения.
    first_new_frame: Option<(Duration, u64)>,
}

impl<Frame> ActiveSwitch<Frame> {
    /// Видело ли окно обе границы открытия (тогда кадр нового медиа кончает смену).
    const fn open_boundaries_seen(&self) -> bool {
        self.install_recorded && self.player_reported_opened
    }

    /// Итог смены, кончившейся так, как говорит `end`.
    fn report(&self, end: MediaSwitchEnd) -> MediaSwitchReport {
        MediaSwitchReport {
            switch_number: self.switch_number,
            end,
            hold_outcome: self.hold.outcome(),
            empty_frames: self.empty_frames,
            held_frames: self.held_frames,
        }
    }
}

/// Владелец кадра на смене медиа (см. описание модуля). Один на `AppState`.
pub(crate) struct MediaSwitchFrameHold<Frame> {
    /// Сколько смен начато за жизнь окна.
    switches_begun: u64,
    /// Идущая смена; `None` — смены нет.
    active: Option<ActiveSwitch<Frame>>,
}

impl<Frame> Default for MediaSwitchFrameHold<Frame> {
    fn default() -> Self {
        Self {
            switches_begun: 0,
            active: None,
        }
    }
}

impl<Frame: SwitchFrame> MediaSwitchFrameHold<Frame> {
    /// Окно начинает ставить новое медиа: с этого мига идёт замер, а «мягкие» сбросы кэша
    /// отдают кадр в удержание (если политика держит).
    ///
    /// `from_render_generation` — поколение рендера, которое окно показывает сейчас (до
    /// установки: после неё снимок уже несёт новое поколение).
    pub(super) fn begin_switch(
        &mut self,
        policy: MediaSwitchFramePolicy,
        from_render_generation: u64,
        now: Instant,
    ) -> MediaSwitchBegin {
        if let Some(active) = &self.active {
            return MediaSwitchBegin::AlreadyInProgress {
                switch_number: active.switch_number,
            };
        }
        // 2⁶⁴ смен за жизнь окна недостижимо; насыщение — только защита от паники.
        self.switches_begun = self.switches_begun.saturating_add(1);
        self.active = Some(ActiveSwitch {
            switch_number: self.switches_begun,
            policy,
            began_at: now,
            from_render_generation,
            hold: HoldSlot::NothingOffered,
            empty_frames: 0,
            held_frames: 0,
            install_recorded: false,
            player_reported_opened: false,
            first_new_frame: None,
        });
        MediaSwitchBegin::Started {
            switch_number: self.switches_begun,
        }
    }

    /// Кэш кадра выбросил `frame` по «мягкой» причине: держать его или отпустить.
    ///
    /// Удержание начинается с первого выброшенного кадра смены; более свежий выброшенный кадр
    /// заменяет удержанный, а срок считается от первого (предел того, сколько зритель видит
    /// застывшую картинку).
    pub(super) fn offer_discarded_frame(
        &mut self,
        frame: Frame,
        now: Instant,
    ) -> DiscardedFrameFate {
        let Some(active) = self.active.as_mut() else {
            return DiscardedFrameFate::DroppedOutsideSwitch;
        };
        let max_hold = match active.policy {
            MediaSwitchFramePolicy::ClearImmediately => {
                active.hold = HoldSlot::DroppedByPolicy;
                return DiscardedFrameFate::DroppedByPolicy;
            }
            MediaSwitchFramePolicy::HoldUntilFirstNewFrame { max_hold } => max_hold,
        };
        let until = match &active.hold {
            HoldSlot::NothingOffered
            | HoldSlot::DroppedByPolicy
            | HoldSlot::HandedOverToNewFrame => now + max_hold,
            HoldSlot::Holding { until, .. } => *until,
            HoldSlot::Expired | HoldSlot::Released(_) => {
                return DiscardedFrameFate::DroppedAfterHoldEnded;
            }
        };
        // Прежний удержанный кадр (если был) отпускается здесь — заменой поля.
        active.hold = HoldSlot::Holding { frame, until };
        DiscardedFrameFate::HeldOnScreen
    }

    /// «Жёсткая» причина: кадр держать больше нельзя. Смена (и замер) продолжаются.
    pub(super) fn release_hold(&mut self, cause: HoldReleaseCause) -> HoldRelease {
        let Some(active) = self.active.as_mut() else {
            return HoldRelease::NothingHeld;
        };
        if !matches!(active.hold, HoldSlot::Holding { .. }) {
            return HoldRelease::NothingHeld;
        }
        active.hold = HoldSlot::Released(cause);
        HoldRelease::Released
    }

    /// Обычный путь подготовил `frame` к показу. Если это уже кадр нового медиа, удержанный кадр
    /// отпускается сразу — не на показе после границ открытия: экрану он больше не нужен (новый
    /// кадр в кэше), а держит весь отставленный декодер прежнего медиа. Смена (и замер) идут до
    /// показа после границ, как прежде.
    pub(super) fn note_frame_ready(&mut self, frame: &Frame) -> HoldRelease {
        let Some(active) = self.active.as_mut() else {
            return HoldRelease::NothingHeld;
        };
        let new_media_frame = frame.render_generation() != active.from_render_generation;
        if !new_media_frame || !matches!(active.hold, HoldSlot::Holding { .. }) {
            return HoldRelease::NothingHeld;
        }
        active.hold = HoldSlot::HandedOverToNewFrame;
        HoldRelease::Released
    }

    /// Смена прервана: кадра нового медиа не будет. Удержание отпускается; итог — для строки
    /// замера (`None` — смены не было).
    pub(super) fn abandon_switch(
        &mut self,
        cause: MediaSwitchAbandonCause,
    ) -> Option<MediaSwitchReport> {
        let active = self.active.take()?;
        Some(active.report(MediaSwitchEnd::Abandoned(cause)))
    }

    /// Кадр для показа вместо пустого поля; по истечении срока удержание кончается здесь.
    pub(super) fn held_frame(&mut self, now: Instant) -> Option<&Frame> {
        self.expire_due_hold(now);
        match &self.active.as_ref()?.hold {
            HoldSlot::Holding { frame, .. } => Some(frame),
            _ => None,
        }
    }

    /// Срок вышел — кадр отпускается (`HoldExpiry::Expired`). Зовётся на каждом пробуждении
    /// окна, а не только когда обычный путь остался без кадра: пока прежнее медиа ещё играет,
    /// запасной путь не спрашивают, а просроченный срок не должен оставаться ни будильником в
    /// прошлом (окно крутилось бы без сна), ни причиной показывать каждый кадр.
    pub(super) fn expire_due_hold(&mut self, now: Instant) -> HoldExpiry {
        let Some(active) = self.active.as_mut() else {
            return HoldExpiry::NotDue;
        };
        match &active.hold {
            HoldSlot::Holding { until, .. } if now >= *until => {
                active.hold = HoldSlot::Expired;
                HoldExpiry::Expired
            }
            _ => HoldExpiry::NotDue,
        }
    }

    /// Когда удержание кончится по сроку (`None` — кадр не держится). Окну нужно проснуться
    /// в этот миг: иначе застывший кадр (и отставленный декодер) живут до следующего события.
    pub(super) fn hold_deadline(&self) -> Option<Instant> {
        match &self.active.as_ref()?.hold {
            HoldSlot::Holding { until, .. } => Some(*until),
            _ => None,
        }
    }

    /// Идёт ли смена медиа.
    pub(super) fn is_switching(&self) -> bool {
        self.active.is_some()
    }

    /// Держится ли сейчас прежний кадр (тесты; окну хватает `hold_deadline`).
    #[cfg(test)]
    pub(super) fn is_holding(&self) -> bool {
        self.hold_deadline().is_some()
    }

    /// Поставлено ли уже новое медиа: поколение рендера ушло от поколения начала смены.
    pub(super) fn new_media_installed(&self, current_render_generation: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.from_render_generation != current_render_generation)
    }

    /// Окно увидело границу открытия нового медиа (см. описание модуля).
    pub(super) fn note_open_boundary(&mut self, boundary: OpenBoundary) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        match boundary {
            OpenBoundary::InstallRecorded => active.install_recorded = true,
            OpenBoundary::PlayerReportedOpened => active.player_reported_opened = true,
        }
    }

    /// Оставить ли в кэше кадр, который сброс на открытии медиа выбросил бы: да, если смена
    /// идёт, политика держит кадры и кадр уже нового медиа (поколение ушло от начала смены).
    /// С выключенным удержанием окно ведёт себя как до 16b3c — кадр уходит.
    pub(super) fn keeps_new_media_frame_cached(&self, frame: &Frame) -> bool {
        self.active.as_ref().is_some_and(|active| {
            matches!(
                active.policy,
                MediaSwitchFramePolicy::HoldUntilFirstNewFrame { .. }
            ) && frame.render_generation() != active.from_render_generation
        })
    }

    /// Что покажет кадр окна с этим видеокадром (`None` — без видео). Состояние не меняется:
    /// учёт — в [`Self::note_frame_presented`], только если кадр правда ушёл на экран.
    pub(super) fn classify_frame(&self, frame: Option<&Frame>) -> MediaSwitchFrameContent {
        let Some(active) = &self.active else {
            return MediaSwitchFrameContent::NotSwitching;
        };
        let Some(frame) = frame else {
            return MediaSwitchFrameContent::NoVideo;
        };
        // Удержанный кадр — всегда «удержанный», даже если он уже нового поколения: смену
        // кончает только кадр обычного пути, иначе после конца смены рисовать было бы нечего.
        if let HoldSlot::Holding { frame: held, .. } = &active.hold
            && held.is_same_frame(frame)
        {
            return MediaSwitchFrameContent::HeldPreviousFrame;
        }
        let render_generation = frame.render_generation();
        if render_generation != active.from_render_generation {
            return MediaSwitchFrameContent::NewMediaFrame { render_generation };
        }
        MediaSwitchFrameContent::PreviousMediaFrame
    }

    /// Кадр окна ушёл на экран. На кадре нового медиа, показанном после обеих границ открытия,
    /// смена кончается: удержание отпускается, итог — для строки замера (`first_frame_ms` —
    /// от самого первого показанного кадра нового медиа, даже если он был до границ).
    pub(super) fn note_frame_presented(
        &mut self,
        content: MediaSwitchFrameContent,
        now: Instant,
    ) -> Option<MediaSwitchReport> {
        let active = self.active.as_mut()?;
        match content {
            MediaSwitchFrameContent::NotSwitching | MediaSwitchFrameContent::PreviousMediaFrame => {
                None
            }
            MediaSwitchFrameContent::NoVideo => {
                active.empty_frames = active.empty_frames.saturating_add(1);
                None
            }
            MediaSwitchFrameContent::HeldPreviousFrame => {
                active.held_frames = active.held_frames.saturating_add(1);
                None
            }
            MediaSwitchFrameContent::NewMediaFrame { render_generation } => {
                let (after, first_generation) = *active.first_new_frame.get_or_insert((
                    now.saturating_duration_since(active.began_at),
                    render_generation,
                ));
                if !active.open_boundaries_seen() {
                    return None;
                }
                let active = self.active.take()?;
                Some(active.report(MediaSwitchEnd::FirstNewFrame {
                    after,
                    render_generation: first_generation,
                }))
            }
        }
    }
}
