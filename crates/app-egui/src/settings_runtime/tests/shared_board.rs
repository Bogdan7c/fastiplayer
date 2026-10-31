//! «Общая доска» (04c): две копии плеера на одном config-файле.
//!
//! Каждая копия — свой `SettingsRuntime` на одном и том же файле, как два процесса. Первая
//! делает Apply в окне настроек (настоящая запись под замком), вторая получает сигнал
//! «файл мог измениться» и принимает разницу. Проверяется наблюдаемое: committed второй
//! копии, snapshot у её потребителей, байты и inode файла, уведомления, черновик окна
//! настроек и состояние очереди.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use fastiplayer_config::{SharedConfigRead, read_shared_config_at, save_validated_atomic_at};

use super::stepwise_apply::StepwiseTestAdapter;
use super::*;
use crate::settings_runtime::MediaRebuildProgress;
use crate::settings_runtime::shared_board::{SettingsFileNotice, SharedBoardFrame};

/// Config-файл во временном каталоге, уже записанный на диск.
pub(super) fn shared_config_file(config: &AppConfig) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    save_validated_atomic_at(&path, config).expect("initial config is written");
    (directory, path)
}

/// Копия плеера, прочитавшая config с диска при старте.
pub(super) fn copy_on(path: &Path, config: &AppConfig) -> SettingsRuntime {
    SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.to_path_buf(),
    ))
    .expect("settings runtime should be built")
}

/// Окно настроек: открыть, поменять значения, Apply.
pub(super) fn apply_in_settings_window(
    runtime: &mut SettingsRuntime,
    adapter: &mut RecordingRuntimeAdapter,
    values: Vec<(&str, SettingValue)>,
) {
    let mut actions = vec![SettingsUiAction::Open];
    actions.extend(
        values
            .into_iter()
            .map(|(setting_id, value)| SettingsUiAction::SetValue {
                setting_id: SettingId::from(setting_id),
                value,
            }),
    );
    actions.push(SettingsUiAction::Apply);
    run_runtime_actions(runtime, actions, adapter);
    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("apply report")
            .final_state,
        ApplyFinalState::FullyApplied,
        "the settings window Apply must succeed"
    );
}

/// Сигнал слежения и один шаг доски (как кадр после пробуждения).
pub(super) fn deliver_file_signal<A>(
    runtime: &mut SettingsRuntime,
    adapter: &mut A,
) -> SharedBoardFrame
where
    A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
{
    runtime.note_config_file_may_have_changed_for_test();
    runtime.advance_shared_settings_board(adapter, Instant::now())
}

fn disk_config(path: &Path) -> AppConfig {
    match read_shared_config_at(path) {
        SharedConfigRead::Parsed(config) => *config,
        other => panic!("config file must be readable: {other:?}"),
    }
}

/// Байты и inode файла: копия, которая только принимает, его не переписывает.
fn file_identity(path: &Path) -> (Vec<u8>, u64) {
    let bytes = fs::read(path).expect("config bytes");
    let inode = fs::metadata(path).expect("config metadata").ino();
    (bytes, inode)
}

pub(super) fn text_size(value: u16) -> SettingValue {
    SettingValue::Integer(i64::from(value))
}

/// Apply в первой копии доходит до второй без перезапуска: committed и snapshot у
/// потребителей изменились, файл вторая копия не переписала, «Settings saved» не было.
#[test]
fn live_setting_applied_in_one_copy_is_adopted_by_the_other_without_rewriting_the_file() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let new_size = config.subtitles.text_size_percent + 20;

    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![("subtitles.text_size_percent", text_size(new_size))],
    );
    assert_eq!(disk_config(&path).subtitles.text_size_percent, new_size);
    let identity_before = file_identity(&path);

    let board_frame = deliver_file_signal(&mut second, &mut second_adapter);

    assert_eq!(board_frame.notice, None);
    assert!(board_frame.needs_redraw);
    assert_eq!(
        second.committed_config().subtitles.text_size_percent,
        new_size
    );
    let synced = second_adapter
        .committed_snapshots
        .last()
        .expect("consumers receive the adopted snapshot");
    assert_eq!(synced.as_config().subtitles.text_size_percent, new_size);
    assert_eq!(
        file_identity(&path),
        identity_before,
        "the adopting copy must not rewrite the file"
    );
    assert!(
        second.latest_apply_report().is_none(),
        "adoption is not the user's Apply"
    );
    assert_eq!(
        second.ui_model().status.summary,
        None,
        "no «Settings saved»"
    );
    assert!(
        second_adapter.delivered_audio_levels.is_empty(),
        "no post-persist hooks (UX-11 volume)"
    );
}

/// Своя запись даёт пустую разницу: ни транзакции, ни уведомления, ни snapshot-а.
#[test]
fn own_write_produces_nothing_to_adopt() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    apply_in_settings_window(
        &mut runtime,
        &mut adapter,
        vec![(
            "subtitles.text_size_percent",
            text_size(config.subtitles.text_size_percent + 10),
        )],
    );
    let snapshots_after_apply = adapter.committed_snapshots.len();
    let finalizes_after_apply = adapter.finalize_calls;
    let report_after_apply = runtime.latest_apply_report().cloned();

    let board_frame = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(board_frame.notice, None);
    assert!(!board_frame.needs_redraw);
    assert_eq!(adapter.committed_snapshots.len(), snapshots_after_apply);
    assert_eq!(adapter.finalize_calls, finalizes_after_apply);
    assert_eq!(runtime.latest_apply_report().cloned(), report_after_apply);
    assert!(!runtime.has_pending_apply());
}

/// Громкость первой копии — запоминаемое состояние: вторая её на лету не берёт, а своя
/// последующая запись второй копии громкость первой в файле не теряет.
#[test]
fn neighbour_volume_is_not_adopted_live_and_survives_the_next_write() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");

    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![("audio.volume", SettingValue::Float(0.15))],
    );
    let board_frame = deliver_file_signal(&mut second, &mut second_adapter);

    assert_eq!(
        board_frame,
        SharedBoardFrame {
            needs_redraw: false,
            notice: None,
            session_only_lifted: false,
        }
    );
    assert_eq!(second.committed_config().audio.volume, config.audio.volume);
    assert!(second_adapter.committed_snapshots.is_empty());
    assert!(
        second_adapter.player_updates.is_empty(),
        "the film keeps its volume"
    );

    apply_in_settings_window(
        &mut second,
        &mut second_adapter,
        vec![(
            "subtitles.text_size_percent",
            text_size(config.subtitles.text_size_percent + 30),
        )],
    );
    let on_disk = disk_config(&path);
    assert_eq!(
        on_disk.audio.volume, 0.15,
        "the first copy's volume is kept"
    );
    assert_eq!(
        on_disk.subtitles.text_size_percent,
        config.subtitles.text_size_percent + 30
    );
}

/// Открытое окно настроек во второй копии: правка того же ключа осталась в черновике,
/// нетронутое поле подтянулось из файла.
#[test]
fn open_settings_window_keeps_the_user_edit_and_pulls_untouched_fields() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let user_edit = config.subtitles.text_size_percent + 50;
    let neighbour_size = config.subtitles.text_size_percent + 20;
    let neighbour_opacity = config.subtitles.background_opacity_percent / 2;

    run_runtime_actions(
        &mut second,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("subtitles.text_size_percent"),
                value: text_size(user_edit),
            },
        ],
        &mut second_adapter,
    );
    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![
            ("subtitles.text_size_percent", text_size(neighbour_size)),
            (
                "subtitles.background_opacity_percent",
                SettingValue::Integer(i64::from(neighbour_opacity)),
            ),
        ],
    );

    let board_frame = deliver_file_signal(&mut second, &mut second_adapter);

    assert_eq!(board_frame.notice, None);
    let draft_value = |runtime: &mut SettingsRuntime, setting_id: &str| {
        runtime
            .ui_model()
            .fields
            .iter()
            .find(|field| field.descriptor.id == SettingId::from(setting_id))
            .map(|field| field.draft_value.clone())
            .expect("field is in the settings window")
    };
    assert_eq!(
        draft_value(&mut second, "subtitles.text_size_percent"),
        text_size(user_edit),
        "the user's unsaved edit stays"
    );
    assert_eq!(
        draft_value(&mut second, "subtitles.background_opacity_percent"),
        SettingValue::Integer(i64::from(neighbour_opacity)),
        "the untouched field follows the file"
    );
    assert_eq!(
        second.committed_config().subtitles.text_size_percent,
        neighbour_size
    );
    assert!(second.is_settings_window_open());
}

/// Битый файл: одно уведомление; то же содержимое ещё раз — тихо; файл починили —
/// изменения приняты.
#[test]
fn broken_file_is_announced_once_and_a_repaired_file_is_adopted() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    fs::write(&path, b"schema_version = [broken").expect("break the file");
    let broken_bytes = fs::read(&path).expect("bytes");

    let first_signal = deliver_file_signal(&mut runtime, &mut adapter);
    let repeated_signal = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(
        first_signal.notice,
        Some(SettingsFileNotice::UnreadableFile)
    );
    assert_eq!(
        repeated_signal.notice, None,
        "same content is not announced twice"
    );
    assert_eq!(
        fs::read(&path).expect("bytes"),
        broken_bytes,
        "nothing written"
    );
    assert!(adapter.committed_snapshots.is_empty(), "nothing applied");

    let mut repaired = config.clone();
    repaired.subtitles.text_size_percent = config.subtitles.text_size_percent + 15;
    save_validated_atomic_at(&path, &repaired).expect("repair the file");
    let repaired_signal = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(repaired_signal.notice, None);
    assert_eq!(
        runtime.committed_config().subtitles.text_size_percent,
        repaired.subtitles.text_size_percent
    );
}

/// Файл от новой версии — своё уведомление; файла нет — тихо.
#[test]
fn newer_file_has_its_own_notice_and_missing_file_is_quiet() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");

    fs::write(&path, b"schema_version = 9999\n").expect("newer file");
    let newer = deliver_file_signal(&mut runtime, &mut adapter);
    fs::remove_file(&path).expect("remove file");
    let missing = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(newer.notice, Some(SettingsFileNotice::NewerFileVersion));
    assert_eq!(
        missing,
        SharedBoardFrame {
            needs_redraw: false,
            notice: None,
            session_only_lifted: false,
        }
    );
    assert!(!path.exists(), "the adopting copy does not create the file");
}

/// Apply пользователя во время принятия (пересборка источника в полёте) не теряется:
/// он ждёт в «запланирован» и стартует после конца принятия; отложенная запись ширины
/// сайдбара тоже ждёт.
#[test]
fn user_apply_during_adoption_waits_and_starts_afterwards() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = StepwiseTestAdapter::new(
        &config,
        vec![
            MediaRebuildProgress::Pending,
            MediaRebuildProgress::Finished(AppRouteApplyResult::Applied),
        ],
    );
    let neighbour_read_ahead = config.network.read_ahead_mb + 1;
    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![(
            "network.read_ahead_mb",
            SettingValue::Integer(neighbour_read_ahead as i64),
        )],
    );

    let started = deliver_file_signal(&mut second, &mut second_adapter);
    assert!(started.needs_redraw);
    assert!(
        second.has_pending_apply(),
        "adoption waits for the media rebuild"
    );
    assert_eq!(second_adapter.begin_calls, 1);

    // Пользователь второй копии жмёт Apply, пока принятие ждёт пересборку.
    second
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("ui.language"),
                    value: SettingValue::Text("en".to_string()),
                },
                SettingsUiAction::Apply,
            ],
            &mut second_adapter,
        )
        .expect("apply frame");
    second
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut second_adapter)
        .expect("transaction frame");
    assert!(
        second.has_pending_apply(),
        "the user's Apply is still scheduled"
    );
    assert_eq!(second.committed_config().ui.language, config.ui.language);
    let waiting_details = second.ui_model().status.details.join(" ");
    assert!(
        waiting_details.contains("another Fastiplayer window"),
        "status explains the wait: {waiting_details}"
    );
    // Отложенная запись ширины сайдбара ждёт конца принятия, как во время Apply.
    let _recorded = second.record_sidebar_width_change(
        SidebarWidthChange {
            width_points: SidebarWidthPoints::from_committed(config.ui.sidebar.width_points + 20),
        },
        Instant::now(),
    );
    assert_eq!(second.next_sidebar_resize_deadline(), None);

    // Опрос 1: пересборка ещё идёт; опрос 2: принятие завершилось.
    let still_pending = second.advance_shared_settings_board(&mut second_adapter, Instant::now());
    assert!(!still_pending.needs_redraw);
    let adopted = second.advance_shared_settings_board(&mut second_adapter, Instant::now());
    assert!(adopted.needs_redraw);
    assert_eq!(
        second.committed_config().network.read_ahead_mb,
        neighbour_read_ahead
    );

    // Следующий кадр начинает и завершает Apply пользователя.
    second
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut second_adapter)
        .expect("apply after adoption");
    assert_eq!(second.committed_config().ui.language, "en");
    assert_eq!(
        second
            .latest_apply_report()
            .expect("the user's report")
            .final_state,
        ApplyFinalState::FullyApplied
    );
    assert!(
        second.next_sidebar_resize_deadline().is_some(),
        "the resize flush resumes"
    );
    let on_disk = disk_config(&path);
    assert_eq!(on_disk.ui.language, "en");
    assert_eq!(on_disk.network.read_ahead_mb, neighbour_read_ahead);
}

/// Отмена на lifecycle-границе прерывает принятие без следов; после возврата — повтор.
#[test]
fn lifecycle_cancel_interrupts_adoption_and_it_is_retried_later() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = StepwiseTestAdapter::new(&config, Vec::new());
    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![(
            "network.read_ahead_mb",
            SettingValue::Integer(config.network.read_ahead_mb as i64 + 2),
        )],
    );
    let _started = deliver_file_signal(&mut second, &mut second_adapter);

    second.cancel_external_adoption(&mut second_adapter);

    assert_eq!(
        second_adapter.cancel_calls, 1,
        "the media rebuild is interrupted"
    );
    assert_eq!(
        second.committed_config().network.read_ahead_mb,
        config.network.read_ahead_mb
    );
    let _retried = second.advance_shared_settings_board(&mut second_adapter, Instant::now());
    assert_eq!(second_adapter.begin_calls, 2, "the next frame tries again");
}

/// Маршрут отказал при принятии: одно уведомление, повтор того же содержимого не
/// запускает транзакцию снова; новая запись соседа — новая попытка.
#[test]
fn failed_adoption_is_not_retried_for_the_same_file_content() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    adapter.fail_media = true;
    let mut neighbour = config.clone();
    neighbour.network.read_ahead_mb = config.network.read_ahead_mb + 3;
    save_validated_atomic_at(&path, &neighbour).expect("neighbour write");

    let failed = deliver_file_signal(&mut runtime, &mut adapter);
    let repeated = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(failed.notice, Some(SettingsFileNotice::AdoptionFailed));
    assert_eq!(adapter.media_updates, 1);
    assert_eq!(repeated.notice, None);
    assert_eq!(
        adapter.media_updates, 1,
        "same content is not applied again"
    );
    assert_eq!(
        runtime.committed_config().network.read_ahead_mb,
        config.network.read_ahead_mb
    );

    adapter.fail_media = false;
    neighbour.network.read_ahead_mb += 1;
    save_validated_atomic_at(&path, &neighbour).expect("next neighbour write");
    let retried = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(retried.notice, None);
    assert_eq!(
        runtime.committed_config().network.read_ahead_mb,
        neighbour.network.read_ahead_mb
    );
}

/// Своё изменение не записалось (файл был битый) и живёт в памяти; файл починили —
/// копия сначала дописывает своё, потом принимает соседское, и своё не откатывается.
#[test]
fn unsaved_own_change_is_written_before_adopting_a_repaired_file() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let own_size = config.subtitles.text_size_percent + 40;
    fs::write(&path, b"schema_version = [broken").expect("break the file");
    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("subtitles.text_size_percent"),
                value: text_size(own_size),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );
    assert_eq!(
        runtime.committed_config().subtitles.text_size_percent,
        own_size
    );

    let mut repaired = config.clone();
    repaired.network.read_ahead_mb = config.network.read_ahead_mb + 5;
    save_validated_atomic_at(&path, &repaired).expect("neighbour repairs the file");
    let board_frame = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(board_frame.notice, None);
    assert_eq!(
        runtime.committed_config().subtitles.text_size_percent,
        own_size,
        "the own unsaved change is not rolled back by the older file value"
    );
    assert_eq!(
        runtime.committed_config().network.read_ahead_mb,
        repaired.network.read_ahead_mb
    );
    let on_disk = disk_config(&path);
    assert_eq!(
        on_disk.subtitles.text_size_percent, own_size,
        "written at last"
    );
    assert_eq!(
        on_disk.network.read_ahead_mb,
        repaired.network.read_ahead_mb
    );
}

/// Сигнал во время своего Apply (пересборка в полёте): чужое ждёт его конца и только
/// потом принимается.
#[test]
fn neighbour_change_waits_for_the_own_apply_in_flight() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = StepwiseTestAdapter::new(
        &config,
        vec![MediaRebuildProgress::Finished(AppRouteApplyResult::Applied)],
    );
    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("network.read_ahead_mb"),
                    value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 7),
                },
                SettingsUiAction::Apply,
            ],
            &mut adapter,
        )
        .expect("apply frame");
    runtime
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("transaction frame");
    assert!(
        runtime.has_pending_apply(),
        "own Apply waits for the rebuild"
    );
    let mut neighbour = config.clone();
    neighbour.ui.language = "en".to_string();
    save_validated_atomic_at(&path, &neighbour).expect("neighbour write");

    let while_applying = deliver_file_signal(&mut runtime, &mut adapter);
    assert!(!while_applying.needs_redraw);
    assert_eq!(runtime.committed_config().ui.language, config.ui.language);

    runtime
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("own Apply finishes");
    let after_apply = runtime.advance_shared_settings_board(&mut adapter, Instant::now());

    assert!(after_apply.needs_redraw);
    assert_eq!(runtime.committed_config().ui.language, "en");
    assert_eq!(
        runtime.committed_config().network.read_ahead_mb,
        config.network.read_ahead_mb + 7
    );
}

/// Запись не удалась (файл битый): одно уведомление с причиной, а не плашка «только на
/// сеанс»; вторая неудачная запись по той же причине — без повторного уведомления.
#[test]
fn not_saved_yet_is_announced_once_per_reason_without_the_session_only_banner() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    fs::write(&path, b"schema_version = [broken").expect("break the file");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("subtitles.text_size_percent"),
                value: text_size(config.subtitles.text_size_percent + 10),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );
    let first_notice = runtime.take_settings_save_notice();
    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("en".to_string()),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let first_notice = first_notice.expect("the failed write is announced");
    assert!(
        first_notice.contains("settings file"),
        "the store's English reason is shown: {first_notice}"
    );
    assert_eq!(
        runtime.take_settings_save_notice(),
        None,
        "same reason, no spam"
    );
    assert_eq!(
        adapter.session_only_reports, 0,
        "not the session-only banner"
    );
    assert_eq!(
        runtime.committed_config().ui.language,
        "en",
        "applied in memory"
    );
}
