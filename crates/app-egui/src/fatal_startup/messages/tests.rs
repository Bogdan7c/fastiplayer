use std::path::Path;

use super::{FATAL_RUNTIME_TITLE, FATAL_STARTUP_TITLE, notice_for};
use crate::app_instance::ProcessArgsError;
use crate::fatal_startup::{
    ConfigDirectoryLocation, ConfigDirectoryProblem, FatalErrorPhase, FatalStartupReason,
};

fn location() -> ConfigDirectoryLocation {
    ConfigDirectoryLocation::relative_to_home(
        Path::new("/home/user/.config/fastiplayer"),
        Some(Path::new("/home/user")),
    )
}

/// Все причины, которые может увидеть пользователь.
fn every_reason() -> Vec<FatalStartupReason> {
    let directory_problems = [
        ConfigDirectoryProblem::OwnedByAnotherUser,
        ConfigDirectoryProblem::NotADirectory,
        ConfigDirectoryProblem::LockFileNotRegularFile {
            lock_file_name: "instance.lock".to_owned(),
        },
        ConfigDirectoryProblem::LockFileReplacedDuringStartup,
        ConfigDirectoryProblem::AccessDenied,
        ConfigDirectoryProblem::ReadOnlyStorage,
        ConfigDirectoryProblem::StorageFull,
        ConfigDirectoryProblem::SystemError,
    ];
    let mut reasons = vec![
        FatalStartupReason::InvalidArguments(ProcessArgsError::UnknownOption),
        FatalStartupReason::ConfigLocationUnknown,
        FatalStartupReason::AlreadyRunning,
        FatalStartupReason::RunningInstanceNotResponding,
        FatalStartupReason::RunningInstanceShuttingDown,
        FatalStartupReason::RunningInstanceDidNotTakeFiles,
        FatalStartupReason::ConfigFileUnusable {
            location: location(),
        },
        FatalStartupReason::UnsupportedPlatform,
        FatalStartupReason::GraphicalSessionUnavailable,
        FatalStartupReason::WindowCreationFailed,
        FatalStartupReason::GraphicsUnavailable,
        FatalStartupReason::InternalFailure,
    ];
    reasons.extend(directory_problems.into_iter().map(|problem| {
        FatalStartupReason::ConfigDirectoryUnusable {
            problem,
            location: location(),
        }
    }));
    reasons
}

/// Слово в стиле Rust-идентификатора (`OwnerMismatch`, `PermissionDenied`):
/// заглавная буква после строчной внутри одного латинского слова.
fn looks_like_rust_identifier(word: &str) -> bool {
    // Название графического API — бренд, а не идентификатор Rust.
    if word == "OpenGL" {
        return false;
    }
    let letters: Vec<char> = word.chars().collect();
    letters
        .windows(2)
        .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
}

#[test]
fn every_message_is_human_text_without_debug_names() {
    for reason in every_reason() {
        let notice = notice_for(&reason, FatalErrorPhase::Startup);

        assert_eq!(notice.title, FATAL_STARTUP_TITLE);
        assert!(!notice.message.trim().is_empty(), "{reason:?}");
        for forbidden in ["{", "}", "::", "Error", "Some(", "None"] {
            assert!(
                !notice.message.contains(forbidden),
                "{reason:?}: \"{forbidden}\" in \"{}\"",
                notice.message
            );
        }
        let rust_like_words: Vec<&str> = notice
            .message
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|word| looks_like_rust_identifier(word))
            .collect();
        assert!(
            rust_like_words.is_empty(),
            "{reason:?}: {rust_like_words:?}"
        );
    }
}

#[test]
fn every_reason_has_its_own_message() {
    let messages: Vec<String> = every_reason()
        .iter()
        .map(|reason| notice_for(reason, FatalErrorPhase::Startup).message)
        .collect();

    for (index, message) in messages.iter().enumerate() {
        assert!(
            !messages[index + 1..].contains(message),
            "The same text for different reasons: {message}"
        );
    }
}

#[test]
fn owner_problem_names_folder_and_gives_copyable_fix_command() {
    let notice = notice_for(
        &FatalStartupReason::ConfigDirectoryUnusable {
            problem: ConfigDirectoryProblem::OwnedByAnotherUser,
            location: location(),
        },
        FatalErrorPhase::Startup,
    );

    assert!(
        notice
            .message
            .contains("The settings folder ~/.config/fastiplayer")
    );
    assert!(notice.message.contains("sudo"));
    assert!(
        notice
            .message
            .ends_with("\nsudo chown -R \"$USER\": ~/.config/fastiplayer"),
        "{}",
        notice.message
    );
}

#[test]
fn lock_file_problem_names_the_file_to_delete() {
    let notice = notice_for(
        &FatalStartupReason::ConfigDirectoryUnusable {
            problem: ConfigDirectoryProblem::LockFileNotRegularFile {
                lock_file_name: "instance.lock".to_owned(),
            },
            location: location(),
        },
        FatalErrorPhase::Startup,
    );

    assert!(notice.message.contains("instance.lock"));
    assert!(notice.message.contains("Delete it"));
}

#[test]
fn runtime_recreation_graphics_failure_speaks_about_resume_without_startup_or_vulkan_wording() {
    let notice = notice_for(
        &FatalStartupReason::GraphicsUnavailable,
        FatalErrorPhase::RuntimeRecreation,
    );

    assert_eq!(notice.title, FATAL_RUNTIME_TITLE);
    assert!(notice.message.contains("after sleep"), "{}", notice.message);
    assert!(notice.message.contains("Start it again"));
    for forbidden in ["Vulkan", "start video output", "could not start"] {
        assert!(!notice.message.contains(forbidden), "{}", notice.message);
        assert!(!notice.title.contains(forbidden), "{}", notice.title);
    }
}

#[test]
fn runtime_recreation_internal_failure_differs_from_startup_text() {
    let startup = notice_for(
        &FatalStartupReason::InternalFailure,
        FatalErrorPhase::Startup,
    );
    let runtime = notice_for(
        &FatalStartupReason::InternalFailure,
        FatalErrorPhase::RuntimeRecreation,
    );

    assert_ne!(startup.message, runtime.message);
    assert!(startup.message.contains("during startup"));
    assert!(!runtime.message.contains("startup"), "{}", runtime.message);
    assert!(runtime.message.contains("after sleep"));
}

#[test]
fn startup_graphics_failure_keeps_the_original_vulkan_text() {
    let notice = notice_for(
        &FatalStartupReason::GraphicsUnavailable,
        FatalErrorPhase::Startup,
    );

    assert_eq!(notice.title, FATAL_STARTUP_TITLE);
    assert!(
        notice
            .message
            .contains("graphics card with Vulkan or OpenGL ES 2.0 support")
    );
}
