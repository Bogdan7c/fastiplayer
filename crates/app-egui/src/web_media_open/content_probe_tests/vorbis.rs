//! Вертикальная регрессия Ogg/Vorbis и forward-seek поверх active HTTP fetch-а.

use std::io::Read as _;

use base64::Engine as _;
use flate2::read::GzDecoder;

use super::*;

/// Exact child path не запускает соседние app tests в изолированном процессе.
const VORBIS_CHILD_TEST_NAME: &str = "web_media_open::content_probe_tests::vorbis::ogg_vorbis_forward_seek_reuses_active_fetch_and_reaches_pcm";

/// `Range 0-0` probe плюс два contiguous prefetch range-а — весь допустимый бюджет.
const MAXIMUM_SUCCESSFUL_REQUESTS: usize = 3;

/// Fixture обязан пересекать default initial prefetch boundary в 64 KiB.
const MINIMUM_VORBIS_SOURCE_BYTES: usize = 64 * 1024 + 1;

/// Gzip-сжатый mono Ogg/Vorbis fixture: 0.12 s синусоиды и deterministic
/// 70 000-символьный comment, созданные FFmpeg. Большой comment делает валидный
/// Ogg длиннее 64 KiB, но gzip не раздувает repository test source.
const OGG_VORBIS_GZIP_BASE64: &str = r#"
H4sIAAAAAAAAA+3Zf1DT5x0H8CeIEiCy6CKNiI5YUxMQa1hpDXfdQfxR8gURv5GKyXlbE1KFgL8i
3mzrHeNXrU07igmFTj3BfLkSSQqpSde4UadNpOgylkpwYmvdTVGCXe92PXfn7tzzDdp2t+3P3W63
9+vI9/s8z+f7PN/neb7P9wl32bh9u44kkIcap/Xn+bNz2DwoWCLYv2uvsdrKFwgKfzBzxcDTM+cu
wUa+JnnwEF9TQMvLHssQPfg/N2tm3ubS6Sh9Yf+LT+etVNG/VXn8NPPTaN5p2lVl3vssDZpoMG81
H8yqrTbO1HtlnoCsXadbw2rLN2s3lj1bBAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/8M2bt+uIwLyUOO0/nwCPdt+9HrKkgf/LPJN
yvff7jgAAAAAAAAAAAD8O4LZ+3ftNVZb52vWPC8gREBErEQmX6hjTEIHyypnLzaVG55bbLqsXi+r
EusOaRdyL2/MdlQyw/PYSuW6xbpSndbBdtCUSaIrlumiKnO3VVLSqk2W127w7Esvud1l6lCtX2wK
65ljpqiKXsey2m6rtOBFeZWkYH3wtvGVVQWOHo1i6MLn7J3clsOGw78ez9DFcpulJdW/Eb5gvesb
u8bUBKUmmyoU/KNO0TjaVsH3kxQmCuJ9TiHkXLlo3aYGPlA4hzQQybo9kgt7Mp9rX3YxIX5JISEJ
hCjevKFof1zbXrW1w7y1o/dAx7sfup6871kZ89ydCqQ1t/YnkmFCpK2yVz/XXmrTtWmfUFZcUgu7
v05T35RURdhQp9G2KdymE6lmq1pHlS3iy2VqTrRUuKb4WFU4q+TZ218YJ1eLM7xLySF6N1uRTRZq
s4qYObK9b2qHs9aG1cpj9lJG4T7doU3OdUSXTh61lzFPcKfT2YtOe1STM5uQxNX0Uylsae0Xkh5C
xE1tbLHyY7uxTPWqnX1TeaGNvaQOdu61qW5nbb6kGs5sLlYGpdYy9a2sKlH2rcxmUR7/DNPoJ13T
LctJaFgtIE3Lqt/hqnt2VL+jeKn3XVePwuW6eaD3Tlev66WePlevK+Dpc3lqDxyv5qPHlS4XzVpc
rthhV83F3lNX/Xdc/ukR784z3v6J464Rf1/Au9PlqTvjrY15VkwE/nTANdXVG3up907AFbvqnQp4
79LsYVfORG9sxBsb8btHvB4Ljfacuuqhde+O8HX77/fGpvxTIwHPiPfLCb/nvtedeuZ2wO8Z8XhO
eftj3tyYv3ZC88lfNo/9cMP416Wf1leM++rG6usMNJu/bUvHurF685ijflu92XCPRjdscZg/rTBv
dZg/22/e8vY6enFFx0Ea/fHbdZ/dM28dP/hpfb3BYZ7YYt56z7ztXsvWDw5+pqswPLNty/iG8bGD
hvGDL/jrrj9GsxV/iLZUdNRfjx40d9YfWxWgHfJPx1wrYkOeWGhVLLCqP1D3N4879fypWGiQFl4b
+nnNxNBXU37VrwI7M88MLPro7v3QYJ+r7heeU7HenFigNja0KxbYY6NZ14r7gakRPvtVbOhJ0cnY
VCD7YmDgqve9i4GVojOeRedrJs59eTWw+5J3peiDP+dceC91zZi/5cZY/clTE6Fdqecjz+wfIqRB
kERX+fdHCh894jky4QKTODuU1SxmU2TOzao5MlMaE8q0ixmtzB6hgSq6BDOPiJkdffZRRsFx5UxJ
tz3K1uT2XtaLuk9LmGq3PaKvUfovs7W0qbxJJ62h4E4/z1QrraVM6IQzTZPNOSOM1m0Oaz85cTKi
Lu5zRlhtn6+csdCmGGaQizB17mAGU53rjOhp4DJb5/bpH6c3txV9cvQIy2j76D1uHbWnFWR3NbUz
yj5HO9vfeSRSpJA32Rgt52tncmaaamH1FvdYJ1uSb2+nTQXpPdy+DF1Ofks5m0rfGNpd3/OWmlx/
ucbS7UwvyVb50k305lIacM43KPuC6YY33L5KXUl+R7ux1j12hclVR/UWJjcaYW1uX6eh9MNgpWWu
PN4U7RXjDkb1bv7m2YOOdj1fQ2/Lt0eMSqWvjamlTRlq83s7jLWDY5Xs66pQpbXEN2bXv04DJTn5
IallZ/5NlpCzDbNIYUOy+cRom4MUSgj5ichkK9oh2XtJrXyDDauXy61pGqHSHC4SyriwWsidvqxO
6rbbNUolJzEo5FxUv4KQE2cFJOvsnA2vBQUpc7cmk7U0G1IEpc2hLE7aNCo9KaEPm5PSpjiJKVw0
LLWGi5QLaWq5vFmsUXL0oODsdA9KyaQbX+2ovTKTdmiBYKZDRpGqNWsfW6Cc11zKLOfemq9PVZo7
mMPy0CX9a+6PrxQnu1uieRZ5KGJc7/640phNyDTt0PRZjeKpKjlpSyKFfA/C6u1iI90eZU8JVXTn
oavvKD+2BTSQJGsOqyfpYtRMHrWKNcLuJnrg7BE66GaWuZMV7+Q+PmBn6e6dvZsQ4cCmKmvjWyfo
dp1ADtAlrmLoZHHrlaEsp8TAD5itcRtFGvqcKunq5qLG6SznZb3Nt58GckP6NXzAEuYD/fnjfODW
tZf5gDX8ISHXZieRgVmL1j29XJqc/WgQtGGTTf2q3WhTTR5dw6hCndawMkTnUj3JB2jX+QDN8gF1
PDV5oqmNT40W3eazQtk+WkbH/q8GcYMfhJ1llBw9vHvyYSpBxj0q+08fHP9YRLKupySRB0mLtldq
C3Iy567k50FMvyqk56XNNn6yW1VCbl8Zo5Xn24oUnHU+rdU0qq+hbz3LLGy2G1NlDrs+he4JFmWf
z84qlMGMEqaPi+ZNLnO260vc/k51EueM6mvcQbGGGfRFWctgPf22d/ui+rpcp11vyQ1FjbVyrkNv6
aOvW2mfL91QwpeVuINXjPStykglX+zcTRqKB/a0Opzvn/v9F/GplAlIYh5DJ55fRzL2sIo+Bz61
llHdmilrpo+KX1Z0RbXFVyNfxhbQsnImmR//ZLyMTyXLZl4POjqaejg78m/KuG9T9m9T8+NrNX7x
TCvxagp+OcdTyfJvJlr2nSknDbP4nfrx28tOjiytferGuVliwv8rklBKfiZobNwkTGieJ4z/3iok
fyWkOKXh0KEdt0Rp4vmSdGlG5pKspcvIdyTGf6NNPDvr299o+eT39qxsnD1fLJ4nPd7ZVX9z7nQO
Lezpidje+Ij0dnW9f2QLGZjNVzi0O7mni16xgCj4bMMA+yg+Go8PXy0TdPyu/Ke/XVT5DEkk9l/e
esUwlHdl1/Ul1xL+DosDmdR0HQEA
"#;

/// Полный candidate path обязан уложиться в минимальный request budget и выдать PCM.
#[test]
fn ogg_vorbis_forward_seek_reuses_active_fetch_and_reaches_pcm() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_vorbis_reaches_pcm("content-probed-vorbis");
        return;
    }

    let origin =
        RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::RequestLimitedOgg {
            ogg_bytes: large_vorbis_fixture(),
            maximum_successful_requests: MAXIMUM_SUCCESSFUL_REQUESTS,
        });
    let fake_tools = TempDir::new().expect("create Vorbis fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let extractor_document = format!(
        r#"{{"id":"content-probed-vorbis","title":"ContentProbed Vorbis","formats":[{{"format_id":"content-probed-vorbis","url":"{}","protocol":"http","ext":"ogg","container":"ogg","vcodec":null,"acodec":null}}]}}"#,
        origin.media_url()
    );
    let child_output = run_content_probe_child(
        fake_tools.path(),
        VORBIS_CHILD_TEST_NAME,
        extractor_document,
    );

    assert_child_succeeded("request-bounded Ogg/Vorbis playback", &child_output);
    assert_eq!(
        origin.request_count(),
        MAXIMUM_SUCCESSFUL_REQUESTS,
        "open должен состоять из probe и двух contiguous Range request-ов без refetch"
    );
}

/// Восстанавливает валидный большой Ogg без запуска FFmpeg во время теста.
pub(super) fn large_vorbis_fixture() -> Vec<u8> {
    let compact_base64 = OGG_VORBIS_GZIP_BASE64
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    let compressed_bytes = base64::engine::general_purpose::STANDARD
        .decode(compact_base64)
        .expect("decode generated Ogg/Vorbis gzip fixture");
    let mut decoder = GzDecoder::new(compressed_bytes.as_slice());
    let mut ogg_bytes = Vec::new();
    decoder
        .read_to_end(&mut ogg_bytes)
        .expect("decompress generated Ogg/Vorbis fixture");
    assert!(ogg_bytes.starts_with(b"OggS"));
    assert!(ogg_bytes.len() >= MINIMUM_VORBIS_SOURCE_BYTES);
    ogg_bytes
}

/// Child проходит fake yt-dlp, выбранный transport, Symphonia и production Vorbis decoder.
pub(super) fn assert_child_vorbis_reaches_pcm(expected_format_id: &str) {
    let page_url = format!("https://page.example.test/{expected_format_id}");
    assert_vorbis_reaches_pcm_at_locator(
        &page_url,
        expected_format_id,
        VorbisTrackExpectation::GeneratedMono8Khz,
    );
}

/// Child сохраняет exact input scheme и требует PCM от production decoder-а.
pub(super) fn assert_child_vorbis_reaches_pcm_at_locator(
    locator_text: &str,
    expected_format_id: &str,
) {
    assert_vorbis_reaches_pcm_at_locator(
        locator_text,
        expected_format_id,
        VorbisTrackExpectation::AnyValidAudio,
    );
}

/// Различает immutable generated fixture и mutable public runtime source.
#[derive(Clone, Copy, Debug)]
enum VorbisTrackExpectation {
    /// Checked-in fixture обязан сохранять exact mono 8 kHz topology.
    GeneratedMono8Khz,
    /// Публичный source может менять sample rate/channels, но не терять audio validity.
    AnyValidAudio,
}

/// Общий decoder-reaching assertion с явно выбранной строгостью track metadata.
fn assert_vorbis_reaches_pcm_at_locator(
    locator_text: &str,
    expected_format_id: &str,
    track_expectation: VorbisTrackExpectation,
) {
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let mut prepared = prepare_content_probed_test_media_at_locator(
        locator_text,
        audio_decoder_factory.audio_decode_capability_snapshot(),
    )
    .expect("prepare content-probed Ogg/Vorbis candidate");
    assert_eq!(
        prepared
            .candidate_selection()
            .exact_identity()
            .format()
            .as_str(),
        expected_format_id
    );
    assert_eq!(
        prepared.stream_configuration().active_candidate().layout,
        StreamLayoutKind::ContentProbed
    );

    let audio_track = prepared
        .demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("production demuxer должен обнаружить Vorbis audio track");
    assert_eq!(audio_track.codec_id, "A_VORBIS");
    match track_expectation {
        VorbisTrackExpectation::GeneratedMono8Khz => {
            assert_eq!(audio_track.sample_rate, Some(8_000));
            assert_eq!(audio_track.channels, Some(1));
        }
        VorbisTrackExpectation::AnyValidAudio => {
            assert!(
                audio_track
                    .sample_rate
                    .is_some_and(|sample_rate| sample_rate > 0)
            );
            assert!(audio_track.channels.is_some_and(|channels| channels > 0));
        }
    }
    let mut decoder = audio_decoder_factory
        .create_decoder(decoder_config_from_track(&audio_track))
        .expect("create production Vorbis decoder");

    let deadline = Instant::now() + DEMUX_EVENT_DEADLINE;
    loop {
        match prepared
            .demuxer
            .next_event()
            .expect("read Ogg/Vorbis demux event")
        {
            DemuxReadEvent::Packet(packet) if packet.track_id == audio_track.id => {
                let encoded_packet = EncodedAudioPacket::new(
                    packet.track_id.get(),
                    audio_packet_timing(&packet),
                    &packet.data,
                );
                let decoded_samples = decoder
                    .decode(&encoded_packet)
                    .expect("decode production Vorbis packet");
                if !decoded_samples.is_empty() {
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
                panic!("Ogg/Vorbis readiness deadline exceeded: {hint:?}");
            }
            DemuxReadEvent::EndOfStream => {
                panic!("Ogg/Vorbis reached EOS before production decoder returned PCM");
            }
        }
    }
}
