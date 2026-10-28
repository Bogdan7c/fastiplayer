//! Наложение патчей исходников (политика — `src/source_patches.rs`).
//!
//! Для каждого патча: пробный прогон GNU `patch`, и только после его успеха —
//! настоящий. Если пробный прогон не прошёл, сборка падает с понятной ошибкой
//! «патч X не применился к FFmpeg Y», а дерево исходников остаётся нетронутым.

use std::path::Path;
use std::process::Command;

use crate::build_error::VendoredBuildError;
use crate::commands;
use crate::source_patches::{self, PatchPass, SourcePatch};
use crate::sources::PinnedSource;

/// Накладывает все патчи `source` на свежераспакованные исходники.
pub fn apply_source_patches(
    source: &PinnedSource,
    source_directory: &Path,
    crate_root: &Path,
    logs_directory: &Path,
) -> Result<(), VendoredBuildError> {
    for patch in source_patches::patches_for(source) {
        apply_one_patch(patch, source_directory, crate_root, logs_directory)?;
    }
    Ok(())
}

fn apply_one_patch(
    patch: &SourcePatch,
    source_directory: &Path,
    crate_root: &Path,
    logs_directory: &Path,
) -> Result<(), VendoredBuildError> {
    let patch_file = crate_root.join(patch.relative_path());
    if !patch_file.is_file() {
        return Err(VendoredBuildError::SourcePatchMissing {
            patch_name: patch.name,
            patch_path: patch_file,
        });
    }

    // Пробный прогон: hunk-и проверяются без записи. Его провал — это ровно
    // «патч устарел для этой версии», и сообщение говорит именно об этом.
    run_patch_pass(
        patch,
        source_directory,
        &patch_file,
        PatchPass::DryRun,
        logs_directory,
    )
    .map_err(|step_error| match step_error {
        VendoredBuildError::StepFailed {
            log_path, log_tail, ..
        } => VendoredBuildError::SourcePatchDoesNotApply {
            patch_name: patch.name,
            patch_path: patch_file.clone(),
            library_name: patch.target.library_name,
            library_version: patch.target.version,
            log_path,
            log_tail,
        },
        other_error => other_error,
    })?;

    run_patch_pass(
        patch,
        source_directory,
        &patch_file,
        PatchPass::Apply,
        logs_directory,
    )
}

fn run_patch_pass(
    patch: &SourcePatch,
    source_directory: &Path,
    patch_file: &Path,
    pass: PatchPass,
    logs_directory: &Path,
) -> Result<(), VendoredBuildError> {
    let mut patch_command = Command::new("patch");
    patch_command.args(source_patches::gnu_patch_arguments(
        source_directory,
        patch_file,
        pass,
    ));
    let step_name = match pass {
        PatchPass::DryRun => format!("patch-check-{}", patch.name),
        PatchPass::Apply => format!("patch-apply-{}", patch.name),
    };
    commands::run_logged(patch_command, &step_name, logs_directory)
}
