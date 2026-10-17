//! Синтез PCM одного access unit-а: программа из независимого substream-а
//! (или ядра AC-3) и его зависимых substream-ов.
//!
//! Для каждого блока и каждого выходного канала раскладки (§E.3.8.2,
//! `layout`): коэффициенты источника → rematrixing (§7.5, только 2/0 AC-3;
//! E-AC-3 делает его при разборе) → множитель DRC (§7.7, слова — по §E.3.8.5)
//! → IMDCT с окном (§7.9.4, шаги 1–5) → overlap-add (шаг 6). Затем — TPNP
//! во временной области (Annex E §3.7). Блоков в кадре 1, 2, 3 или 6
//! (E-AC-3) — все substream-ы программы имеют одно число блоков (§2.3.1.2,
//! проверено разбором).
//!
//! Синтез не может завершиться ошибкой: все проверки сделаны разбором и
//! сведением раскладки, поэтому кадр либо отвергается до синтеза, либо
//! целиком превращается в PCM.

use super::layout::{ChannelSource, CodedChannel, ProgramLayout};
use super::substream::SubstreamFrame;
use crate::output::DecodedAudio;
use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::syntax::audblk::output::{DecodedAudioBlock, TransformLength};
use crate::syntax::bsi_values::AudioCodingMode;
use crate::syntax::eac3::audfrm::TransientPreNoise;
use crate::syntax::syncframe::SAMPLES_PER_AUDIO_BLOCK;
use crate::synthesis::dynamic_range::{DynamicRangeControl, GainWords};
use crate::synthesis::overlap::OverlapBank;
use crate::synthesis::rematrix::{StereoPairBandwidth, restore_left_right};
use crate::synthesis::transform::imdct::{Imdct, TRANSFORM_LENGTH, WindowedBlock};
use crate::synthesis::transient_pre_noise::{
    PlanarFrame, TransientPreNoiseProcessing, TransientPreNoiseProcessor,
};

/// Индекс канала Ch2 в режиме 1+1 (Таблица 5.8: Ch1, Ch2).
const DUAL_MONO_SECOND_CHANNEL: usize = 1;

/// Коэффициенты одного канала блока.
type Coefficients = [f32; COEFFICIENTS_PER_BLOCK];

/// Как синтезировать (части [`crate::DecoderConfig`], нужные синтезу).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SynthesisSettings {
    pub(crate) dynamic_range: DynamicRangeControl,
    pub(crate) transient_pre_noise: TransientPreNoiseProcessing,
}

/// Кадр программы: substream-ы и сведённая из них раскладка.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProgramFrame<'a> {
    pub(crate) sample_rate_hz: u32,
    /// Substream-ы по порядку потока: независимый (ядро AC-3), затем зависимые.
    pub(crate) substreams: &'a [SubstreamFrame<'a>],
    pub(crate) layout: &'a ProgramLayout,
}

impl ProgramFrame<'_> {
    /// Блоков в кадре (у всех substream-ов одинаково; защитно — минимум).
    fn audio_blocks(&self) -> usize {
        self.substreams
            .iter()
            .map(|substream| substream.blocks.len())
            .min()
            .unwrap_or(0)
    }

    /// Substream, чьи `dynrng`/`compr` действуют на всю программу (§E.3.8.5):
    /// при воспроизведении полного микса слова независимого substream-а 0
    /// игнорируются, действуют слова ПОСЛЕДНЕГО зависимого («shall apply to
    /// all substreams in the program, including the independent substream»).
    fn gain_source(&self) -> Option<&SubstreamFrame<'_>> {
        self.substreams.last()
    }
}

/// Множители DRC блока для двух программ режима 1+1.
#[derive(Debug, Clone, Copy)]
struct BlockGains {
    /// Ch1 и все прочие каналы, включая LFE.
    first_program: f32,
    /// Ch2 режима 1+1.
    second_program: f32,
}

/// Синтезатор PCM: таблицы IMDCT, хвосты каналов, TPNP и рабочие буферы.
#[derive(Debug, Clone)]
pub(crate) struct Synthesizer {
    imdct: Imdct,
    overlaps: OverlapBank,
    transient_pre_noise: TransientPreNoiseProcessor,
    /// L/R после rematrixing по substream-ам текущего блока (только у
    /// substream-ов с флагами rematrixing).
    rematrixed: Vec<[Coefficients; 2]>,
    /// Коэффициенты выходного канала после DRC.
    scaled: Coefficients,
    windowed: WindowedBlock,
}

impl Synthesizer {
    pub(crate) fn new() -> Self {
        Self {
            imdct: Imdct::new(),
            overlaps: OverlapBank::default(),
            transient_pre_noise: TransientPreNoiseProcessor::new(),
            rematrixed: Vec::new(),
            scaled: [0.0; COEFFICIENTS_PER_BLOCK],
            windowed: [0.0; TRANSFORM_LENGTH],
        }
    }

    /// Забывает хвосты overlap-add и состояние TPNP (разрыв потока: ошибка
    /// кадра или `reset`).
    pub(crate) fn discard_overlap(&mut self) {
        self.overlaps.discard();
        self.transient_pre_noise.discard();
    }

    #[cfg(test)]
    pub(crate) const fn transient_pre_noise(&self) -> &TransientPreNoiseProcessor {
        &self.transient_pre_noise
    }

    /// Превращает коэффициенты кадра программы в PCM.
    pub(crate) fn synthesize(
        &mut self,
        program: ProgramFrame<'_>,
        settings: SynthesisSettings,
    ) -> DecodedAudio {
        let positions = program.layout.positions();
        self.overlaps
            .begin_frame(program.sample_rate_hz, &positions);
        let audio_blocks = program.audio_blocks();
        let samples_per_channel = audio_blocks * SAMPLES_PER_AUDIO_BLOCK;
        let mut planar = vec![0.0; positions.len() * samples_per_channel];
        if self.rematrixed.len() < program.substreams.len() {
            self.rematrixed
                .resize(program.substreams.len(), [[0.0; COEFFICIENTS_PER_BLOCK]; 2]);
        }
        for block_index in 0..audio_blocks {
            let gains = block_gains(&program, block_index, settings.dynamic_range);
            self.prepare_rematrixed(&program, block_index);
            let block_offset = block_index * SAMPLES_PER_AUDIO_BLOCK;
            for (output_index, channel) in program.layout.channels().iter().enumerate() {
                let Some(substream) = program.substreams.get(channel.source.substream) else {
                    continue;
                };
                let Some(block) = substream.blocks.get(block_index) else {
                    continue;
                };
                let gain = channel_gain(substream, channel.source.coded, gains);
                let transform = self.scale_source(channel.source, block, gain);
                self.imdct
                    .inverse_transform(transform, &self.scaled, &mut self.windowed);
                let start = output_index * samples_per_channel + block_offset;
                if let Some(pcm) = planar.get_mut(start..start + SAMPLES_PER_AUDIO_BLOCK) {
                    self.overlaps.overlap_add(output_index, &self.windowed, pcm);
                }
            }
        }
        if settings.transient_pre_noise == TransientPreNoiseProcessing::Apply {
            let events = transient_events(&program);
            self.transient_pre_noise.process_frame(
                PlanarFrame {
                    sample_rate_hz: program.sample_rate_hz,
                    positions: &positions,
                    samples_per_channel,
                    planar: &mut planar,
                },
                &events,
            );
        }
        DecodedAudio::from_planar(
            program.sample_rate_hz,
            positions,
            samples_per_channel,
            planar,
        )
    }

    /// Rematrixing L/R блока у substream-ов, где он передан (§7.5).
    fn prepare_rematrixed(&mut self, program: &ProgramFrame<'_>, block_index: usize) {
        for (substream, pair) in program.substreams.iter().zip(&mut self.rematrixed) {
            let Some(block) = substream.blocks.get(block_index) else {
                continue;
            };
            let Some(flags) = block.rematrix.as_ref() else {
                continue;
            };
            let [left_spectrum, right_spectrum, ..] = &block.full_bandwidth;
            let [left, right] = pair;
            *left = left_spectrum.coefficients;
            *right = right_spectrum.coefficients;
            restore_left_right(
                left,
                right,
                flags,
                StereoPairBandwidth {
                    coupling_begin_subband: block.coupling_begin_subband,
                    left_end_bin: left_spectrum.end_bin,
                    right_end_bin: right_spectrum.end_bin,
                },
            );
        }
    }

    /// Копирует коэффициенты источника в `scaled` с множителем DRC (до
    /// `end_bin`, дальше нули) и возвращает длину преобразования.
    fn scale_source(
        &mut self,
        source: ChannelSource,
        block: &DecodedAudioBlock,
        gain: f32,
    ) -> TransformLength {
        let (coefficients, spectrum) = match source.coded {
            CodedChannel::FullBandwidth(channel) => {
                let Some(spectrum) = block.full_bandwidth.get(channel) else {
                    self.scaled = [0.0; COEFFICIENTS_PER_BLOCK];
                    return TransformLength::Long;
                };
                let rematrixed = block
                    .rematrix
                    .is_some()
                    .then(|| self.rematrixed.get(source.substream))
                    .flatten()
                    .and_then(|pair| pair.get(channel));
                (rematrixed.unwrap_or(&spectrum.coefficients), spectrum)
            }
            // LFE: только длинное преобразование (blksw для LFE нет, §5.4.3.1).
            CodedChannel::LowFrequencyEffects => (&block.lfe.coefficients, &block.lfe),
        };
        self.scaled = *coefficients;
        if gain != 1.0 {
            for coefficient in self.scaled.iter_mut().take(spectrum.end_bin) {
                *coefficient *= gain;
            }
        }
        match source.coded {
            CodedChannel::FullBandwidth(_) => spectrum.transform,
            CodedChannel::LowFrequencyEffects => TransformLength::Long,
        }
    }
}

/// Множители DRC блока по словам substream-а из [`ProgramFrame::gain_source`].
fn block_gains(
    program: &ProgramFrame<'_>,
    block_index: usize,
    dynamic_range: DynamicRangeControl,
) -> BlockGains {
    let Some(source) = program.gain_source() else {
        return BlockGains {
            first_program: 1.0,
            second_program: 1.0,
        };
    };
    let (first_word, second_word) = source.blocks.get(block_index).map_or((0, 0), |block| {
        (block.dynamic_range_word, block.dual_mono_dynamic_range_word)
    });
    let first_program = dynamic_range.gain(GainWords {
        dynamic_range: first_word,
        heavy_compression: source.compression.first_program,
    });
    // Слова Ch2 есть только у программы 1+1; иначе Ch2 1+1 какого-то
    // другого substream-а (нестандартная комбинация) следует слову Ch1.
    let second_program = if source.coding_mode == AudioCodingMode::DualMono {
        dynamic_range.gain(GainWords {
            dynamic_range: second_word,
            heavy_compression: source.compression.second_program,
        })
    } else {
        first_program
    };
    BlockGains {
        first_program,
        second_program,
    }
}

/// Множитель канала: Ch2 режима 1+1 — своя программа, остальные — Ch1.
fn channel_gain(substream: &SubstreamFrame<'_>, coded: CodedChannel, gains: BlockGains) -> f32 {
    let second_dual_mono_channel = substream.coding_mode == AudioCodingMode::DualMono
        && coded == CodedChannel::FullBandwidth(DUAL_MONO_SECOND_CHANNEL);
    if second_dual_mono_channel {
        gains.second_program
    } else {
        gains.first_program
    }
}

/// Параметры TPNP источника каждого выходного канала (LFE — без TPNP).
fn transient_events(program: &ProgramFrame<'_>) -> Vec<Option<TransientPreNoise>> {
    program
        .layout
        .channels()
        .iter()
        .map(|channel| match channel.source.coded {
            CodedChannel::FullBandwidth(coded) => program
                .substreams
                .get(channel.source.substream)
                .and_then(|substream| substream.transient_pre_noise.get(coded).copied())
                .flatten(),
            CodedChannel::LowFrequencyEffects => None,
        })
        .collect()
}

#[cfg(test)]
impl Synthesizer {
    /// Синтез одиночного кадра AC-3 по его коэффициентам (тестам D2 нужно
    /// синтезировать изменённые коэффициенты в обход разбора).
    pub(crate) fn synthesize_ac3_frame(
        &mut self,
        info: &crate::syntax::syncframe::SyncframeInfo,
        blocks: &[DecodedAudioBlock],
        dynamic_range: DynamicRangeControl,
    ) -> DecodedAudio {
        let positions =
            crate::output::ac3_channel_positions(info.bsi.audio_coding_mode, info.bsi.lfe_on);
        let substreams = [SubstreamFrame::from_ac3(info, &positions, blocks)];
        let layout = ProgramLayout::combine(&substreams).unwrap();
        self.synthesize(
            ProgramFrame {
                sample_rate_hz: info.sample_rate_hz,
                substreams: &substreams,
                layout: &layout,
            },
            SynthesisSettings {
                dynamic_range,
                transient_pre_noise: TransientPreNoiseProcessing::Skip,
            },
        )
    }
}
