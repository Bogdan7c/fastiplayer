//! N06 functional regressions direct HTTP/FTP ingress-а без extractor process-а.

use std::io;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use audio::decoder::EncodedAudioPacket;
use audio::{AudioDecoderFactory, ProductionAudioDecoderFactory};
use fastiplayer_config::AppConfig;
use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, TrackKind};
use service_ytdlp::{ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpExtractorAdapter};
use source_core::CancellationToken;
use symphonia_demux::DemuxSeekability;
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    EndpointExpiryObserver, EndpointExpiryReason, EndpointExpiryResourceKind, EndpointExpirySignal,
    MediaComponentIdentity, MediaComponentRole, SourceGeneration,
};

use super::ftp_vorbis::FtpVorbisOrigin;
use super::vorbis;
use super::{DEMUX_EVENT_DEADLINE, FixtureOriginResponse, RangeFixtureOrigin, audio_packet_timing};

/// HTTP Range owner читает generated Vorbis fixture тремя запросами и 73 077 media bytes.
const N14A_HTTP_OGG_INITIAL_BODY_BYTES: usize = 73_077;
/// FTP owner читает тот же fixture шестью RETR и 219 634 media bytes до первого PCM.
const N14A_FTP_OGG_INITIAL_BODY_BYTES: usize = 219_634;

/// Общий hermetic process spy падает при любом extractor spawn attempt-е.
#[derive(Default)]
pub(crate) struct ZeroProcessSpy {
    /// Atomic counter не зависит от thread-а, на котором ошибочно вызвали бы extractor.
    invocation_count: AtomicUsize,
}

impl ExtractorProcessLauncher for ZeroProcessSpy {
    fn spawn(
        &self,
        _command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        self.invocation_count.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other(
            "hermetic media test запрещает extractor process spawn",
        ))
    }
}

impl ZeroProcessSpy {
    /// Делает spy единственным extractor owner-ом конкретного lifecycle attempt-а.
    pub(crate) fn install_as_attempt_owner(
        self: &Arc<Self>,
        settings: &mut crate::media_open::WebMediaOpenSettings,
    ) {
        settings.yt_dlp_config.enabled = true;
        settings.extractor_adapter = YtDlpExtractorAdapter::with_process_launcher(self.clone());
    }

    /// Возвращает exact число попыток запуска child process-а.
    pub(crate) fn invocation_count(&self) -> usize {
        self.invocation_count.load(Ordering::SeqCst)
    }
}

/// Открывает уже классифицированный locator через production app runtime.
fn open_direct(
    locator: &str,
    app_config: &AppConfig,
) -> crate::direct_progressive_open::DirectProgressiveOpenResult {
    let classified = crate::direct_progressive_open::classify_direct_media_url(locator)
        .expect("fixture locator должен классифицироваться direct");
    crate::direct_progressive_open::open_direct_media(
        &classified,
        &app_config.network,
        &app_config.player.demux,
        CancellationToken::new(),
    )
    .expect("direct progressive runtime должен открыть fixture")
}

/// Читает production Ogg/Vorbis packets до первого непустого PCM buffer-а.
fn assert_nonzero_vorbis_pcm(demuxer: &mut dyn Demuxer) {
    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("direct demuxer должен обнаружить audio track");
    assert_eq!(audio_track.codec_id, "A_VORBIS");

    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let mut decoder = audio_decoder_factory
        .create_decoder(super::decoder_config_from_track(&audio_track))
        .expect("create production Vorbis decoder");
    let deadline = Instant::now() + DEMUX_EVENT_DEADLINE;

    loop {
        match demuxer.next_event().expect("read direct Ogg event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == audio_track.id => {
                let encoded_packet = EncodedAudioPacket::new(
                    packet.track_id.get(),
                    audio_packet_timing(&packet),
                    &packet.data,
                );
                let decoded_samples = decoder
                    .decode(&encoded_packet)
                    .expect("decode direct Vorbis packet");
                if !decoded_samples.is_empty() {
                    super::assert_pcm_advances_clock(
                        &decoded_samples,
                        decoder.sample_rate(),
                        decoder.channels(),
                    );
                    return;
                }
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                panic!("direct Ogg readiness deadline exceeded: {hint:?}");
            }
            DemuxReadEvent::EndOfStream => {
                panic!("direct Ogg reached EOS before nonzero PCM");
            }
        }
    }
}

/// Seekable HTTP Ogg проходит open/seek/reopen без extractor и duplicate root probe-а.
#[test]
fn n14b_lifecycle_http_ogg_seek_forward_back_and_reopen_reaches_pcm_without_extractor() {
    let origin =
        RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::RequestLimitedOgg {
            ogg_bytes: vorbis::large_vorbis_fixture(),
            maximum_successful_requests: 6,
        });
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    let locator = origin.media_url();
    crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("HTTP Ogg classifier должен принять fixture");
    assert_eq!(
        origin.request_count(),
        0,
        "direct HTTP classifier не должен fetch-ить root до open"
    );

    let first_open = open_direct(&locator, &app_config);
    let (mut first_demuxer, _first_endpoint_recovery) = first_open.into_runtime_parts();
    assert_eq!(first_demuxer.seekability(), DemuxSeekability::Seekable);
    assert_nonzero_vorbis_pcm(&mut *first_demuxer);
    assert_eq!(
        origin.request_count(),
        3,
        "initial open должен выполнить exact 3 Range requests"
    );

    first_demuxer
        .seek(Duration::from_millis(100))
        .expect("seekable direct Ogg должен принять nonzero seek");
    assert_nonzero_vorbis_pcm(&mut *first_demuxer);
    assert_eq!(
        origin.request_count(),
        3,
        "seek внутри downloaded source не должен повторно разрешать/open-ить root"
    );

    first_demuxer
        .seek(Duration::ZERO)
        .expect("seekable direct Ogg должен принять обратный seek к началу");
    assert_nonzero_vorbis_pcm(&mut *first_demuxer);
    assert_eq!(
        origin.request_count(),
        3,
        "обратный seek внутри downloaded source также не должен reopen-ить root"
    );

    drop(first_demuxer);
    let reopened = open_direct(&locator, &app_config);
    let (mut reopened_demuxer, endpoint_recovery) = reopened.into_runtime_parts();
    assert_nonzero_vorbis_pcm(&mut *reopened_demuxer);
    assert_eq!(
        origin.request_count(),
        6,
        "explicit reopen создаёт ровно один новый open cohort"
    );
    endpoint_recovery.observe_endpoint_expiry(direct_expiry_signal());
    assert!(
        endpoint_recovery.claim_pending_signal().is_some(),
        "полностью открытый direct source обязан arm-ить stable-resource recovery"
    );
}

/// N14A: HTTP Ogg достигает PCM/clock с exact initial request и byte accounting.
#[test]
fn n14a_consumer_http_ogg_reaches_pcm_clock_with_exact_accounting() {
    let origin =
        RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::RequestLimitedOgg {
            ogg_bytes: vorbis::large_vorbis_fixture(),
            maximum_successful_requests: 3,
        });
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    let locator = origin.media_url();
    crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("HTTP Ogg classifier должен принять N14A fixture");
    assert_eq!(origin.request_count(), 0);
    assert_eq!(origin.response_body_bytes(), 0);

    let opened = open_direct(&locator, &app_config);
    let (mut demuxer, _endpoint_recovery) = opened.into_runtime_parts();
    assert_nonzero_vorbis_pcm(demuxer.as_mut());

    assert_eq!(origin.request_count(), 3);
    assert_eq!(
        origin.response_body_bytes(),
        N14A_HTTP_OGG_INITIAL_BODY_BYTES
    );
}

/// Строит typed late-expiry signal без URL или другого transport material.
fn direct_expiry_signal() -> EndpointExpirySignal {
    let source = SourceIdentity::new(7001);
    let component = MediaComponentIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new("direct-recovery-fixture").expect("format identity"),
        ),
        SemanticIdentity::new(source, "direct-recovery-fixture").expect("semantic identity"),
        MediaComponentRole::Muxed,
    )
    .expect("component identity shares source lineage");
    EndpointExpirySignal::new(
        component,
        SourceGeneration::new(1),
        EndpointExpiryResourceKind::ProgressiveRange,
        EndpointExpiryReason::AuthorizationExpired,
    )
}

/// HTTP `200` body передаётся demux worker-у и остаётся честно forward-only.
#[test]
fn forward_only_http_ogg_reuses_initial_body_and_rejects_seek() {
    let origin = RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::FullBodyOgg(
        vorbis::large_vorbis_fixture(),
    ));
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    let opened = open_direct(&origin.media_url(), &app_config);
    let (mut demuxer, _endpoint_recovery) = opened.into_runtime_parts();
    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    demuxer
        .seek(Duration::from_millis(100))
        .expect_err("forward-only direct source не должен обещать seek");
    assert_nonzero_vorbis_pcm(&mut *demuxer);
    assert_eq!(
        origin.request_count(),
        1,
        "classification + probe обязаны передать initial 200 body без второго GET"
    );
}

/// FTP Ogg проходит production provider до PCM; REST seek и reopen имеют exact RETR accounting.
#[test]
fn n14b_lifecycle_ftp_ogg_seek_forward_back_and_reopen_reaches_pcm_without_extractor() {
    let origin = FtpVorbisOrigin::spawn(vorbis::large_vorbis_fixture());
    let locator = origin.credentialed_media_url();
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("FTP Ogg classifier должен принять fixture");
    assert_eq!(
        origin.retrieval_count(),
        0,
        "direct FTP classifier не должен выполнять RETR до open"
    );

    let first_open = open_direct(&locator, &app_config);
    let (mut first_demuxer, _first_endpoint_recovery) = first_open.into_runtime_parts();
    assert_eq!(first_demuxer.seekability(), DemuxSeekability::Seekable);
    assert_nonzero_vorbis_pcm(&mut *first_demuxer);
    let retrievals_after_open = origin.retrieval_count();
    assert!(retrievals_after_open > 0);

    first_demuxer
        .seek(Duration::from_millis(100))
        .expect("FTP REST-backed Ogg должен принять seek");
    assert_nonzero_vorbis_pcm(&mut *first_demuxer);
    let retrievals_after_seek = origin.retrieval_count();
    assert!(retrievals_after_seek > retrievals_after_open);

    first_demuxer
        .seek(Duration::ZERO)
        .expect("FTP REST-backed Ogg должен принять обратный seek к началу");
    assert_nonzero_vorbis_pcm(&mut *first_demuxer);
    let retrievals_after_backward_seek = origin.retrieval_count();
    assert!(retrievals_after_backward_seek > retrievals_after_seek);

    drop(first_demuxer);
    let reopened = open_direct(&locator, &app_config);
    let (mut reopened_demuxer, _reopened_endpoint_recovery) = reopened.into_runtime_parts();
    assert_nonzero_vorbis_pcm(&mut *reopened_demuxer);
    assert!(origin.retrieval_count() > retrievals_after_backward_seek);
}

/// N14A: FTP Ogg initial open достигает PCM/clock с exact RETR и byte accounting.
#[test]
fn n14a_consumer_ftp_ogg_reaches_pcm_clock_with_exact_accounting() {
    let origin = FtpVorbisOrigin::spawn(vorbis::large_vorbis_fixture());
    let locator = origin.credentialed_media_url();
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("FTP Ogg classifier должен принять N14A fixture");
    assert_eq!(origin.retrieval_count(), 0);
    assert_eq!(origin.transferred_body_bytes(), 0);

    let opened = open_direct(&locator, &app_config);
    let (mut demuxer, _endpoint_recovery) = opened.into_runtime_parts();
    assert_nonzero_vorbis_pcm(demuxer.as_mut());

    assert_eq!(origin.retrieval_count(), 6);
    assert_eq!(
        origin.transferred_body_bytes(),
        N14A_FTP_OGG_INITIAL_BODY_BYTES
    );
}
