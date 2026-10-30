//! Обрезанный файл «до кадра» (общий бэклог п. 24): демуксер ломается посреди файла,
//! а плеер доигрывает всё, что успел прочитать, и только потом сообщает ошибку.
//!
//! Файл открывается так же, как локальный файл в приложении: `LocalFileSource` →
//! реестр демуксеров → `PreparedMedia`, без фонового демуксера. Дальше — безоконная
//! сессия с настоящими декодерами (`support/playback.rs`).
//!
//! Обрезанными открываются только форматы без индекса в конце: фрагментированный MP4 и
//! WebM, записанный «вживую» (так пишут OBS и MediaRecorder). Обычные MP4/WebM без
//! хвоста не открываются вовсе (Symphonia не находит `moov`/`Cues`), это другой путь.
//! Фикстуры и их происхождение — `fixtures/generate.sh`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::io::Write;
use std::num::NonZeroUsize;
use std::time::Duration;

use demux_api::{DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use player_core::{PlaybackState, PreparedMedia};
use source_core::{CancellationToken, LocalFileSource};
use support::media::own_fixture;
use support::playback::{PlaybackReport, play_controlled};

/// Фрагментированный MP4: H.264 + AAC, 4 с, 25 к/с, ключевой кадр раз в 0,4 с.
const FRAGMENTED_MP4: &str = "file-h264-aac-fragmented.mp4";
/// WebM «вживую»: VP9 + Opus, те же 4 с.
const LIVE_WEBM: &str = "file-vp9-opus-live.webm";
/// Какую долю файла оставить: обрыв приходится на ~2,4 с из 4.
const KEPT_FRACTION_PERCENT: usize = 60;
/// Кадров в целом ролике.
const CLIP_FRAMES: usize = 100;
/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);

/// Допуск «показан почти последний прочитанный кадр»: два кадра.
///
/// При доигрывании decoder (EOF/DPB drain) отдаёт все кадры прочитанных пакетов, и
/// последним показывается ровно последний прочитанный кадр (так и выходит в прогонах);
/// два кадра — запас на границу часов в конце. Без доигрывания плеер встаёт там, где
/// демуксер наткнулся на обрыв, а прочитанное впрок (звук ~0,2 с, очередь кадров,
/// кадры внутри decoder-а) пропадает — это на порядок больше допуска.
const SHOWN_FRAME_MARGIN: Duration = Duration::from_millis(80);

/// Допуск «прозвучал почти весь прочитанный звук»: один AAC/Opus-пакет с запасом
/// (AAC 1024 сэмпла при 48 кГц ≈ 21 мс; Opus 20 мс; плюс шаг обвязки 5 мс).
const AUDIO_MARGIN: Duration = Duration::from_millis(60);

/// Что прочитал бы демуксер обрезанного файла: факты для сравнения с показанным.
#[derive(Debug)]
struct ReadFacts {
    /// PTS последнего целого видеопакета.
    last_video_pts: Duration,
    /// Конец (PTS + длительность) последнего целого аудиопакета.
    last_audio_end: Duration,
    /// Текст ошибки демуксера на месте обрыва.
    failure: String,
}

/// Обрезанный файл на диске (удаляется вместе со значением).
struct TruncatedFile {
    file: tempfile::NamedTempFile,
}

impl TruncatedFile {
    /// Записывает первые `KEPT_FRACTION_PERCENT` % фикстуры во временный файл.
    fn new(fixture_name: &str) -> Self {
        let bytes = own_fixture(fixture_name);
        let kept = bytes.len() * KEPT_FRACTION_PERCENT / 100;
        let extension = fixture_name.rsplit('.').next().expect("extension");
        let mut file = tempfile::Builder::new()
            .prefix("truncated-")
            .suffix(&format!(".{extension}"))
            .tempfile()
            .expect("temporary file");
        file.write_all(&bytes[..kept])
            .expect("write truncated file");
        file.flush().expect("flush truncated file");
        Self { file }
    }

    /// Открывает файл как приложение открывает локальный файл.
    fn open_demuxer(&self) -> Box<dyn Demuxer + Send> {
        let mut registry = DemuxRegistry::new();
        registry
            .register(Box::new(
                symphonia_demux::SymphoniaDemuxFactory::new(
                    symphonia_demux::DemuxerOptions::default(),
                )
                .expect("symphonia factory"),
            ))
            .expect("register symphonia");
        let source = LocalFileSource::open(self.file.path()).expect("open local file");
        let budget = DemuxSniffBudget::new(
            NonZeroUsize::new(256 * 1024).expect("non-zero"),
            NonZeroUsize::new(8).expect("non-zero"),
            Duration::from_secs(2),
        )
        .expect("sniff budget");
        registry
            .open(
                DemuxInput::byte_source(Box::new(source)),
                DemuxHints::none(),
                budget,
                CancellationToken::never_cancelled(),
            )
            .expect("a truncated file without an index at the end still opens")
    }

    /// Читает файл отдельным демуксером до ошибки и запоминает, что было прочитано.
    fn read_facts(&self) -> ReadFacts {
        let mut demuxer = self.open_demuxer();
        let mut last_video_pts = None;
        let mut last_audio_end = None;
        loop {
            match demuxer.next_event() {
                Ok(DemuxReadEvent::Packet(packet)) => match packet.kind {
                    TrackKind::Video => last_video_pts = last_video_pts.max(Some(packet.pts)),
                    TrackKind::Audio => {
                        let end = packet.pts + packet.duration.unwrap_or_default();
                        last_audio_end = last_audio_end.max(Some(end));
                    }
                    _ => {}
                },
                Ok(DemuxReadEvent::EndOfStream) => panic!("a truncated file must not end cleanly"),
                Ok(_) => {}
                Err(error) => {
                    return ReadFacts {
                        last_video_pts: last_video_pts.expect("video before the break"),
                        last_audio_end: last_audio_end.expect("audio before the break"),
                        failure: format!("{error:#}"),
                    };
                }
            }
        }
    }

    /// Играет файл в безоконной сессии с настоящими декодерами.
    fn play(&self) -> PlaybackReport {
        let prepared =
            PreparedMedia::from_external_label("truncated file test", self.open_demuxer());
        play_controlled(prepared, |_| {})
    }
}

/// Общая проверка: прочитанное доиграно, конец — `Failed` с исходной ошибкой ровно раз.
fn assert_played_out_then_failed(fixture_name: &str, expected_failure: &str) {
    let truncated = TruncatedFile::new(fixture_name);
    let facts = truncated.read_facts();
    assert!(
        facts.failure.contains(expected_failure),
        "{fixture_name}: the demuxer breaks with `{expected_failure}`, got `{}`",
        facts.failure
    );

    let report = truncated.play();
    let last_shown = report.frames.last().expect("frames were shown").pts;
    eprintln!(
        "TRUNCATED {fixture_name}: shown up to {last_shown:?} ({} frames), video read up to {:?}; \
         audio played {:?}, audio read up to {:?}; state {:?}",
        report.frames.len(),
        facts.last_video_pts,
        report.audio_duration(),
        facts.last_audio_end,
        report.final_state
    );

    assert_eq!(report.final_state, PlaybackState::Failed, "{fixture_name}");
    assert!(
        !report
            .state_trace
            .iter()
            .any(|(_, state, _)| *state == PlaybackState::Ended),
        "{fixture_name}: a broken file must not end cleanly"
    );
    assert_eq!(
        report.errors.len(),
        1,
        "{fixture_name}: exactly one error: {:?}",
        report.errors
    );
    assert!(
        report.errors[0].contains("DemuxError") && report.errors[0].contains(expected_failure),
        "{fixture_name}: the original error class and text: {:?}",
        report.errors
    );
    assert!(
        report
            .frames
            .windows(2)
            .all(|pair| pair[0].pts < pair[1].pts),
        "{fixture_name}: frames in order"
    );
    assert!(
        last_shown + SHOWN_FRAME_MARGIN >= facts.last_video_pts,
        "{fixture_name}: shown up to {last_shown:?}, read up to {:?}",
        facts.last_video_pts
    );
    assert!(
        last_shown <= facts.last_video_pts,
        "{fixture_name}: nothing past the break"
    );
    assert!(report.frames.len() < CLIP_FRAMES, "{fixture_name}");
    assert!(
        report.audio_duration() + AUDIO_MARGIN >= facts.last_audio_end,
        "{fixture_name}: audio played {:?}, read up to {:?}",
        report.audio_duration(),
        facts.last_audio_end
    );
    // Синус фикстур (амплитуда 1/8) даёт RMS ~0,09; тишина — 0.
    assert!(
        report.audio_rms() > 0.05,
        "{fixture_name}: audible sound, RMS {}",
        report.audio_rms()
    );
    // Видео не «доиграно» скачком: последний кадр показан в своё время.
    let last_frame = report.frames.last().expect("frames");
    assert!(
        last_frame.pts.abs_diff(last_frame.position) <= FRAME * 3,
        "{fixture_name}: the last frame was shown on time: {last_frame:?}"
    );
}

#[test]
fn truncated_fragmented_mp4_plays_everything_read_then_fails() {
    assert_played_out_then_failed(FRAGMENTED_MP4, "unexpected end of atom");
}

#[test]
fn truncated_live_webm_plays_everything_read_then_fails() {
    assert_played_out_then_failed(LIVE_WEBM, "element payload extends beyond document end");
}
