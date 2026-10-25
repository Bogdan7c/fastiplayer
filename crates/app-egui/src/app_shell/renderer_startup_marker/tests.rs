//! Метка незавершённого старта на настоящих файлах во временном каталоге.

use std::fs;
use std::path::PathBuf;

use render_shell_api::RendererFamily;
use tempfile::TempDir;

use super::RendererStartupWatch;
use crate::render_backend::{RendererStartupMarker, UnfinishedStartupTracker};
use crate::telemetry::{RenderFrameTotals, Telemetry};

const WGPU: RendererFamily = RendererFamily::WgpuVulkan;
const GLES2: RendererFamily = RendererFamily::Gles2;

fn marker_in(directory: &TempDir) -> PathBuf {
    directory.path().join("renderer-startup.marker")
}

/// Нет файла — прошлый запуск завершился штатно.
#[test]
fn missing_marker_means_no_unfinished_startup() {
    let directory = TempDir::new().expect("temp dir");
    let watch = RendererStartupWatch::new(marker_in(&directory));

    assert_eq!(
        watch.unfinished_startup(),
        RendererStartupMarker::NoUnfinishedStartup
    );
}

/// Известный идентификатор читается как упавший старт этого семейства, пробелы вокруг не мешают.
#[test]
fn known_family_id_is_read_as_unfinished_startup() {
    let directory = TempDir::new().expect("temp dir");
    for family in [WGPU, GLES2] {
        fs::write(marker_in(&directory), format!("{}\n", family.stable_id())).expect("write");
        let watch = RendererStartupWatch::new(marker_in(&directory));

        assert_eq!(
            watch.unfinished_startup(),
            RendererStartupMarker::UnfinishedStartup(family)
        );
    }
}

/// Мусор и неизвестный идентификатор не ломают старт: читаются как «метки нет».
#[test]
fn garbage_or_unknown_marker_is_ignored() {
    let directory = TempDir::new().expect("temp dir");
    for contents in [&b"\xff\xfe\x00garbage"[..], b"dx10-future", b""] {
        fs::write(marker_in(&directory), contents).expect("write");
        let watch = RendererStartupWatch::new(marker_in(&directory));

        assert_eq!(
            watch.unfinished_startup(),
            RendererStartupMarker::NoUnfinishedStartup,
            "{contents:?}"
        );
    }
}

/// Метка пишется ДО создания рендера и содержит идентификатор семейства.
#[test]
fn creation_starting_writes_the_family_id_before_the_renderer_exists() {
    let directory = TempDir::new().expect("temp dir");
    let mut watch = RendererStartupWatch::new(marker_in(&directory));

    watch.creation_starting(GLES2);

    assert_eq!(
        fs::read_to_string(marker_in(&directory)).expect("marker"),
        "gles2"
    );
}

/// Путь «кадр показан»: настоящий счётчик телеметрии, метка удаляется ровно один раз.
#[test]
fn first_presented_frame_removes_the_marker_exactly_once() {
    let directory = TempDir::new().expect("temp dir");
    let telemetry = Telemetry::new();
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.begin_startup(telemetry.render_frame_totals());
    watch.creation_starting(WGPU);

    // Рендер создан, но кадра ещё не было: метка стоит.
    watch.settle_after_frame(|| telemetry.frames_presented_to_surface());
    assert!(marker_in(&directory).exists());

    telemetry.record_frame_presented_to_surface();
    watch.settle_after_frame(|| telemetry.frames_presented_to_surface());
    assert!(!marker_in(&directory).exists());

    // Второй раз метку не трогаем: чужая метка (другого запуска) остаётся на месте.
    fs::write(marker_in(&directory), "gles2").expect("write");
    telemetry.record_frame_presented_to_surface();
    watch.settle_after_frame(|| telemetry.frames_presented_to_surface());
    assert!(
        marker_in(&directory).exists(),
        "the marker is cleared only once"
    );
}

/// После suspend счётчик телеметрии уже не нулевой: кадр СТАРОГО рендера не снимает метку нового.
#[test]
fn frames_presented_before_this_startup_do_not_clear_the_marker() {
    let directory = TempDir::new().expect("temp dir");
    let telemetry = Telemetry::new();
    for _ in 0..5 {
        telemetry.record_frame_presented_to_surface();
    }
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.begin_startup(telemetry.render_frame_totals());
    watch.creation_starting(WGPU);

    watch.settle_after_frame(|| telemetry.frames_presented_to_surface());
    assert!(marker_in(&directory).exists());

    telemetry.record_frame_presented_to_surface();
    watch.settle_after_frame(|| telemetry.frames_presented_to_surface());
    assert!(!marker_in(&directory).exists());
}

/// Штатное закрытие до первого кадра метку удаляет; после снятия повторное закрытие ничего не трогает.
#[test]
fn orderly_close_before_the_first_frame_removes_the_marker() {
    let directory = TempDir::new().expect("temp dir");
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.creation_starting(WGPU);
    assert!(marker_in(&directory).exists());

    watch.renderer_closed(RenderFrameTotals::default());
    assert!(!marker_in(&directory).exists());

    fs::write(marker_in(&directory), "gles2").expect("write");
    watch.renderer_closed(RenderFrameTotals::default());
    assert!(
        marker_in(&directory).exists(),
        "idle watch leaves foreign markers alone"
    );
}

/// Падение до первого кадра: процесс не успел ничего снять — метка остаётся, и СЛЕДУЮЩИЙ
/// запуск читает её как упавший старт этого семейства.
#[test]
fn crash_before_the_first_frame_leaves_a_marker_the_next_launch_reads() {
    let directory = TempDir::new().expect("temp dir");
    {
        let mut crashing_launch = RendererStartupWatch::new(marker_in(&directory));
        crashing_launch.creation_starting(GLES2);
        // Процесс «умирает»: ни settle, ни renderer_closed не вызываются.
    }

    let next_launch = RendererStartupWatch::new(marker_in(&directory));

    assert_eq!(
        next_launch.unfinished_startup(),
        RendererStartupMarker::UnfinishedStartup(GLES2)
    );
}

/// Обработанная ошибка создания метку снимает (процесс жив, это не падение).
#[test]
fn handled_creation_failure_removes_the_marker() {
    let directory = TempDir::new().expect("temp dir");
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.creation_starting(WGPU);

    watch.creation_failed(WGPU);

    assert!(!marker_in(&directory).exists());
}

/// Если метку записать нельзя (каталога нет), старт не падает; первый кадр всё равно доходит
/// до снятия без паники, а устаревшая метка прошлого запуска убирается.
#[test]
fn unwritable_marker_does_not_break_startup() {
    let directory = TempDir::new().expect("temp dir");
    let missing_directory_marker = directory
        .path()
        .join("missing")
        .join("renderer-startup.marker");
    let mut watch = RendererStartupWatch::new(missing_directory_marker.clone());

    watch.creation_starting(WGPU);
    assert!(!missing_directory_marker.exists());

    let telemetry = Telemetry::new();
    telemetry.record_frame_presented_to_surface();
    watch.settle_after_frame(|| telemetry.frames_presented_to_surface());

    // Фаза завершена: дальше settle не читает счётчик.
    let mut counter_reads = 0;
    watch.settle_after_frame(|| {
        counter_reads += 1;
        0
    });
    assert_eq!(counter_reads, 0);
}

/// Рендер «жив, но нем»: кадры отказаны (`Failed`), показов нет. Старт не завершён — метка
/// остаётся, следующий запуск её прочитает.
#[test]
fn close_after_failed_frames_without_any_presented_frame_keeps_the_marker() {
    let directory = TempDir::new().expect("temp dir");
    let telemetry = Telemetry::new();
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.begin_startup(telemetry.render_frame_totals());
    watch.creation_starting(GLES2);

    telemetry.record_render_failed_frame();
    watch.renderer_closed(telemetry.render_frame_totals());

    assert!(
        marker_in(&directory).exists(),
        "the failed start is remembered"
    );
    assert_eq!(
        RendererStartupWatch::new(marker_in(&directory)).unfinished_startup(),
        RendererStartupMarker::UnfinishedStartup(GLES2)
    );
}

/// Отказы прошлого рендера (до этого старта) метку нового не удерживают.
#[test]
fn failed_frames_of_an_earlier_renderer_do_not_keep_the_new_marker() {
    let directory = TempDir::new().expect("temp dir");
    let telemetry = Telemetry::new();
    telemetry.record_render_failed_frame();
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.begin_startup(telemetry.render_frame_totals());
    watch.creation_starting(WGPU);

    watch.renderer_closed(telemetry.render_frame_totals());

    assert!(!marker_in(&directory).exists());
}

/// Были и отказы, и показ: старт завершён, метка снимается.
#[test]
fn close_after_a_presented_frame_removes_the_marker_even_with_failures() {
    let directory = TempDir::new().expect("temp dir");
    let telemetry = Telemetry::new();
    let mut watch = RendererStartupWatch::new(marker_in(&directory));
    watch.begin_startup(telemetry.render_frame_totals());
    watch.creation_starting(WGPU);

    telemetry.record_render_failed_frame();
    telemetry.record_frame_presented_to_surface();
    watch.renderer_closed(telemetry.render_frame_totals());

    assert!(!marker_in(&directory).exists());
}

/// Метка семейства пишется до пробы; то же семейство при создании не переписывается
/// (второй fsync на старте без фолбэка не нужен), а другое — переписывается.
#[test]
fn probe_marks_the_family_once_and_creation_rewrites_only_a_different_family() {
    let directory = TempDir::new().expect("temp dir");
    let mut watch = RendererStartupWatch::new(marker_in(&directory));

    watch.probe_starting(WGPU);
    assert_eq!(
        fs::read_to_string(marker_in(&directory)).expect("marker"),
        "wgpu-vulkan"
    );

    // Подмена файла: если запись повторится, содержимое вернётся к «wgpu».
    fs::write(marker_in(&directory), "tampered").expect("write");
    watch.creation_starting(WGPU);
    assert_eq!(
        fs::read_to_string(marker_in(&directory)).expect("marker"),
        "tampered",
        "the same family is not written twice"
    );

    watch.probe_starting(GLES2);
    assert_eq!(
        fs::read_to_string(marker_in(&directory)).expect("marker"),
        "gles2"
    );
}
