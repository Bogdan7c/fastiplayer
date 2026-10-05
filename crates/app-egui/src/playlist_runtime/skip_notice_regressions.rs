//! Сквозные проверки сессии 07: битый файл в очереди пропускается, и пользователь видит
//! итог в уведомлении.
//!
//! Тесты ведут настоящий `PlaylistController` и `PlaylistRuntime` через тот же install
//! protocol, что и приложение (admission → ready → authorization → Installed), и проверяют
//! три наблюдаемых результата:
//! - какой элемент в итоге реально стал active media (игра дошла до Installed);
//! - какой итог цепочки забрало приложение через `drain_automatic_queue_notices`;
//! - какой вид показа и текст выбирает для него приложение (`playlist_skip_message`).
//!   Как эти виды рисуются (плашка 5/8 с, ошибка в центре), закрепляют тесты сессии 04.

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackState};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, ManualNavigationDirection, PlaylistItemDraft,
    PlaylistItemId, PlaylistMediaKind, PlaylistQueue, RepeatMode,
};
use playlist_state::{PlaylistStateSnapshot, PlaylistStateStore, QuarantineFileName};

use super::transport_execution::playlist_install_request;
use super::{
    AutomaticQueueNotice, PlaylistRuntime, PlaylistStartupDrainOutcome,
    PlaylistTargetFailureSummary,
};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenRequestId};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerAppendOutcome,
    ControllerManualNavigationOutcome, ControllerPlayItemOutcome, DiscoveryManualWaitAvailability,
    EndedSnapshotKind, InstallReadyOutcome, PlannedPlaylistInstall, PlaylistController,
    PlaylistErrorBehavior, PreviousRestartThreshold, UnstagedPlannedTargetFailureOutcome,
};
use crate::playlist_runtime::identity::TransportActionOrigin;
use crate::playlist_skip_message::{PlaylistQueueNoticeDelivery, playlist_queue_notice_delivery};

/// Имена файлов очереди A → B → C; путь к папке в уведомление попадать не должен.
const QUEUE_FILE_NAMES: [&str; 3] = ["a-ok.mkv", "b-broken.mkv", "c-ok.mkv"];

/// Каталог, который не должен просочиться в текст уведомления (решение владельца 1а).
const PRIVATE_DIRECTORY: &str = "/home/private-owner/Videos";

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

fn local_draft(file_name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(PRIVATE_DIRECTORY).join(file_name)),
        None,
        // Fallback-имя отличается от имени файла: уведомление обязано брать имя файла.
        CachedPlaylistMetadata::new(format!("row title {file_name}"), PlaylistMediaKind::Video),
    )
}

/// Доводит уже принятый controller-ом install до Installed тем же протоколом, что и app.
fn drive_admitted_install_to_installed(
    controller: &mut PlaylistController,
    request_id: MediaOpenRequestId,
    player_request_id: MediaInstallRequestId,
    media_instance_seed: u64,
) {
    assert!(matches!(
        controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id)
        .expect("authorization dispatch");
    controller
        .resolve_authorization_dispatch(
            request_id,
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue barrier");
    controller
        .on_installed(
            request_id,
            player_request_id,
            MediaInstanceId::from_non_zero(non_zero(media_instance_seed)),
            PlaylistBindingGeneration(1),
        )
        .expect("exact Installed");
}

/// Принимает planned install и доводит его до Installed.
fn install_planned(
    controller: &mut PlaylistController,
    install: PlannedPlaylistInstall,
    request_seed: u64,
) {
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(request_seed));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(request_seed + 1_000));
    controller
        .accept_install_request(playlist_install_request(
            request_id,
            player_request_id,
            install,
        ))
        .expect("install admission");
    drive_admitted_install_to_installed(
        controller,
        request_id,
        player_request_id,
        request_seed + 2_000,
    );
}

/// Очередь A → B → C с политикой `behavior`; A открыт вручную и реально играет.
fn queue_with_a_playing(
    behavior: PlaylistErrorBehavior,
) -> (PlaylistController, Vec<PlaylistItemId>) {
    let mut controller = PlaylistController::new();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(
            QUEUE_FILE_NAMES
                .iter()
                .map(|name| local_draft(name))
                .collect(),
        )
        .expect("append A, B, C")
    else {
        panic!("очередь A, B, C не пустая");
    };
    controller.set_error_behavior(behavior);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[0], TransportActionOrigin::Ui)
    else {
        panic!("A стартует install");
    };
    install_planned(&mut controller, install, 10);
    // Ручное открытие A не даёт никакого итога пропусков.
    assert!(controller.drain_automatic_queue_notices().is_empty());
    (controller, item_ids)
}

/// A доиграл до конца: controller планирует автоматический переход на B.
fn clean_eof_plans_next(controller: &mut PlaylistController) -> PlannedPlaylistInstall {
    let active = controller.active_media().expect("A active");
    let AutomaticLifecycleOutcome::OpenItem { install } = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    ) else {
        panic!("clean EOF A планирует автоматический переход");
    };
    install
}

fn runtime_with(controller: PlaylistController) -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.controller.install(controller);
    runtime
}

fn controller_mut(runtime: &mut PlaylistRuntime) -> &mut PlaylistController {
    runtime.controller.as_mut().expect("controller installed")
}

fn active_item_id(runtime: &PlaylistRuntime) -> Option<PlaylistItemId> {
    runtime
        .playlist_controller()
        .and_then(PlaylistController::active_media)
        .and_then(|active| active.item_id())
}

/// Переводит итоги в то, что увидит пользователь: тексты плашек и ошибка в центре.
///
/// Повторяет маршрут `AppState::show_playlist_queue_notices` без окна: информационные и
/// временные плашки — в список toast-ов, `MediaFailure` — ошибка в центре.
fn user_visible_messages(notices: &[AutomaticQueueNotice]) -> (Vec<String>, Option<String>) {
    let mut toasts = Vec::new();
    let mut center_error = None;
    for notice in notices {
        match playlist_queue_notice_delivery(notice) {
            PlaylistQueueNoticeDelivery::InfoToast(text)
            | PlaylistQueueNoticeDelivery::TransientToast(text) => toasts.push(text),
            PlaylistQueueNoticeDelivery::MediaFailure(text) => center_error = Some(text),
        }
    }
    (toasts, center_error)
}

/// A(ok) → B(битый) → C(ok): после A играет C, плашка называет B по имени файла.
#[test]
fn broken_middle_item_is_skipped_and_named_in_info_toast() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    assert_eq!(b_install.item_id, item_ids[1]);
    let mut runtime = runtime_with(controller);

    // B не открылся (например, файл удалён с диска): skip продолжает на C.
    let UnstagedPlannedTargetFailureOutcome::OpenItem { install: c_install } =
        runtime.report_unstaged_planned_playlist_navigation_failure(b_install)
    else {
        panic!("Skip продолжает очередь после битого B");
    };
    assert_eq!(c_install.item_id, item_ids[2]);
    // Пока C не встал, итога нет: цепочка ещё идёт.
    assert!(runtime.drain_automatic_queue_notices().is_empty());

    install_planned(controller_mut(&mut runtime), c_install, 30);

    // C реально стал active media — воспроизведение продолжилось.
    assert_eq!(active_item_id(&runtime), Some(item_ids[2]));
    let notices = runtime.drain_automatic_queue_notices();
    let (toasts, center_error) = user_visible_messages(&notices);
    assert_eq!(toasts, ["Пропущен из-за ошибки 1 файл: b-broken.mkv"]);
    assert_eq!(center_error, None);
    assert!(
        toasts.iter().all(|text| !text.contains(PRIVATE_DIRECTORY)),
        "в уведомлении только имя файла, без пути к папке"
    );
    // Итог отдаётся ровно один раз.
    assert!(runtime.drain_automatic_queue_notices().is_empty());
}

/// После A всё оставшееся битое: остановка без цикла и одна плашка «дальше ничего нет».
#[test]
fn all_remaining_broken_after_playback_stops_once_with_end_of_queue_toast() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    let UnstagedPlannedTargetFailureOutcome::OpenItem { install: c_install } =
        runtime.report_unstaged_planned_playlist_navigation_failure(b_install)
    else {
        panic!("B failure продолжает на C");
    };
    // C тоже битый: кандидатов больше нет, очередь останавливается, а не зацикливается.
    assert!(matches!(
        runtime.report_unstaged_planned_playlist_navigation_failure(c_install),
        UnstagedPlannedTargetFailureOutcome::Stopped { .. }
    ));

    // Active остаётся доигравший A: новых install-ов не было.
    assert_eq!(active_item_id(&runtime), Some(item_ids[0]));
    let notices = runtime.drain_automatic_queue_notices();
    assert_eq!(notices.len(), 1, "ровно один итог: {notices:?}");
    let (toasts, center_error) = user_visible_messages(&notices);
    assert_eq!(
        toasts,
        ["Пропущено из-за ошибки 2 файла: b-broken.mkv, c-ok.mkv. Дальше в очереди ничего нет"]
    );
    // A играл, поэтому это не «ни один файл очереди не открылся».
    assert_eq!(center_error, None);
}

/// Явно выбранная политика `stop`: очередь останавливается на B, плашка называет причину.
#[test]
fn explicit_stop_policy_keeps_old_behavior_and_shows_reason() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Stop);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    // Поведение как раньше: C не планируется.
    assert!(matches!(
        runtime.report_unstaged_planned_playlist_navigation_failure(b_install),
        UnstagedPlannedTargetFailureOutcome::Stopped { .. }
    ));
    assert_eq!(active_item_id(&runtime), Some(item_ids[0]));

    let (toasts, center_error) = user_visible_messages(&runtime.drain_automatic_queue_notices());
    assert_eq!(toasts, ["Очередь остановлена из-за ошибки: b-broken.mkv"]);
    assert_eq!(center_error, None);
}

/// Пользователь сам выбрал строку посреди цепочки: про прерванную цепочку не сообщаем.
#[test]
fn manual_row_play_during_skip_chain_discards_summary() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);
    let UnstagedPlannedTargetFailureOutcome::OpenItem { .. } =
        runtime.report_unstaged_planned_playlist_navigation_failure(b_install)
    else {
        panic!("B failure продолжает на C");
    };

    // Автоматический план на C брошен: пользователь сам кликает по строке C.
    let controller = controller_mut(&mut runtime);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[2], TransportActionOrigin::Ui)
    else {
        panic!("ручной Play C стартует install");
    };
    install_planned(controller, install, 40);

    // C играет, но это выбор пользователя, а не итог автоматической цепочки.
    assert_eq!(active_item_id(&runtime), Some(item_ids[2]));
    assert!(runtime.drain_automatic_queue_notices().is_empty());
}

/// D55: ручной Next на битый элемент не пропускает автоматически и не даёт итога пропусков.
#[test]
fn manual_next_onto_broken_item_never_auto_skips() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let ControllerManualNavigationOutcome::StartInstall { install } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero threshold"),
        DiscoveryManualWaitAvailability::Exhausted,
    ) else {
        panic!("ручной Next с A стартует install B");
    };
    assert_eq!(install.item_id, item_ids[1]);
    let mut runtime = runtime_with(controller);

    // B не открылся: это ручная навигация, автоматика не включается.
    assert!(matches!(
        runtime.report_unstaged_planned_playlist_navigation_failure(install),
        UnstagedPlannedTargetFailureOutcome::Manual
    ));
    assert_eq!(active_item_id(&runtime), Some(item_ids[0]));
    assert!(runtime.drain_automatic_queue_notices().is_empty());
}

/// Сохраняет очередь с current = первый элемент, как будто приложение закрыли на нём.
fn persisted_state_store(file_names: &[&str]) -> (tempfile::TempDir, Arc<PlaylistStateStore>) {
    let mut queue = PlaylistQueue::new();
    let playlist_core::AddItemsOutcome::Added(allocated_item_ids) = queue
        .append_batch(file_names.iter().map(|name| local_draft(name)).collect())
        .expect("append persisted queue")
    else {
        panic!("сохранённая очередь не пустая");
    };
    let item_ids = allocated_item_ids.into_vec();
    queue
        .set_traversal_current(item_ids[0])
        .expect("persisted current");
    let bytes =
        playlist_state::serialize_state(PlaylistStateSnapshot::new(&queue, RepeatMode::StopAtEnd))
            .expect("serialize persisted queue");
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory
        .path()
        .join(playlist_state::PLAYLIST_STATE_FILENAME);
    std::fs::write(&state_path, bytes).expect("write persisted queue");
    (directory, Arc::new(PlaylistStateStore::new(state_path)))
}

/// Запуск приложения: читает сохранённую очередь и включает политику Skip из config-а.
fn restarted_runtime(store: Arc<PlaylistStateStore>) -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    let quarantine_file_name = QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH);
    let mut ready = false;
    for _ in 0..20_000 {
        match runtime
            .drain_playlist_state_startup(quarantine_file_name.clone())
            .expect("startup decision")
        {
            PlaylistStartupDrainOutcome::Ready => {
                ready = true;
                break;
            }
            PlaylistStartupDrainOutcome::NoCompletion
            | PlaylistStartupDrainOutcome::ApplyingQuarantine
            | PlaylistStartupDrainOutcome::StaleCompletionIgnored => thread::yield_now(),
        }
    }
    assert!(
        ready,
        "startup decision завершился за ограниченное число шагов"
    );
    controller_mut(&mut runtime).set_error_behavior(PlaylistErrorBehavior::Skip);
    runtime
}

/// После рестарта текущий файл перемещён: играет следующий, плашка называет пропущенный.
#[test]
fn restore_with_moved_current_plays_next_and_names_skipped_file() {
    let (_directory, store) = persisted_state_store(&["moved-current.mkv", "next-ok.mkv"]);
    let mut runtime = restarted_runtime(store);

    let restored = runtime
        .startup_restored_current()
        .expect("restored current");
    let fallback = runtime
        .report_startup_restore_failure(restored, Arc::from("Файл не найден"))
        .expect("Skip переходит к следующему элементу");
    let fallback_item_id = fallback.item_id();
    assert!(runtime.drain_automatic_queue_notices().is_empty());

    let request_id = MediaOpenRequestId::from_non_zero(non_zero(50));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(1_050));
    runtime
        .accept_startup_restore_install(request_id, player_request_id, fallback)
        .expect("fallback admission");
    drive_admitted_install_to_installed(
        controller_mut(&mut runtime),
        request_id,
        player_request_id,
        2_050,
    );

    assert_eq!(active_item_id(&runtime), Some(fallback_item_id));
    let (toasts, center_error) = user_visible_messages(&runtime.drain_automatic_queue_notices());
    assert_eq!(toasts, ["Пропущен из-за ошибки 1 файл: moved-current.mkv"]);
    assert_eq!(center_error, None);
}

/// После рестарта не открылся ни один файл очереди: одна ошибка в центре, без цикла.
#[test]
fn restore_with_whole_queue_missing_shows_single_center_error() {
    let (_directory, store) = persisted_state_store(&["gone-1.mkv", "gone-2.mkv", "gone-3.mkv"]);
    let mut runtime = restarted_runtime(store);

    let mut target = runtime.startup_restored_current();
    let mut attempts = 0;
    while let Some(failed) = target.take() {
        attempts += 1;
        assert!(
            attempts <= 3,
            "цепочка пропусков ограничена размером очереди"
        );
        target = runtime.report_startup_restore_failure(failed, Arc::from("Файл не найден"));
    }

    assert_eq!(
        attempts, 3,
        "каждый элемент очереди пробовали ровно один раз"
    );
    assert_eq!(active_item_id(&runtime), None);
    let notices = runtime.drain_automatic_queue_notices();
    assert_eq!(notices.len(), 1, "ровно один итог: {notices:?}");
    let (toasts, center_error) = user_visible_messages(&notices);
    assert!(toasts.is_empty());
    assert_eq!(
        center_error.as_deref(),
        Some("Ни один файл очереди не удалось открыть")
    );
}

/// Реальный путь приложения: B удалён, его подготовка падает асинхронно, когда controller
/// ещё не принял план B (ровно это поймал ручной прогон). Skip продолжает на C, бейдж B
/// получает понятную причину, после установки C — плашка про B.
#[test]
fn async_preparation_failure_before_admission_continues_to_c() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let b_install = clean_eof_plans_next(&mut controller);
    let mut runtime = runtime_with(controller);

    // Request ID уже выдан coordinator-ом, но staging (и admission плана) не наступил.
    let c_install = runtime
        .report_playlist_target_failure_before_admission(
            MediaOpenRequestId::from_non_zero(non_zero(60)),
            b_install,
            PlaylistTargetFailureSummary::Specific(Arc::from("файл не найден")),
        )
        .expect("Skip продолжает очередь после асинхронного провала B");
    assert_eq!(c_install.item_id, item_ids[2]);
    let b_badge = controller_mut(&mut runtime)
        .runtime_errors
        .get(&item_ids[1])
        .map(|runtime_error| runtime_error.safe_summary().to_owned());
    assert_eq!(b_badge.as_deref(), Some("файл не найден"));

    install_planned(controller_mut(&mut runtime), c_install, 70);

    assert_eq!(active_item_id(&runtime), Some(item_ids[2]));
    let (toasts, _center_error) = user_visible_messages(&runtime.drain_automatic_queue_notices());
    assert_eq!(toasts, ["Пропущен из-за ошибки 1 файл: b-broken.mkv"]);
}

/// Ручной Next на файл, не подготовившийся до admission, — прежняя D55-семантика:
/// автопропуска нет, очередь ждёт пользователя, итога пропусков нет.
#[test]
fn async_preparation_failure_of_manual_next_waits_for_user() {
    let (mut controller, item_ids) = queue_with_a_playing(PlaylistErrorBehavior::Skip);
    let ControllerManualNavigationOutcome::StartInstall { install } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero threshold"),
        DiscoveryManualWaitAvailability::Exhausted,
    ) else {
        panic!("ручной Next с A стартует install B");
    };
    let mut runtime = runtime_with(controller);

    let continuation = runtime.report_playlist_target_failure_before_admission(
        MediaOpenRequestId::from_non_zero(non_zero(80)),
        install,
        PlaylistTargetFailureSummary::Specific(Arc::from("файл не найден")),
    );

    assert!(
        continuation.is_none(),
        "ручной Next не пропускает автоматически"
    );
    assert!(
        runtime
            .playlist_controller()
            .expect("controller")
            .view_snapshot()
            .awaiting_user_after_navigation_failure()
    );
    assert_eq!(active_item_id(&runtime), Some(item_ids[0]));
    assert!(runtime.drain_automatic_queue_notices().is_empty());
}
