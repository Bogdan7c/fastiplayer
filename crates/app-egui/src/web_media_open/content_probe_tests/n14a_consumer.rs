//! N14A proofs, общие для extractor page и production audio clock.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use audio::clock::AudioClock;
use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use service_ytdlp::{ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpExtractorAdapter};
use tempfile::TempDir;

use super::{
    RangeFixtureOrigin, YT_DLP_DOCUMENT_ENV, assert_prepared_opus_reaches_pcm, install_fake_yt_dlp,
    ogg_opus_fixture, path_with_fake_tools_first, prepare_content_probed_test_media_with_adapter,
    yt_dlp_document,
};

/// In-process launcher владеет exact N14A page-resolution process accounting-ом.
struct CountingPageFixtureLauncher {
    /// Изолированный tools directory выбирает только hermetic fake `yt-dlp`.
    fake_tools_directory: PathBuf,
    /// Process-scoped document направляет candidate на test-owned loopback origin.
    extractor_document: String,
    /// Atomic counter наблюдает тот же launcher, который передан production attempt-у.
    invocation_count: AtomicUsize,
}

impl ExtractorProcessLauncher for CountingPageFixtureLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        self.invocation_count.fetch_add(1, Ordering::SeqCst);
        command
            .env(
                "PATH",
                path_with_fake_tools_first(&self.fake_tools_directory),
            )
            .env(YT_DLP_DOCUMENT_ENV, &self.extractor_document)
            .spawn()
    }
}

impl CountingPageFixtureLauncher {
    /// Возвращает exact число production extractor child attempts.
    fn invocation_count(&self) -> usize {
        self.invocation_count.load(Ordering::SeqCst)
    }
}

/// Проводит настоящий decoded PCM через production audio clock callback boundary.
pub(crate) fn assert_pcm_advances_clock(decoded_samples: &[f32], sample_rate: u32, channels: u32) {
    assert!(
        !decoded_samples.is_empty(),
        "PCM buffer не должен быть пустым"
    );
    assert!(sample_rate > 0, "decoded PCM обязан иметь sample rate");
    assert!(channels > 0, "decoded PCM обязан иметь channel count");
    let channel_count =
        usize::try_from(channels).expect("decoded channel count должен помещаться в usize");
    assert_eq!(
        decoded_samples.len() % channel_count,
        0,
        "interleaved PCM обязан содержать целое число audio frames"
    );
    let audio_clock = AudioClock::new(sample_rate, channels);
    let initial_position = audio_clock.now();
    let submitted_samples = u64::try_from(decoded_samples.len())
        .expect("decoded PCM sample count должен помещаться в u64");
    audio_clock.record_written(submitted_samples);
    audio_clock.record_played(submitted_samples);
    assert!(
        audio_clock.now() > initial_position,
        "production audio clock обязан продвинуться после played PCM callback"
    );
}

/// N14A: extractor-backed page делает ровно один process и доводит candidate до PCM/clock.
#[test]
fn n14a_consumer_extractor_page_reaches_pcm_clock_with_exact_accounting() {
    let ogg_fixture = ogg_opus_fixture();
    // HTTP owner сначала читает однобайтовый capability probe, затем exact Ogg body.
    let expected_http_body_bytes = ogg_fixture.bytes.len() + 1;
    let origin = RangeFixtureOrigin::spawn(ogg_fixture.bytes);
    let fake_tools = TempDir::new().expect("create N14A extractor tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let launcher = Arc::new(CountingPageFixtureLauncher {
        fake_tools_directory: fake_tools.path().to_path_buf(),
        extractor_document: yt_dlp_document(&origin.media_url()),
        invocation_count: AtomicUsize::new(0),
    });
    let extractor_adapter = YtDlpExtractorAdapter::with_process_launcher(launcher.clone());
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let audio_capabilities = audio_decoder_factory.audio_decode_capability_snapshot();
    assert_eq!(launcher.invocation_count(), 0);
    assert_eq!(origin.request_count(), 0);
    assert_eq!(origin.response_body_bytes(), 0);

    let mut prepared = prepare_content_probed_test_media_with_adapter(
        "https://catalog.example/page-fixture",
        audio_capabilities,
        &extractor_adapter,
    )
    .expect("prepare N14A extractor-backed page fixture");
    assert_prepared_opus_reaches_pcm(&mut prepared, &audio_decoder_factory, "content-probed-ogg");

    assert_eq!(launcher.invocation_count(), 1);
    assert_eq!(origin.request_count(), 2);
    assert_eq!(origin.response_body_bytes(), expected_http_body_bytes);
}
