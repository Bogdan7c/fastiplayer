//! Тесты вопроса «сядет ли перемотка с полки» (`PushedStreamMonitor::seek_outlook`,
//! живой скраб окна моста, 04a).
//!
//! Фикстуры: `video-h264-64-4s.mp4` — 10 фрагментов по 0,4 с (фрагмент N —
//! [N × 0,4 с; (N + 1) × 0,4 с)); `audio-aac-4s.mp4` — 10 фрагментов, фрагмент N
//! начинается в N × 405,33 мс.
//!
//! Главная проверка — сверка с НАСТОЯЩЕЙ перемоткой буфера: для каждой цели сетки
//! ответ «с полки / попросит страницу» совпадает с тем, поставила ли перемотка
//! просьбу странице. Если правило вопроса разойдётся с правилом перемотки (другая
//! функция, забытая полоса, позднее вступление), сверка упадёт.

use std::ops::Range;
use std::time::Duration;

use super::{PushedStreamMonitor, ShelfSeekOutlook};
use crate::container::PushedContainer;
use crate::lane::LaneId;
use crate::policy::PushedSourcePolicy;
use crate::stream::{PushedStreamWriter, pushed_stream};
use crate::test_fixtures::{AUDIO_AAC_4S, SplitMp4, VIDEO_H264_64_4S, split_mp4};
use crate::time::PushedDuration;

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
const MUXED_TS: &[u8] = include_bytes!("../fixtures/muxed-h264-aac.ts");
/// Шаг сетки целей при сверке с настоящей перемоткой.
const GRID_STEP_MS: u64 = 50;
/// Конец сетки: чуть дальше конца медиа (4 с).
const GRID_END_MS: u64 = 4_200;

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// Поток «видео + звук» длиной 4 с: init отправлены, фрагменты — по сценарию теста.
struct Stream {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
    video: SplitMp4,
    audio: SplitMp4,
}

impl Stream {
    fn new() -> Self {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, _opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer
            .add_lane(AUDIO, PushedContainer::FragmentedMp4)
            .expect("audio lane");
        writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
        let video = split_mp4(VIDEO_H264_64_4S);
        let audio = split_mp4(AUDIO_AAC_4S);
        writer.append(VIDEO, &video.init).expect("video init");
        writer.append(AUDIO, &audio.init).expect("audio init");
        Self {
            monitor: writer.monitor(),
            writer,
            video,
            audio,
        }
    }

    fn push(&mut self, lane: LaneId, fragments: Range<usize>) {
        let media = if lane == VIDEO {
            &self.video
        } else {
            &self.audio
        };
        let bytes = media.fragments[fragments].concat();
        self.writer.append(lane, &bytes).expect("fragments");
    }

    fn outlook(&self, target_ms: u64) -> ShelfSeekOutlook {
        self.monitor.seek_outlook(ms(target_ms))
    }

    /// Что сделала настоящая перемотка буфера к цели: попросила страницу или нет.
    fn real_seek(&self, target_ms: u64) -> ShelfSeekOutlook {
        // Незабранная прежняя просьба не должна смешаться с ответом этой перемотки.
        let _earlier = self.monitor.take_page_seek_request();
        let seek = self.monitor.buffer.seek(ms(target_ms)).expect("seek");
        drop(seek);
        match self.monitor.take_page_seek_request() {
            Some(_) => ShelfSeekOutlook::AsksPage,
            None => ShelfSeekOutlook::FromShelf,
        }
    }

    /// Сверяет вопрос с настоящей перемоткой по всей сетке целей; возвращает
    /// ответы (цель, исход) для проверок сценария.
    fn assert_outlook_matches_real_seeks(&self) -> Vec<(u64, ShelfSeekOutlook)> {
        let mut answers = Vec::new();
        for target_ms in (0..=GRID_END_MS).step_by(usize::try_from(GRID_STEP_MS).expect("step")) {
            let predicted = self.outlook(target_ms);
            let actual = self.real_seek(target_ms);
            assert_eq!(
                predicted, actual,
                "outlook disagrees with the real seek at {target_ms} ms"
            );
            answers.push((target_ms, predicted));
        }
        answers
    }
}

fn outcome_at(answers: &[(u64, ShelfSeekOutlook)], target_ms: u64) -> ShelfSeekOutlook {
    answers
        .iter()
        .find(|(target, _)| *target == target_ms)
        .map(|(_, outcome)| *outcome)
        .expect("target on the grid")
}

#[test]
fn a_shelf_with_a_video_hole_lands_on_both_sides_and_asks_the_page_inside() {
    let mut stream = Stream::new();
    // Видео 0–1,6 с и 2,4–4 с (дыра 1,6–2,4 с), звук целиком.
    stream.push(VIDEO, 0..4);
    stream.push(VIDEO, 6..10);
    stream.push(AUDIO, 0..10);

    let answers = stream.assert_outlook_matches_real_seeks();

    // Два куска полки: до дыры и после неё садимся с полки, в дыре — к странице.
    for target_ms in [0, 500, 1_200, 1_550, 2_400, 3_000, 3_950] {
        assert_eq!(
            outcome_at(&answers, target_ms),
            ShelfSeekOutlook::FromShelf,
            "{target_ms} ms is downloaded"
        );
    }
    for target_ms in [1_650, 2_000, 2_250] {
        assert_eq!(
            outcome_at(&answers, target_ms),
            ShelfSeekOutlook::AsksPage,
            "{target_ms} ms is in the hole"
        );
    }
    // До 100 мс перед сегментом плеер перешагивает: правило полки, не догадка вопроса.
    assert_eq!(stream.outlook(2_350), ShelfSeekOutlook::FromShelf);
}

#[test]
fn audio_starting_after_the_target_joins_late_and_the_seek_stays_on_the_shelf() {
    let mut stream = Stream::new();
    // Звук только с фрагмента 1 (405 мс): у цели 200 мс звука нет, видео есть.
    stream.push(VIDEO, 0..10);
    stream.push(AUDIO, 1..10);

    let answers = stream.assert_outlook_matches_real_seeks();

    // Правило позднего вступления: звук подхватит со своего первого сегмента.
    assert_eq!(outcome_at(&answers, 200), ShelfSeekOutlook::FromShelf);
    assert_eq!(outcome_at(&answers, 0), ShelfSeekOutlook::FromShelf);
}

#[test]
fn an_audio_hole_in_the_middle_is_not_a_late_start_and_asks_the_page() {
    let mut stream = Stream::new();
    // Звук без фрагмента 2 (810–1216 мс): это дыра посреди полосы, не позднее начало.
    stream.push(VIDEO, 0..10);
    stream.push(AUDIO, 0..2);
    stream.push(AUDIO, 3..10);

    let answers = stream.assert_outlook_matches_real_seeks();

    assert_eq!(outcome_at(&answers, 900), ShelfSeekOutlook::AsksPage);
    assert_eq!(outcome_at(&answers, 500), ShelfSeekOutlook::FromShelf);
    assert_eq!(outcome_at(&answers, 1_500), ShelfSeekOutlook::FromShelf);
}

#[test]
fn lanes_without_bytes_ask_the_page_everywhere() {
    let stream = Stream::new();
    assert_eq!(stream.outlook(0), ShelfSeekOutlook::AsksPage);
    assert_eq!(stream.outlook(1_000), ShelfSeekOutlook::AsksPage);
}

#[test]
fn a_stream_without_lanes_or_with_a_transport_stream_lane_cannot_seek_by_time() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream");
    assert_eq!(
        writer.monitor().seek_outlook(ms(0)),
        ShelfSeekOutlook::Unavailable
    );
    writer
        .add_lane(VIDEO, PushedContainer::MpegTs)
        .expect("ts lane");
    writer.append(VIDEO, MUXED_TS).expect("ts bytes");
    assert_eq!(
        writer.monitor().seek_outlook(ms(0)),
        ShelfSeekOutlook::Unavailable
    );
}

#[test]
fn asking_changes_nothing_in_the_buffer_or_in_the_page_conversation() {
    let mut stream = Stream::new();
    stream.push(VIDEO, 0..4);
    stream.push(AUDIO, 0..10);
    let status_before = stream.monitor.buffer_status();
    let held_before = stream.monitor.held_segment_bytes();
    let common_start_before = stream.monitor.earliest_common_start();
    // Страница сама идёт на 3 с: следующая перемотка плеера туда просьбу не шлёт.
    stream.writer.note_page_seek(ms(3_000));

    for target_ms in [0, 800, 3_000, 3_900] {
        let _outlook = stream.outlook(target_ms);
    }

    assert_eq!(stream.monitor.buffer_status(), status_before);
    assert_eq!(stream.monitor.held_segment_bytes(), held_before);
    assert_eq!(stream.monitor.earliest_common_start(), common_start_before);
    // Вопрос не поставил просьбу странице и не израсходовал отметку её перемотки:
    // настоящая перемотка к 3 с всё ещё узнаёт перемотку страницы.
    assert_eq!(stream.monitor.take_page_seek_request(), None);
    assert_eq!(stream.outlook(3_000), ShelfSeekOutlook::AsksPage);
    drop(stream.monitor.buffer.seek(ms(3_000)).expect("seek"));
    assert_eq!(stream.monitor.take_page_seek_request(), None);
}
