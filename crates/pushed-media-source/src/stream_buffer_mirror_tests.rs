//! Предохранитель памяти — «зеркало браузера» (решение владельца 1.11.2026):
//! выкидывает только у полосы сверх её нормы и в порядке браузера (самое раннее
//! сыгранное, затем самое далёкое будущее), играющий кусок не трогает никогда,
//! а когда честно выкинуть нечего — «полон», без потерь.
//!
//! Полки собираются синтетически (байты — нули, время — заданное): здесь
//! проверяется только выбор, что выселять. Сквозная проверка с настоящим
//! плеером — `tests/mirror_guard_to_frame.rs`.

use std::sync::Arc;

use bytes::Bytes;

use super::{StreamBuffer, StreamBufferRules};
use crate::buffer_window::BufferIntake;
use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::error::PushedStreamError;
use crate::feed::{EpisodeKey, FeedCursor};
use crate::framing::{LaneContent, MediaTime, MediaTimeSpan, TimedSpan};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::lane::{LaneId, LaneMailbox};
use crate::policy::PushedSourcePolicy;
use crate::shelf::ShelfEntry;
use crate::stream::pushed_demux_registry;
use crate::time::TimestampOffset;

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

fn at(milliseconds: i64) -> MediaTime {
    MediaTime::from_micros(milliseconds * 1_000)
}

/// Буфер с двумя полосами (видео и звук) и заданными порогами.
struct Bench {
    buffer: StreamBuffer,
    ledger: Arc<IntakeLedger>,
}

impl Bench {
    /// `ceiling` — жёсткий потолок, `full_at` — порог «полон», `reserve` — резерв
    /// полосы без видео (всё в байтах).
    fn new(ceiling: usize, full_at: usize, reserve: usize) -> Self {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let rules = StreamBufferRules {
            full_at_held_bytes: full_at,
            lane_without_video_reserve_bytes: reserve,
            ..policy.stream_buffer_rules()
        };
        let ledger = Arc::new(IntakeLedger::new(ceiling));
        let factory =
            EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy);
        let buffer = StreamBuffer::new(Arc::clone(&ledger), factory, rules);
        for (lane, content) in [
            (VIDEO, LaneContent::CarriesVideo),
            (AUDIO, LaneContent::WithoutVideo),
        ] {
            buffer.add_lane(
                lane,
                PushedContainer::FragmentedMp4,
                Arc::new(LaneMailbox::default()),
                4,
            );
            buffer.note_lane_content(lane, content).expect("lane");
        }
        Self { buffer, ledger }
    }

    /// Кладёт на полку полосы сегменты `[start, start + length)` по `bytes` байтов.
    fn shelve(&self, lane: LaneId, starts_ms: &[i64], length_ms: i64, bytes: usize) {
        let key = EpisodeKey::new(PushedContainer::FragmentedMp4, None, TimestampOffset::ZERO);
        let mut state = self.buffer.lock();
        let buffered = state.lanes.get_mut(&lane).expect("lane");
        for &start in starts_ms {
            let media =
                ChargedSegment::charge(&self.ledger, Bytes::from(vec![0; bytes])).expect("room");
            let span = MediaTimeSpan::new(at(start), at(start + length_ms)).expect("span");
            buffered.shelf.insert(ShelfEntry::waiting(
                TimedSpan::exact(span),
                media,
                key.clone(),
            ));
        }
    }

    /// Как [`Self::shelve`], но возвращает копии байтов — как труба эпизода, которая
    /// ещё держит поданные сегменты (их предохранитель не трогает).
    fn shelve_held_by_a_pipe(
        &self,
        lane: LaneId,
        starts_ms: &[i64],
        length_ms: i64,
        bytes: usize,
    ) -> Vec<ChargedSegment> {
        let key = EpisodeKey::new(PushedContainer::FragmentedMp4, None, TimestampOffset::ZERO);
        let mut state = self.buffer.lock();
        let buffered = state.lanes.get_mut(&lane).expect("lane");
        let mut pipe = Vec::new();
        for &start in starts_ms {
            let media =
                ChargedSegment::charge(&self.ledger, Bytes::from(vec![0; bytes])).expect("room");
            pipe.push(media.clone());
            let span = MediaTimeSpan::new(at(start), at(start + length_ms)).expect("span");
            buffered.shelf.insert(ShelfEntry::waiting(
                TimedSpan::exact(span),
                media,
                key.clone(),
            ));
        }
        pipe
    }

    /// Подача у обеих полос дошла до `cursor_ms`, плеер стоит на `position_ms`.
    fn play_at(&self, cursor_ms: i64, position_ms: i64) {
        let mut state = self.buffer.lock();
        for lane in [VIDEO, AUDIO] {
            state
                .lanes
                .get_mut(&lane)
                .expect("lane")
                .feed
                .advance_cursor(FeedCursor::exact(at(cursor_ms)));
        }
        state.reported_position = Some(at(position_ms));
    }

    /// Один проход предохранителя у порога «полон».
    fn relieve(&self) {
        let mut state = self.buffer.lock();
        self.buffer.relieve_memory_pressure(&mut state);
    }

    /// Начала сегментов на полке полосы, мс.
    fn starts_ms(&self, lane: LaneId) -> Vec<i64> {
        self.buffer.lock().lanes[&lane]
            .shelf
            .layout()
            .into_iter()
            .map(|(start, _, _)| start / 1_000)
            .collect()
    }
}

#[test]
fn played_video_goes_earliest_first_while_audio_under_its_reserve_stays_whole() {
    // Видео 0–2 с по 200 мс (10 × 100 Б), звук 0–2 с по 400 мс (5 × 20 Б).
    // Порог 800 Б, резерв звука 500 Б: норма видео 300 Б, держим 1100 Б.
    let bench = Bench::new(2_000, 800, 500);
    bench.shelve(
        VIDEO,
        &[0, 200, 400, 600, 800, 1000, 1200, 1400, 1600, 1800],
        200,
        100,
    );
    bench.shelve(AUDIO, &[0, 400, 800, 1200, 1600], 400, 20);
    bench.play_at(2_000, 1_000);

    bench.relieve();

    // Видео: сыгранное с самого раннего, пока байты не стали ниже порога
    // (1100 − 4 × 100 = 700 < 800). Сегмент у позиции (800–1000 кончился ровно
    // на ней) пока не нужен был.
    assert_eq!(bench.starts_ms(VIDEO), [800, 1000, 1200, 1400, 1600, 1800]);
    // Звук у мест возврата цел: страница его второй раз не пришлёт.
    assert_eq!(bench.starts_ms(AUDIO), [0, 400, 800, 1200, 1600]);
    assert_eq!(bench.ledger.held_bytes(), 700);
    assert_eq!(bench.buffer.lock().memory_guard.evicted_segments, 4);
}

#[test]
fn without_played_video_the_farthest_future_goes_first_and_the_playing_run_stays() {
    // Видео играет кусок 800–1800 с позиции 800, впереди после дыры — 5000–5600
    // (время прежних мест после перемотки назад). Порог 750 Б, резерв звука
    // 500 Б → норма видео 250 Б; держим 8 × 100 + 5 × 20 = 900 Б.
    let bench = Bench::new(2_000, 750, 500);
    bench.shelve(VIDEO, &[800, 1000, 1200, 1400, 1600], 200, 100);
    bench.shelve(VIDEO, &[5000, 5200, 5400], 200, 100);
    bench.shelve(AUDIO, &[0, 400, 800, 1200, 1600], 400, 20);
    bench.play_at(800, 800);

    bench.relieve();

    // Сыгранного видео нет: уходит самое далёкое будущее (5400, потом 5200),
    // пока байты не стали ниже порога (900 − 2 × 100 = 700 < 750).
    assert_eq!(bench.starts_ms(VIDEO), [800, 1000, 1200, 1400, 1600, 5000]);
    assert_eq!(bench.starts_ms(AUDIO), [0, 400, 800, 1200, 1600]);
}

#[test]
fn nothing_honest_to_evict_keeps_everything_and_says_full() {
    // Видео — только играющий кусок у позиции, звук — в своём резерве; байты
    // выше порога. Выкинуть можно было бы только нужное сейчас — не выкидываем.
    let bench = Bench::new(2_000, 500, 500);
    bench.shelve(VIDEO, &[1000, 1200, 1400, 1600, 1800], 200, 100);
    bench.shelve(AUDIO, &[0, 400, 800, 1200, 1600], 400, 20);
    bench.play_at(1_000, 1_000);

    bench.relieve();

    assert_eq!(bench.starts_ms(VIDEO), [1000, 1200, 1400, 1600, 1800]);
    assert_eq!(bench.starts_ms(AUDIO), [0, 400, 800, 1200, 1600]);
    assert_eq!(bench.buffer.lock().memory_guard.evicted_segments, 0);
    // Честная страница слышит «полон» и ждёт — ничего не потеряно.
    assert_eq!(bench.buffer.buffer_status().intake, BufferIntake::Full);
}

#[test]
fn audio_over_its_reserve_loses_its_earliest_played_sound_and_video_within_norm_is_kept() {
    // Многочасовой ролик: звука накопилось больше резерва (100 Б при резерве
    // 50 Б), видео в норме. Браузер тоже выкинул самый ранний звук — у нас так же.
    let bench = Bench::new(2_000, 300, 50);
    bench.shelve(VIDEO, &[1600, 1800], 200, 100);
    bench.shelve(AUDIO, &[0, 400, 800, 1200, 1600], 400, 20);
    bench.play_at(2_000, 1_700);

    bench.relieve();

    // 300 Б держим → ниже порога после одного выселения: самый ранний звук.
    assert_eq!(bench.starts_ms(AUDIO), [400, 800, 1200, 1600]);
    assert_eq!(bench.starts_ms(VIDEO), [1600, 1800]);
}

#[test]
fn the_hard_ceiling_takes_played_video_first_and_refuses_when_only_the_playing_run_is_left() {
    // Потолок 620 Б почти полон (5 × 100 видео + 5 × 20 звука = 600 Б), порог
    // «полон» — 75 % потолка (465 Б): резерв звука урезан до порога, норма видео 0.
    let bench = Bench::new(620, 465, 500);
    bench.shelve(VIDEO, &[0, 200, 400, 600, 800], 200, 100);
    bench.shelve(AUDIO, &[0, 400, 800, 1200, 1600], 400, 20);
    bench.play_at(1_000, 400);

    // Страница несёт ещё 100 Б, не слушая «полон»: уходит сыгранное видео (0–200
    // и 200–400 кончились не позже позиции), звук не тронут.
    let first = bench
        .buffer
        .charge(Bytes::from(vec![0; 100]))
        .expect("room");
    assert_eq!(bench.starts_ms(VIDEO), [200, 400, 600, 800]);
    let second = bench
        .buffer
        .charge(Bytes::from(vec![0; 100]))
        .expect("room");
    assert_eq!(bench.starts_ms(VIDEO), [400, 600, 800]);
    assert_eq!(bench.starts_ms(AUDIO), [0, 400, 800, 1200, 1600]);
    // Дальше у видео только играющий кусок, а у звука — сыгранное (0–400):
    // у потолка выселяется и оно (страница игнорирует «полон»).
    let third = bench.buffer.charge(Bytes::from(vec![0; 40])).expect("room");
    assert_eq!(bench.starts_ms(AUDIO), [400, 800, 1200, 1600]);
    // Больше честно нечего: играющий кусок обеих полос — отказ, а не потеря.
    assert_eq!(
        bench.buffer.charge(Bytes::from(vec![0; 100])).map(drop),
        Err(PushedStreamError::IntakeLimitExceeded { limit: 620 })
    );
    drop((first, second, third));
}

/// Находка ревью (пакет F): старое место ещё держит труба старого эпизода, когда
/// перемотка уходит вне полки, — предохранитель в самой перемотке его не трогает, и
/// поток «полон»; честная страница при «полон» молчит. Труба отпускает байты позже
/// (поток эпизода выходит сам), а позиции старого места, пока перемотка ждёт цель,
/// отвергаются. Предохранитель обязан пройти и на отвергнутой позиции: иначе «полон»
/// держался бы до срока перемотки, хотя выкинуть уже можно.
#[test]
fn memory_freed_by_the_old_episode_is_relieved_on_a_rejected_position_while_the_seek_waits() {
    let bench = Bench::new(10_000, 1_000, 200);
    // Видео 0–2 с: 10 × 100 Б = порог «полон», всё ещё в трубе старого эпизода.
    let pipe = bench.shelve_held_by_a_pipe(
        VIDEO,
        &[0, 200, 400, 600, 800, 1000, 1200, 1400, 1600, 1800],
        200,
        100,
    );
    bench.play_at(2_000, 1_900);

    drop(
        bench
            .buffer
            .seek(std::time::Duration::from_secs(5))
            .expect("seek off the shelf")
            .stale,
    );
    assert_eq!(
        bench.buffer.buffer_status().intake,
        BufferIntake::Full,
        "the old episode still holds the old place"
    );

    // Поток старого эпизода вышел и отпустил байты; приложение сообщает позицию старого
    // места — её отвергают, но память освобождается.
    drop(pipe);
    bench.buffer.note_playback_position_at(
        std::time::Duration::from_millis(1_950),
        std::time::Instant::now(),
    );

    assert_eq!(bench.buffer.buffer_status().intake, BufferIntake::Accepting);
    assert_eq!(
        bench.buffer.lock().reported_position,
        Some(at(5_000)),
        "the stale position is still not believed"
    );
}
