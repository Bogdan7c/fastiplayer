//! Помощники тестов E-AC-3: нарезка потока на кадры и access unit-ы,
//! разбор кадра до коэффициентов с учётом бит.

use crate::config::{CrcPolicy, DecoderConfig};
use crate::error::Ac3DecodeError;
use crate::spectral::dither::{DitherGenerator, DitherMode};
use crate::spectral::spx::SpxNoiseGenerator;
use crate::syntax::audblk::output::DecodedAudioBlock;
use crate::syntax::eac3::audblk::mantissas::AhtFrameStore;
use crate::syntax::eac3::bsi::{StreamType, peek_frame_header};
use crate::syntax::eac3::syncframe::{
    DecodedEnhancedSyncframe, EnhancedFrameResources, decode_eac3_syncframe, verify_enhanced_crc,
};
use crate::syntax::eac3::tool_usage::EnhancedToolUsage;
use crate::syntax::syncframe::{AUDIO_BLOCKS_PER_SYNCFRAME, frame_words};

/// `bsid` кадра (байт 5, старшие 5 бит).
pub(crate) fn bitstream_id(frame: &[u8]) -> u8 {
    frame[5] >> 3
}

/// Нарезает поток на кадры. Начало и места сбоя (файл, вырезанный из TS
/// с середины кадра) пропускаются до синхрослова, за которым по длине
/// кадра идёт ещё одно синхрослово (или конец потока).
pub(crate) fn split_frames(stream: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut position = 0usize;
    while position + 6 <= stream.len() {
        let rest = &stream[position..];
        let length = if rest[0] == 0x0B && rest[1] == 0x77 {
            probe_length(rest)
        } else {
            None
        };
        let chained = length.filter(|&length| {
            rest.len() == length
                || (rest.len() > length + 1 && rest[length] == 0x0B && rest[length + 1] == 0x77)
        });
        match chained {
            Some(length) => {
                frames.push(&rest[..length]);
                position += length;
            }
            None => position += 1,
        }
    }
    frames
}

/// Длина кадра по заголовку, если заголовок разбирается и кадр помещается.
fn probe_length(frame: &[u8]) -> Option<usize> {
    let length = if bitstream_id(frame) <= 10 {
        frame_words(frame).ok()? * 2
    } else if bitstream_id(frame) <= 16 {
        peek_frame_header(frame).ok()?.frame_bytes
    } else {
        return None;
    };
    (length >= 6 && length <= frame.len()).then_some(length)
}

/// Начинает ли кадр новый access unit (AC-3 или независимый substream 0).
pub(crate) fn starts_access_unit(frame: &[u8]) -> bool {
    if bitstream_id(frame) <= 10 {
        return true;
    }
    let header = peek_frame_header(frame).unwrap();
    header.stream_type != StreamType::Dependent && header.substream_id == 0
}

/// Склеивает подряд идущие кадры в access unit-ы (Annex E §3.8).
pub(crate) fn split_access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = Vec::new();
    for frame in split_frames(stream) {
        if starts_access_unit(frame) || units.is_empty() {
            units.push(Vec::new());
        }
        units.last_mut().unwrap().extend_from_slice(frame);
    }
    units
}

/// Ресурсы разбора одного потока E-AC-3 (как у декодера).
pub(crate) struct EnhancedHarness {
    pub(crate) aht: Box<AhtFrameStore>,
    pub(crate) dither: DitherGenerator,
    pub(crate) spx_noise: SpxNoiseGenerator,
    pub(crate) blocks: Box<[DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME]>,
    pub(crate) usage: EnhancedToolUsage,
}

impl EnhancedHarness {
    pub(crate) fn new(dither: DitherMode) -> Self {
        Self {
            aht: AhtFrameStore::new(),
            dither: DitherGenerator::new(dither),
            spx_noise: SpxNoiseGenerator::new(),
            blocks: Box::new(std::array::from_fn(|_| DecodedAudioBlock::silent())),
            usage: EnhancedToolUsage::default(),
        }
    }

    /// CRC по политике, затем разбор кадра до коэффициентов.
    pub(crate) fn decode(
        &mut self,
        frame: &[u8],
        crc: CrcPolicy,
    ) -> Result<DecodedEnhancedSyncframe, Ac3DecodeError> {
        if crc == CrcPolicy::Verify {
            verify_enhanced_crc(frame)?;
        }
        decode_eac3_syncframe(
            frame,
            EnhancedFrameResources {
                aht: &mut self.aht,
                dither: &mut self.dither,
                spx_noise: &mut self.spx_noise,
            },
            &mut self.blocks,
            &mut self.usage,
        )
    }
}

/// Настройки декодера без dither-а.
pub(crate) fn quiet_config(crc: CrcPolicy) -> DecoderConfig {
    DecoderConfig {
        crc,
        dither: DitherMode::Disabled,
        ..DecoderConfig::default()
    }
}

/// Биты `[start, start + count)` кадра нулевые.
pub(crate) fn bits_are_zero(frame: &[u8], start: usize, count: usize) -> bool {
    (start..start + count).all(|bit| frame[bit / 8] & (0x80 >> (bit % 8)) == 0)
}

/// Фикстура ffmpeg E-AC-3 (см. `tests/fixtures/eac3/README.md`).
pub(crate) struct EnhancedFixture {
    pub(crate) name: &'static str,
    pub(crate) bytes: &'static [u8],
}

macro_rules! eac3_fixture {
    ($name:literal) => {
        EnhancedFixture {
            name: $name,
            bytes: include_bytes!(concat!("../../../tests/fixtures/eac3/", $name)),
        }
    };
}

/// Все фикстуры E-AC-3.
pub(crate) const EAC3_FIXTURES: [EnhancedFixture; 11] = [
    eac3_fixture!("mono-48k-64k.eac3"),
    eac3_fixture!("stereo-48k-96k.eac3"),
    eac3_fixture!("stereo-44k1-192k.eac3"),
    eac3_fixture!("stereo-32k-640k.eac3"),
    eac3_fixture!("stereo-48k-128k-cpl-high.eac3"),
    eac3_fixture!("3f-48k-256k.eac3"),
    eac3_fixture!("2f1r-48k-192k.eac3"),
    eac3_fixture!("quad-44k1-256k.eac3"),
    eac3_fixture!("2p1-48k-192k.eac3"),
    eac3_fixture!("5p1-48k-384k.eac3"),
    eac3_fixture!("5p1-32k-1000k.eac3"),
];

/// Фикстура E-AC-3 по имени.
pub(crate) fn eac3_fixture(name: &str) -> &'static [u8] {
    EAC3_FIXTURES
        .iter()
        .find(|fixture| fixture.name == name)
        .map(|fixture| fixture.bytes)
        .unwrap()
}
