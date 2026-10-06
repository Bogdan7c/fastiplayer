//! Сквозные тесты: фатальная ошибка → показ (через fake) → код выхода.

use std::io::ErrorKind;
use std::process::ExitCode;

use fastiplayer_config::{ConfigError, ConfigPaths};

use super::test_support::RecordingPresenter;
use super::{
    ConfigDirectoryProblem, FatalStartupError, FatalStartupErrorSlot, FatalStartupReason,
    ProcessConclusion, conclude_process,
};
use crate::app_instance::{
    AppInstanceLeaseError, AppInstanceLeaseIoOperation, ProcessArgsError, ProcessBootstrapError,
    UnsafeAppInstanceArtifact,
};

fn test_config_paths() -> ConfigPaths {
    ConfigPaths::from_config_dir("/srv/test-root/fastiplayer")
}

fn lease_failure(lease_error: AppInstanceLeaseError) -> FatalStartupError {
    FatalStartupError::from_bootstrap_error(&ProcessBootstrapError::Lease {
        lease_error,
        config_dir: test_config_paths().config_dir().to_path_buf(),
    })
}

/// Показывает ошибку через fake и возвращает итог процесса и показанный текст.
fn conclude_with_fake(error: FatalStartupError) -> (ProcessConclusion, RecordingPresenter) {
    let presenter = RecordingPresenter::default();
    let conclusion = conclude_process(Err(error), &presenter);
    (conclusion, presenter)
}

fn single_shown_message(presenter: &RecordingPresenter) -> String {
    let shown = presenter.shown_notices();
    assert_eq!(shown.len(), 1, "ожидался ровно один показ: {shown:?}");
    shown[0].notice.message.clone()
}

#[test]
fn normal_run_completes_with_zero_exit_code_and_shows_nothing() {
    let presenter = RecordingPresenter::default();

    let conclusion = conclude_process(Ok(()), &presenter);

    assert_eq!(conclusion, ProcessConclusion::Completed);
    assert_eq!(conclusion.exit_code(), ExitCode::SUCCESS);
    assert_eq!(presenter.presentation_count(), 0);
    assert!(presenter.text_output().is_empty());
}

#[test]
fn missing_gpu_adapter_is_shown_as_vulkan_problem_with_nonzero_exit() {
    // Тот же текст, что `render-wgpu-shell` кладёт в контекст при отсутствии адаптера.
    let renderer_error = anyhow::anyhow!("adapter request failed: no suitable adapter")
        .context("Не удалось получить GPU адаптер. Проверьте драйверы Vulkan");

    let error = FatalStartupError::graphics_unavailable(&renderer_error);
    let (conclusion, presenter) = conclude_with_fake(error.clone());

    assert_eq!(conclusion, ProcessConclusion::FailedToStart);
    assert_eq!(conclusion.exit_code(), ExitCode::from(1));
    assert!(single_shown_message(&presenter).contains("видеокарта с поддержкой Vulkan"));
    // Вся цепочка причин сохраняется для лога, а не только верхний контекст.
    assert!(error.technical_detail().contains("no suitable adapter"));
    assert!(
        presenter
            .text_output()
            .contains("Fastiplayer не запустился.")
    );
}

#[test]
fn window_creation_failure_is_shown_with_nonzero_exit() {
    let error = FatalStartupError::window_creation_failed(&"compositor refused surface");
    let (conclusion, presenter) = conclude_with_fake(error);

    assert_eq!(conclusion, ProcessConclusion::FailedToStart);
    assert!(single_shown_message(&presenter).contains("Не удалось создать окно плеера"));
}

#[test]
fn missing_graphical_session_is_shown_with_nonzero_exit() {
    let error =
        FatalStartupError::graphical_session_unavailable(&"neither WAYLAND_DISPLAY nor DISPLAY");
    let (conclusion, presenter) = conclude_with_fake(error);

    assert_eq!(conclusion, ProcessConclusion::FailedToStart);
    assert!(single_shown_message(&presenter).contains("графическому сеансу"));
}

#[test]
fn config_directory_owned_by_root_shows_folder_and_fix_command() {
    let (conclusion, presenter) =
        conclude_with_fake(lease_failure(AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::ConfigDirectoryOwnerMismatch,
        }));

    let message = single_shown_message(&presenter);
    assert_eq!(conclusion, ProcessConclusion::FailedToStart);
    assert!(message.contains("/srv/test-root/fastiplayer принадлежит другому пользователю"));
    assert!(message.contains("sudo chown -R \"$USER\": /srv/test-root/fastiplayer"));
    assert!(!message.contains("OwnerMismatch"));
}

#[test]
fn bad_command_line_is_shown_with_nonzero_exit() {
    let error = FatalStartupError::from_bootstrap_error(&ProcessBootstrapError::Arguments(
        ProcessArgsError::UnknownOption,
    ));
    let (conclusion, presenter) = conclude_with_fake(error);

    assert_eq!(conclusion, ProcessConclusion::FailedToStart);
    assert!(single_shown_message(&presenter).contains("неизвестным параметром"));
}

#[test]
fn already_running_is_shown_as_plain_fact() {
    let (_, presenter) = conclude_with_fake(lease_failure(AppInstanceLeaseError::AlreadyRunning));

    assert!(single_shown_message(&presenter).starts_with("Fastiplayer уже запущен."));
}

#[test]
fn lease_errors_map_to_distinct_user_reasons() {
    let cases = [
        (
            AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactOwnerMismatch,
            },
            ConfigDirectoryProblem::OwnedByAnotherUser,
        ),
        (
            AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::ConfigDirectoryIsNotDirectory,
            },
            ConfigDirectoryProblem::NotADirectory,
        ),
        (
            AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactIsNotRegularFile,
            },
            ConfigDirectoryProblem::LockFileNotRegularFile {
                lock_file_name: test_config_paths()
                    .app_instance_lock_file()
                    .file_name()
                    .expect("lock file name")
                    .to_string_lossy()
                    .into_owned(),
            },
        ),
        (
            AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactIdentityChanged,
            },
            ConfigDirectoryProblem::LockFileReplacedDuringStartup,
        ),
        (
            AppInstanceLeaseError::Io {
                operation: AppInstanceLeaseIoOperation::CreateConfigDirectory,
                kind: ErrorKind::PermissionDenied,
            },
            ConfigDirectoryProblem::AccessDenied,
        ),
        (
            AppInstanceLeaseError::Io {
                operation: AppInstanceLeaseIoOperation::CreateConfigDirectory,
                kind: ErrorKind::ReadOnlyFilesystem,
            },
            ConfigDirectoryProblem::ReadOnlyStorage,
        ),
        (
            AppInstanceLeaseError::Io {
                operation: AppInstanceLeaseIoOperation::OpenLockArtifact,
                kind: ErrorKind::StorageFull,
            },
            ConfigDirectoryProblem::StorageFull,
        ),
        (
            AppInstanceLeaseError::Io {
                operation: AppInstanceLeaseIoOperation::AcquireLock,
                kind: ErrorKind::Interrupted,
            },
            ConfigDirectoryProblem::SystemError,
        ),
    ];

    for (lease_error, expected_problem) in cases {
        let error = lease_failure(lease_error);

        assert!(
            matches!(
                error.reason(),
                FatalStartupReason::ConfigDirectoryUnusable { problem, location }
                    if *problem == expected_problem
                        && location.readable_text() == "/srv/test-root/fastiplayer"
            ),
            "{lease_error:?} -> {:?}",
            error.reason()
        );
    }
    assert_eq!(
        lease_failure(AppInstanceLeaseError::UnsupportedPlatform).reason(),
        &FatalStartupReason::UnsupportedPlatform
    );
}

#[test]
fn config_load_and_path_discovery_failures_keep_technical_detail_for_log() {
    let load_error = FatalStartupError::from_bootstrap_error(&ProcessBootstrapError::LoadConfig {
        config_error: Box::new(ConfigError::ProjectDirsUnavailable),
        config_dir: test_config_paths().config_dir().to_path_buf(),
    });
    let discover_error = FatalStartupError::from_bootstrap_error(
        &ProcessBootstrapError::DiscoverPaths(ConfigError::ProjectDirsUnavailable),
    );

    assert!(matches!(
        load_error.reason(),
        FatalStartupReason::ConfigFileUnusable { location }
            if location.readable_text() == "/srv/test-root/fastiplayer"
    ));
    assert!(
        load_error
            .technical_detail()
            .contains("не удалось загрузить config")
    );
    assert_eq!(
        discover_error.reason(),
        &FatalStartupReason::ConfigLocationUnknown
    );
}

#[test]
fn slot_keeps_first_error_and_is_empty_after_take() {
    let mut slot = FatalStartupErrorSlot::default();
    assert_eq!(slot.take(), None);

    let first = FatalStartupError::window_creation_failed(&"first");
    slot.record(first.clone());
    slot.record(FatalStartupError::internal_failure("later", &"second"));

    assert_eq!(slot.take(), Some(first));
    assert_eq!(slot.take(), None);
}
