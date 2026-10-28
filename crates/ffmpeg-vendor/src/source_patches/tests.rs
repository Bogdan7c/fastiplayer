//! Тесты политики патчей: настоящий GNU `patch` на временном дереве и на
//! нетронутом `libdav1d.c` из проверенного архива FFmpeg.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{
    ALL_SOURCE_PATCHES, FFMPEG_LIBDAV1D_DIRECT_RENDERING, PatchPass, gnu_patch_arguments,
    patch_fingerprint_lines, patches_for,
};
use crate::checksum::ChecksumError;
use crate::sources::{DAV1D_SOURCE, FFMPEG_SOURCE};

/// Корень crate-а: от него build script ищет `patches/`.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Создаёт пустой временный каталог для одного теста.
fn temporary_directory(test_name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "ffmpeg-vendor-patches-{test_name}-{}",
        std::process::id()
    ));
    // Остатки прошлого прогона мешают воспроизводимости, поэтому удаляем их.
    if directory.exists() {
        fs::remove_dir_all(&directory).expect("remove stale test directory");
    }
    fs::create_dir_all(&directory).expect("create test directory");
    directory
}

/// Запускает GNU `patch` теми же аргументами, что и build script.
/// Возвращает true при успехе.
fn run_gnu_patch(source_directory: &Path, patch_file: &Path, pass: PatchPass) -> bool {
    Command::new("patch")
        .args(gnu_patch_arguments(source_directory, patch_file, pass))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("GNU patch is a required build tool of ffmpeg-vendor and must be in PATH")
        .success()
}

/// Маленький исходник и патч к нему — эталон строгого поведения без FFmpeg.
const SAMPLE_SOURCE: &str = "int first(void);\nint second(void);\nint third(void);\n";
const SAMPLE_PATCHED: &str = "int first(void);\nint second_renamed(void);\nint third(void);\n";
const SAMPLE_PATCH: &str = "\
Заголовок патча — свободный текст, его GNU patch пропускает.

--- a/lib/sample.h
+++ b/lib/sample.h
@@ -1,3 +1,3 @@
 int first(void);
-int second(void);
+int second_renamed(void);
 int third(void);
";

/// Дерево `<root>/src/lib/sample.h` + файл патча; возвращает (каталог исходников, патч).
fn sample_tree(test_name: &str, source_text: &str) -> (PathBuf, PathBuf) {
    let root = temporary_directory(test_name);
    let source_directory = root.join("src");
    fs::create_dir_all(source_directory.join("lib")).expect("create sample tree");
    fs::write(source_directory.join("lib/sample.h"), source_text).expect("write sample");
    let patch_file = root.join("0001-sample.patch");
    fs::write(&patch_file, SAMPLE_PATCH).expect("write sample patch");
    (source_directory, patch_file)
}

fn read_sample(source_directory: &Path) -> String {
    fs::read_to_string(source_directory.join("lib/sample.h")).expect("read sample")
}

#[test]
fn dry_run_checks_without_touching_files_and_apply_changes_them() {
    let (source_directory, patch_file) = sample_tree("dry-run-then-apply", SAMPLE_SOURCE);

    assert!(run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::DryRun
    ));
    assert_eq!(
        read_sample(&source_directory),
        SAMPLE_SOURCE,
        "dry run must not modify sources"
    );

    assert!(run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::Apply
    ));
    assert_eq!(read_sample(&source_directory), SAMPLE_PATCHED);
    assert!(
        !source_directory.join("lib/sample.h.orig").exists(),
        "no backup files next to sources"
    );
}

#[test]
fn changed_context_is_rejected_instead_of_fuzzed() {
    // Новая «версия» исходника: строка контекста рядом с правкой изменилась.
    // С fuzz GNU patch наложил бы hunk; строгий режим обязан отказать.
    let drifted_source = "int first(void);\nint second(void);\nint third_changed(void);\n";
    let (source_directory, patch_file) = sample_tree("context-drift", drifted_source);

    assert!(!run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::DryRun
    ));
    assert_eq!(
        read_sample(&source_directory),
        drifted_source,
        "rejected dry run must leave sources untouched"
    );
}

#[test]
fn already_applied_patch_is_an_error_not_a_reverse() {
    let (source_directory, patch_file) = sample_tree("already-applied", SAMPLE_PATCHED);

    assert!(!run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::DryRun
    ));
    assert_eq!(
        read_sample(&source_directory),
        SAMPLE_PATCHED,
        "a reversed-looking patch must not be undone"
    );
}

#[test]
fn every_patch_file_exists_under_its_pinned_source_version() {
    let mut names = HashSet::new();
    for patch in ALL_SOURCE_PATCHES {
        assert!(
            names.insert(patch.name),
            "duplicate patch name {}",
            patch.name
        );
        assert_eq!(patch.file_name, format!("{}.patch", patch.name));

        let relative_path = patch.relative_path();
        assert!(
            relative_path
                .starts_with(Path::new("patches").join(patch.target.unpacked_directory_name)),
            "{} must live in the directory of the pinned version",
            patch.name
        );
        let patch_text = fs::read_to_string(crate_root().join(&relative_path))
            .unwrap_or_else(|error| panic!("read {}: {error}", relative_path.display()));
        assert!(
            patch_text.contains("\n--- a/") && patch_text.contains("\n+++ b/"),
            "{} must be a -p1 unified diff",
            patch.name
        );
    }
}

#[test]
fn ffmpeg_gets_the_dav1d_patch_and_dav1d_gets_none() {
    let ffmpeg_patches: Vec<_> = patches_for(&FFMPEG_SOURCE).collect();
    assert_eq!(ffmpeg_patches, [&FFMPEG_LIBDAV1D_DIRECT_RENDERING]);
    assert_eq!(patches_for(&DAV1D_SOURCE).count(), 0);
}

#[test]
fn fingerprint_follows_patch_content() {
    let fake_crate_root = temporary_directory("fingerprint");
    let patch_path = fake_crate_root.join(FFMPEG_LIBDAV1D_DIRECT_RENDERING.relative_path());
    fs::create_dir_all(patch_path.parent().expect("patch directory")).expect("create patches");

    fs::write(&patch_path, "first revision").expect("write patch");
    let first_lines = patch_fingerprint_lines(&fake_crate_root).expect("fingerprint");
    fs::write(&patch_path, "second revision").expect("rewrite patch");
    let second_lines = patch_fingerprint_lines(&fake_crate_root).expect("fingerprint");

    assert_eq!(first_lines.len(), ALL_SOURCE_PATCHES.len());
    assert_ne!(
        first_lines, second_lines,
        "editing a patch must invalidate the build stamp"
    );
    assert!(first_lines[0].contains(FFMPEG_SOURCE.version));
}

#[test]
fn missing_patch_file_breaks_the_fingerprint() {
    let empty_crate_root = temporary_directory("missing-patch");

    let fingerprint_error =
        patch_fingerprint_lines(&empty_crate_root).expect_err("missing patch must be an error");

    assert!(matches!(
        fingerprint_error,
        ChecksumError::Unreadable { .. }
    ));
}

/// Проверенный архив FFmpeg, который build script этого же crate-а скачал
/// (или взял по `FASTIPLAYER_FFMPEG_ARCHIVE`) и сверил по SHA-256.
fn verified_ffmpeg_archive() -> PathBuf {
    Path::new(env!("OUT_DIR"))
        .join("fastiplayer-vendored/downloads")
        .join(FFMPEG_SOURCE.archive_file_name)
}

/// Распаковывает из архива только файлы, которые трогают патчи FFmpeg.
fn unpack_pristine_ffmpeg_file(destination: &Path, relative_file: &str) -> PathBuf {
    let member = format!("{}/{relative_file}", FFMPEG_SOURCE.unpacked_directory_name);
    let unpacked = Command::new("tar")
        .arg("-xf")
        .arg(verified_ffmpeg_archive())
        .arg("-C")
        .arg(destination)
        .arg(&member)
        .status()
        .expect("tar is a required build tool")
        .success();
    assert!(unpacked, "unpack {member} from the verified FFmpeg archive");
    destination.join(FFMPEG_SOURCE.unpacked_directory_name)
}

#[test]
fn dav1d_patch_applies_exactly_to_pristine_ffmpeg() {
    let root = temporary_directory("pristine-ffmpeg");
    let source_directory = unpack_pristine_ffmpeg_file(&root, "libavcodec/libdav1d.c");
    let patch_file = crate_root().join(FFMPEG_LIBDAV1D_DIRECT_RENDERING.relative_path());
    let wrapper = source_directory.join("libavcodec/libdav1d.c");
    let pristine_text = fs::read_to_string(&wrapper).expect("read pristine wrapper");
    assert!(
        !pristine_text.contains("AV_CODEC_CAP_DR1"),
        "upstream 8.1.3 wrapper has no DR1 — otherwise the patch is obsolete"
    );

    assert!(run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::DryRun
    ));
    assert!(run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::Apply
    ));
    let patched_text = fs::read_to_string(&wrapper).expect("read patched wrapper");
    assert!(patched_text.contains("AV_CODEC_CAP_DR1"));
    assert!(patched_text.contains("c->get_buffer2(c, f, AV_GET_BUFFER_FLAG_REF)"));

    // Повторное наложение — ошибка, а не тихий откат правки.
    assert!(!run_gnu_patch(
        &source_directory,
        &patch_file,
        PatchPass::DryRun
    ));
    // То, что собранная библиотека действительно с патчем, проверяет
    // `video-ffmpeg` (libdav1d объявляет DR1 и пишет кадры в блоки плеера).
}
