//! Сессия 18: установка из Open не ждёт player на UI-потоке и забирает только свой terminal.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use player_core::MediaInstallCancellationCause;

use super::*;
use crate::media_open::{
    MediaOpenStartError, MediaOpenTerminalOutcome, MediaOpenUserFailureReason,
};

/// Сколько «медленный» fake-player думает над файлом.
const SLOW_PLAYER_DELAY: Duration = Duration::from_millis(150);
/// Верхняя граница одного опроса: он не должен ждать player-а.
const NONBLOCKING_POLL_BUDGET: Duration = Duration::from_millis(30);

fn request(raw: u64) -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(NonZeroU64::new(raw).expect("test request id is non-zero"))
}

fn clip_target() -> LocalOpenInstallTarget {
    LocalOpenInstallTarget::new(
        PathBuf::from("/private-parent-dir/videos/clip.mkv"),
        LocalMediaKind::VideoContaining,
        StablePlaybackIntent::Playing,
    )
}

/// Чем закончится транзакция в fake-слоте.
enum FakeTerminal {
    Installed(&'static str),
    Failed(StrongMediaOpenError),
}

/// Общий слот с player-ом, который отвечает только после `ready_at`.
///
/// Как и настоящий слот, после terminal-а слот пустеет: второй раз его не забрать.
struct SlowPlayerSlot {
    slot_request: Option<MediaOpenRequestId>,
    ready_at: Instant,
    terminal: Option<FakeTerminal>,
    poll_count: usize,
}

impl SlowPlayerSlot {
    fn holding(request_id: MediaOpenRequestId, delay: Duration, terminal: FakeTerminal) -> Self {
        Self {
            slot_request: Some(request_id),
            ready_at: Instant::now() + delay,
            terminal: Some(terminal),
            poll_count: 0,
        }
    }
}

impl StrongOpenSlotPort for SlowPlayerSlot {
    type Installed = &'static str;
    type Failure = StrongMediaOpenError;

    fn pending_request_id(&self) -> Option<MediaOpenRequestId> {
        self.slot_request
    }

    fn poll(&mut self) -> StrongOpenSlotPoll<Self::Installed, Self::Failure> {
        self.poll_count += 1;
        if Instant::now() < self.ready_at {
            return StrongOpenSlotPoll::Pending;
        }
        self.slot_request = None;
        match self.terminal.take() {
            Some(FakeTerminal::Installed(media)) => StrongOpenSlotPoll::Installed(media),
            Some(FakeTerminal::Failed(error)) => StrongOpenSlotPoll::Failed(error),
            None => panic!("terminal fake-слота забран второй раз"),
        }
    }
}

/// Опрашивает, как это делает кадр, пока не будет итога; каждый опрос обязан быть быстрым.
fn poll_until_finished(
    owner: &mut LocalOpenInstallOwner,
    slot: &mut SlowPlayerSlot,
) -> LocalOpenInstallProgress<&'static str, StrongMediaOpenError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let poll_started = Instant::now();
        let progress = owner.poll(slot);
        assert!(
            poll_started.elapsed() < NONBLOCKING_POLL_BUDGET,
            "опрос установки ждал player-а на UI-потоке"
        );
        if !matches!(progress, LocalOpenInstallProgress::Pending) {
            return progress;
        }
        assert!(Instant::now() < deadline, "установка так и не завершилась");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn idle_owner_never_touches_shared_slot() {
    let mut owner = LocalOpenInstallOwner::default();
    let mut startup_slot = SlowPlayerSlot::holding(
        request(7),
        Duration::ZERO,
        FakeTerminal::Installed("startup.mkv"),
    );

    assert!(matches!(
        owner.poll(&mut startup_slot),
        LocalOpenInstallProgress::Idle
    ));
    assert_eq!(startup_slot.poll_count, 0);
    assert_eq!(startup_slot.pending_request_id(), Some(request(7)));
    assert!(!owner.is_installing());
}

#[test]
fn slow_player_does_not_block_and_installed_is_delivered_exactly_once() {
    let mut owner = LocalOpenInstallOwner::default();
    let mut slot = SlowPlayerSlot::holding(
        request(11),
        SLOW_PLAYER_DELAY,
        FakeTerminal::Installed("clip.mkv installed"),
    );
    assert!(owner.track(request(11), clip_target()).is_none());

    let first_poll_started = Instant::now();
    assert!(matches!(
        owner.poll(&mut slot),
        LocalOpenInstallProgress::Pending
    ));
    assert!(first_poll_started.elapsed() < NONBLOCKING_POLL_BUDGET);
    assert!(owner.is_installing());
    assert!(owner.owns_request(request(11)));
    assert_eq!(
        owner.installing_path(),
        Some(Path::new("/private-parent-dir/videos/clip.mkv"))
    );

    let LocalOpenInstallProgress::Installed { installed, target } =
        poll_until_finished(&mut owner, &mut slot)
    else {
        panic!("медленный player обязан дойти до Installed");
    };
    assert!(first_poll_started.elapsed() >= SLOW_PLAYER_DELAY);
    assert_eq!(installed, "clip.mkv installed");
    // Действия после установки получают ровно то, что выбрал пользователь.
    assert_eq!(target, clip_target());
    assert!(!owner.is_installing());
    assert!(!owner.owns_request(request(11)));
    assert_eq!(owner.installing_path(), None);

    let polls_after_install = slot.poll_count;
    assert!(matches!(
        owner.poll(&mut slot),
        LocalOpenInstallProgress::Idle
    ));
    assert_eq!(slot.poll_count, polls_after_install);
}

#[test]
fn unsupported_codec_reaches_user_with_session_03_text() {
    let mut owner = LocalOpenInstallOwner::default();
    let mut slot = SlowPlayerSlot::holding(
        request(12),
        SLOW_PLAYER_DELAY,
        FakeTerminal::Failed(StrongMediaOpenError::Terminal(
            MediaOpenTerminalOutcome::PlayerFailed {
                request_id: request(12),
                reason: PlayerInstallFailureReason::UnsupportedVideoFormat,
            },
        )),
    );
    owner.track(request(12), clip_target());

    let LocalOpenInstallProgress::Failed { error, target } =
        poll_until_finished(&mut owner, &mut slot)
    else {
        panic!("отказ player-а обязан дойти до владельца Open");
    };
    let StrongMediaOpenUserOutcome::Failed(reason) = error.user_outcome() else {
        panic!("отказ по кодеку — ошибка для пользователя");
    };
    assert_eq!(
        local_open_failure_message(&target.path, reason),
        "Не удалось открыть «clip.mkv»: формат видео не поддерживается"
    );
    assert!(!owner.is_installing());
}

#[test]
fn transport_cancel_and_busy_stay_distinct_from_failure() {
    let cancelled = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
        request_id: request(13),
        cause: MediaInstallCancellationCause::Superseded,
    });
    let mut owner = LocalOpenInstallOwner::default();
    let mut slot =
        SlowPlayerSlot::holding(request(13), Duration::ZERO, FakeTerminal::Failed(cancelled));
    owner.track(request(13), clip_target());
    let LocalOpenInstallProgress::Failed { error, .. } = owner.poll(&mut slot) else {
        panic!("отмена тоже terminal");
    };
    assert_eq!(error.user_outcome(), StrongMediaOpenUserOutcome::Cancelled);

    assert_eq!(
        StrongMediaOpenError::Start(MediaOpenStartError::Busy).user_outcome(),
        StrongMediaOpenUserOutcome::Busy
    );
    assert_eq!(
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerFailed {
            request_id: request(13),
            reason: PlayerInstallFailureReason::PreparationTimedOut,
        })
        .user_outcome(),
        StrongMediaOpenUserOutcome::Failed(MediaOpenUserFailureReason::PlayerInstall(
            PlayerInstallFailureReason::PreparationTimedOut
        ))
    );
}

#[test]
fn foreign_request_in_slot_is_never_polled_by_open_owner() {
    let mut owner = LocalOpenInstallOwner::default();
    // В слоте транзакция плейлиста: её terminal принадлежит playlist transport-у.
    let mut playlist_slot = SlowPlayerSlot::holding(
        request(21),
        Duration::ZERO,
        FakeTerminal::Installed("playlist row"),
    );
    owner.track(request(20), clip_target());

    let LocalOpenInstallProgress::OwnershipLost { target } = owner.poll(&mut playlist_slot) else {
        panic!("чужой request в слоте — потеря владения, а не установка Open");
    };
    assert_eq!(target, clip_target());
    assert_eq!(playlist_slot.poll_count, 0);
    assert_eq!(playlist_slot.pending_request_id(), Some(request(21)));
    assert!(!owner.is_installing());
}

#[test]
fn second_track_returns_previous_target_instead_of_dropping_it() {
    let mut owner = LocalOpenInstallOwner::default();
    assert!(owner.track(request(30), clip_target()).is_none());
    let other = LocalOpenInstallTarget::new(
        PathBuf::from("/videos/other.mkv"),
        LocalMediaKind::VideoContaining,
        StablePlaybackIntent::Paused,
    );

    assert_eq!(owner.track(request(31), other), Some(clip_target()));
    assert!(owner.owns_request(request(31)));
    assert!(!owner.owns_request(request(30)));
}

/// Регрессия UX 18: путь Open не должен снова ждать player на UI-потоке.
#[test]
fn open_path_uses_only_stepwise_strong_install_boundary() {
    let owner_source = include_str!("../local_open_install.rs");
    let media_jobs_source = include_str!("../media_jobs.rs");
    // В media_jobs.rs есть и чужие пути (восстановление после пересборки настроек ждёт
    // receipt законно), поэтому проверяется только участок Open: опрос job-а и его итог.
    let open_path_start = media_jobs_source
        .find("pub fn poll_local_file_open_job(")
        .expect("опрос Open живёт в media_jobs.rs");
    let open_path_end = media_jobs_source
        .find("pub(crate) fn open_selected_local_file(")
        .expect("за итогом job-а идёт выбор файла");
    let media_jobs_open_path = &media_jobs_source[open_path_start..open_path_end];
    for (file, source) in [
        ("local_open_install.rs", owner_source),
        ("media_jobs.rs (путь Open)", media_jobs_open_path),
    ] {
        for blocking_call in [
            "install_prepared_media_strong(",
            "wait_for_media_open_progress(",
            "wait_until_signal_available(",
            "wait_for_outcome(",
        ] {
            let production = source.split("#[cfg(test)]").next().unwrap_or(source);
            assert!(
                !production.contains(blocking_call),
                "{file} снова вызывает блокирующий {blocking_call}"
            );
        }
    }
    assert!(owner_source.contains("self.begin_prepared_media_strong("));
    assert!(media_jobs_source.contains("self.begin_local_open_install(*prepared"));
    assert!(media_jobs_source.contains("self.poll_local_open_install(playlist_runtime)"));
}

/// Guard транспорта узнаёт установку из Open раньше общего «чужой request — это старт».
#[test]
fn transport_guard_routes_open_install_before_startup_fallback() {
    let guard_source = include_str!("../playlist_transport/guarded_transport.rs");
    let open_check = guard_source
        .find("self.local_open_install_owns_request(request_id)")
        .expect("guard обязан спрашивать владельца Open");
    let startup_fallback = guard_source
        .find("self.foreign_request_is_pending(request_id) {\n            SupersededRequestOwner::StartupOrchestration")
        .expect("guard сохраняет ветку старта");
    assert!(open_check < startup_fallback);
}
