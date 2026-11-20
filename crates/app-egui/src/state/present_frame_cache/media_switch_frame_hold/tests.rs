//! Владелец кадра на смене медиа без GPU: подделка кадра считает освобождения, время — явное.
//!
//! Каждый тест проверяет то, что увидит зритель или судья стенда: что рисуется вместо пустого
//! поля, когда кадр прежнего медиа отпускается (держит ли он ещё декодер) и что попадёт в
//! строку замера.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::app_glue::{DiscardRole, discard_role};
use super::report::media_switch_report_fields;
use super::*;
use crate::state::present_frame_cache::CachedPresentFrameDiscardReason;

/// Предел удержания в тестах (как умолчание настроек).
const MAX_HOLD: Duration = Duration::from_secs(3);
/// Поколение рендера прежнего медиа.
const OLD_GENERATION: u64 = 4;
/// Поколение рендера нового медиа (установка двигает поколение).
const NEW_GENERATION: u64 = 5;

/// Подделка кадра: поколение, номер кадра и счётчик освобождений общий для всех копий —
/// как аренда настоящего кадра, которая возвращается декодеру, когда умирает последняя копия.
#[derive(Debug)]
struct FakeFrame {
    render_generation: u64,
    frame_number: u64,
    releases: Rc<Cell<u32>>,
}

impl FakeFrame {
    fn new(render_generation: u64, frame_number: u64, releases: &Rc<Cell<u32>>) -> Self {
        Self {
            render_generation,
            frame_number,
            releases: Rc::clone(releases),
        }
    }
}

impl Drop for FakeFrame {
    fn drop(&mut self) {
        self.releases.set(self.releases.get() + 1);
    }
}

impl SwitchFrame for FakeFrame {
    fn render_generation(&self) -> u64 {
        self.render_generation
    }

    fn is_same_frame(&self, other: &Self) -> bool {
        self.render_generation == other.render_generation && self.frame_number == other.frame_number
    }
}

/// Политика окна браузерного режима.
const HOLD: MediaSwitchFramePolicy =
    MediaSwitchFramePolicy::HoldUntilFirstNewFrame { max_hold: MAX_HOLD };

/// Окно, начавшее смену с прежнего поколения в момент `now`.
fn switching(policy: MediaSwitchFramePolicy, now: Instant) -> MediaSwitchFrameHold<FakeFrame> {
    let mut hold = MediaSwitchFrameHold::default();
    assert_eq!(
        hold.begin_switch(policy, OLD_GENERATION, now),
        MediaSwitchBegin::Started { switch_number: 1 }
    );
    hold
}

/// Окно увидело обе границы открытия нового медиа: запись установки и событие плеера.
fn open_new_media(hold: &mut MediaSwitchFrameHold<FakeFrame>) {
    hold.note_open_boundary(OpenBoundary::InstallRecorded);
    hold.note_open_boundary(OpenBoundary::PlayerReportedOpened);
}

/// Показывает кадр окна так, как это делает видео-стадия: обычный путь дал `current` (или
/// ничего) — иначе удержанный кадр; возвращает итог смены, если она кончилась на этом кадре.
fn present(
    hold: &mut MediaSwitchFrameHold<FakeFrame>,
    current: Option<&FakeFrame>,
    now: Instant,
) -> Option<MediaSwitchReport> {
    let content = match current {
        Some(frame) => hold.classify_frame(Some(frame)),
        None => {
            let shown = hold.held_frame(now).map(|frame| frame.frame_number);
            match shown {
                Some(_) => MediaSwitchFrameContent::HeldPreviousFrame,
                None => hold.classify_frame(None),
            }
        }
    };
    hold.note_frame_presented(content, now)
}

/// Обычное окно (и окно браузерного режима с выключенной настройкой): кадр прежнего медиа
/// уходит сразу — до первого кадра нового рисуется пустое поле, замер это видит.
#[test]
fn clear_immediately_drops_the_previous_frame_and_counts_empty_frames() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(MediaSwitchFramePolicy::ClearImmediately, start);

    let fate = hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);

    assert_eq!(fate, DiscardedFrameFate::DroppedByPolicy);
    assert_eq!(
        releases.get(),
        1,
        "the previous frame went back to its decoder at once"
    );
    assert!(hold.held_frame(start).is_none());
    assert!(!hold.is_holding());
    assert_eq!(
        present(&mut hold, None, start + Duration::from_millis(16)),
        None
    );
    assert_eq!(
        present(&mut hold, None, start + Duration::from_millis(33)),
        None
    );
    open_new_media(&mut hold);
    let new_frame = FakeFrame::new(NEW_GENERATION, 1, &releases);
    let report = present(
        &mut hold,
        Some(&new_frame),
        start + Duration::from_millis(50),
    )
    .expect("the first new frame ends the switch");
    assert_eq!(report.hold_outcome, MediaSwitchHoldOutcome::Disabled);
    assert_eq!(report.empty_frames, 2);
    assert_eq!(report.held_frames, 0);
}

/// Главное (решение 380): прежний кадр рисуется вместо пустого поля до первого кадра нового
/// медиа, ни одного пустого кадра; на первом кадре нового медиа удержанный отпускается.
#[test]
fn the_previous_frame_is_shown_until_the_first_new_frame_then_released() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);

    let fate = hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 7, &releases), start);

    assert_eq!(fate, DiscardedFrameFate::HeldOnScreen);
    assert!(hold.is_holding());
    assert_eq!(hold.hold_deadline(), Some(start + MAX_HOLD));
    assert_eq!(
        hold.held_frame(start).map(|frame| frame.frame_number),
        Some(7),
        "the previous picture stays on screen"
    );
    assert_eq!(
        present(&mut hold, None, start + Duration::from_millis(16)),
        None
    );
    assert_eq!(
        present(&mut hold, None, start + Duration::from_millis(33)),
        None
    );
    assert_eq!(
        releases.get(),
        0,
        "the held frame is still alive while it is shown"
    );

    open_new_media(&mut hold);
    let new_frame = FakeFrame::new(NEW_GENERATION, 1, &releases);
    let report = present(
        &mut hold,
        Some(&new_frame),
        start + Duration::from_millis(48),
    )
    .expect("the first new frame ends the switch");

    assert_eq!(
        releases.get(),
        1,
        "the held frame is released with the first new frame"
    );
    assert_eq!(
        report,
        MediaSwitchReport {
            switch_number: 1,
            end: MediaSwitchEnd::FirstNewFrame {
                after: Duration::from_millis(48),
                render_generation: NEW_GENERATION,
            },
            hold_outcome: MediaSwitchHoldOutcome::Held,
            empty_frames: 0,
            held_frames: 2,
        }
    );
    assert!(!hold.is_switching());
    assert_eq!(
        media_switch_report_fields(&report),
        "switch=1 end=first_frame outcome=held release=none empty_frames=0 held_frames=2 \
         first_frame_ms=48 render_generation=5"
    );
}

/// Вышел срок: удержанный кадр отпускается (декодер прежнего медиа свободен), дальше — пустое
/// поле, как без удержания; строка говорит `expired` и считает пустые кадры после срока.
#[test]
fn the_hold_expires_at_its_limit_and_empty_frames_follow() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);

    assert!(
        hold.held_frame(start + MAX_HOLD - Duration::from_millis(1))
            .is_some()
    );
    assert_eq!(present(&mut hold, None, start + MAX_HOLD), None);

    assert_eq!(
        releases.get(),
        1,
        "the held frame is released when the limit is reached"
    );
    assert!(!hold.is_holding());
    assert_eq!(hold.hold_deadline(), None);
    assert_eq!(
        present(
            &mut hold,
            None,
            start + MAX_HOLD + Duration::from_millis(16)
        ),
        None
    );
    // После срока выброшенный кадр больше не держится: срок — предел всей смены.
    let late = hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 2, &releases), start);
    assert_eq!(late, DiscardedFrameFate::DroppedAfterHoldEnded);
    assert_eq!(releases.get(), 2);
    open_new_media(&mut hold);
    let new_frame = FakeFrame::new(NEW_GENERATION, 1, &releases);
    let report = present(&mut hold, Some(&new_frame), start + Duration::from_secs(4))
        .expect("the first new frame ends the switch");
    assert_eq!(report.hold_outcome, MediaSwitchHoldOutcome::Expired);
    assert_eq!(report.empty_frames, 2);
    assert_eq!(
        report.end,
        MediaSwitchEnd::FirstNewFrame {
            after: Duration::from_secs(4),
            render_generation: NEW_GENERATION,
        }
    );
}

/// Жёсткая причина посреди смены (поломка поверхности, рендера, смена бэкенда): кадр отпущен
/// сразу, смена и замер идут дальше, более поздний мягкий сброс кадр уже не держит.
#[test]
fn a_hard_release_drops_the_held_frame_but_the_switch_keeps_measuring() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);

    assert_eq!(
        hold.release_hold(HoldReleaseCause::SurfaceLifecycleBreak),
        HoldRelease::Released
    );

    assert_eq!(releases.get(), 1);
    assert!(hold.is_switching());
    assert!(hold.held_frame(start).is_none());
    assert_eq!(
        hold.release_hold(HoldReleaseCause::RenderFailure),
        HoldRelease::NothingHeld
    );
    let fate = hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 2, &releases), start);
    assert_eq!(fate, DiscardedFrameFate::DroppedAfterHoldEnded);
    assert_eq!(releases.get(), 2);
    assert_eq!(
        present(&mut hold, None, start + Duration::from_millis(16)),
        None
    );
    open_new_media(&mut hold);
    let new_frame = FakeFrame::new(NEW_GENERATION, 1, &releases);
    let report = present(
        &mut hold,
        Some(&new_frame),
        start + Duration::from_millis(40),
    )
    .expect("the switch still ends with the first new frame");
    assert_eq!(
        report.hold_outcome,
        MediaSwitchHoldOutcome::Released(HoldReleaseCause::SurfaceLifecycleBreak)
    );
    assert_eq!(report.empty_frames, 1);
    assert!(
        media_switch_report_fields(&report)
            .contains("outcome=released release=surface_lifecycle_break empty_frames=1")
    );
}

/// Смена оборвана (плеер остановлен, установка не удалась): кадр отпущен, строка — с причиной
/// и без «первого кадра».
#[test]
fn an_abandoned_switch_releases_the_frame_and_reports_the_cause() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);
    present(&mut hold, None, start + Duration::from_millis(16));

    let report = hold
        .abandon_switch(MediaSwitchAbandonCause::PlayerStopped)
        .expect("a switch was in progress");

    assert_eq!(releases.get(), 1);
    assert!(!hold.is_switching());
    assert_eq!(
        media_switch_report_fields(&report),
        "switch=1 end=player_stopped outcome=held release=none empty_frames=0 held_frames=1 \
         first_frame_ms=none render_generation=none"
    );
    assert_eq!(
        hold.abandon_switch(MediaSwitchAbandonCause::PlayerStopped),
        None,
        "nothing to report twice"
    );
}

/// Вне смены (перемотка, пауза, обычная жизнь кэша) выброшенный кадр отпускается сразу —
/// поведение кэша не меняется; учёт кадров ничего не пишет.
#[test]
fn outside_a_switch_the_discarded_frame_is_dropped_and_nothing_is_counted() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = MediaSwitchFrameHold::<FakeFrame>::default();

    let fate = hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);

    assert_eq!(fate, DiscardedFrameFate::DroppedOutsideSwitch);
    assert_eq!(releases.get(), 1);
    assert!(hold.held_frame(start).is_none());
    assert_eq!(
        hold.classify_frame(None),
        MediaSwitchFrameContent::NotSwitching
    );
    assert_eq!(present(&mut hold, None, start), None);
    assert_eq!(
        hold.release_hold(HoldReleaseCause::BackendSwap),
        HoldRelease::NothingHeld
    );
    assert_eq!(
        hold.abandon_switch(MediaSwitchAbandonCause::LoadFailed),
        None
    );
}

/// Пока новое медиа ставится, прежнее играет: его живые кадры не кончают смену и не считаются
/// пустыми; более свежий выброшенный кадр заменяет удержанный (тот отпускается), срок — от
/// первого удержания.
#[test]
fn live_frames_of_the_previous_media_neither_end_nor_count_and_a_fresher_frame_replaces_the_held_one()
 {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    let still_playing = FakeFrame::new(OLD_GENERATION, 1, &releases);

    assert_eq!(
        hold.classify_frame(Some(&still_playing)),
        MediaSwitchFrameContent::PreviousMediaFrame
    );
    assert_eq!(present(&mut hold, Some(&still_playing), start), None);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 2, &releases), start);
    let later = start + Duration::from_millis(500);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 3, &releases), later);

    assert_eq!(
        releases.get(),
        1,
        "the older held frame is released when replaced"
    );
    assert_eq!(
        hold.held_frame(later).map(|frame| frame.frame_number),
        Some(3)
    );
    assert_eq!(hold.hold_deadline(), Some(start + MAX_HOLD));
    let held_copy = FakeFrame::new(OLD_GENERATION, 3, &releases);
    assert_eq!(
        hold.classify_frame(Some(&held_copy)),
        MediaSwitchFrameContent::HeldPreviousFrame
    );
}

/// Новое медиа, заменившее ещё не показанное, — та же смена: номер и начало прежние.
#[test]
fn a_superseding_load_continues_the_switch_in_progress() {
    let start = Instant::now();
    let mut hold = switching(HOLD, start);

    let again = hold.begin_switch(HOLD, OLD_GENERATION + 1, start + Duration::from_millis(200));

    assert_eq!(
        again,
        MediaSwitchBegin::AlreadyInProgress { switch_number: 1 }
    );
    assert!(!hold.new_media_installed(OLD_GENERATION));
    assert!(hold.new_media_installed(NEW_GENERATION));
    let releases = Rc::new(Cell::new(0));
    open_new_media(&mut hold);
    let new_frame = FakeFrame::new(NEW_GENERATION, 1, &releases);
    let report = present(
        &mut hold,
        Some(&new_frame),
        start + Duration::from_millis(300),
    )
    .expect("the first new frame ends the switch");
    assert_eq!(
        report.end,
        MediaSwitchEnd::FirstNewFrame {
            after: Duration::from_millis(300),
            render_generation: NEW_GENERATION,
        }
    );
    assert_eq!(report.hold_outcome, MediaSwitchHoldOutcome::NoPreviousFrame);
    assert_eq!(
        hold.begin_switch(HOLD, NEW_GENERATION, start),
        MediaSwitchBegin::Started { switch_number: 2 }
    );
}

/// Роли причин сброса кэша: смена медиа сама по себе — мягкая; поломки — отпускают кадр;
/// конец медиа без замены (в т.ч. точный release, раньше шедший как граница открытия) — кончает
/// смену.
#[test]
fn discard_reasons_split_into_soft_releasing_and_ending() {
    for reason in [
        CachedPresentFrameDiscardReason::MediaOpenBoundary,
        CachedPresentFrameDiscardReason::CurrentVideoFrameMissing,
        CachedPresentFrameDiscardReason::SourceLabelChanged,
        CachedPresentFrameDiscardReason::RenderGenerationChanged,
        CachedPresentFrameDiscardReason::PlayerMediaOpenRequested,
        CachedPresentFrameDiscardReason::PlayerMediaOpened,
    ] {
        assert_eq!(discard_role(reason), DiscardRole::MayHold, "{reason:?}");
    }
    for (reason, cause) in [
        (
            CachedPresentFrameDiscardReason::SurfaceLifecycleBreak,
            HoldReleaseCause::SurfaceLifecycleBreak,
        ),
        (
            CachedPresentFrameDiscardReason::RenderFailure,
            HoldReleaseCause::RenderFailure,
        ),
        (
            CachedPresentFrameDiscardReason::WorkerRenderError,
            HoldReleaseCause::WorkerRenderError,
        ),
        (
            CachedPresentFrameDiscardReason::RuntimeDrop,
            HoldReleaseCause::RendererReplaced,
        ),
    ] {
        assert_eq!(
            discard_role(reason),
            DiscardRole::ReleasesHold(cause),
            "{reason:?}"
        );
    }
    for (reason, cause) in [
        (
            CachedPresentFrameDiscardReason::InstalledMediaReleased,
            MediaSwitchAbandonCause::MediaReleased,
        ),
        (
            CachedPresentFrameDiscardReason::PlaylistClearReset,
            MediaSwitchAbandonCause::MediaReset,
        ),
        (
            CachedPresentFrameDiscardReason::PlayerStopped,
            MediaSwitchAbandonCause::PlayerStopped,
        ),
        (
            CachedPresentFrameDiscardReason::PlayerFailed,
            MediaSwitchAbandonCause::PlayerFailed,
        ),
        (
            CachedPresentFrameDiscardReason::PlayerShutdownRequested,
            MediaSwitchAbandonCause::PlayerShutdown,
        ),
        (
            CachedPresentFrameDiscardReason::PlayerFatalError,
            MediaSwitchAbandonCause::PlayerFatalError,
        ),
    ] {
        assert_eq!(
            discard_role(reason),
            DiscardRole::EndsSwitch(cause),
            "{reason:?}"
        );
    }
}

/// Прежнее медиа ещё играет дольше срока (установка нового затянулась): запасной путь не
/// спрашивают, но проверка срока на пробуждении окна отпускает кадр — будильник не остаётся в
/// прошлом, и «держу кадр» больше не заставляет показывать каждый кадр.
#[test]
fn the_wake_check_releases_an_overdue_hold_even_while_the_previous_media_plays() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);
    let still_playing = FakeFrame::new(OLD_GENERATION, 2, &releases);
    assert_eq!(
        present(&mut hold, Some(&still_playing), start + MAX_HOLD),
        None
    );

    assert_eq!(
        hold.expire_due_hold(start + MAX_HOLD - Duration::from_millis(1)),
        HoldExpiry::NotDue
    );
    assert!(hold.is_holding());
    assert_eq!(hold.expire_due_hold(start + MAX_HOLD), HoldExpiry::Expired);

    assert_eq!(releases.get(), 1);
    assert!(!hold.is_holding());
    assert_eq!(hold.hold_deadline(), None);
    assert!(
        hold.is_switching(),
        "the switch still waits for the new media"
    );
}

/// Плеер показал кадр нового медиа раньше, чем окно записало установку: смена не кончается,
/// пока окно не увидит обе границы открытия (их поздний сброс кэша иначе прошёл бы мимо
/// замера); время до первого кадра — от самого первого показанного кадра нового медиа.
#[test]
fn a_new_frame_before_the_open_boundaries_keeps_the_switch_open() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    let first_new = FakeFrame::new(NEW_GENERATION, 1, &releases);

    assert_eq!(
        present(
            &mut hold,
            Some(&first_new),
            start + Duration::from_millis(20)
        ),
        None
    );
    hold.note_open_boundary(OpenBoundary::InstallRecorded);
    assert_eq!(
        present(
            &mut hold,
            Some(&first_new),
            start + Duration::from_millis(36)
        ),
        None,
        "the player has not reported the open yet"
    );
    assert!(hold.is_switching());
    assert!(hold.keeps_new_media_frame_cached(&first_new));
    hold.note_open_boundary(OpenBoundary::PlayerReportedOpened);
    let report = present(
        &mut hold,
        Some(&first_new),
        start + Duration::from_millis(52),
    )
    .expect("both boundaries seen: the new frame ends the switch");

    assert_eq!(
        report.end,
        MediaSwitchEnd::FirstNewFrame {
            after: Duration::from_millis(20),
            render_generation: NEW_GENERATION,
        }
    );
    assert_eq!(report.empty_frames, 0);
    assert!(
        !hold.keeps_new_media_frame_cached(&first_new),
        "no switch, nothing to keep"
    );
}

/// Удержанный кадр, который уже принадлежит новому медиа, смену не кончает: кончает только
/// кадр обычного пути (иначе после конца смены рисовать было бы нечего).
#[test]
fn a_held_frame_of_the_new_media_does_not_end_the_switch() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    open_new_media(&mut hold);
    hold.offer_discarded_frame(FakeFrame::new(NEW_GENERATION, 1, &releases), start);

    assert_eq!(
        present(&mut hold, None, start + Duration::from_millis(16)),
        None
    );

    assert!(hold.is_switching());
    let next_new = FakeFrame::new(NEW_GENERATION, 2, &releases);
    let report = present(
        &mut hold,
        Some(&next_new),
        start + Duration::from_millis(33),
    )
    .expect("a frame of the regular path ends the switch");
    assert_eq!(report.held_frames, 1);
}

/// С выключенным удержанием поздний сброс на открытии выбрасывает и кадр нового медиа — как до
/// 16b3c (по этому режиму судья стенда сравнивает «до» и «после»).
#[test]
fn with_the_hold_disabled_a_late_open_clear_drops_even_a_new_media_frame() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let hold = switching(MediaSwitchFramePolicy::ClearImmediately, start);

    assert!(!hold.keeps_new_media_frame_cached(&FakeFrame::new(NEW_GENERATION, 1, &releases)));
    assert!(!hold.keeps_new_media_frame_cached(&FakeFrame::new(OLD_GENERATION, 1, &releases)));
}

/// Обычный путь подготовил кадр нового медиа: удержанный кадр прежнего отпускается сразу (с ним
/// и отставленный декодер), хотя смена для замера идёт до показа после границ открытия; итог —
/// всё ещё «держался».
#[test]
fn a_ready_new_media_frame_releases_the_held_frame_before_the_switch_ends() {
    let start = Instant::now();
    let releases = Rc::new(Cell::new(0));
    let mut hold = switching(HOLD, start);
    hold.offer_discarded_frame(FakeFrame::new(OLD_GENERATION, 1, &releases), start);
    let still_playing = FakeFrame::new(OLD_GENERATION, 2, &releases);
    assert_eq!(
        hold.note_frame_ready(&still_playing),
        HoldRelease::NothingHeld
    );
    assert_eq!(
        releases.get(),
        0,
        "a frame of the previous media releases nothing"
    );

    let new_frame = FakeFrame::new(NEW_GENERATION, 1, &releases);
    assert_eq!(hold.note_frame_ready(&new_frame), HoldRelease::Released);

    assert_eq!(
        releases.get(),
        1,
        "the held frame and its decoder are free at once"
    );
    assert!(hold.is_switching());
    assert_eq!(hold.hold_deadline(), None);
    open_new_media(&mut hold);
    let report = present(
        &mut hold,
        Some(&new_frame),
        start + Duration::from_millis(30),
    )
    .expect("the new frame ends the switch");
    assert_eq!(report.hold_outcome, MediaSwitchHoldOutcome::Held);
    assert_eq!(report.empty_frames, 0);
}
