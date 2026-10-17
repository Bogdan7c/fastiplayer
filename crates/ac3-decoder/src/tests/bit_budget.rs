//! Учёт бит (A/52 §5.4.4.1): после шести аудиоблоков позиция чтения +
//! `nauxbits` + поля хвоста ровно равны длине кадра.
//!
//! Сильная сторона проверки — нулевые `auxbits`: ffmpeg (и реальные кодеры
//! Dolby в проверенных потоках) заполняют свободное место нулями. Если бы
//! разбор недочитал хоть одну мантиссу или поле, в «свободной» области
//! оказались бы ненулевые биты аудиоданных; если бы перечитал — кадр упал бы
//! с overrun или CRC.

use super::support::{FIXTURES, decoder, split_frames};
use crate::config::CrcPolicy;
use crate::spectral::dither::DitherMode;
use crate::syntax::audblk::output::TransformLength;
use crate::syntax::syncframe::frame_words;

/// Биты хвоста без `auxdatal`: `auxdatae` (1) + `crcrsv` (1) + `crc2` (16).
const TRAILER_BITS_WITHOUT_USER_DATA: usize = 18;

fn bits_are_zero(frame: &[u8], start: usize, count: usize) -> bool {
    (start..start + count).all(|bit| frame[bit / 8] & (0x80 >> (bit % 8)) == 0)
}

#[test]
fn six_blocks_and_trailer_consume_every_fixture_frame_exactly() {
    for fixture in &FIXTURES {
        let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
        for (index, frame) in split_frames(fixture.bytes).into_iter().enumerate() {
            let decoded = decoder
                .decode_coefficients(frame)
                .unwrap_or_else(|error| panic!("{} #{index}: {error}", fixture.name));
            let trailer = decoded.trailer;
            assert_eq!(trailer.user_data_bits, None, "{} #{index}", fixture.name);
            assert_eq!(
                trailer.audio_end_bits + trailer.auxiliary_bits + TRAILER_BITS_WITHOUT_USER_DATA,
                frame.len() * 8,
                "{} #{index}",
                fixture.name
            );
            assert!(
                bits_are_zero(frame, trailer.audio_end_bits, trailer.auxiliary_bits),
                "{} #{index}",
                fixture.name
            );
            // Кодер ffmpeg оставляет меньше 1 % кадра (плюс-минус округление) свободным:
            // разбор, остановившийся рано, дал бы большой «хвост».
            assert!(
                trailer.auxiliary_bits * 10 < frame.len() * 8,
                "{} #{index}: {}",
                fixture.name,
                trailer.auxiliary_bits
            );
        }
    }
}

/// Сводка по потоку реальных кодеров (dev-проверка).
#[derive(Debug, Default)]
struct StreamSummary {
    decoded_frames: usize,
    crc_errors: usize,
    other_errors: usize,
    short_transform_blocks: usize,
    coupled_blocks: usize,
    frames_with_nonzero_aux: usize,
}

fn summarize_stream(bytes: &[u8]) -> StreamSummary {
    let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
    let mut summary = StreamSummary::default();
    let mut offset = 0;
    while offset < bytes.len() {
        let Ok(words) = frame_words(&bytes[offset..]) else {
            offset += 1;
            continue;
        };
        let end = (offset + words * 2).min(bytes.len());
        let frame = &bytes[offset..end];
        match decoder.decode_coefficients(frame) {
            Ok(decoded) => {
                summary.decoded_frames += 1;
                let blocks = decoder.decoded_blocks();
                summary.short_transform_blocks += blocks
                    .iter()
                    .flat_map(|block| block.full_bandwidth.iter())
                    .filter(|channel| channel.transform == TransformLength::ShortPair)
                    .count();
                summary.coupled_blocks += blocks
                    .iter()
                    .filter(|block| block.coupling_begin_subband.is_some())
                    .count();
                let trailer = decoded.trailer;
                if !bits_are_zero(frame, trailer.audio_end_bits, trailer.auxiliary_bits) {
                    summary.frames_with_nonzero_aux += 1;
                }
            }
            Err(crate::error::Ac3DecodeError::CrcMismatch { .. }) => summary.crc_errors += 1,
            Err(_) => summary.other_errors += 1,
        }
        offset = end;
    }
    summary
}

/// Dev-проверка на потоках реальных кодеров (не коммитятся: неизвестная
/// лицензия). Запуск:
/// `AC3_DECODER_REAL_STREAMS_DIR=<dir> cargo test -p ac3-decoder --release real_encoder_streams -- --ignored --nocapture`.
/// Ожидание: ни одной ошибки, кроме CRC в действительно битых кадрах (их
/// подтверждает `ffmpeg -err_detect crccheck`), и нулевые `auxbits` везде.
#[test]
#[ignore = "нужен каталог с потоками реальных кодеров: AC3_DECODER_REAL_STREAMS_DIR"]
fn real_encoder_streams_parse_with_exact_bit_accounting() {
    let directory = std::env::var("AC3_DECODER_REAL_STREAMS_DIR").unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    for path in paths {
        let summary = summarize_stream(&std::fs::read(&path).unwrap());
        println!("{}: {summary:?}", path.display());
        assert!(summary.decoded_frames > 0, "{}", path.display());
        assert_eq!(summary.other_errors, 0, "{}", path.display());
        assert_eq!(summary.frames_with_nonzero_aux, 0, "{}", path.display());
    }
}
