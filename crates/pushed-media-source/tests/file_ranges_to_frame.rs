//! Файл по запросам байтов «до кадра»: обычный MP4 и WebM, которые мы не качаем,
//! — каждый диапазон запрашивается у «страницы», кадры доходят до показа.
//!
//! Настоящие демуксеры, декодеры видео и звука, настоящее время (обвязка
//! `support/`). Окно предзагрузки в тестах маленькое (8 КиБ при файле ~100 КиБ; очередь пакетов — тоже 8 КиБ):
//! перемотка и конец файла обязаны ходить за байтами, а не доставать их из окна.
//! Фикстуры: `file-h264-aac-moov-end.mp4` / `-faststart.mp4` / `file-vp9-opus.webm`
//! (4 с, 25 к/с = 100 кадров, ключевой кадр раз в 0,4 с) и зашифрованный
//! `file-h264-cenc.mp4`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::thread;
use std::time::{Duration, Instant};

use media_prefetch::PrefetchConfig;
use player_core::PlaybackState;
use pushed_media_source::{
    FileOpenProgress, FileSeekability, OpenedPushedFile, PushedFileError, PushedFileFailureClass,
    PushedFileOpener, PushedFilePolicy, RangeBytesVerdict, RangeFailure, RangeRequestIds,
    RangeRetryPolicy, UnsupportedFileLayout,
};
use support::media::own_fixture;
use support::playback::{PlaybackReport, PresentedFrame, play_controlled};
use support::progressive_file::prepare_file;
use support::range_page::{FakePage, PageFault, PageScript, SizeMode};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
/// Кадров в ролике.
const CLIP_FRAMES: usize = 100;
/// Сколько тест ждёт открытия.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

const MOOV_AT_END: &str = "file-h264-aac-moov-end.mp4";
const FASTSTART: &str = "file-h264-aac-faststart.mp4";
const WEBM: &str = "file-vp9-opus.webm";
const MOOV_END_RECEIVED_MS: u64 = 2360;
const CENC: &str = "file-h264-cenc.mp4";

/// Политика с маленьким окном: файл (~100 КиБ) не помещается целиком.
fn policy(retry: RangeRetryPolicy) -> PushedFilePolicy {
    let prefetch = PrefetchConfig::new(2 * 1024, 4 * 1024, 8 * 1024).expect("prefetch");
    PushedFilePolicy::new(prefetch, retry).expect("policy")
}

/// Быстрые повторы: тест не ждёт секундами.
fn quick_retry(budget: Duration) -> RangeRetryPolicy {
    RangeRetryPolicy::with_budget(budget)
        .with_delays(Duration::from_millis(5), Duration::from_millis(20))
}

/// Ждёт, пока файл откроется или откроется ошибкой.
fn open(opener: &mut PushedFileOpener) -> Result<OpenedPushedFile, PushedFileError> {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        match opener.poll_open() {
            FileOpenProgress::Ready(opened) => return Ok(opened),
            FileOpenProgress::Failed(error) => return Err(error),
            FileOpenProgress::Pending => {
                assert!(Instant::now() < deadline, "the file did not open in time");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Файл целиком: открыть и сыграть, ничего не делая между шагами.
fn play_whole(page_file: &str, script: PageScript) -> (PlaybackReport, FakePage) {
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (page, mut opener) = FakePage::start(own_fixture(page_file), script, policy);
    let opened = open(&mut opener).expect("the file opens");
    let report = play_controlled(prepare_file(opened, &policy), |_| {});
    (report, page)
}

/// Ролик сыгран целиком: 100 кадров по порядку, звук 4 с, ошибок нет.
fn assert_played_whole_clip(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(
        report.frames.len(),
        CLIP_FRAMES,
        "every frame reached the screen"
    );
    assert!(
        report
            .frames
            .windows(2)
            .all(|pair| pair[0].pts < pair[1].pts)
    );
    assert_eq!(report.frames[0].pts, Duration::ZERO);
    assert_eq!(report.frames[CLIP_FRAMES - 1].pts, CLIP - FRAME);
    assert!(
        report.audio_duration().abs_diff(CLIP) < Duration::from_millis(150),
        "audio lasts {:?}",
        report.audio_duration()
    );
    assert!(
        report.audio_rms() > 0.03,
        "the sine must be audible: rms {}",
        report.audio_rms()
    );
}

/// Смещение коробки верхнего уровня в файле.
fn top_level_box_offset(file: &[u8], wanted: &[u8; 4]) -> u64 {
    let mut position = 0_usize;
    while position + 8 <= file.len() {
        let size = u32::from_be_bytes(file[position..position + 4].try_into().unwrap()) as usize;
        if &file[position + 4..position + 8] == wanted {
            return position as u64;
        }
        position += size;
    }
    panic!("box {wanted:?} not found");
}

#[test]
fn mp4_with_the_movie_box_at_the_end_plays_every_frame_and_the_page_was_asked_for_the_end() {
    let file = own_fixture(MOOV_AT_END);
    let moov = top_level_box_offset(&file, b"moov");
    assert!(
        moov > file.len() as u64 / 2,
        "fixture must keep moov at the end"
    );
    let (report, page) = play_whole(MOOV_AT_END, PageScript::default());
    assert_played_whole_clip(&report);
    let served = page.served();
    assert!(
        served.iter().any(|request| request.offset <= moov
            && moov < request.offset + u64::from(request.length.get())),
        "nobody asked for the movie box at {moov}: {served:?}"
    );
    assert!(page.failure().is_none());
}

#[test]
fn faststart_mp4_plays_every_frame() {
    let (report, page) = play_whole(FASTSTART, PageScript::default());
    assert_played_whole_clip(&report);
    assert!(page.failure().is_none());
    // Фильм читался вперёд: после открытия просили всё дальше по файлу.
    let served = page.served();
    let last = served.last().expect("requests");
    assert!(
        last.offset > 50_000,
        "the tail of the file was requested: {served:?}"
    );
}

#[test]
fn webm_plays_every_frame() {
    let (report, page) = play_whole(WEBM, PageScript::default());
    assert_played_whole_clip(&report);
    assert!(page.failure().is_none());
}

#[test]
fn odd_and_one_byte_frames_play_exactly_like_big_ones() {
    for frame_sizes in [vec![1], vec![7, 1021, 3], vec![1 << 20]] {
        let script = PageScript {
            frame_sizes: frame_sizes.clone(),
            ..PageScript::default()
        };
        let (report, _page) = play_whole(FASTSTART, script);
        assert_played_whole_clip(&report);
    }
}

/// Кадры, впервые показанные после команды перемотки `seek_index`.
fn frames_after_seek(report: &PlaybackReport, seek_index: usize) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[seek_index];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .copied()
        .collect()
}

/// Посадка после перемотки: первый кадр — цель ± кадр, старых кадров нет, дальше
/// по порядку до конца ролика.
fn assert_landed_and_played_through(report: &PlaybackReport, target: Duration) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let after = frames_after_seek(report, 0);
    let first = after.first().expect("frames after the seek");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame after the seek at {:?}, target {target:?}",
        first.pts
    );
    assert!(
        after.iter().all(|frame| frame.pts + FRAME >= target),
        "a frame from before the target was shown: {:?}",
        after.iter().map(|frame| frame.pts).collect::<Vec<_>>()
    );
    assert!(after.windows(2).all(|pair| pair[0].pts < pair[1].pts));
    assert_eq!(after.last().map(|frame| frame.pts), Some(CLIP - FRAME));
}

#[test]
fn seek_to_the_middle_asks_the_page_for_bytes_near_the_target_not_from_the_start() {
    for name in [FASTSTART, MOOV_AT_END] {
        let target = Duration::from_millis(2400);
        let file = own_fixture(name);
        let mdat = top_level_box_offset(&file, b"mdat");
        let policy = policy(quick_retry(Duration::from_secs(5)));
        let (page, mut opener) = FakePage::start(file.clone(), PageScript::default(), policy);
        let opened = open(&mut opener).expect("opens");
        assert_eq!(opened.seekability, FileSeekability::Seekable);
        assert_eq!(opened.content_length, Some(file.len() as u64));
        let mut served_before_seek = None;
        let report = play_controlled(prepare_file(opened, &policy), |control| {
            if served_before_seek.is_none()
                && control.observation().position > Duration::from_millis(200)
            {
                served_before_seek = Some(page.served().len());
                control.seek(target);
            }
        });
        assert_landed_and_played_through(&report, target);
        // Цель 2,4 с из 4 с — около 60 % тела файла (аудио и видео перемешаны).
        let after_seek = &page.served()[served_before_seek.expect("seek happened")..];
        let body = file.len() as u64 - mdat;
        let expected = mdat + body * 6 / 10;
        assert!(
            after_seek
                .iter()
                .any(|request| request.offset.abs_diff(expected) < body / 5),
            "{name}: no request near byte {expected} after the seek: {after_seek:?}"
        );
        assert!(
            after_seek
                .iter()
                .all(|request| request.offset > mdat + body / 10 || request.offset < 100),
            "{name}: the page was asked to re-read from the start: {after_seek:?}"
        );
        assert!(page.failure().is_none());
    }
}

#[test]
fn late_bytes_of_a_request_cancelled_by_a_seek_are_dropped_and_playback_goes_on() {
    // Перемотка назад: цель далеко за окном предзагрузки, значит читать надо заново.
    let target = Duration::from_millis(200);
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (page, mut opener) = FakePage::start(own_fixture(FASTSTART), PageScript::default(), policy);
    let opened = open(&mut opener).expect("opens");
    #[derive(Debug, PartialEq)]
    enum Phase {
        WaitFirstFrame,
        WaitStall,
        WaitCancelled { issued_at_stall: usize },
        Done,
    }
    let mut phase = Phase::WaitFirstFrame;
    let report = play_controlled(prepare_file(opened, &policy), |control| {
        match phase {
            Phase::WaitFirstFrame
                if control.observation().position > Duration::from_millis(600) =>
            {
                // Следующий запрос страница начнёт и остановится после первой рамки.
                page.arm(PageFault::StallMidway);
                phase = Phase::WaitStall;
            }
            Phase::WaitStall if page.is_stalled() => {
                // Запрос в полёте. Перемотка далеко за окно: источник бросит его.
                let issued_at_stall = page.issued_count();
                control.seek(target);
                phase = Phase::WaitCancelled { issued_at_stall };
            }
            Phase::WaitCancelled { issued_at_stall } if page.issued_count() > issued_at_stall => {
                // Источник выдал новый запрос: страница досылает остаток брошенного.
                page.release_stalled();
                phase = Phase::Done;
            }
            _ => {}
        }
    });
    assert_eq!(phase, Phase::Done, "the scenario did not run to its end");
    assert!(
        page.stale_verdicts() >= 1,
        "late frames of the cancelled request must be reported as stale"
    );
    assert!(
        page.failure().is_none(),
        "stale bytes are not an attack: {:?}",
        page.failure()
    );
    assert_landed_and_played_through(&report, target);
}

#[test]
fn hostile_page_at_open_ends_the_open_with_input_misuse() {
    for fault in [
        PageFault::ForeignId,
        PageFault::WrongOffset,
        PageFault::ExtraBytes,
        PageFault::ShortWithoutEnd,
    ] {
        let policy = policy(RangeRetryPolicy::disabled());
        let (page, mut opener) =
            FakePage::start(own_fixture(FASTSTART), PageScript::default(), policy);
        page.arm(fault);
        let error = open(&mut opener).expect_err("a hostile page cannot open a file");
        assert_eq!(
            error.failure_class(),
            PushedFileFailureClass::InputMisuse,
            "{fault:?}: {error:?}"
        );
        assert_eq!(
            page.failure().map(|error| error.failure_class()),
            Some(PushedFileFailureClass::InputMisuse)
        );
    }
}

#[test]
fn hostile_page_during_playback_kills_the_file_with_input_misuse_instead_of_hanging() {
    for fault in [
        PageFault::ForeignId,
        PageFault::WrongOffset,
        PageFault::ExtraBytes,
        PageFault::ShortWithoutEnd,
        PageFault::ChangedSize,
    ] {
        let policy = policy(RangeRetryPolicy::disabled());
        let (page, mut opener) =
            FakePage::start(own_fixture(FASTSTART), PageScript::default(), policy);
        let opened = open(&mut opener).expect("opens");
        let mut armed = false;
        let report = play_controlled(prepare_file(opened, &policy), |control| {
            if !armed && control.observation().position > Duration::from_millis(100) {
                armed = true;
                page.arm(fault);
            }
        });
        let error = page
            .failure()
            .unwrap_or_else(|| panic!("{fault:?}: the page was never rejected"));
        assert_eq!(
            error.failure_class(),
            PushedFileFailureClass::InputMisuse,
            "{fault:?}: {error:?}"
        );
        assert_eq!(report.final_state, PlaybackState::Failed, "{fault:?}");
        assert!(
            report.frames.len() < CLIP_FRAMES,
            "{fault:?}: the clip must not play through"
        );
    }
}

/// Ушедшая вкладка посреди файла: страница отдаёт байты до `leave_at_offset` и уходит.
fn play_until_the_tab_leaves(name: &str, leave_at_offset: u64) -> PlaybackReport {
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (page, mut opener) = FakePage::start(own_fixture(name), PageScript::default(), policy);
    let opened = open(&mut opener).expect("the file opens before the tab leaves");
    page.leave_at(leave_at_offset);
    play_controlled(prepare_file(opened, &policy), |_| {})
}

/// Допуск на «сколько успело показаться». Видео-пакеты, целиком лежащие в полученных
/// байтах, доходят до времени `received_until` (из `ffprobe -show_entries
/// packet=pts_time,pos,size`). Плеер держит несколько кадров в декодере и очереди
/// демуксера (очередь фикстур — 8 КиБ ≈ 0,3 с видео, плюс реордер и подготовка
/// декодера), поэтому последний показанный кадр может отставать от полученного; 1 с
/// покрывает это с запасом и всё ещё далеко от прежних 0,72 с при 2,2 с полученных.
const SEAL_SHOWN_MARGIN: Duration = Duration::from_secs(1);

#[test]
fn leaving_the_tab_mid_file_plays_everything_that_arrived_and_ends_cleanly() {
    // Полученное доигрывается и плеер приходит в `Ended` (а не `Failed`): после
    // `seal` конец полученного — конец файла. `received_until` — время последнего
    // видеопакета, целиком лежащего до смещения ухода страницы (фикстуры:
    // moov-end.mp4 до 60000 байт — 2,36 с; webm до 30000 — 1,80 с).
    for (name, leave_at, received_until) in [
        (
            MOOV_AT_END,
            60_000,
            Duration::from_millis(MOOV_END_RECEIVED_MS),
        ),
        (WEBM, 30_000, Duration::from_millis(1800)),
    ] {
        let report = play_until_the_tab_leaves(name, leave_at);
        assert!(
            report.errors.is_empty(),
            "{name}: player errors {:?}",
            report.errors
        );
        assert_eq!(report.final_state, PlaybackState::Ended, "{name}");
        assert!(
            report
                .frames
                .windows(2)
                .all(|pair| pair[0].pts < pair[1].pts)
        );
        let last = report.frames.last().expect("frames").pts;
        assert!(
            last + SEAL_SHOWN_MARGIN >= received_until,
            "{name}: played only up to {last:?}, received up to {received_until:?} ({} frames)",
            report.frames.len()
        );
        assert!(
            report.frames.len() < CLIP_FRAMES,
            "{name}: the missing part cannot be shown"
        );
        eprintln!(
            "SEAL {name}: {} frames, last {last:?}, received until {received_until:?}",
            report.frames.len()
        );
    }
}

#[test]
fn real_damage_without_seal_is_still_an_error() {
    // Тот же файл, но без ухода страницы: мусор посреди `mdat` — настоящая порча.
    // Реальный исход (проверено прогоном): декодер видео отвергает испорченный
    // пакет, плеер уходит в `Failed`, часть кадров до порчи показана (в прогоне 42).
    // Демуксер здесь не ошибается (контейнер цел), поэтому ошибки демуксера без
    // `seal` проверяет модульный тест обёртки; а тут важно, что без `seal` плеер НЕ
    // заканчивает чистым `Ended` со всеми кадрами.
    let mut damaged = own_fixture(FASTSTART);
    let middle = damaged.len() / 2;
    for byte in &mut damaged[middle..middle + 4000] {
        *byte = 0xFF;
    }
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (_page, mut opener) = FakePage::start(damaged, PageScript::default(), policy);
    let opened = open(&mut opener).expect("the head of the file is fine");
    let report = play_controlled(prepare_file(opened, &policy), |_| {});
    assert_eq!(report.final_state, PlaybackState::Failed);
    assert_ne!(report.final_state, PlaybackState::Ended);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("Video decoder thread failed")),
        "{:?}",
        report.errors
    );
    assert!(
        (1..CLIP_FRAMES).contains(&report.frames.len()),
        "frames before the damage were shown, the rest were not: {}",
        report.frames.len()
    );
}

#[test]
fn a_file_truncated_for_real_is_still_an_error_even_though_the_page_completes_it() {
    // Страница честно сообщает размер обрезанного файла, `Completed` приходит ровно
    // у его конца, `seal` не было: это повреждённый файл, а не обрыв вкладки.
    // MP4: Symphonia отказывается открывать файл с `mdat` длиннее файла.
    let mut truncated = own_fixture(FASTSTART);
    truncated.truncate(60_000);
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (_page, mut opener) = FakePage::start(truncated, PageScript::default(), policy);
    let error = open(&mut opener).expect_err("a truncated MP4 does not open");
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia
    );

    // WebM: то же самое (индекс в конце файла не находится).
    let mut truncated = own_fixture(WEBM);
    truncated.truncate(30_000);
    let (_page, mut opener) = FakePage::start(truncated, PageScript::default(), policy);
    let error = open(&mut opener).expect_err("a truncated WebM does not open");
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia
    );
}

#[test]
fn unknown_size_plays_forward_without_a_seek() {
    let script = PageScript {
        size: SizeMode::Unknown,
        ..PageScript::default()
    };
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (page, mut opener) = FakePage::start(own_fixture(FASTSTART), script, policy);
    let opened = open(&mut opener).expect("opens");
    assert_eq!(opened.seekability, FileSeekability::ForwardOnly);
    assert_eq!(opened.content_length, None);
    let report = play_controlled(prepare_file(opened, &policy), |_| {});
    assert_played_whole_clip(&report);
    assert!(page.failure().is_none());
}

#[test]
fn unknown_size_with_the_movie_box_at_the_end_is_an_honest_refusal() {
    let script = PageScript {
        size: SizeMode::Unknown,
        ..PageScript::default()
    };
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (_page, mut opener) = FakePage::start(own_fixture(MOOV_AT_END), script, policy);
    let error = open(&mut opener).expect_err("must refuse, not hang");
    assert_eq!(
        error,
        PushedFileError::UnsupportedLayout {
            layout: UnsupportedFileLayout::MovieAtEndWithoutKnownSize
        }
    );
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnsupportedMedia
    );
}

#[test]
fn encrypted_mp4_is_refused_as_protected_content() {
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let (_page, mut opener) = FakePage::start(own_fixture(CENC), PageScript::default(), policy);
    let error = open(&mut opener).expect_err("DRM is not played");
    assert_eq!(error, PushedFileError::ProtectedContent);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::ProtectedContent
    );
}

#[test]
fn garbage_bytes_are_unparsable_media() {
    let policy = policy(quick_retry(Duration::from_secs(5)));
    let garbage = (0..40_000_u32)
        .map(|index| u8::try_from(index.wrapping_mul(31) % 251).unwrap())
        .collect();
    let (_page, mut opener) = FakePage::start(garbage, PageScript::default(), policy);
    let error = open(&mut opener).expect_err("not media");
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia,
        "{error:?}"
    );
}

#[test]
fn two_network_failures_then_success_still_play_the_whole_clip() {
    let script = PageScript {
        fail_first_requests: 2,
        failure: RangeFailure::Network,
        ..PageScript::default()
    };
    let (report, page) = play_whole(FASTSTART, script);
    assert_played_whole_clip(&report);
    assert!(page.failure().is_none());
    // Повторы — новые запросы: два сбойных плюс успешные.
    assert!(page.served().len() >= 3);
}

#[test]
fn retry_budget_exhaustion_makes_the_file_unavailable() {
    for failure in [RangeFailure::Network, RangeFailure::Http] {
        let script = PageScript {
            fail_first_requests: usize::MAX,
            fail_without_bytes: true,
            failure,
            ..PageScript::default()
        };
        let policy = policy(quick_retry(Duration::from_millis(80)));
        let (page, mut opener) = FakePage::start(own_fixture(FASTSTART), script, policy);
        let error = open(&mut opener).expect_err("the page never answers");
        assert!(
            matches!(error, PushedFileError::RetryBudgetExhausted { .. }),
            "{error:?}"
        );
        assert_eq!(
            error.failure_class(),
            PushedFileFailureClass::SourceUnavailable
        );
        drop(page);
    }
}

#[test]
fn resource_changed_makes_the_file_unavailable_without_retries() {
    let script = PageScript {
        fail_first_requests: 1,
        failure: RangeFailure::ResourceChanged,
        ..PageScript::default()
    };
    let policy = policy(quick_retry(Duration::from_secs(30)));
    let (page, mut opener) = FakePage::start(own_fixture(FASTSTART), script, policy);
    let error = open(&mut opener).expect_err("the file changed");
    assert_eq!(error, PushedFileError::ResourceChanged);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::SourceUnavailable
    );
    assert_eq!(page.served().len(), 1, "a changed file is not retried");
}

#[test]
fn request_numbers_continue_across_files_and_a_late_frame_of_the_old_file_is_stale() {
    let ids = RangeRequestIds::new();
    let policy = policy(RangeRetryPolicy::disabled());
    let (mut old_writer, mut old_opener, old_requests) =
        pushed_media_source::pushed_file(policy, ids.clone(), std::sync::Arc::new(Silent));
    let _ = old_opener.poll_open();
    let old_request = wait_for_request(&old_requests);
    let (mut new_writer, mut new_opener, new_requests) =
        pushed_media_source::pushed_file(policy, ids, std::sync::Arc::new(Silent));
    let _ = new_opener.poll_open();
    let new_request = wait_for_request(&new_requests);
    assert!(new_request.id > old_request.id, "numbers are session-wide");
    // Запоздавшая рамка прежнего файла, прилетевшая новому: не атака.
    assert_eq!(
        new_writer.accept_range_bytes(old_request.id, 12345, b"late"),
        Ok(RangeBytesVerdict::StaleDropped)
    );
    assert!(new_writer.failure().is_none());
    // Номер, которого не выдавали ни тому, ни другому, — нарушение.
    let foreign = pushed_media_source::RangeRequestId::new(new_request.id.get() + 50);
    let error = new_writer
        .accept_range_bytes(foreign, 0, b"x")
        .expect_err("unissued");
    assert_eq!(error.failure_class(), PushedFileFailureClass::InputMisuse);
    old_writer.seal();
}

/// Будильник, которому нечего делать (запросы забирает тест).
struct Silent;

impl pushed_media_source::RangeRequestNotifier for Silent {
    fn range_requested(&self) {}
}

fn wait_for_request(
    requests: &pushed_media_source::PushedFileRequests,
) -> pushed_media_source::RangeRequest {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        if let Some(request) = requests.take_requests().into_iter().next() {
            return request;
        }
        assert!(Instant::now() < deadline, "no request was issued");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Коробка MP4 с 32-битной длиной.
fn mp4_box(box_type: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(8 + body.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(body);
    bytes
}

/// Коробка MP4 с 64-битной длиной (`size == 1`).
fn mp4_large_box(box_type: &[u8; 4], large_size: u64) -> Vec<u8> {
    let mut bytes = 1_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(&large_size.to_be_bytes());
    bytes
}

fn open_hostile_layout(file: Vec<u8>, size: SizeMode) -> (PushedFileError, FakePage) {
    let script = PageScript {
        size,
        ..PageScript::default()
    };
    let (page, mut opener) = FakePage::start(file, script, policy(RangeRetryPolicy::disabled()));
    (
        open(&mut opener).expect_err("a hostile layout must be refused"),
        page,
    )
}

#[test]
fn a_box_length_that_overflows_the_offset_is_unparsable_not_a_clamped_walk() {
    let mut file = mp4_box(b"ftyp", b"isom\0\0\0\0");
    file.extend(mp4_large_box(b"free", u64::MAX));
    let (error, _page) = open_hostile_layout(file, SizeMode::Known);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia
    );
    assert!(error.to_string().contains("overflows"), "{error}");
}

#[test]
fn a_box_walk_to_the_top_of_the_offset_space_ends_instead_of_spinning() {
    // Размер неизвестен, `ftyp` на 2^64 - 8 байт: следующий заголовок лежит у самого
    // предела смещений, и просить там нечего.
    let file = mp4_large_box(b"ftyp", u64::MAX - 7);
    let (error, page) = open_hostile_layout(file, SizeMode::Unknown);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia
    );
    assert!(page.served().len() < 10, "requests: {:?}", page.served());
}

#[test]
fn an_endless_string_of_tiny_boxes_is_cut_off_after_a_bounded_number_of_requests() {
    let mut file = mp4_box(b"ftyp", b"isom\0\0\0\0");
    for _ in 0..1000 {
        file.extend(mp4_box(b"free", b""));
    }
    let (error, page) = open_hostile_layout(file, SizeMode::Known);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia
    );
    assert!(error.to_string().contains("top-level"), "{error}");
    assert!(
        page.served().len() < 300,
        "a round trip per box forever: {} requests",
        page.served().len()
    );
}

#[test]
fn a_movie_box_declared_beyond_the_end_of_the_file_is_refused_before_allocating_it() {
    let mut file = mp4_box(b"ftyp", b"isom\0\0\0\0");
    // `moov` на 20 МиБ (меньше предела источника) в файле из двух коробок.
    file.extend_from_slice(&20_000_000_u32.to_be_bytes());
    file.extend_from_slice(b"moov");
    let (error, _page) = open_hostile_layout(file, SizeMode::Known);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::UnparsableMedia
    );
    assert!(error.to_string().contains("beyond the end"), "{error}");
}
