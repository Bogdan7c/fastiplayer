//! Приёмка на реальном медиа (`#[ignore]`): субтитры рядом с настоящим фильмом доходят до пикселей.
//!
//! Тест обходит папки-сценарии (`01_utf8_srt` ... `11_non_utf8_name`) в каталоге из
//! `FASTIPLAYER_SUBTITLE_ASSETS_DIR`. В каждой папке лежит видеофайл (симлинк на фильм) и
//! файлы субтитров в разных кодировках и местах. Имена файлов в код не вшиты: фильм находится
//! по расширению `.mkv`, ожидаемое время первой реплики берётся из самого файла субтитров.
//!
//! Запуск (путь абсолютный: cargo test идёт из каталога крейта):
//! `FASTIPLAYER_SUBTITLE_ASSETS_DIR=$PWD/test-assets/subs cargo test -p app-egui --locked -- --ignored real_sidecar`
//!
//! Что проверяется в каждом сценарии:
//! 1. поиск + загрузка настоящим телом фоновой задачи выбирают ожидаемый файл (место, формат,
//!    язык), кодировка определена верно, дорожка не пустая;
//! 2. тот же путь через `SubtitleOwner` (поток, mailbox, шрифты системы) даёт готовую дорожку;
//! 3. на кадре в момент `старт первой реплики + 1 мс` с настоящей геометрией картинки
//!    (1280x544 в окне 1920x1080) overlay есть, и после GPU-прохода пиксели субтитров лежат
//!    в нижней половине картинки и не заходят на чёрные полосы; за 1 мс до старта overlay нет.
//!
//! Совпадение шкалы pts декодированного кадра и времени субтитров здесь НЕ проверяется:
//! для этого нужен декодер MKV и GPU-материализация кадра (большой отдельный стенд). Обе
//! шкалы — просто время от начала media, фильм начинается с pts 0.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use render_core::{PictureLayout, PictureRect, PictureSize, RenderViewport};
use render_wgpu_shell::SubtitleOverlayOutcome;
use subtitle_core::SubtitleTime;
use subtitle_formats::load_text_subtitle;
use subtitle_sidecar::{
    SidecarLocation, SidecarSubtitleCandidate, SidecarSubtitleFormat, find_sidecar_subtitles,
};

use super::super::limits::{SIDECAR_SEARCH_LIMITS, SUBTITLE_FILE_MAX_BYTES, SUBTITLE_PARSE_LIMITS};
use super::super::sidecar_job::{
    FontLibraryPlan, SidecarJobOutcome, SidecarJobRequest, encoding_hint_for, run_sidecar_job,
};
use super::gpu_chain::Harness;
use super::*;

/// Переменная с каталогом, где лежат папки-сценарии.
const ASSETS_ENV: &str = "FASTIPLAYER_SUBTITLE_ASSETS_DIR";
/// Размер окна в тесте.
const VIEWPORT: (u32, u32) = (1920, 1080);
/// Размер кадра фильма: 1280x544 (2.35:1), поэтому в окне 16:9 есть чёрные полосы.
const FILM_SIZE: (u32, u32) = (1280, 544);
const GENERATION: u64 = 3;

/// Что ожидается от выбора файла в сценарии.
struct Expectation {
    /// Префикс имени папки (`01`, `02`, ...); номер сценария из README.
    scenario_prefix: &'static str,
    /// Ожидаемое место найденного файла; `None` — субтитров нет.
    location: Option<SidecarLocation>,
    format: SidecarSubtitleFormat,
    /// Допустимые основные языки выбранного файла (`None` — язык в имени не указан).
    /// Несколько значений — когда выбор между языками решает сессия 04, а не поиск.
    languages: &'static [Option<&'static str>],
    /// Ожидаемое имя кодировки из парсера; `None` — не проверяем (UTF-8).
    encoding_name: Option<&'static str>,
}

const EXPECTATIONS: &[Expectation] = &[
    Expectation {
        scenario_prefix: "01",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[None],
        encoding_name: Some("UTF-8"),
    },
    Expectation {
        scenario_prefix: "02",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[Some("ru"), Some("uk")],
        encoding_name: Some("UTF-8"),
    },
    Expectation {
        scenario_prefix: "03",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[None],
        encoding_name: Some("windows-1251"),
    },
    Expectation {
        scenario_prefix: "04",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[None],
        encoding_name: Some("UTF-16LE"),
    },
    Expectation {
        scenario_prefix: "05",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::WebVtt,
        languages: &[None],
        encoding_name: Some("UTF-8"),
    },
    Expectation {
        scenario_prefix: "06",
        location: Some(SidecarLocation::SubtitleDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[Some("fr")],
        encoding_name: Some("UTF-8"),
    },
    Expectation {
        scenario_prefix: "07",
        location: Some(SidecarLocation::MovieNamedDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[Some("en")],
        encoding_name: Some("UTF-8"),
    },
    Expectation {
        scenario_prefix: "08",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[Some("ar")],
        encoding_name: Some("windows-1256"),
    },
    Expectation {
        scenario_prefix: "09",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[Some("ja")],
        encoding_name: Some("UTF-8"),
    },
    Expectation {
        scenario_prefix: "10",
        location: None,
        format: SidecarSubtitleFormat::Srt,
        languages: &[None],
        encoding_name: None,
    },
    Expectation {
        scenario_prefix: "11",
        location: Some(SidecarLocation::VideoDirectory),
        format: SidecarSubtitleFormat::Srt,
        languages: &[None],
        encoding_name: None,
    },
];

/// Папка-сценарий: каталог и путь к «фильму» внутри него.
struct Scenario {
    directory: PathBuf,
    movie: PathBuf,
}

/// Находит папки-сценарии; громко падает, если каталог не читается или папка без `.mkv`.
fn discover_scenarios(root: &Path) -> Vec<Scenario> {
    let mut directories: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("{ASSETS_ENV} is not readable: {error}"))
        .map(|entry| entry.expect("read scenario entry").path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.chars().take(2).all(|c| c.is_ascii_digit()))
        })
        .collect();
    directories.sort();
    directories
        .into_iter()
        .map(|directory| {
            let movies: Vec<PathBuf> = std::fs::read_dir(&directory)
                .expect("read scenario directory")
                .map(|entry| entry.expect("read entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "mkv"))
                .collect();
            assert_eq!(
                movies.len(),
                1,
                "scenario {} must contain exactly one .mkv, found {movies:?}",
                directory.display()
            );
            Scenario {
                movie: movies.into_iter().next().expect("one movie"),
                directory,
            }
        })
        .collect()
}

/// Ожидание для папки по номеру в её имени; `None` — папка вне таблицы (её пропускаем).
fn expectation_for(directory: &Path) -> Option<&'static Expectation> {
    let name = directory
        .file_name()
        .and_then(OsStr::to_str)
        .expect("scenario folder names are UTF-8");
    EXPECTATIONS
        .iter()
        .find(|expectation| name.starts_with(expectation.scenario_prefix))
}

/// Первый подходящий кандидат поиска (тот же выбор, что делает задача).
fn picked_candidate(movie: &Path) -> Option<SidecarSubtitleCandidate> {
    let outcome = find_sidecar_subtitles(movie, &SIDECAR_SEARCH_LIMITS, &|| false)
        .expect("sidecar search must succeed");
    outcome
        .candidates
        .into_iter()
        .find(|candidate| candidate.format.is_renderable_now())
}

/// Результат проверки выбора файла: время старта первой реплики для кадров ниже.
fn assert_pick_and_load(scenario: &Scenario, expectation: &Expectation) -> Option<SubtitleTime> {
    let candidate = picked_candidate(&scenario.movie);
    let outcome = run_sidecar_job(
        &SidecarJobRequest {
            video_path: scenario.movie.clone(),
            fonts: FontLibraryPlan::ReuseExisting,
        },
        &|| false,
    );

    let Some(expected_location) = expectation.location else {
        assert!(candidate.is_none(), "no subtitle files: nothing is picked");
        assert!(
            matches!(outcome, SidecarJobOutcome::NoRenderableCandidate { .. }),
            "scenario without subtitles must end as NoRenderableCandidate"
        );
        return None;
    };

    let candidate = candidate.expect("a subtitle file must be picked");
    assert_eq!(candidate.location, expected_location, "location");
    assert_eq!(candidate.format, expectation.format, "format");
    let picked_language = candidate.language.as_ref().map(|l| l.primary_language());
    assert!(
        expectation.languages.contains(&picked_language),
        "language of the picked file: {picked_language:?} not in {:?}",
        expectation.languages
    );
    if expectation.scenario_prefix == "11" {
        // Имя файла не UTF-8: выбранный путь обязан быть байт-в-байт тем файлом, что лежит на диске.
        let on_disk = std::fs::read_dir(&scenario.directory)
            .expect("read scenario directory")
            .map(|entry| entry.expect("entry").file_name())
            .find(|name| name.to_str().is_none())
            .expect("scenario 11 must contain a non-UTF-8 file name");
        assert_eq!(
            std::os::unix::ffi::OsStrExt::as_bytes(
                candidate.path.file_name().expect("picked file has a name")
            ),
            std::os::unix::ffi::OsStrExt::as_bytes(on_disk.as_os_str()),
            "picked file name bytes are identical to the on-disk name"
        );
    }

    // Кодировку задача наружу не отдаёт: разбираем выбранный файл теми же лимитами и подсказкой.
    let bytes = std::fs::read(&candidate.path).expect("read picked subtitle file");
    assert!(bytes.len() <= SUBTITLE_FILE_MAX_BYTES);
    let text_format = match candidate.format {
        SidecarSubtitleFormat::WebVtt => subtitle_formats::TextSubtitleFormat::WebVtt,
        _ => subtitle_formats::TextSubtitleFormat::Srt,
    };
    let loaded = load_text_subtitle(
        &bytes,
        text_format,
        encoding_hint_for(candidate.language.as_ref()),
        &SUBTITLE_PARSE_LIMITS,
    )
    .expect("picked subtitle file must parse");
    if let Some(expected_name) = expectation.encoding_name {
        assert_eq!(loaded.encoding().encoding_name(), expected_name, "encoding");
    }
    assert!(
        !loaded.had_replacement_characters(),
        "no U+FFFD: the text was decoded cleanly"
    );

    let SidecarJobOutcome::Loaded(track) = outcome else {
        panic!("scenario must end with a loaded track");
    };
    assert_eq!(
        track.file_name.as_deref(),
        candidate.path.file_name(),
        "the job loaded exactly the picked file"
    );
    assert!(!track.timeline.is_empty(), "track has cues");
    eprintln!(
        "  picked {:?} location={:?} lang={:?} encoding={} cues={}",
        candidate.path.file_name().unwrap_or_default(),
        candidate.location,
        candidate.language.as_ref().map(|l| l.as_str()),
        loaded.encoding().encoding_name(),
        track.timeline.len()
    );
    track.timeline.iter().map(|(_, event)| event.start()).min()
}

/// Картинка фильма в окне 1920x1080 (чёрные полосы сверху и снизу).
fn film_rect() -> PictureRect {
    PictureLayout::fit(
        PictureSize::new(FILM_SIZE.0, FILM_SIZE.1),
        RenderViewport::full_surface(VIEWPORT.0, VIEWPORT.1),
    )
    .expect("film layout")
    .rect()
}

/// Кадр на `time` (микросекунды -> pts кадра) со сдвигом в миллисекундах.
fn frame_at(
    time: SubtitleTime,
    shift_millis: i64,
    rect: PictureRect,
) -> SubmittedFrameForSubtitles {
    let micros = time
        .as_micros()
        .saturating_add(shift_millis.saturating_mul(1_000));
    frame_from_micros(micros, rect)
}

fn frame_from_micros(micros: i64, rect: PictureRect) -> SubmittedFrameForSubtitles {
    SubmittedFrameForSubtitles::for_test(
        Duration::from_micros(u64::try_from(micros).expect("pts is non-negative")),
        GENERATION,
        rect,
    )
}

/// Строки (по y), где кадр с субтитрами отличается от кадра без них.
fn changed_rows(harness: &Harness, with: &[u8], without: &[u8]) -> Vec<u32> {
    let row_bytes = (harness.width * 4) as usize;
    with.chunks_exact(row_bytes)
        .zip(without.chunks_exact(row_bytes))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(row, _)| u32::try_from(row).expect("row fits u32"))
        .collect()
}

/// Загрузка через настоящий `SubtitleOwner` и проверка пикселей на кадрах у первой реплики.
fn assert_owner_and_pixels(
    scenario: &Scenario,
    first_cue_start: SubtitleTime,
    harness: Option<&mut Harness>,
) {
    let media = instance(1);
    // Владелец с системными шрифтами, как в продакшене (нужны для арабского и японского).
    let mut owner = SubtitleOwner::new(AppWakePort::disconnected(AppWakeOwner::Subtitles), None);
    begin_and_wait(&mut owner, media, &scenario.movie);
    assert_eq!(
        owner.lifecycle_view(),
        SubtitleLifecycleView::Ready,
        "owner must end Ready"
    );

    let rect = film_rect();
    let before_start = owner.overlay_for_frame(
        current(media, GENERATION),
        &frame_at(first_cue_start, -1, rect),
    );
    assert!(
        before_start.is_none(),
        "1 ms before the first cue: no overlay"
    );

    let overlay = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame_at(first_cue_start, 1, rect),
        )
        .expect("overlay 1 ms after the first cue starts")
        .clone();
    assert!(!overlay.bitmaps().is_empty(), "overlay has bitmaps");

    let Some(harness) = harness else {
        eprintln!("  no GPU adapter: pixel checks skipped");
        return;
    };
    let (_, video_only) = harness.frame(rect, None);
    let (outcome, with_cue) = harness.frame(rect, Some(&overlay));
    assert_eq!(
        outcome,
        SubtitleOverlayOutcome::Drawn {
            bitmaps: overlay.bitmaps().len()
        }
    );
    let rows = changed_rows(harness, &with_cue, &video_only);
    let picture_top = rect.y.round() as u32;
    let picture_bottom = picture_top + rect.height.round() as u32;
    let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else {
        panic!("subtitle must change pixels of the frame");
    };
    assert!(
        first >= picture_top + (picture_bottom - picture_top) / 2,
        "subtitle pixels start in the lower half of the picture (row {first})"
    );
    assert!(
        last < picture_bottom && first > picture_top,
        "subtitle pixels stay inside the picture, not on the bars (rows {first}..={last})"
    );
    // Нижняя полоса и верхняя полоса остаются ровно «видео».
    let row_bytes = (harness.width * 4) as usize;
    for bar_row in (0..picture_top).chain(picture_bottom..harness.height) {
        let range = bar_row as usize * row_bytes..(bar_row as usize + 1) * row_bytes;
        assert_eq!(
            with_cue[range.clone()],
            video_only[range],
            "bar row {bar_row}"
        );
    }
    let (outcome_without, without_cue) = harness.frame(rect, None);
    assert_eq!(outcome_without, SubtitleOverlayOutcome::NothingToDraw);
    assert_eq!(without_cue, video_only, "no cue: frame equals plain video");
    eprintln!(
        "  pixels: rows {first}..={last} of picture {picture_top}..{picture_bottom}, bitmaps={}",
        overlay.bitmaps().len()
    );
}

#[test]
#[ignore = "needs owner-local media: set FASTIPLAYER_SUBTITLE_ASSETS_DIR"]
fn real_sidecar_scenarios_reach_pixels_over_the_real_picture_geometry() {
    let Some(root) = std::env::var_os(ASSETS_ENV).map(PathBuf::from) else {
        eprintln!("skip: {ASSETS_ENV} is not set");
        return;
    };
    let scenarios = discover_scenarios(&root);
    // Каждый сценарий таблицы обязан существовать: пропавшая папка — ошибка, а не тихий пропуск.
    for expectation in EXPECTATIONS {
        assert!(
            scenarios.iter().any(|scenario| {
                scenario
                    .directory
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(expectation.scenario_prefix))
            }),
            "scenario folder {} is missing under {}",
            expectation.scenario_prefix,
            root.display()
        );
    }
    let mut harness = Harness::with_size(VIEWPORT.0, VIEWPORT.1);
    for scenario in &scenarios {
        eprintln!("scenario {}", scenario.directory.display());
        let Some(expectation) = expectation_for(&scenario.directory) else {
            eprintln!("  skipped: folder is not in the expectation table");
            continue;
        };
        match assert_pick_and_load(scenario, expectation) {
            Some(first_cue_start) => {
                assert_owner_and_pixels(scenario, first_cue_start, harness.as_mut());
            }
            None => {
                let media = instance(1);
                let mut owner = owner();
                begin_and_wait(&mut owner, media, &scenario.movie);
                assert_eq!(
                    owner.lifecycle_view(),
                    SubtitleLifecycleView::NoSubtitleFile,
                    "no subtitles next to the movie"
                );
            }
        }
    }
}
