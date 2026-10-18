//! Инвариант: сбои сегментов субтитров DASH не доходят до recovery видео.
//!
//! Реальный путь `static_text_subtitles` -> `load_index` -> `fetch_segment` по настоящему
//! HTTP-серверу, который на всё отвечает 404, при присоединённом наблюдателе expiry.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fastiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportLimits,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    EndpointExpiryObserver, EndpointExpirySignal, MediaComponentIdentity, MediaComponentRole,
    MediaPresentation, RedirectHopLimit, RedirectPolicy, SecretRequestContext, SecretRequestScope,
    SourceGeneration, TransportOpenRequest, TransportProviderId,
};

use super::static_text_subtitles;
use crate::text::DashTextError;

#[derive(Default)]
struct RecordingObserver {
    signals: Mutex<Vec<EndpointExpirySignal>>,
}

impl EndpointExpiryObserver for RecordingObserver {
    fn observe_endpoint_expiry(&self, signal: EndpointExpirySignal) {
        self.signals.lock().expect("signals mutex").push(signal);
    }
}

/// Сервер, отвечающий 404 на любой запрос и считающий их.
struct NotFoundServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

impl NotFoundServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let address = listener.local_addr().expect("address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (thread_stop, thread_requests) = (Arc::clone(&stop), Arc::clone(&requests));
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0_u8; 4096];
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.read(&mut buffer);
                        thread_requests.fetch_add(1, Ordering::AcqRel);
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Self {
            address,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address)).expect("target")
    }
}

impl Drop for NotFoundServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn video_context_with_observer(
    target: &HttpRequestTarget,
    observer: Arc<dyn EndpointExpiryObserver>,
) -> AdaptiveHttpContext {
    let source = SourceIdentity::new(92);
    let generation = SourceGeneration::new(1);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("dash-isolation-test").expect("format"),
    );
    let semantic = SemanticIdentity::new(source, "dash-isolation-test").expect("semantic");
    let component =
        MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed).expect("component");
    let scope = SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("scope"));
    let secrets = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("headers"))
        .build();
    let request = TransportOpenRequest::new(
        TransportProviderId::new("dash-isolation-test").expect("provider"),
        component,
        target.clone(),
        MediaPresentation::Vod,
        generation,
        secrets,
        RedirectPolicy::same_origin(RedirectHopLimit::new(4).expect("hops")),
        CancellationToken::new(),
    )
    .expect("request")
    .with_endpoint_expiry_observer(observer);
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("config");
    AdaptiveHttpContext::new(
        request,
        &source_config,
        AdaptiveTransportLimits::new(
            NonZeroUsize::new(64 * 1_024).expect("manifest"),
            NonZeroUsize::new(64 * 1_024).expect("resource"),
            NonZeroUsize::new(64).expect("descriptor"),
        ),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(2).expect("attempts"),
            Duration::from_millis(5),
            Duration::from_millis(10),
            Duration::from_millis(10),
        )
        .expect("retry"),
    )
    .expect("context")
}

#[test]
fn text_segment_404_stays_a_text_error_and_never_reaches_video_recovery() {
    let server = NotFoundServer::start();
    let manifest_target = server.target("/vod/manifest.mpd");
    let observer = Arc::new(RecordingObserver::default());
    let video_context = video_context_with_observer(&manifest_target, observer.clone());
    let mpd = super::tests::mpd_with_text(
        r#"<AdaptationSet mimeType="text/vtt" lang="en">
          <Representation id="t"><BaseURL>en.vtt</BaseURL></Representation></AdaptationSet>"#,
        "",
    );
    let subtitles = static_text_subtitles(
        &mpd,
        &manifest_target,
        &video_context,
        SourceGeneration::new(1),
        NonZeroUsize::new(64).expect("segments"),
    )
    .expect("text track exists");
    let track = subtitles.tracks()[0].id;
    let cancellation = CancellationToken::new();
    let index = subtitles.load_index(track, &cancellation).expect("index");
    assert!(!index.segments.is_empty());

    for _ in 0..3 {
        let error = subtitles
            .fetch_segment(
                &index.segments[0].handle,
                NonZeroUsize::new(1024).expect("limit"),
                &cancellation,
            )
            .expect_err("404 must fail the text segment");
        assert!(
            !matches!(error, DashTextError::Cancelled),
            "plain failure, got {error:?}"
        );
    }
    assert!(
        server.requests.load(Ordering::Acquire) >= 3,
        "text requests really reached the server"
    );
    assert!(
        observer.signals.lock().expect("signals").is_empty(),
        "text failures must not publish video endpoint expiry"
    );

    // Контроль: тот же сервер/наблюдатель при видео-запросе сигнал публикует.
    let _ = video_context.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        server.target("/vod/video.m4s"),
        NonZeroUsize::new(64).expect("bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    ));
    assert_eq!(observer.signals.lock().expect("signals").len(), 1);
}
