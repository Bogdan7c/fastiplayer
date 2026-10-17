//! Случайная порча и обрезка packet-ов через adapter: без паник, по политике тишины.
//!
//! Генератор детерминированный (LCG), поэтому падение воспроизводится. Для
//! каждого packet-а проверяется ровно один из допустимых исходов:
//! - `Decoded` — PCM в опубликованном формате, длина = кадр × каналы;
//! - `Concealed` — нули длиной последнего удачного кадра в его формате;
//! - `DroppedBeforeFirstFrame` — только пока удачного кадра ещё не было;
//! - `Rejected` — только постоянная причина (не битый packet), и одна и та же
//!   причина не сообщается дважды подряд.

use audio_core::AudioDecoder;

use super::super::frame_format::PublishedFrameFormat;
use super::super::{Ac3CodecIdentity, Ac3DecodeError, Ac3FamilyDecoder, PacketOutcome};
use super::{STEREO_AC3, SURROUND_AC3, SURROUND_EAC3, concrete_decoder, packets};

/// Детерминированный генератор псевдослучайных чисел (Numerical Recipes LCG).
struct TestRandom(u64);

impl TestRandom {
    /// Следующие 32 бита.
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 32) as u32
    }

    /// Число в `0..bound` (`bound > 0`).
    fn below(&mut self, bound: usize) -> usize {
        self.next_u32() as usize % bound
    }
}

/// Как испортить packet.
#[derive(Debug, Clone, Copy)]
enum Damage {
    /// Без изменений.
    Intact,
    /// Несколько случайных байт заменены случайными значениями.
    RandomBytes,
    /// Packet обрезан в случайном месте.
    Truncated,
    /// Packet удлинён случайным мусором.
    TrailingGarbage,
}

/// Применяет порчу к копии packet-а.
fn damage(packet_bytes: &[u8], kind: Damage, random: &mut TestRandom) -> Vec<u8> {
    let mut damaged = packet_bytes.to_vec();
    match kind {
        Damage::Intact => {}
        Damage::RandomBytes => {
            for _ in 0..=random.below(8) {
                let index = random.below(damaged.len());
                damaged[index] = random.next_u32() as u8;
            }
        }
        Damage::Truncated => damaged.truncate(random.below(damaged.len())),
        Damage::TrailingGarbage => {
            damaged.extend((0..=random.below(64)).map(|_| random.next_u32() as u8));
        }
    }
    damaged
}

/// Проверяет исход одного packet-а относительно состояния до него.
fn assert_outcome_follows_policy(
    decoder: &Ac3FamilyDecoder,
    outcome: &PacketOutcome,
    format_before: Option<PublishedFrameFormat>,
    previous_rejection: Option<&Ac3DecodeError>,
) {
    let silence_of = |format: PublishedFrameFormat| {
        vec![0.0; format.frames_per_packet * format.layout.channel_count() as usize]
    };
    match outcome {
        PacketOutcome::Decoded(samples) => {
            let published = decoder.frame_format.published().unwrap();
            assert_eq!(samples.len(), silence_of(published).len());
            assert!(samples.iter().all(|sample| sample.is_finite()));
        }
        PacketOutcome::Concealed(samples) => {
            let format = format_before.unwrap();
            assert_eq!(*samples, silence_of(format));
            assert_eq!(decoder.frame_format.published(), Some(format));
        }
        PacketOutcome::DroppedBeforeFirstFrame => {
            assert_eq!(format_before, None);
            assert_eq!(decoder.frame_format.published(), None);
        }
        PacketOutcome::Rejected(error) => {
            assert!(
                matches!(
                    error,
                    Ac3DecodeError::UnsupportedStream { .. }
                        | Ac3DecodeError::UnconvertibleChannelLayout { .. }
                ),
                "corruption must be concealed, not reported: {error}"
            );
            assert_ne!(Some(error), previous_rejection, "reported twice in a row");
            assert_eq!(decoder.frame_format.published(), format_before);
        }
    }
}

/// Прогоняет поток с порчей каждого packet-а и возвращает число удачных кадров.
fn run_damaged_stream(identity: Ac3CodecIdentity, stream: &[u8], seed: u64) -> usize {
    let kinds = [
        Damage::Intact,
        Damage::RandomBytes,
        Damage::Truncated,
        Damage::TrailingGarbage,
    ];
    let mut random = TestRandom(seed);
    let mut decoder = concrete_decoder(identity, 2);
    let mut previous_rejection = None;
    let mut decoded_frames = 0;
    for round in 0..4 {
        for packet_bytes in packets(stream) {
            let kind = if round == 0 {
                Damage::Intact
            } else {
                kinds[random.below(kinds.len())]
            };
            let damaged = damage(packet_bytes, kind, &mut random);
            let format_before = decoder.frame_format.published();

            let outcome = decoder.decode_access_unit(&damaged);

            assert_outcome_follows_policy(
                &decoder,
                &outcome,
                format_before,
                previous_rejection.as_ref(),
            );
            if matches!(outcome, PacketOutcome::Decoded(_)) {
                decoded_frames += 1;
            }
            previous_rejection = match outcome {
                PacketOutcome::Rejected(error) => Some(error),
                PacketOutcome::Decoded(_) => None,
                _ => previous_rejection,
            };
        }
        // Seek посреди испорченного потока не ломает политику.
        decoder.reset().unwrap();
    }
    decoded_frames
}

#[test]
fn random_corruption_of_ac3_packets_never_panics_and_follows_silence_policy() {
    for (stream, seed) in [(STEREO_AC3, 1), (SURROUND_AC3, 2), (SURROUND_AC3, 3)] {
        let decoded_frames = run_damaged_stream(Ac3CodecIdentity::Ac3, stream, seed);
        // Нетронутый первый проход декодируется целиком.
        assert!(decoded_frames >= packets(stream).len());
    }
}

#[test]
fn random_corruption_of_eac3_packets_never_panics_and_follows_silence_policy() {
    for seed in [4, 5] {
        let decoded_frames = run_damaged_stream(Ac3CodecIdentity::Eac3, SURROUND_EAC3, seed);
        assert!(decoded_frames >= packets(SURROUND_EAC3).len());
    }
}

#[test]
fn every_prefix_of_a_packet_is_concealed_without_panic() {
    let packet_list = packets(SURROUND_AC3);
    let mut decoder = concrete_decoder(Ac3CodecIdentity::Ac3, 6);
    super::decode_packet(&mut decoder, packet_list[0]);
    let format = decoder.frame_format.published().unwrap();

    for prefix_length in 0..packet_list[1].len() {
        let outcome = decoder.decode_access_unit(&packet_list[1][..prefix_length]);
        assert_outcome_follows_policy(&decoder, &outcome, Some(format), None);
        assert!(
            matches!(outcome, PacketOutcome::Concealed(_)),
            "prefix {prefix_length}: {outcome:?}"
        );
    }
}
