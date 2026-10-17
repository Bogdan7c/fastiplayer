//! Общие помощники сквозных тестов: фикстуры, нарезка на кадры, декодер.

use crate::config::{CrcPolicy, DecoderConfig};
use crate::decoder::Ac3Decoder;
use crate::spectral::dither::DitherMode;
use crate::syntax::syncframe::frame_words;

/// Фикстура ffmpeg: имя файла и байты (см. `tests/fixtures/README.md`).
pub(super) struct Fixture {
    pub(super) name: &'static str,
    pub(super) bytes: &'static [u8],
}

macro_rules! fixture {
    ($name:literal) => {
        Fixture {
            name: $name,
            bytes: include_bytes!(concat!("../../tests/fixtures/", $name)),
        }
    };
}

/// Все фикстуры crate-а.
pub(super) const FIXTURES: [Fixture; 18] = [
    fixture!("mono-48k-96k.ac3"),
    fixture!("stereo-48k-192k.ac3"),
    fixture!("stereo-44k1-128k-plain.ac3"),
    fixture!("stereo-48k-32k.ac3"),
    fixture!("3f-48k-256k.ac3"),
    fixture!("2f1r-32k-192k.ac3"),
    fixture!("3f1r-48k-256k.ac3"),
    fixture!("2f2r-48k-320k.ac3"),
    fixture!("3f2r-48k-448k-cpl.ac3"),
    fixture!("mono-lfe-48k-128k.ac3"),
    fixture!("stereo-lfe-44k1-192k.ac3"),
    fixture!("2f1r-lfe-48k-256k.ac3"),
    fixture!("3f-lfe-32k-256k.ac3"),
    fixture!("3f1r-lfe-48k-384k.ac3"),
    fixture!("5p1-48k-640k-annexd.ac3"),
    fixture!("5p1-44k1-448k-annexd.ac3"),
    fixture!("5p1-32k-192k.ac3"),
    fixture!("stereo-48k-160k-cpl-high.ac3"),
];

/// Фикстуры синтеза PCM (пакет D2). Отдельно от [`FIXTURES`]: у
/// `stereo-48k-192k-short0` CRC намеренно не пересчитан, а тесты разбора
/// D1 сверяют каждый элемент `FIXTURES` с таблицей BSI и проверяют CRC.
pub(super) const SYNTHESIS_FIXTURES: [Fixture; 3] = [
    fixture!("overload-stereo-48k-192k.ac3"),
    fixture!("stereo-48k-256k-rematrix.ac3"),
    fixture!("stereo-48k-192k-short0.ac3"),
];

/// Фикстура по имени.
pub(super) fn fixture(name: &str) -> &'static [u8] {
    FIXTURES
        .iter()
        .chain(&SYNTHESIS_FIXTURES)
        .find(|fixture| fixture.name == name)
        .map(|fixture| fixture.bytes)
        .unwrap()
}

/// Нарезает поток подряд идущих кадров по их собственным заголовкам.
pub(super) fn split_frames(stream: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut rest = stream;
    while !rest.is_empty() {
        let length = frame_words(rest).unwrap() * 2;
        let (frame, tail) = rest.split_at(length);
        frames.push(frame);
        rest = tail;
    }
    frames
}

/// Декодер без dither-а и с указанной политикой CRC.
pub(super) fn decoder(crc: CrcPolicy, dither: DitherMode) -> Ac3Decoder {
    Ac3Decoder::new(DecoderConfig {
        crc,
        dither,
        ..DecoderConfig::default()
    })
}
