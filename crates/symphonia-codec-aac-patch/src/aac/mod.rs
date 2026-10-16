// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// Previous Author: Kostya Shishkov <kostya.shiskov@gmail.com>
//
// This source file includes code originally written for the NihAV
// project. With the author's permission, it has been relicensed for,
// and ported to the Symphonia project.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioSpec, Channels, GenericAudioBufferRef, layouts,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia_core::codecs::audio::{AudioDecoder, FinalizeResult};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};
use symphonia_core::packet::PacketRef;
use symphonia_core::{codec_profile, support_audio_codec};

use symphonia_common::mpeg::audio::{
    AudioObjectType, AudioSpecificConfig, Mpeg4AudioChannels, get_mpeg4_audio_channels_by_config_index,
};

mod codebooks;
mod common;
mod cpe;
mod dsp;
mod ics;
mod pce;
mod window;

use common::*;

/// Тип одного channel element в AAC raw_data_block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AacChannelElementKind {
    /// Single Channel Element для center/rear-center.
    Single,
    /// Channel Pair Element для left/right пары.
    Pair,
    /// Low Frequency Effects element.
    LowFrequencyEffects,
}

impl AacChannelElementKind {
    /// Возвращает отдельное пространство 4-bit tags для каждого типа element-а.
    const fn tag_namespace_index(self) -> usize {
        match self {
            Self::Single => 0,
            Self::Pair => 1,
            Self::LowFrequencyEffects => 2,
        }
    }
}

/// Каноническая destination plane для одного AAC channel element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AacChannelElement {
    /// Ожидаемый тип coded element-а.
    kind: AacChannelElementKind,
    /// Первая destination plane; pair занимает также следующую plane.
    first_plane: usize,
}

impl AacChannelElement {
    /// Создаёт mapping для Single Channel Element.
    const fn single_at_plane(first_plane: usize) -> Self {
        Self {
            kind: AacChannelElementKind::Single,
            first_plane,
        }
    }

    /// Создаёт mapping для Channel Pair Element.
    const fn pair_at_plane(first_plane: usize) -> Self {
        Self {
            kind: AacChannelElementKind::Pair,
            first_plane,
        }
    }

    /// Создаёт mapping для LFE element-а.
    const fn lfe_at_plane(first_plane: usize) -> Self {
        Self {
            kind: AacChannelElementKind::LowFrequencyEffects,
            first_plane,
        }
    }
}

/// AAC coded element order → canonical Symphonia plane mapping.
const AAC_MONO_ELEMENTS: &[AacChannelElement] = &[AacChannelElement::single_at_plane(0)];
const AAC_STEREO_ELEMENTS: &[AacChannelElement] = &[AacChannelElement::pair_at_plane(0)];
const AAC_3P0_ELEMENTS: &[AacChannelElement] = &[
    AacChannelElement::single_at_plane(2),
    AacChannelElement::pair_at_plane(0),
];
const AAC_4P0_ELEMENTS: &[AacChannelElement] = &[
    AacChannelElement::single_at_plane(2),
    AacChannelElement::pair_at_plane(0),
    AacChannelElement::single_at_plane(3),
];
const AAC_QUADRAPHONIC_ELEMENTS: &[AacChannelElement] = &[
    AacChannelElement::pair_at_plane(0),
    AacChannelElement::pair_at_plane(2),
];
const AAC_5P0_ELEMENTS: &[AacChannelElement] = &[
    AacChannelElement::single_at_plane(2),
    AacChannelElement::pair_at_plane(0),
    AacChannelElement::pair_at_plane(3),
];
const AAC_5P1_ELEMENTS: &[AacChannelElement] = &[
    AacChannelElement::single_at_plane(2),
    AacChannelElement::pair_at_plane(0),
    AacChannelElement::pair_at_plane(4),
    AacChannelElement::lfe_at_plane(3),
];
const AAC_7P1_ELEMENTS: &[AacChannelElement] = &[
    AacChannelElement::single_at_plane(2),
    AacChannelElement::pair_at_plane(6),
    AacChannelElement::pair_at_plane(0),
    AacChannelElement::pair_at_plane(4),
    AacChannelElement::lfe_at_plane(3),
];

/// Возвращает mapping стандартного AAC channel configuration.
fn canonical_aac_channel_elements(channels: &Channels) -> Option<&'static [AacChannelElement]> {
    if channels == &layouts::CHANNEL_LAYOUT_MONO {
        Some(AAC_MONO_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_STEREO {
        Some(AAC_STEREO_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_AAC_3P0 {
        Some(AAC_3P0_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_AAC_4P0 {
        Some(AAC_4P0_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_AAC_QUADRAPHONIC {
        Some(AAC_QUADRAPHONIC_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_AAC_5P0 {
        Some(AAC_5P0_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_AAC_5P1 {
        Some(AAC_5P1_ELEMENTS)
    } else if channels == &layouts::CHANNEL_LAYOUT_AAC_7P1 {
        Some(AAC_7P1_ELEMENTS)
    } else {
        None
    }
}

/// Per-frame cursor стандартного indexed AAC channel configuration.
///
/// Для `channel_configuration != 0` пространственную роль задаёт coded position,
/// а `element_instance_tag` только идентифицирует element внутри его type namespace.
/// Поэтому tag проверяется на повтор, но не выбирает destination plane.
struct IndexedChannelElementCursor {
    configured_elements: &'static [AacChannelElement],
    next_coded_element_index: usize,
    decoded_tags_by_kind: [u16; 3],
}

impl IndexedChannelElementCursor {
    /// Начинает проверку одного raw_data_block без сохранения packet-local state в decoder-е.
    fn new(configured_elements: &'static [AacChannelElement]) -> Self {
        Self {
            configured_elements,
            next_coded_element_index: 0,
            decoded_tags_by_kind: [0; 3],
        }
    }

    /// Сопоставляет следующий coded element с canonical destination по позиции.
    fn consume(
        &mut self,
        actual_kind: AacChannelElementKind,
        element_instance_tag: u32,
    ) -> Result<usize> {
        let Some(configured_element) = self
            .configured_elements
            .get(self.next_coded_element_index)
        else {
            return decode_error("aac: too many channel elements for indexed layout");
        };
        if configured_element.kind != actual_kind {
            return decode_error("aac: channel element order does not match indexed layout");
        }
        if element_instance_tag >= 16 {
            return decode_error("aac: channel element tag exceeds its 4-bit field");
        }

        let tag_namespace = actual_kind.tag_namespace_index();
        let tag_bit = 1_u16 << element_instance_tag;
        if self.decoded_tags_by_kind[tag_namespace] & tag_bit != 0 {
            return decode_error("aac: duplicate channel element tag in one frame");
        }

        self.decoded_tags_by_kind[tag_namespace] |= tag_bit;
        let configured_element_index = self.next_coded_element_index;
        self.next_coded_element_index += 1;
        Ok(configured_element_index)
    }

    /// Не позволяет синтезировать неполный configured layout.
    fn finish(self) -> Result<()> {
        if self.next_coded_element_index != self.configured_elements.len() {
            return decode_error("aac: decoded channel elements do not fill configured layout");
        }
        Ok(())
    }
}

/// Advanced Audio Coding (AAC) decoder.
///
/// Implements a decoder for Advanced Audio Decoding Low-Complexity (AAC-LC) as defined in
/// ISO/IEC 13818-7 and ISO/IEC 14496-3.
pub struct AacDecoder {
    // info: NACodecInfoRef,
    asc: AudioSpecificConfig,
    pairs: Vec<cpe::ChannelPair>,
    dsp: dsp::Dsp,
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    /// Coded AAC element order → canonical Symphonia plane mapping.
    channel_elements: &'static [AacChannelElement],
}

impl AacDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        // This decoder only supports AAC.
        if params.codec != CODEC_ID_AAC {
            return unsupported_error("aac: invalid codec");
        }

        // If extra data present, parse the audio specific config
        let asc = if let Some(extra_data_buf) = &params.extra_data {
            validate!(extra_data_buf.len() >= 2);
            // fastiplayer patch: `channel_configuration == 0` + PCE приводится к эквивалентной
            // индексной конфигурации 1..=7 до разбора ASC (upstream такой ASC отвергает).
            let indexed_extra_data = pce::resolve_indexed_audio_specific_config(extra_data_buf)?;
            AudioSpecificConfig::read(&indexed_extra_data)?
        }
        else {
            // Otherwise, assume there is no ASC and use the codec parameters for ADTS.
            let mut asc = AudioSpecificConfig::default();

            asc.object_type = AudioObjectType::Lc;
            asc.samples = 1024;

            asc.sample_rate = match params.sample_rate {
                Some(rate) => rate,
                None => return unsupported_error("aac: sample rate is required"),
            };

            asc.channels = params.channels.clone();

            asc
        };

        // The channel configuration must be known.
        //
        // PCE из ASC уже свёрнут в индексную конфигурацию выше; PCE внутри пакетов
        // сверяется с этой раскладкой в `verify_in_band_program_config`.
        let channels = match &asc.channels {
            Some(channels) => channels.clone(),
            _ => return unsupported_error("aac: channels or channel layout is required"),
        };
        let channel_elements = match canonical_aac_channel_elements(&channels) {
            Some(channel_elements) => channel_elements,
            None => return unsupported_error("aac: unsupported channel layout"),
        };

        // Check complexity.
        //
        // fastiplayer patch: убрали ограничение `channels.count() > 2` и явно
        // сопоставляем AAC element tags с canonical Symphonia planes. Поэтому
        // multichannel synthesis не путает coded AAC order с buffer lane order.
        if asc.object_type != AudioObjectType::Lc || asc.sbr_present || asc.samples != 1024 {
            return unsupported_error("aac: aac too complex");
        }

        // Clone and amend the codec parameters with information from the extra data.
        let mut params = params.clone();

        params.with_channels(channels.clone()).with_sample_rate(asc.sample_rate);

        let sbinfo = GASubbandInfo::find(asc.sample_rate);

        let buf = AudioBuffer::new(AudioSpec::new(asc.sample_rate, channels), asc.samples);
        let pairs = channel_elements
            .iter()
            .map(|element| {
                cpe::ChannelPair::new(
                    element.kind == AacChannelElementKind::Pair,
                    element.first_plane,
                    sbinfo,
                )
            })
            .collect();

        Ok(AacDecoder {
            asc,
            pairs,
            dsp: dsp::Dsp::new(),
            params,
            buf,
            channel_elements,
        })
    }

    /// Проверяет, что in-band PCE описывает ту же раскладку, с которой создан декодер.
    ///
    /// Совпадение — no-op; конфликт — `Unsupported`, потому что смена раскладки посреди
    /// потока потребовала бы пересоздать декодер (это решает вызывающий код).
    fn verify_in_band_program_config(&self, program_config: &pce::ProgramConfig) -> Result<()> {
        let index = program_config.indexed_channel_configuration()?;
        let announced_elements = match get_mpeg4_audio_channels_by_config_index(u32::from(index)) {
            Mpeg4AudioChannels::Channels(channels) => canonical_aac_channel_elements(&channels),
            _ => None,
        };
        if announced_elements != Some(self.channel_elements) {
            return unsupported_error("aac: in-band program config conflicts with configured layout");
        }
        Ok(())
    }

    fn decode_ga<B: ReadBitsLtr + FiniteBitStream>(&mut self, bs: &mut B) -> Result<()> {
        let mut channel_element_cursor =
            IndexedChannelElementCursor::new(self.channel_elements);
        // Точка отсчёта для byte_alignment() внутри in-band PCE: начало raw_data_block.
        let block_start_bits_left = bs.bits_left();
        while bs.bits_left() > 3 {
            let id = bs.read_bits_leq32(3)?;

            match id {
                0 => {
                    // ID_SCE
                    let tag = bs.read_bits_leq32(4)?;
                    let channel_element_index = channel_element_cursor
                        .consume(AacChannelElementKind::Single, tag)?;
                    self.pairs[channel_element_index]
                        .decode_ga_sce(bs, self.asc.object_type)?;
                }
                1 => {
                    // ID_CPE
                    let tag = bs.read_bits_leq32(4)?;
                    let channel_element_index = channel_element_cursor
                        .consume(AacChannelElementKind::Pair, tag)?;
                    self.pairs[channel_element_index]
                        .decode_ga_cpe(bs, self.asc.object_type)?;
                }
                2 => {
                    // ID_CCE
                    return unsupported_error("aac: coupling channel element");
                }
                3 => {
                    // ID_LFE
                    let tag = bs.read_bits_leq32(4)?;
                    let channel_element_index = channel_element_cursor
                        .consume(AacChannelElementKind::LowFrequencyEffects, tag)?;
                    self.pairs[channel_element_index]
                        .decode_ga_sce(bs, self.asc.object_type)?;
                }
                4 => {
                    // ID_DSE
                    let _id = bs.read_bits_leq32(4)?;
                    let align = bs.read_bool()?;
                    let mut count = bs.read_bits_leq32(8)?;
                    if count == 255 {
                        count += bs.read_bits_leq32(8)?;
                    }
                    if align {
                        bs.realign(); // ????
                    }
                    bs.ignore_bits(count * 8)?; // no SBR payload or such
                }
                5 => {
                    // ID_PCE: раскладка уже задана конфигурацией, поэтому совпадающий PCE
                    // только пропускается (с корректным продвижением по битам).
                    let program_config =
                        pce::read_program_config_element(bs, block_start_bits_left)?;
                    self.verify_in_band_program_config(&program_config)?;
                }
                6 => {
                    // ID_FIL
                    let mut count = bs.read_bits_leq32(4)? as usize;
                    if count == 15 {
                        count += bs.read_bits_leq32(8)? as usize;
                        count -= 1;
                    }

                    // Check if the ID_FIL element contains SBR data. Note that ID_FIL elements with
                    // SBR data may not contain other extension payloads.
                    if count > 0 {
                        let ext_type = bs.read_bits_leq32(4)?;

                        match ext_type {
                            // EXT_SBR_DATA (0xd)
                            // EXT_SBR_DATA_CRC (0xe)
                            0xd | 0xe => self.asc.sbr_present = true,
                            // EXT_FILL (0x0)
                            // EXT_FILL_DATA (0x1)
                            // EXT_DATA_ELEMENT (0x2)
                            // EXT_DYNAMIC_RANGE (0xb)
                            // EXT_SAC_DATA (0xc)
                            _ => (),
                        }

                        // Ignore extension payload(s).
                        bs.ignore_bits(4)?;
                        for _ in 0..count - 1 {
                            bs.ignore_bits(8)?;
                        }
                    }
                }
                7 => {
                    // ID_TERM
                    break;
                }
                _ => unreachable!(),
            };
        }
        channel_element_cursor.finish()?;
        let rate_idx = GASubbandInfo::find_idx(self.asc.sample_rate);
        for pair in &mut self.pairs {
            pair.synth_audio(&mut self.dsp, &mut self.buf, rate_idx);
        }
        Ok(())
    }

    // fn flush(&mut self) {
    //     for pair in self.pairs.iter_mut() {
    //         pair.ics[0].delay = [0.0; 1024];
    //         pair.ics[1].delay = [0.0; 1024];
    //     }
    // }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        // Clear the audio output buffer.
        self.buf.clear();
        self.buf.render_uninit(None);

        let mut bs = BitReaderLtr::new(packet.data);

        // Choose decode step based on the object type.
        match self.asc.object_type {
            AudioObjectType::Lc => self.decode_ga(&mut bs)?,
            _ => return unsupported_error("aac: object type"),
        }

        Ok(())
    }
}

impl AudioDecoder for AacDecoder {
    fn reset(&mut self) {
        for pair in self.pairs.iter_mut() {
            pair.reset();
        }
    }

    fn codec_info(&self) -> &CodecInfo {
        // Only one codec is supported.
        &Self::supported_codecs().first().unwrap().info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        }
        else {
            Ok(self.buf.as_generic_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for AacDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(AacDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        use symphonia_core::codecs::audio::well_known::profiles::CODEC_PROFILE_AAC_LC;

        &[support_audio_codec!(
            CODEC_ID_AAC,
            "aac",
            "Advanced Audio Coding",
            &[codec_profile!(CODEC_PROFILE_AAC_LC, "aac-lc", "Low Complexity"),]
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Разворачивает SCE/CPE/LFE mapping в coded-lane → canonical-plane list.
    fn expanded_destination_planes(elements: &[AacChannelElement]) -> Vec<usize> {
        let mut planes = Vec::new();
        for element in elements {
            planes.push(element.first_plane);
            if element.kind == AacChannelElementKind::Pair {
                planes.push(element.first_plane + 1);
            }
        }
        planes
    }

    #[test]
    fn standard_multichannel_configs_map_coded_order_to_canonical_planes() {
        let cases = [
            (&layouts::CHANNEL_LAYOUT_AAC_3P0, vec![2, 0, 1]),
            (&layouts::CHANNEL_LAYOUT_AAC_4P0, vec![2, 0, 1, 3]),
            (&layouts::CHANNEL_LAYOUT_AAC_5P0, vec![2, 0, 1, 3, 4]),
            (
                &layouts::CHANNEL_LAYOUT_AAC_5P1,
                vec![2, 0, 1, 4, 5, 3],
            ),
            (
                &layouts::CHANNEL_LAYOUT_AAC_7P1,
                vec![2, 6, 7, 0, 1, 4, 5, 3],
            ),
        ];

        for (layout, expected_planes) in cases {
            let elements = canonical_aac_channel_elements(layout).unwrap();
            assert_eq!(expanded_destination_planes(elements), expected_planes);
        }
    }

    #[test]
    fn five_point_one_accepts_arbitrary_unique_tags_without_changing_destinations() {
        let elements = canonical_aac_channel_elements(&layouts::CHANNEL_LAYOUT_AAC_5P1).unwrap();
        let coded_elements = [
            (AacChannelElementKind::Single, 15),
            (AacChannelElementKind::Pair, 9),
            (AacChannelElementKind::Pair, 3),
            (AacChannelElementKind::LowFrequencyEffects, 12),
        ];
        let mut cursor = IndexedChannelElementCursor::new(elements);
        let destination_planes = coded_elements
            .into_iter()
            .map(|(kind, tag)| {
                let element_index = cursor.consume(kind, tag).unwrap();
                elements[element_index].first_plane
            })
            .collect::<Vec<_>>();

        cursor.finish().unwrap();
        assert_eq!(destination_planes, vec![2, 0, 4, 3]);
    }

    #[test]
    fn duplicate_type_tag_is_rejected_before_synthesis() {
        let mut cursor = IndexedChannelElementCursor::new(AAC_5P1_ELEMENTS);
        cursor.consume(AacChannelElementKind::Single, 7).unwrap();
        cursor.consume(AacChannelElementKind::Pair, 11).unwrap();

        assert!(cursor.consume(AacChannelElementKind::Pair, 11).is_err());
    }

    #[test]
    fn indexed_layout_rejects_wrong_order_missing_and_extra_elements() {
        let mut wrong_order_cursor = IndexedChannelElementCursor::new(AAC_5P1_ELEMENTS);
        assert!(
            wrong_order_cursor
                .consume(AacChannelElementKind::Pair, 5)
                .is_err()
        );
        assert_eq!(
            wrong_order_cursor
                .consume(AacChannelElementKind::Single, 5)
                .unwrap(),
            0
        );

        let missing_element_cursor = IndexedChannelElementCursor::new(AAC_STEREO_ELEMENTS);
        assert!(missing_element_cursor.finish().is_err());

        let mut extra_element_cursor = IndexedChannelElementCursor::new(AAC_MONO_ELEMENTS);
        extra_element_cursor
            .consume(AacChannelElementKind::Single, 1)
            .unwrap();
        assert!(
            extra_element_cursor
                .consume(AacChannelElementKind::Single, 2)
                .is_err()
        );
    }

    /// Декодер, созданный из реального ASC ffmpeg: channelConfiguration 0 + PCE (5.1).
    fn decoder_from_ffmpeg_pce_asc() -> AacDecoder {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_extra_data(Box::new(pce::tests::FFMPEG_PCE_5P1_EXTRA_DATA));
        AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap()
    }

    /// Собирает raw_data_block: ID_PCE с заданными списками element-ов и ID_TERM.
    fn raw_data_block_with_pce(front: &[bool], side: &[bool], back: &[bool], lfe: u32) -> Vec<u8> {
        let mut writer = pce::tests::BitWriter::default();
        writer.push(5, 3);
        pce::tests::pce_bits(&mut writer, front, side, back, lfe, 0, 0);
        writer.push(7, 3);
        writer.into_bytes()
    }

    fn decode_block(decoder: &mut AacDecoder, block: &[u8]) -> Result<()> {
        use symphonia_core::units::{Duration, Timestamp};
        let packet = PacketRef::new(0, Timestamp::ZERO, Duration::ZERO, block);
        decoder.decode_ref(&packet).map(|_| ())
    }

    #[test]
    fn pce_asc_creates_five_point_one_decoder_with_six_planes() {
        let decoder = decoder_from_ffmpeg_pce_asc();

        assert_eq!(decoder.channel_elements, AAC_5P1_ELEMENTS);
        assert_eq!(decoder.codec_params().sample_rate, Some(48_000));
        assert_eq!(decoder.codec_params().channels.as_ref().map(Channels::count), Some(6));
    }

    #[test]
    fn matching_in_band_pce_is_skipped_and_decoding_continues_to_layout_check() {
        let mut decoder = decoder_from_ffmpeg_pce_asc();
        // PCE 5.1 + ID_TERM, но без самих channel elements: PCE пропущен без ошибки разбора,
        // а отказ приходит уже от проверки полноты раскладки (decode error, не Unsupported).
        let block = raw_data_block_with_pce(&[false, true], &[], &[true], 1);

        match decode_block(&mut decoder, &block) {
            Err(symphonia_core::errors::Error::DecodeError(message)) => {
                assert_eq!(message, "aac: decoded channel elements do not fill configured layout");
            }
            other => panic!("expected layout-fill decode error, got {other:?}"),
        }
    }

    #[test]
    fn conflicting_or_unknown_in_band_pce_stays_unsupported() {
        let mut decoder = decoder_from_ffmpeg_pce_asc();
        let stereo_block = raw_data_block_with_pce(&[true], &[], &[], 0);
        let quad_block = raw_data_block_with_pce(&[true], &[], &[true], 0);

        for block in [stereo_block, quad_block] {
            let error = decode_block(&mut decoder, &block).unwrap_err();
            assert!(matches!(error, symphonia_core::errors::Error::Unsupported(_)), "{error:?}");
        }
    }

    #[test]
    fn truncated_in_band_pce_is_an_error_not_a_panic() {
        let mut decoder = decoder_from_ffmpeg_pce_asc();
        let block = raw_data_block_with_pce(&[false, true], &[], &[true], 1);

        for length in 0..block.len() - 1 {
            assert!(decode_block(&mut decoder, &block[..length]).is_err(), "length {length}");
        }
    }
}
