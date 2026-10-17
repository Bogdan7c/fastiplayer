//! Публичный декодер access unit-ов AC-3 и E-AC-3.
//!
//! Один экземпляр декодирует оба синтаксиса: вид каждого access unit-а
//! определяется по `bsid` его первого кадра (≤ 10 — AC-3, 11–16 — E-AC-3),
//! поэтому переход AC-3 ↔ E-AC-3 между access unit-ами (одна дорожка TS)
//! и смена формата (частота, раскладка, число блоков) разрешены — формат
//! каждого кадра сообщает [`DecodedAudio`].
//!
//! Access unit — один кадр AC-3, или независимый substream 0 E-AC-3 (или
//! ядро AC-3, как на Blu-ray) с его зависимыми substream-ами; кадры других
//! независимых программ (substreamid 1–7) пропускаются (Annex E §3.8.1).
//!
//! Владелец состояния между кадрами: генератор dither-а, разборщик access
//! unit-ов (буферы substream-ов и генератор шума SPX) и синтезатор (хвосты
//! overlap-add §7.9.4, история TPNP Annex E §3.7). Всё остальное живёт
//! внутри одного кадра (A/52 §7.1.2).
//!
//! Порядок: CRC и полный разбор всех substream-ов программы до
//! коэффициентов → сведение раскладки (§3.8.2) → синтез. Синтез начинается
//! только после успешного разбора всего access unit-а, поэтому отвергнутый
//! access unit не оставляет «половины» PCM; любая ошибка считается разрывом
//! потока и обнуляет хвосты overlap-add и состояние TPNP.

use crate::config::DecoderConfig;
use crate::error::Ac3DecodeError;
use crate::output::DecodedAudio;
use crate::spectral::dither::DitherGenerator;
use crate::syntax::eac3::access_unit::EnhancedAccessUnitParser;
use crate::synthesis::program::frame::{ProgramFrame, SynthesisSettings, Synthesizer};
use crate::synthesis::program::layout::ProgramLayout;
use crate::synthesis::program::substream::SubstreamFrame;

/// Декодер AC-3 и E-AC-3.
#[derive(Debug, Clone)]
pub struct Ac3Decoder {
    config: DecoderConfig,
    dither: DitherGenerator,
    parser: EnhancedAccessUnitParser,
    synthesizer: Box<Synthesizer>,
}

impl Ac3Decoder {
    /// Создаёт декодер с заданными настройками.
    #[must_use]
    pub fn new(config: DecoderConfig) -> Self {
        Self {
            config,
            dither: DitherGenerator::new(config.dither),
            parser: EnhancedAccessUnitParser::new(config.crc),
            synthesizer: Box::new(Synthesizer::new()),
        }
    }

    /// Сбрасывает межкадровое состояние (после seek или смены потока):
    /// генераторы dither-а и шума SPX возвращаются к зерну, хвосты
    /// overlap-add и история TPNP обнуляются — следующий access unit
    /// декодируется как первый, и тот же вход даёт тот же PCM.
    pub fn reset(&mut self) {
        self.dither.reset();
        self.parser.reset();
        self.synthesizer.discard_overlap();
    }

    /// Декодирует один access unit (кадр AC-3 или программу 0 E-AC-3) в PCM.
    ///
    /// Выход: 256 × число блоков (6 у AC-3; 1, 2, 3 или 6 у E-AC-3) сэмплов
    /// на канал, позиции каналов — после сведения substream-ов. Первый
    /// access unit после создания, `reset` или ошибки складывается с нулевыми
    /// хвостами (первые 256 сэмплов — нарастающая половина окна); декодер
    /// ничего не обрезает и не задерживает. При ошибке хвосты обнуляются.
    pub fn decode_access_unit(
        &mut self,
        access_unit: &[u8],
    ) -> Result<DecodedAudio, Ac3DecodeError> {
        let result = self.decode_program(access_unit);
        if result.is_err() {
            self.synthesizer.discard_overlap();
        }
        result
    }

    fn decode_program(&mut self, access_unit: &[u8]) -> Result<DecodedAudio, Ac3DecodeError> {
        let parsed = self.parser.parse(access_unit, &mut self.dither)?;
        let sample_rate_hz = parsed.sample_rate_hz();
        let substreams: Vec<SubstreamFrame<'_>> = parsed
            .substreams()
            .map(|substream| SubstreamFrame::from_parsed(&substream))
            .collect();
        let layout = ProgramLayout::combine(&substreams)?;
        Ok(self.synthesizer.synthesize(
            ProgramFrame {
                sample_rate_hz,
                substreams: &substreams,
                layout: &layout,
            },
            SynthesisSettings {
                dynamic_range: self.config.dynamic_range,
                transient_pre_noise: self.config.transient_pre_noise,
            },
        ))
    }
}

#[cfg(test)]
impl Ac3Decoder {
    /// Проверяет CRC по политике и разбирает ОДИН кадр AC-3 до коэффициентов
    /// (они остаются в буфере substream-а 0, см. [`Self::decoded_blocks`]).
    /// Тесты пакетов D1/D2: учёт бит и коэффициенты отдельного кадра.
    pub(crate) fn decode_coefficients(
        &mut self,
        access_unit: &[u8],
    ) -> Result<crate::syntax::syncframe::DecodedSyncframe, Ac3DecodeError> {
        use crate::bitstream::crc::verify_syncframe_crcs;
        use crate::config::CrcPolicy;
        use crate::syntax::syncframe::{decode_ac3_syncframe, frame_words};
        let words = frame_words(access_unit)?;
        let frame = access_unit.get(..words * 2).unwrap_or(access_unit);
        if self.config.crc == CrcPolicy::Verify {
            verify_syncframe_crcs(frame, words)
                .map_err(|region| Ac3DecodeError::CrcMismatch { region })?;
        }
        decode_ac3_syncframe(frame, &mut self.dither, self.parser.primary_blocks_mut())
    }

    /// Коэффициенты substream-а 0 последнего разбора (после ошибки
    /// содержимое не определено).
    pub(crate) fn decoded_blocks(
        &self,
    ) -> &[crate::syntax::audblk::output::DecodedAudioBlock;
         crate::syntax::syncframe::AUDIO_BLOCKS_PER_SYNCFRAME] {
        self.parser.primary_blocks()
    }

    /// Спектры выходных каналов последнего access unit-а (`[канал][блок]`,
    /// до DRC) и множитель DRC каждого блока по политике декодера (слова —
    /// по §E.3.8.5). Dev-сверке с ffmpeg нужны длины преобразований и бины.
    pub(crate) fn program_spectra(&self) -> ProgramSpectra {
        let parsed = self.parser.last_access_unit();
        let substreams: Vec<SubstreamFrame<'_>> = parsed
            .substreams()
            .map(|substream| SubstreamFrame::from_parsed(&substream))
            .collect();
        let layout = ProgramLayout::combine(&substreams).unwrap();
        let blocks = substreams.iter().map(|s| s.blocks.len()).min().unwrap_or(0);
        let channels = layout
            .channels()
            .iter()
            .map(|channel| {
                let substream = &substreams[channel.source.substream];
                substream.blocks[..blocks]
                    .iter()
                    .map(|block| match channel.source.coded {
                        crate::synthesis::program::layout::CodedChannel::FullBandwidth(index) => {
                            block.full_bandwidth[index]
                        }
                        crate::synthesis::program::layout::CodedChannel::LowFrequencyEffects => {
                            block.lfe
                        }
                    })
                    .collect()
            })
            .collect();
        let source = substreams.last().unwrap();
        let gains = source.blocks[..blocks]
            .iter()
            .map(|block| {
                self.config
                    .dynamic_range
                    .gain(crate::synthesis::dynamic_range::GainWords {
                        dynamic_range: block.dynamic_range_word,
                        heavy_compression: source.compression.first_program,
                    })
            })
            .collect();
        let uses_aht = parsed
            .substreams()
            .any(|substream| substream.tools.aht_channels > 0);
        ProgramSpectra {
            channels,
            gains,
            uses_aht,
        }
    }

    /// Другое зерно шума SPX (см. `EnhancedAccessUnitParser::reseed_spx_noise`).
    pub(crate) fn reseed_spx_noise(&mut self, seed: u32) {
        self.parser.reseed_spx_noise(seed);
    }

    /// Счётчики TPNP синтезатора.
    pub(crate) fn transient_pre_noise_counters(
        &self,
    ) -> crate::synthesis::transient_pre_noise::TransientPreNoiseCounters {
        self.synthesizer.transient_pre_noise().counters()
    }
}

/// Спектры программы последнего access unit-а (см. `Ac3Decoder::program_spectra`).
#[cfg(test)]
pub(crate) struct ProgramSpectra {
    /// `[выходной канал][блок]`.
    pub(crate) channels: Vec<Vec<crate::syntax::audblk::output::ChannelSpectrum>>,
    /// Множитель DRC блока (Ch1).
    pub(crate) gains: Vec<f32>,
    /// Хоть один набор каналов access unit-а кодирован AHT.
    pub(crate) uses_aht: bool,
}
