//! Тесты упорядоченной передачи кадров показа (cpu-parity, сессия 06a).
//!
//! Две группы:
//! - контракт передачи: порядок, переполнение, новая последовательность, очистка, `Busy`,
//!   освобождение lease-ов;
//! - модель каденции: worker публикует в ритме видео с дрожанием часов, UI забирает строго
//!   по vsync (худший случай XWayland/FIFO). Та же модель на прежнем слоте «последний
//!   побеждает» обязана терять кадры — это доказывает, что сценарий действительно ловит
//!   совпадение фаз, а не проходит вхолостую.

use std::sync::Arc;
use std::time::Duration;

use codec_core::VideoColorMetadata;
use crossbeam_channel::{Receiver, Sender, unbounded};
use video_core::{DecodedFrame, FrameResourceHandle};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::{VideoFrameLease, VideoFrameLeaseConfig};

use super::*;
use crate::render_lease_bridge::{
    LatestPresentFrameHandoff, RenderLeaseRelease, RenderLeaseReleaseSink,
};

/// Поколение рендера и декодера по умолчанию для кадров одной последовательности.
const DEFAULT_GENERATION: u64 = 1;

/// Строит lease кадра с заданными поколениями и PTS; release ack уходит в `release_tx`.
fn frame_lease(
    render_generation: u64,
    decoded_generation: u64,
    pts: Duration,
    release_tx: &Sender<RenderLeaseRelease>,
) -> VideoFrameLease {
    let pts_micros = u64::try_from(pts.as_micros()).unwrap_or(u64::MAX);
    let decoded_frame = DecodedFrame {
        generation: decoded_generation,
        pts,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 64,
        height: 36,
        render_width: 64,
        render_height: 36,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        // Уникальный handle на PTS: identity кадров одной последовательности различаются.
        resource_handle: FrameResourceHandle(pts_micros),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    };
    VideoFrameLease::new(VideoFrameLeaseConfig::new(
        render_generation,
        decoded_frame,
        Arc::new(RenderLeaseReleaseSink::new(release_tx.clone())),
    ))
}

/// Кадр последовательности по умолчанию с PTS в миллисекундах.
fn frame_at_ms(pts_ms: u64, release_tx: &Sender<RenderLeaseRelease>) -> VideoFrameLease {
    frame_lease(
        DEFAULT_GENERATION,
        DEFAULT_GENERATION,
        Duration::from_millis(pts_ms),
        release_tx,
    )
}

/// PTS забранного кадра; `None` — передача пуста или занята.
fn take_pts(handoff: &OrderedPresentFrameHandoff) -> Option<Duration> {
    match handoff.try_take_next() {
        LatestPresentFrameAcquire::Acquired(frame) => Some(frame.decoded_frame().pts),
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => None,
    }
}

/// PTS освобождённых кадров в порядке release ack-ов.
fn released_pts(release_rx: &Receiver<RenderLeaseRelease>) -> Vec<FrameResourceHandle> {
    release_rx
        .try_iter()
        .map(|release| release.resource_handle)
        .collect()
}

#[test]
fn empty_handoff_has_no_frame_for_render_or_worker() {
    let handoff = OrderedPresentFrameHandoff::new();

    assert!(matches!(
        handoff.try_take_next(),
        LatestPresentFrameAcquire::Empty
    ));
    assert_eq!(
        handoff.try_next_identity(),
        PresentFrameIdentityProbe::Known(None)
    );
    assert_eq!(handoff.newest_identity(), None);
}

/// Главный сценарий дефекта: две публикации между двумя чтениями UI — оба кадра доходят
/// до UI по порядку, ни один не перезаписан.
#[test]
fn two_publications_between_reads_reach_render_in_order() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();

    assert_eq!(
        handoff.publish(frame_at_ms(0, &release_tx)),
        OrderedPresentFramePublish::Queued
    );
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(0)));
    assert_eq!(
        handoff.publish(frame_at_ms(16, &release_tx)),
        OrderedPresentFramePublish::Queued
    );
    assert_eq!(
        handoff.publish(frame_at_ms(33, &release_tx)),
        OrderedPresentFramePublish::Queued
    );

    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(16)));
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(33)));
    // Новых кадров нет: повторное чтение возвращает уже забранный, очередь не «перескакивает».
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(33)));
    // Ни один кадр не освобождён непоказанным: 0 и 16 отпущены слотом только после того,
    // как UI забрал следующий (их держит кэш UI, пока показывает).
    assert_eq!(
        released_pts(&release_rx),
        vec![FrameResourceHandle(0), FrameResourceHandle(16_000)]
    );
}

/// Identity для UI показывает следующий кадр, а для worker-а — самый свежий; чтение
/// identity очередь не продвигает.
#[test]
fn identity_probes_report_next_for_render_and_newest_for_worker() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, _release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(0, &release_tx));
    let _ = take_pts(&handoff);
    let _ = handoff.publish(frame_at_ms(16, &release_tx));
    let _ = handoff.publish(frame_at_ms(33, &release_tx));

    let next_pts = match handoff.try_next_identity() {
        PresentFrameIdentityProbe::Known(Some(identity)) => identity.pts(),
        other => panic!("next identity must be known, got {other:?}"),
    };
    assert_eq!(next_pts, Duration::from_millis(16));
    assert_eq!(
        handoff.newest_identity().map(|identity| identity.pts()),
        Some(Duration::from_millis(33))
    );
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(16)));
}

/// UI отстал больше чем на ёмкость очереди: сбрасывается самый старый незабранный кадр,
/// это видно в итоге публикации, а порядок оставшихся сохраняется.
#[test]
fn overflow_drops_only_the_oldest_untaken_frame() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();

    let _ = handoff.publish(frame_at_ms(0, &release_tx));
    let _ = handoff.publish(frame_at_ms(16, &release_tx));
    assert_eq!(
        handoff.publish(frame_at_ms(33, &release_tx)),
        OrderedPresentFramePublish::DroppedOldestUntaken
    );

    assert_eq!(released_pts(&release_rx), vec![FrameResourceHandle(0)]);
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(16)));
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(33)));
}

/// Seek/смена поколения: незабранные кадры старой последовательности не показываются,
/// UI сразу получает кадр новой.
#[test]
fn new_generation_discards_untaken_frames_of_the_old_sequence() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(0, &release_tx));
    let _ = take_pts(&handoff);
    let _ = handoff.publish(frame_at_ms(16, &release_tx));
    let _ = handoff.publish(frame_at_ms(33, &release_tx));

    let after_seek = frame_lease(
        DEFAULT_GENERATION + 1,
        DEFAULT_GENERATION,
        Duration::from_millis(5_000),
        &release_tx,
    );
    assert_eq!(
        handoff.publish(after_seek),
        OrderedPresentFramePublish::RestartedSequence {
            discarded_untaken_frames: 2
        }
    );

    assert_eq!(
        released_pts(&release_rx),
        vec![FrameResourceHandle(16_000), FrameResourceHandle(33_000)]
    );
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(5_000)));
}

/// Шаг назад по PTS в том же поколении (повторное открытие, обратный seek без смены
/// generation) тоже начинает новую последовательность, а не встаёт в очередь «после».
#[test]
fn backwards_pts_restarts_the_sequence_even_in_the_same_generation() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, _release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(1_000, &release_tx));

    assert_eq!(
        handoff.publish(frame_at_ms(500, &release_tx)),
        OrderedPresentFramePublish::RestartedSequence {
            discarded_untaken_frames: 1
        }
    );
    let changed_decoder = frame_lease(
        DEFAULT_GENERATION,
        DEFAULT_GENERATION + 1,
        Duration::from_millis(600),
        &release_tx,
    );
    assert_eq!(
        handoff.publish(changed_decoder),
        OrderedPresentFramePublish::RestartedSequence {
            discarded_untaken_frames: 1
        }
    );
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(600)));
}

/// Новая последовательность, когда незабранных нет: ничего не отбрасывается, а забранный UI
/// кадр остаётся в слоте до взятия нового.
#[test]
fn restart_without_untaken_frames_keeps_the_taken_frame_until_replaced() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(1_000, &release_tx));
    let _ = take_pts(&handoff);

    assert_eq!(
        handoff.publish(frame_at_ms(0, &release_tx)),
        OrderedPresentFramePublish::RestartedSequence {
            discarded_untaken_frames: 0
        }
    );
    assert!(released_pts(&release_rx).is_empty());
    assert_eq!(take_pts(&handoff), Some(Duration::ZERO));
    assert_eq!(
        released_pts(&release_rx),
        vec![FrameResourceHandle(1_000_000)]
    );
}

/// Очистка отпускает и забранный, и незабранные кадры; передача снова пуста.
#[test]
fn clear_releases_every_held_frame() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(0, &release_tx));
    let _ = take_pts(&handoff);
    let _ = handoff.publish(frame_at_ms(16, &release_tx));

    handoff.clear();

    assert_eq!(
        released_pts(&release_rx),
        vec![FrameResourceHandle(0), FrameResourceHandle(16_000)]
    );
    assert!(matches!(
        handoff.try_take_next(),
        LatestPresentFrameAcquire::Empty
    ));
    assert_eq!(handoff.newest_identity(), None);
}

/// Пока worker держит mutex, UI получает `Busy` и не продвигает очередь.
#[test]
fn busy_handoff_does_not_advance_the_queue() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, _release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(0, &release_tx));
    let _ = handoff.publish(frame_at_ms(16, &release_tx));

    {
        let _worker_guard = handoff.frames_guard();
        assert!(matches!(
            handoff.try_take_next(),
            LatestPresentFrameAcquire::Busy
        ));
        assert_eq!(handoff.try_next_identity(), PresentFrameIdentityProbe::Busy);
    }

    assert_eq!(take_pts(&handoff), Some(Duration::ZERO));
    assert_eq!(take_pts(&handoff), Some(Duration::from_millis(16)));
}

/// Слот держит не больше `1 + UNTAKEN_PRESENT_FRAME_CAPACITY` кадров при любом отставании UI.
#[test]
fn held_frames_stay_bounded_when_render_never_reads() {
    let handoff = OrderedPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let _ = handoff.publish(frame_at_ms(0, &release_tx));
    let _ = take_pts(&handoff);
    let published_frames: u64 = 100;
    for frame_index in 1..=published_frames {
        let _ = handoff.publish(frame_at_ms(frame_index * 16, &release_tx));
    }

    let released = released_pts(&release_rx).len() as u64;
    let held = published_frames + 1 - released;
    assert_eq!(held, 1 + UNTAKEN_PRESENT_FRAME_CAPACITY as u64);
}

// ----- Модель каденции -----

/// Один сценарий каденции: частота видео и дисплея, длительность, дрожание.
#[derive(Clone, Copy)]
struct CadenceScenario {
    /// Частота кадров источника, кадров/с.
    source_fps: f64,
    /// Частота обновления дисплея, Гц.
    display_hz: f64,
    /// Сколько секунд моделировать.
    seconds: f64,
    /// Сдвиг сетки публикаций относительно сетки vsync, мс (0 — худшая фаза).
    publish_phase_ms: f64,
    /// Полуширина дрожания момента публикации (часы звука, планировщик), мс.
    publish_jitter_ms: f64,
}

/// Что увидел UI за прогон модели.
#[derive(Debug)]
struct CadenceOutcome {
    /// Сколько кадров опубликовал worker.
    published_frames: usize,
    /// Сколько опубликованных кадров UI так и не получил.
    lost_frames: usize,
    /// Пары «повтор кадра, а в следующие 3 vsync — пропуск», симптом дефекта D3.
    repeat_then_skip_pairs: usize,
}

/// Общий интерфейс двух передач для модели: публикация и взятие по vsync.
trait ModeledHandoff {
    fn publish_frame(&self, frame: VideoFrameLease);
    fn take_for_vsync(&self) -> Option<Duration>;
}

impl ModeledHandoff for OrderedPresentFrameHandoff {
    fn publish_frame(&self, frame: VideoFrameLease) {
        let _ = self.publish(frame);
    }

    fn take_for_vsync(&self) -> Option<Duration> {
        take_pts(self)
    }
}

impl ModeledHandoff for LatestPresentFrameHandoff {
    fn publish_frame(&self, frame: VideoFrameLease) {
        self.publish(Some(frame));
    }

    fn take_for_vsync(&self) -> Option<Duration> {
        match self.try_clone_latest() {
            LatestPresentFrameAcquire::Acquired(frame) => Some(frame.decoded_frame().pts),
            LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => None,
        }
    }
}

/// Детерминированное «дрожание» в `[-1, 1]` (линейный конгруэнтный генератор):
/// тест повторяем, а распределение достаточно ровное для моделирования часов.
struct DeterministicJitter(u64);

impl DeterministicJitter {
    fn next_unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let top_bits = (self.0 >> 11) as f64 / (1u64 << 53) as f64;
        top_bits * 2.0 - 1.0
    }
}

/// Прогоняет модель: события публикации и vsync в порядке времени, UI забирает кадр на
/// каждом vsync (главный поток заблокирован в acquire swapchain, как под XWayland/FIFO).
fn run_cadence(handoff: &dyn ModeledHandoff, scenario: CadenceScenario) -> CadenceOutcome {
    let (release_tx, _release_rx) = unbounded();
    let frame_interval_ms = 1_000.0 / scenario.source_fps;
    let vsync_interval_ms = 1_000.0 / scenario.display_hz;
    let duration_ms = scenario.seconds * 1_000.0;
    let mut jitter = DeterministicJitter(0x06A0_2026);

    let mut next_frame_index: u64 = 0;
    let mut next_publish_at = scenario.publish_phase_ms;
    let mut next_vsync_at = vsync_interval_ms;
    let mut shown_per_vsync: Vec<Option<Duration>> = Vec::new();
    let mut published_pts: Vec<Duration> = Vec::new();

    while next_vsync_at < duration_ms {
        if next_publish_at <= next_vsync_at {
            let pts =
                Duration::from_secs_f64(next_frame_index as f64 * frame_interval_ms / 1_000.0);
            handoff.publish_frame(frame_lease(
                DEFAULT_GENERATION,
                DEFAULT_GENERATION,
                pts,
                &release_tx,
            ));
            published_pts.push(pts);
            next_frame_index += 1;
            next_publish_at = scenario.publish_phase_ms
                + next_frame_index as f64 * frame_interval_ms
                + jitter.next_unit() * scenario.publish_jitter_ms;
        } else {
            shown_per_vsync.push(handoff.take_for_vsync());
            next_vsync_at += vsync_interval_ms;
        }
    }
    summarize_cadence(&published_pts, &shown_per_vsync)
}

/// Считает потерянные кадры и пары «повтор → пропуск» по последовательности показов.
fn summarize_cadence(
    published_pts: &[Duration],
    shown_per_vsync: &[Option<Duration>],
) -> CadenceOutcome {
    let index_of = |pts: Duration| published_pts.iter().position(|published| *published == pts);
    let shown_indices: Vec<Option<usize>> = shown_per_vsync
        .iter()
        .map(|shown| shown.and_then(index_of))
        .collect();
    let last_shown = shown_indices.iter().flatten().max().copied().unwrap_or(0);
    let mut was_shown = vec![false; last_shown + 1];
    for index in shown_indices.iter().flatten() {
        was_shown[*index] = true;
    }
    // Кадры после последнего показанного ещё «в пути» к UI — не потери.
    let lost_frames = was_shown.iter().filter(|shown| !**shown).count();

    let mut repeat_then_skip_pairs = 0;
    for vsync in 1..shown_indices.len() {
        let (Some(previous), Some(current)) = (shown_indices[vsync - 1], shown_indices[vsync])
        else {
            continue;
        };
        if current != previous {
            continue;
        }
        let skip_follows = (vsync + 1..(vsync + 4).min(shown_indices.len())).any(|later| {
            matches!(
                (shown_indices[later - 1], shown_indices[later]),
                (Some(before), Some(after)) if after > before + 1
            )
        });
        if skip_follows {
            repeat_then_skip_pairs += 1;
        }
    }
    CadenceOutcome {
        published_frames: published_pts.len(),
        lost_frames,
        repeat_then_skip_pairs,
    }
}

/// Сколько кадров неизбежно теряется, если видео быстрее дисплея: разница частот × время
/// плюс один кадр на округление краёв окна.
fn unavoidable_losses(scenario: CadenceScenario) -> usize {
    let excess = (scenario.source_fps - scenario.display_hz).max(0.0) * scenario.seconds;
    excess.ceil() as usize + 1
}

/// 60 fps на дисплее 59,96 Гц (реальный монитор сессии 06) в худшей фазе с дрожанием
/// часов ±0,5 мс: без пар «повтор + пропуск», потери — только неизбежные; прежний слот на
/// том же сценарии теряет кадры (сценарий действительно ловит совпадение фаз).
#[test]
fn cadence_60fps_on_5996hz_has_no_repeat_skip_pairs() {
    let scenario = CadenceScenario {
        source_fps: 60.0,
        display_hz: 59.961,
        seconds: 120.0,
        publish_phase_ms: 0.0,
        publish_jitter_ms: 0.5,
    };

    let ordered = run_cadence(&OrderedPresentFrameHandoff::new(), scenario);
    let latest_wins = run_cadence(&LatestPresentFrameHandoff::new(), scenario);

    assert!(ordered.published_frames > 7_000, "{ordered:?}");
    assert_eq!(ordered.repeat_then_skip_pairs, 0, "{ordered:?}");
    assert!(
        ordered.lost_frames <= unavoidable_losses(scenario),
        "{ordered:?}"
    );
    assert!(
        latest_wins.lost_frames > 4 * unavoidable_losses(scenario)
            && latest_wins.repeat_then_skip_pairs > 0,
        "the model must reproduce the latest-wins defect, got {latest_wins:?}"
    );
}

/// Ровно 60 на 60 Гц с совпавшей фазой — постоянное столкновение: ни одной потери.
#[test]
fn cadence_60fps_on_60hz_in_colliding_phase_loses_nothing() {
    let scenario = CadenceScenario {
        source_fps: 60.0,
        display_hz: 60.0,
        seconds: 60.0,
        publish_phase_ms: 0.0,
        publish_jitter_ms: 0.5,
    };

    let ordered = run_cadence(&OrderedPresentFrameHandoff::new(), scenario);
    let latest_wins = run_cadence(&LatestPresentFrameHandoff::new(), scenario);

    assert_eq!(ordered.lost_frames, 0, "{ordered:?}");
    assert_eq!(ordered.repeat_then_skip_pairs, 0, "{ordered:?}");
    assert!(latest_wins.lost_frames > 0, "{latest_wins:?}");
}

/// Видео медленнее дисплея (50, 30, 24 fps на 60 Гц) в любой фазе: каждый кадр показан,
/// повторы — только естественные (pulldown), без пропусков.
#[test]
fn cadence_slower_sources_on_60hz_show_every_frame() {
    for source_fps in [50.0, 30.0, 24.0] {
        for publish_phase_ms in [0.0, 4.0, 8.3, 16.0] {
            let scenario = CadenceScenario {
                source_fps,
                display_hz: 60.0,
                seconds: 60.0,
                publish_phase_ms,
                publish_jitter_ms: 0.5,
            };
            let outcome = run_cadence(&OrderedPresentFrameHandoff::new(), scenario);
            assert_eq!(
                outcome.lost_frames, 0,
                "{source_fps} fps phase {publish_phase_ms}: {outcome:?}"
            );
            assert_eq!(
                outcome.repeat_then_skip_pairs, 0,
                "{source_fps} fps phase {publish_phase_ms}: {outcome:?}"
            );
        }
    }
}

/// Хорошая фаза (публикация посреди интервала vsync): поведение как у прежнего слота —
/// потери только неизбежные, очередь не вносит своих.
#[test]
fn cadence_in_non_colliding_phase_matches_the_unavoidable_bound() {
    let scenario = CadenceScenario {
        source_fps: 60.0,
        display_hz: 59.961,
        seconds: 20.0,
        publish_phase_ms: 8.0,
        publish_jitter_ms: 0.5,
    };

    let ordered = run_cadence(&OrderedPresentFrameHandoff::new(), scenario);

    assert!(
        ordered.lost_frames <= unavoidable_losses(scenario),
        "{ordered:?}"
    );
    assert_eq!(ordered.repeat_then_skip_pairs, 0, "{ordered:?}");
}
