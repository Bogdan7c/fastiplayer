//! Синтез программы на НАСТОЯЩИХ коэффициентах в роли зависимого substream-а
//! (`synthesis::program`): кадр стерео-фикстуры подставляется зависимым
//! substream-ом к кадру 5.1 — так проверяются замена каналов, источник слов
//! DRC (Annex E §3.8.5) и TPNP (§3.7) на реальном сигнале, а не на тишине.

use super::support::{eac3_fixture, split_frames};
use crate::config::{CrcPolicy, DecoderConfig, DynamicRangeControl};
use crate::decoder::Ac3Decoder;
use crate::output::ChannelPosition::{self, Center, LeftSurround, RightSurround};
use crate::output::DecodedAudio;
use crate::spectral::dither::{DitherGenerator, DitherMode};
use crate::syntax::audblk::output::DecodedAudioBlock;
use crate::syntax::bsi_values::AudioCodingMode;
use crate::syntax::eac3::access_unit::EnhancedAccessUnitParser;
use crate::syntax::eac3::audfrm::TransientPreNoise;
use crate::synthesis::program::frame::{ProgramFrame, SynthesisSettings, Synthesizer};
use crate::synthesis::program::layout::ProgramLayout;
use crate::synthesis::program::substream::{CompressionWords, SubstreamFrame};
use crate::synthesis::transient_pre_noise::TransientPreNoiseProcessing;

/// `dynrng` = 110 00000₂ → множитель 1/4.
const QUARTER_GAIN_WORD: u8 = 0b110_00000;
const SURROUND: [ChannelPosition; 2] = [LeftSurround, RightSurround];

/// Коэффициенты кадров фикстуры (разбор без dither-а).
fn frame_blocks(name: &str) -> Vec<Vec<DecodedAudioBlock>> {
    let mut parser = EnhancedAccessUnitParser::new(CrcPolicy::Verify);
    let mut dither = DitherGenerator::new(DitherMode::Disabled);
    split_frames(eac3_fixture(name))
        .into_iter()
        .map(|frame| {
            let parsed = parser.parse(frame, &mut dither).unwrap();
            let substream = parsed.substreams().next().unwrap();
            substream.blocks.to_vec()
        })
        .collect()
}

fn decode_alone(name: &str) -> Vec<DecodedAudio> {
    let mut decoder = Ac3Decoder::new(DecoderConfig {
        dither: DitherMode::Disabled,
        dynamic_range: DynamicRangeControl::Disabled,
        ..DecoderConfig::default()
    });
    split_frames(eac3_fixture(name))
        .into_iter()
        .map(|frame| decoder.decode_access_unit(frame).unwrap())
        .collect()
}

/// Тест-стенд: 5.1 независимый + стерео «зависимый» на позициях Ls/Rs.
struct Program {
    independent: Vec<Vec<DecodedAudioBlock>>,
    dependent: Vec<Vec<DecodedAudioBlock>>,
    independent_positions: Vec<ChannelPosition>,
}

impl Program {
    fn new() -> Self {
        Self {
            independent: frame_blocks("5p1-48k-384k.eac3"),
            dependent: frame_blocks("stereo-48k-128k-cpl-high.eac3"),
            independent_positions: crate::output::ac3_channel_positions(
                AudioCodingMode::ThreeFrontStereoSurround,
                true,
            ),
        }
    }

    /// Синтез всех кадров одним синтезатором.
    fn synthesize(
        &self,
        settings: SynthesisSettings,
        independent_tpnp: impl Fn(usize) -> Option<TransientPreNoise>,
    ) -> Vec<DecodedAudio> {
        let mut synthesizer = Synthesizer::new();
        self.independent
            .iter()
            .zip(&self.dependent)
            .enumerate()
            .map(|(index, (main, extension))| {
                let mut transient_pre_noise = [None; 5];
                transient_pre_noise[1] = independent_tpnp(index);
                let substreams = [
                    SubstreamFrame {
                        coding_mode: AudioCodingMode::ThreeFrontStereoSurround,
                        positions: &self.independent_positions,
                        blocks: main,
                        compression: CompressionWords::default(),
                        transient_pre_noise,
                    },
                    SubstreamFrame {
                        coding_mode: AudioCodingMode::Stereo,
                        positions: &SURROUND,
                        blocks: extension,
                        compression: CompressionWords::default(),
                        transient_pre_noise: [None; 5],
                    },
                ];
                let layout = ProgramLayout::combine(&substreams).unwrap();
                synthesizer.synthesize(
                    ProgramFrame {
                        sample_rate_hz: 48_000,
                        substreams: &substreams,
                        layout: &layout,
                    },
                    settings,
                )
            })
            .collect()
    }
}

fn settings(dynamic_range: DynamicRangeControl) -> SynthesisSettings {
    SynthesisSettings {
        dynamic_range,
        transient_pre_noise: TransientPreNoiseProcessing::Apply,
    }
}

fn channel(audio: &[DecodedAudio], index: usize) -> Vec<f32> {
    audio
        .iter()
        .flat_map(|frame| frame.channel_samples(index).unwrap().to_vec())
        .collect()
}

#[test]
fn replaced_channels_carry_the_dependent_signal_exactly() {
    let program = Program::new();
    let audio = program.synthesize(settings(DynamicRangeControl::Disabled), |_| None);
    let surround = decode_alone("5p1-48k-384k.eac3");
    let stereo = decode_alone("stereo-48k-128k-cpl-high.eac3");
    assert_eq!(audio[0].channels(), program.independent_positions);
    // L, C, R и LFE — независимый; Ls/Rs — L/R стерео-кадров отсчёт в отсчёт.
    for index in [0, 1, 2, 5] {
        assert_eq!(channel(&audio, index), channel(&surround, index));
    }
    assert_eq!(channel(&audio, 3), channel(&stereo, 0));
    assert_eq!(channel(&audio, 4), channel(&stereo, 1));
}

/// Слова `dynrng` независимого substream-а игнорируются, слова последнего
/// зависимого действуют на все каналы (§3.8.5).
#[test]
fn only_the_last_dependent_dynamic_range_words_scale_the_program() {
    let mut program = Program::new();
    let reference = program.synthesize(settings(DynamicRangeControl::Standard), |_| None);
    for blocks in &mut program.independent {
        for block in blocks.iter_mut() {
            block.dynamic_range_word = QUARTER_GAIN_WORD;
        }
    }
    let independent_words = program.synthesize(settings(DynamicRangeControl::Standard), |_| None);
    assert_eq!(independent_words, reference);
    for blocks in &mut program.dependent {
        for block in blocks.iter_mut() {
            block.dynamic_range_word = QUARTER_GAIN_WORD;
        }
    }
    let dependent_words = program.synthesize(settings(DynamicRangeControl::Standard), |_| None);
    let unscaled = program.synthesize(settings(DynamicRangeControl::Disabled), |_| None);
    for index in 0..6 {
        let expected: Vec<f32> = channel(&unscaled, index)
            .iter()
            .map(|value| value * 0.25)
            .collect();
        assert_eq!(
            channel(&dependent_words, index),
            expected,
            "channel {index}"
        );
    }
}

/// TPNP независимого C на реальном сигнале: меняется ровно область
/// `[T − (PN + translen + 256), T)` выхода C и ничего больше; `Skip` — как без события.
#[test]
fn transient_pre_noise_rewrites_only_the_signalled_region_of_its_channel() {
    let program = Program::new();
    let event = TransientPreNoise {
        location_samples: 1000,
        length_samples: 50,
    };
    let plain = program.synthesize(settings(DynamicRangeControl::Disabled), |_| None);
    let processed = program.synthesize(settings(DynamicRangeControl::Disabled), |index| {
        (index == 2).then_some(event)
    });
    let skipped = program.synthesize(
        SynthesisSettings {
            dynamic_range: DynamicRangeControl::Disabled,
            transient_pre_noise: TransientPreNoiseProcessing::Skip,
        },
        |index| (index == 2).then_some(event),
    );
    assert_eq!(skipped, plain);
    // Транзиент: кадр 2, 1000 + 256 (начало кадра кодера); PN = 256 + 1256 mod 256.
    let transient = 2 * 1536 + 1000 + 256;
    let pre_noise = 256 + (1000 + 256) % 256;
    let start = transient - (pre_noise + 50 + 256);
    let shift = 256 + pre_noise - 50;
    let center = 1;
    assert_eq!(program.independent_positions[center], Center);
    for index in 0..6 {
        let (before, after) = (channel(&plain, index), channel(&processed, index));
        if index != center {
            assert_eq!(before, after, "channel {index}");
            continue;
        }
        assert_eq!(before[..start], after[..start]);
        assert_eq!(before[transient..], after[transient..]);
        // Середина области (после первого перехода, до последнего) — копия
        // исходного сигнала на `shift` отсчётов раньше.
        for sample in start + 256..transient - 128 {
            assert_eq!(after[sample], before[sample - shift], "sample {sample}");
        }
        assert_ne!(before[start + 300], after[start + 300]);
    }
}
