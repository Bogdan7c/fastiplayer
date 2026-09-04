//! Focused P4 proof на canonical PIFF corpus и injected S28A adapters.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use demux_api::{
    CompositeComponentLeadPolicy, DemuxContainerId, DemuxHints, DemuxInput, DemuxRegistry,
    DemuxSniffBudget, ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome,
    ProgressiveDemuxBufferLimits, ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, Demuxer, TrackKind};
use symphonia_demux::{
    DemuxerOptions, PresentationWindowOrderedIsoMp4Demuxer, SymphoniaDemuxFactory,
};

use crate::demux::seek::{SmoothSeekPlan, smooth_ticks_to_duration};
use crate::demux::{
    SmoothAudioDemuxOpenRequest, SmoothIsoBmffDemuxFactory, SmoothVideoDemuxOpenRequest,
    SmoothVodDemuxPolicy,
};
use crate::source::tests::{FixtureOrigin, fragment_policy, prepare, selection};

/// Production-shaped injected factory поверх existing S28A registrations.
pub(crate) struct TestSymphoniaFactory;

impl SmoothIsoBmffDemuxFactory for TestSymphoniaFactory {
    /// Открывает ordinary ordered video через registry content proof.
    fn open_video(
        &self,
        request: SmoothVideoDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        let parts = request.into_parts();
        let mut registry = DemuxRegistry::new();
        registry
            .register(Box::new(SymphoniaDemuxFactory::new(
                DemuxerOptions::default(),
            )?))
            .context("register Symphonia test factory")?;
        registry
            .open_required_container(
                DemuxInput::ordered_segments(parts.source),
                DemuxHints::none(),
                parts.sniff_budget,
                parts.cancellation,
                DemuxContainerId::new("iso-bmff")?,
            )
            .context("open Smooth test video")
    }

    /// Открывает provenance-aware audio adapter из F3A.
    fn open_audio(
        &self,
        request: SmoothAudioDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        let parts = request.into_parts();
        Ok(Box::new(PresentationWindowOrderedIsoMp4Demuxer::new(
            parts.source,
            parts.cancellation,
            parts.sniff_budget,
            DemuxerOptions::default(),
        )?))
    }
}

/// Production adapter с наблюдением overlap двух независимых component open-ов.
#[derive(Default)]
struct OverlappingSymphoniaFactory {
    /// Число adapter open-ов, которые прямо сейчас находятся внутри readiness.
    active_opens: AtomicUsize,
    /// Максимальное одновременно наблюдавшееся число adapter open-ов.
    maximum_active_opens: AtomicUsize,
}

impl OverlappingSymphoniaFactory {
    /// Выполняет настоящий adapter open, оставляя короткое окно для детерминированного overlap.
    fn observe_open<Output>(
        &self,
        open: impl FnOnce() -> anyhow::Result<Output>,
    ) -> anyhow::Result<Output> {
        let active = self.active_opens.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active_opens
            .fetch_max(active, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(25));
        let outcome = open();
        self.active_opens.fetch_sub(1, Ordering::SeqCst);
        outcome
    }

    /// Возвращает доказанный максимум concurrency для startup либо seek транзакции.
    fn maximum_active_opens(&self) -> usize {
        self.maximum_active_opens.load(Ordering::SeqCst)
    }

    /// Начинает отдельное наблюдение следующей операции.
    fn reset_observation(&self) {
        assert_eq!(self.active_opens.load(Ordering::SeqCst), 0);
        self.maximum_active_opens.store(0, Ordering::SeqCst);
    }
}

impl SmoothIsoBmffDemuxFactory for OverlappingSymphoniaFactory {
    /// Открывает настоящий video adapter и учитывает его concurrency interval.
    fn open_video(
        &self,
        request: SmoothVideoDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        self.observe_open(|| TestSymphoniaFactory.open_video(request))
    }

    /// Открывает настоящий audio adapter и учитывает его concurrency interval.
    fn open_audio(
        &self,
        request: SmoothAudioDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        self.observe_open(|| TestSymphoniaFactory.open_audio(request))
    }
}

/// Factory probe, который доказывает worker thread без fragment fetch.
struct ThreadProbeFactory {
    caller_thread: thread::ThreadId,
    observed_thread: Arc<Mutex<Option<thread::ThreadId>>>,
}

/// Делает ровно второй audio open неуспешным для atomic replacement proof.
struct FailSecondAudioFactory {
    audio_open_count: AtomicUsize,
}

impl SmoothIsoBmffDemuxFactory for FailSecondAudioFactory {
    /// Video всегда использует production-shaped test adapter.
    fn open_video(
        &self,
        request: SmoothVideoDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        TestSymphoniaFactory.open_video(request)
    }

    /// Второй вызов соответствует первой seek replacement transaction.
    fn open_audio(
        &self,
        request: SmoothAudioDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        let call = self.audio_open_count.fetch_add(1, Ordering::SeqCst);
        if call == 1 {
            anyhow::bail!("intentional replacement audio failure");
        }
        TestSymphoniaFactory.open_audio(request)
    }
}

impl SmoothIsoBmffDemuxFactory for ThreadProbeFactory {
    /// Записывает worker identity и завершает open фиксированной ошибкой.
    fn open_video(
        &self,
        _request: SmoothVideoDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        *self.observed_thread.lock().expect("thread probe lock") = Some(thread::current().id());
        anyhow::bail!("intentional worker probe failure")
    }

    /// Параллельная audio ветка тоже возвращает намеренную probe-ошибку.
    fn open_audio(
        &self,
        _request: SmoothAudioDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>> {
        anyhow::bail!("intentional audio worker probe failure")
    }
}

/// Собирает explicit bounded P4 policy.
pub(crate) fn demux_policy() -> SmoothVodDemuxPolicy {
    SmoothVodDemuxPolicy::new(
        DemuxSniffBudget::new(
            NonZeroUsize::new(256 * 1_024).expect("sniff bytes"),
            NonZeroUsize::new(2).expect("sniff segments"),
            Duration::from_secs(2),
        )
        .expect("sniff policy"),
        CompositeComponentLeadPolicy::new(
            Duration::from_secs(2),
            NonZeroUsize::new(32).expect("bootstrap packets"),
            NonZeroUsize::new(4 * 1_024 * 1_024).expect("bootstrap bytes"),
        )
        .expect("lead policy"),
        ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(64).expect("pending events"),
            NonZeroUsize::new(8 * 1_024 * 1_024).expect("pending encoded bytes"),
        ),
        DemuxRetryHint::new(Duration::from_millis(2)).expect("retry hint"),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(8).expect("outstanding seek receipts")),
    )
}

/// Готовит canonical high-video/audio P3 sources.
fn selected_sources(origin: &FixtureOrigin) -> crate::SmoothSelectedFragmentSources {
    let prepared = prepare(origin);
    let exact_selection = selection(&prepared, 1_501_000);
    prepared
        .into_selected_fragment_sources(exact_selection, fragment_policy())
        .expect("selected canonical sources")
}

#[test]
fn demux_adapter_open_runs_off_caller_thread() {
    let origin = FixtureOrigin::start();
    let sources = selected_sources(&origin);
    let caller_thread = thread::current().id();
    let observed_thread = Arc::new(Mutex::new(None));
    let factory = Arc::new(ThreadProbeFactory {
        caller_thread,
        observed_thread: Arc::clone(&observed_thread),
    });

    let mut demuxer = sources
        .into_progressive_demuxer(factory.clone(), demux_policy())
        .expect("progressive runtime")
        .into_demuxer();
    let deadline = Instant::now() + Duration::from_secs(2);
    while observed_thread.lock().expect("thread probe lock").is_none() {
        assert!(Instant::now() < deadline, "worker factory did not run");
        let _ = demuxer.next_event();
        thread::sleep(Duration::from_millis(2));
    }
    let worker_thread = observed_thread
        .lock()
        .expect("thread probe lock")
        .expect("worker thread");

    assert_ne!(worker_thread, factory.caller_thread);
}

#[test]
fn canonical_sources_publish_stable_av_tracks_and_manifest_duration() {
    let origin = FixtureOrigin::start();
    let sources = selected_sources(&origin);
    let expected_duration = sources.aligned_span().end_exclusive();
    let factory = Arc::new(OverlappingSymphoniaFactory::default());
    let result = sources
        .into_progressive_demuxer(factory.clone(), demux_policy())
        .expect("progressive Smooth runtime");

    assert_eq!(
        result.duration().as_secs(),
        expected_duration.ticks() / 10_000_000
    );
    let mut demuxer = result.into_demuxer();
    let deadline = Instant::now() + Duration::from_secs(5);
    let update = loop {
        assert!(Instant::now() < deadline, "Smooth demux readiness timeout");
        match demuxer.next_event().expect("Smooth progressive event") {
            DemuxReadEvent::TracksChanged(update) => break update,
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                thread::sleep(hint.retry_after());
            }
            other => panic!("tracks must be first progressive publication: {other:?}"),
        }
    };

    assert_eq!(update.duration, Some(Duration::from_secs(734)));
    assert_eq!(update.tracks.len(), 2);
    assert_eq!(update.tracks[0].kind, TrackKind::Video);
    assert_eq!(update.tracks[1].kind, TrackKind::Audio);
    assert_eq!(
        factory.maximum_active_opens(),
        2,
        "Smooth startup обязан готовить независимые A/V adapters параллельно"
    );

    let public_video_track_id = update.tracks[0].id;
    let public_audio_track_id = update.tracks[1].id;
    let mut second_video_fragment_observed = false;
    let mut second_audio_fragment_observed = false;
    let mut video_packet_count = 0_usize;
    let mut audio_packet_count = 0_usize;
    while !second_video_fragment_observed || !second_audio_fragment_observed {
        assert!(
            Instant::now() < deadline,
            "Smooth A/V packet interleave did not cross the first fragment boundary"
        );
        let event = demuxer.next_event().unwrap_or_else(|error| {
            panic!(
                "Smooth progressive packet event failed after {video_packet_count} video and {audio_packet_count} audio packets: {error:#}"
            )
        });
        match event {
            DemuxReadEvent::Packet(packet) if packet.track_id == public_video_track_id => {
                video_packet_count += 1;
                second_video_fragment_observed |= packet.pts >= Duration::from_secs(4);
            }
            DemuxReadEvent::Packet(packet) if packet.track_id == public_audio_track_id => {
                audio_packet_count += 1;
                second_audio_fragment_observed |= packet.pts >= Duration::from_secs(4);
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                thread::sleep(hint.retry_after());
            }
            other => panic!("unexpected event before second A/V fragments: {other:?}"),
        }
    }
}

#[test]
fn receipted_seek_rebuilds_both_axes_at_exact_manifest_anchors() {
    let origin = FixtureOrigin::start();
    let sources = selected_sources(&origin);
    let factory = Arc::new(OverlappingSymphoniaFactory::default());
    let result = sources
        .into_progressive_demuxer(factory.clone(), demux_policy())
        .expect("seekable Smooth runtime");
    let handle = result.async_seek_handle();
    let mut demuxer = result.into_demuxer();
    wait_for_tracks_changed(demuxer.as_mut());
    factory.reset_observation();
    let fence = ProgressiveSeekFence {
        runtime_generation: handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    handle
        .enqueue(
            fence,
            DemuxSeekRequest::accurate(Duration::from_millis(4_100)),
        )
        .expect("enqueue Smooth seek");
    let receipt = wait_for_receipt(&handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(seek_result) = receipt.outcome else {
        panic!(
            "Smooth seek must succeed: {:?}; requests={:?}",
            receipt.outcome,
            origin.request_targets()
        );
    };

    assert_eq!(
        seek_result.requested_position.as_duration(),
        Duration::from_millis(4_100)
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(4)
    );
    assert_eq!(
        factory.maximum_active_opens(),
        2,
        "Smooth seek обязан готовить независимые A/V replacements параллельно"
    );
    let requests = origin.request_targets();
    let replacement_requests = &requests[requests.len().saturating_sub(2)..];
    assert!(
        replacement_requests.iter().any(|target| {
            target == "/media/QualityLevels(1501000)/Fragments(video_eng=40000000)"
        })
    );
    assert!(
        replacement_requests.iter().any(|target| {
            target == "/media/QualityLevels(64008)/Fragments(audio_eng=39680000)"
        })
    );
}

#[test]
fn seek_planning_is_pure_exact_and_rejects_target_after_duration() {
    let origin = FixtureOrigin::start();
    let sources = selected_sources(&origin);
    let root_end = sources.aligned_span().end_exclusive();
    let duration = smooth_ticks_to_duration(root_end.ticks(), root_end.timescale().get());
    let (_, _, _, _, _, _, source_factory) = sources.into_demux_parts();

    let plan = SmoothSeekPlan::for_request(
        &source_factory,
        DemuxSeekRequest::accurate(Duration::from_millis(4_100)),
        duration,
    )
    .expect("pure seek plan");
    assert_eq!(plan.video_fragment_index, 1);
    assert_eq!(plan.audio_fragment_index, 1);
    assert_eq!(
        plan.result.actual_position.as_duration(),
        Duration::from_secs(4)
    );
    assert_eq!(origin.request_count(), 1);

    assert!(
        SmoothSeekPlan::for_request(
            &source_factory,
            DemuxSeekRequest::accurate(duration + Duration::from_nanos(1)),
            duration,
        )
        .is_err()
    );
    assert_eq!(origin.request_count(), 1);
}

#[test]
fn failed_audio_replacement_keeps_worker_seekable_for_next_transaction() {
    let origin = FixtureOrigin::start();
    let sources = selected_sources(&origin);
    let result = sources
        .into_progressive_demuxer(
            Arc::new(FailSecondAudioFactory {
                audio_open_count: AtomicUsize::new(0),
            }),
            demux_policy(),
        )
        .expect("transactional Smooth runtime");
    let handle = result.async_seek_handle();
    let mut demuxer = result.into_demuxer();
    wait_for_tracks_changed(demuxer.as_mut());
    let first_fence = ProgressiveSeekFence {
        runtime_generation: handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    handle
        .enqueue(
            first_fence,
            DemuxSeekRequest::accurate(Duration::from_millis(4_100)),
        )
        .expect("enqueue failing seek");
    let failed = wait_for_receipt(&handle);
    assert_eq!(failed.outcome, ProgressiveAsyncSeekOutcome::Failed);

    let second_fence = ProgressiveSeekFence {
        runtime_generation: handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(2),
    };
    handle
        .enqueue(
            second_fence,
            DemuxSeekRequest::accurate(Duration::from_millis(100)),
        )
        .expect("enqueue recovery seek");
    let recovered = wait_for_receipt(&handle);
    assert!(matches!(
        recovered.outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(_)
    ));
}

/// Ждёт bounded terminal receipt без чтения player-facing packet queue.
fn wait_for_receipt(
    handle: &demux_api::ProgressiveAsyncSeekHandle,
) -> demux_api::ProgressiveAsyncSeekReceipt {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(receipt) = handle.poll_receipt() {
            return receipt;
        }
        assert!(Instant::now() < deadline, "Smooth seek receipt timeout");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Дожидается initial readiness до публикации seek control пользователю.
fn wait_for_tracks_changed(demuxer: &mut dyn Demuxer) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "Smooth track readiness timeout");
        match demuxer.next_event().expect("Smooth readiness event") {
            DemuxReadEvent::TracksChanged(_) => return,
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                thread::sleep(hint.retry_after());
            }
            other => panic!("tracks must be first readiness event: {other:?}"),
        }
    }
}
