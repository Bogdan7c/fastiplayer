//! Тесты владельца субтитров: от файла на диске до overlay (и до пикселей в `gpu_chain`).
//!
//! Общие помощники: временный каталог с «фильмом» и файлом субтитров, владелец с быстрыми
//! встроенными шрифтами, ожидание фоновой загрузки через настоящий `begin_media`.

mod appearance_edit;
mod appearance_edit_ui;
mod ass_gpu_support;
mod ass_layer1_gpu;
mod ass_position_gpu;
mod ass_position_lift_gpu;
mod ass_sidecar;
mod control_actions;
mod drop_flow;
mod embedded_ass;
mod embedded_auto_select;
mod embedded_feed;
mod embedded_gpu;
mod embedded_menu;
mod embedded_support;
mod embedded_test_font;
mod gpu_chain;
mod language;
mod memory_chain;
mod memory_restore;
mod placement;
mod presentation;
mod real_sidecar;
mod sidecar_job;
mod track_selection;

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fastiplayer_config::SubtitlesConfig;
use player_core::MediaInstanceId;
use render_core::{PictureLayout, PictureRect, PictureSize, RenderViewport};

use super::auto_select::PreferredLanguages;
use super::intents::SubtitleVisibility;
use super::limits::LoadLimits;
use super::sidecar_job::{
    DiscoveryOutcome, FontLibraryPlan, PreloadTarget, SidecarJobOutcome, SidecarJobRequest,
    SidecarJobTask,
};
use super::{
    CurrentMediaForSubtitles, SubmittedFrameForSubtitles, SubtitleLifecycleView,
    SubtitleMediaSource, SubtitleOwner,
};
use crate::app_wake::{AppWakeOwner, AppWakePort};

/// Сколько ждём фоновую загрузку в тестах.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// Две реплики: [1 с, 3 с) и [5 с, 7 с). Между ними пауза без субтитров.
pub(super) const TWO_CUE_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nFirst cue\n\n\
2\n00:00:05,000 --> 00:00:07,000\nSecond cue\n";

/// Детерминированный идентификатор media для тестов.
pub(super) fn instance(raw: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(raw).expect("test instance id is non-zero"))
}

/// Лимиты загрузки по умолчанию (как в продукте).
pub(super) fn default_load_limits() -> LoadLimits {
    LoadLimits::from_config(&SubtitlesConfig::default())
}

/// Запрос «найти рядом с видео и прочитать выбранную автовыбором дорожку» без языков.
pub(super) fn discovery_request(video: &Path, fonts: FontLibraryPlan) -> SidecarJobRequest {
    discovery_request_with(video, fonts, &[], PreloadTarget::Selected)
}

/// То же с языками и целью чтения.
pub(super) fn discovery_request_with(
    video: &Path,
    fonts: FontLibraryPlan,
    preferred: &[&str],
    preload: PreloadTarget,
) -> SidecarJobRequest {
    let codes: Vec<String> = preferred.iter().map(|code| (*code).to_owned()).collect();
    SidecarJobRequest {
        task: SidecarJobTask::DiscoverNextToVideo {
            video_path: Some(video.to_path_buf()),
            preferred_languages: PreferredLanguages::resolve(&codes, None),
            preload,
            remembered_file: None,
        },
        fonts,
        limits: default_load_limits(),
    }
}

/// Итог поиска из результата задачи (иначе тест падает с понятным сообщением).
pub(super) fn expect_discovery(outcome: SidecarJobOutcome) -> DiscoveryOutcome {
    match outcome {
        SidecarJobOutcome::Discovered(discovery) => *discovery,
        SidecarJobOutcome::FileLoaded { .. } => panic!("expected a search result, got a file"),
        SidecarJobOutcome::FontsBuilt(_) => panic!("expected a search result, got fonts"),
        SidecarJobOutcome::Failed(failure) => panic!("expected a search result, got {failure}"),
        SidecarJobOutcome::Cancelled => panic!("expected a search result, got cancellation"),
    }
}

/// Владелец с отключённым wake-портом и встроенными шрифтами (быстро и без шрифтов машины).
pub(super) fn owner() -> SubtitleOwner {
    owner_with_config(&enabled_config())
}

/// Конфиг с включёнными субтитрами (по умолчанию в продукте они выключены).
pub(super) fn enabled_config() -> SubtitlesConfig {
    SubtitlesConfig {
        enabled: true,
        ..SubtitlesConfig::default()
    }
}

/// Владелец с заданным конфигом, отключённым wake-портом и встроенными шрифтами.
pub(super) fn owner_with_config(config: &SubtitlesConfig) -> SubtitleOwner {
    let mut owner = SubtitleOwner::new(
        AppWakePort::disconnected(AppWakeOwner::Subtitles),
        None,
        config,
    );
    owner.use_embedded_fonts_only();
    owner
}

/// Кладёт `Movie.mkv` (содержимое не важно: задаче нужен только путь) и, если задан,
/// `Movie.srt` рядом. Возвращает путь к «видео».
pub(super) fn write_movie(directory: &Path, srt: Option<&str>) -> PathBuf {
    let video_path = directory.join("Movie.mkv");
    std::fs::write(&video_path, b"not a real video").expect("write dummy video");
    if let Some(srt) = srt {
        std::fs::write(directory.join("Movie.srt"), srt).expect("write srt");
    }
    video_path
}

/// Запускает загрузку через настоящий `begin_media` и ждёт, пока задача завершится.
///
/// Проходит весь production-путь: поток, mailbox, `poll_jobs`, `accept_job_outcome`.
pub(super) fn begin_and_wait(owner: &mut SubtitleOwner, media: MediaInstanceId, video: &Path) {
    owner.begin_media(media, SubtitleMediaSource::LocalFile(video.to_path_buf()));
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while owner.lifecycle_view() == SubtitleLifecycleView::Loading {
        assert!(
            Instant::now() < deadline,
            "subtitle job did not finish in time"
        );
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Владелец, у которого для `media` уже загружена дорожка `TWO_CUE_SRT`.
pub(super) fn ready_owner(media: MediaInstanceId) -> (SubtitleOwner, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let mut owner = owner();
    begin_and_wait(&mut owner, media, &video);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    (owner, directory)
}

/// Что видит плеер: media и поколение ресурсов рендера.
pub(super) fn current(media: MediaInstanceId, render_generation: u64) -> CurrentMediaForSubtitles {
    CurrentMediaForSubtitles {
        media_instance_id: Some(media),
        render_generation,
    }
}

/// Кадр с заданным pts в поколении `render_generation` и картинкой `rect`.
pub(super) fn frame(
    pts_millis: u64,
    render_generation: u64,
    rect: PictureRect,
) -> SubmittedFrameForSubtitles {
    SubmittedFrameForSubtitles::for_test(Duration::from_millis(pts_millis), render_generation, rect)
}

/// Картинка 2.39:1 в области 16:9 заданного размера (полосы сверху и снизу).
pub(super) fn letterboxed_rect(viewport_width: u32, viewport_height: u32) -> PictureRect {
    PictureLayout::fit(
        PictureSize::new(2390, 1000),
        RenderViewport::full_surface(viewport_width, viewport_height),
    )
    .expect("letterbox layout")
    .rect()
}

/// Картинка 16:9 без полос.
pub(super) fn full_rect(width: u32, height: u32) -> PictureRect {
    PictureRect {
        x: 0.0,
        y: 0.0,
        width: width as f32,
        height: height as f32,
    }
}

/// Охранный тест единой точки запуска: все пути установки media идут через
/// `record_installed_media_observables`, а оба пути сброса очищают субтитры.
/// `AppState` нельзя собрать без окна, поэтому хук закрепляем проверкой исходника.
#[test]
fn media_lifecycle_hooks_start_and_clear_subtitles() {
    let source = include_str!("../../media_jobs.rs");
    let installed = function_body(source, "fn record_installed_media_observables");
    assert!(installed.contains("start_subtitles_for_installed_media"));
    for clearing in [
        "fn clear_released_installed_media_source",
        "fn record_cleared_media_after_exact_reset",
    ] {
        assert!(
            function_body(source, clearing).contains("clear_subtitles_for_released_media"),
            "{clearing} must clear subtitles"
        );
    }
    // Оба публичных входа установки обязаны проходить через общую функцию с `MediaInstanceId`.
    assert!(source.contains("installed_media_instance_id(installed)"));
    assert!(
        source.contains(
            "record_installed_media_observables(source.clone(), Some(media_instance_id))"
        )
    );
}

/// Текст функции от её сигнатуры до следующей функции того же уровня (грубо, для guard-тестов).
fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source.find(signature).expect("function exists");
    let rest = &source[start..];
    let end = rest[signature.len()..]
        .find("\n    fn ")
        .or_else(|| rest[signature.len()..].find("\n    pub"))
        .map_or(rest.len(), |offset| offset + signature.len());
    &rest[..end]
}
