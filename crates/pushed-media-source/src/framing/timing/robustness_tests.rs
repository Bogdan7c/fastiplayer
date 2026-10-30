//! Страница — враг: обрезки и порча настоящих сегментов не роняют разборщик.
//!
//! `proptest` в дереве нет; генератор свой (xorshift64*, как в
//! `browser-bridge-protocol/tests/property.rs`): упавший прогон повторяется по зерну.

use bytes::Bytes;

use crate::container::PushedContainer;

use super::test_support::frame_stream;
use super::{SegmentTiming, SegmentTimingContext};

const VIDEO_H264_64: &[u8] = include_bytes!("../../../fixtures/video-h264-64.mp4");
const AUDIO_AAC: &[u8] = include_bytes!("../../../fixtures/audio-aac.mp4");
const MUXED_WEBM: &[u8] =
    include_bytes!("../../../../browser-bridge-protocol/fixtures/media/muxed.webm");

/// Порченых вариантов на один сегмент.
const MUTATIONS_PER_SEGMENT: usize = 400;

/// Детерминированный генератор xorshift64*.
struct Generator(u64);

impl Generator {
    fn next_u64(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Число в `0..bound` (`bound > 0`).
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next_u64() % bound as u64).expect("below usize bound")
    }
}

/// Init и медиасегменты одной фикстуры.
struct Corpus {
    container: PushedContainer,
    initialization: Bytes,
    media: Vec<Bytes>,
}

fn corpora() -> Vec<Corpus> {
    [
        (PushedContainer::FragmentedMp4, VIDEO_H264_64),
        (PushedContainer::FragmentedMp4, AUDIO_AAC),
        (PushedContainer::WebM, MUXED_WEBM),
    ]
    .into_iter()
    .map(|(container, fixture)| {
        let stream = frame_stream(container, fixture);
        Corpus {
            container,
            initialization: stream.initializations[0].clone(),
            media: stream.media,
        }
    })
    .collect()
}

/// Порча: обрезка, замена байта или вставка «огромного» 32-битного числа.
fn mutate(original: &[u8], generator: &mut Generator) -> Vec<u8> {
    let mut mutated = original.to_vec();
    match generator.below(3) {
        0 => mutated.truncate(generator.below(original.len())),
        1 => {
            let position = generator.below(mutated.len());
            mutated[position] ^= u8::try_from(generator.below(255) + 1).expect("byte");
        }
        _ => {
            let position = generator.below(mutated.len().saturating_sub(4).max(1));
            let end = (position + 4).min(mutated.len());
            mutated[position..end].fill(0xFF);
        }
    }
    mutated
}

#[test]
fn every_truncated_media_segment_is_an_error_not_a_wrong_span() {
    for corpus in corpora() {
        let context =
            SegmentTimingContext::from_initialization(corpus.container, &corpus.initialization)
                .expect("fixture init");
        for segment in &corpus.media {
            // Сам сегмент размечается, а любая обрезка внутри заголовка
            // (`moof` целиком у fMP4, заголовок и первые блоки кластера) — ошибка.
            assert!(matches!(
                context.media_segment_timing(segment),
                Ok(SegmentTiming::Timed(_))
            ));
            let header_len = match corpus.container {
                PushedContainer::FragmentedMp4 => usize::try_from(u32::from_be_bytes([
                    segment[0], segment[1], segment[2], segment[3],
                ]))
                .expect("moof size"),
                PushedContainer::WebM | PushedContainer::MpegTs => segment.len(),
            };
            for cut in 0..header_len {
                assert!(
                    context.media_segment_timing(&segment[..cut]).is_err(),
                    "{:?} cut at {cut} of {header_len}",
                    corpus.container
                );
            }
        }
    }
}

#[test]
fn random_corruption_never_panics_and_often_reports_errors() {
    let mut generator = Generator(0x5EED_7111_0E00_03B0);
    let mut rejected = 0_usize;
    let mut attempts = 0_usize;
    for corpus in corpora() {
        let context =
            SegmentTimingContext::from_initialization(corpus.container, &corpus.initialization)
                .expect("fixture init");
        for _ in 0..MUTATIONS_PER_SEGMENT {
            let broken_init = mutate(&corpus.initialization, &mut generator);
            attempts += 1;
            rejected += usize::from(
                SegmentTimingContext::from_initialization(corpus.container, &broken_init).is_err(),
            );
        }
        for segment in &corpus.media {
            for _ in 0..MUTATIONS_PER_SEGMENT {
                let broken = mutate(segment, &mut generator);
                attempts += 1;
                match context.media_segment_timing(&broken) {
                    Ok(SegmentTiming::Timed(timed)) => {
                        let span = timed.span();
                        assert!(span.start() <= span.end(), "ordered span");
                        assert!(
                            span.start() <= timed.certain_end()
                                && timed.certain_end() <= span.end(),
                            "certain end inside the span"
                        );
                    }
                    Ok(SegmentTiming::Untimed) => panic!("fMP4/WebM are always timed"),
                    Err(_) => rejected += 1,
                }
            }
        }
    }
    // Порча не проходит молча: заметная доля вариантов отвергается. Доля не
    // может быть близка к 100 %: большинство байтов сегмента — тела кадров
    // (`mdat`, байты блоков), и их порча время честно не меняет.
    assert!(
        rejected * 10 > attempts,
        "rejected {rejected} of {attempts}"
    );
}
