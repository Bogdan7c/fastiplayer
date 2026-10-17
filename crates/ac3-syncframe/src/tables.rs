//! Табличные значения A/52: частоты, битрейты, длины кадров.

use crate::SyncframeHeaderError;

/// Число сэмплов в одном audio block-е (A/52 §5.4.3).
pub(crate) const SAMPLES_PER_AUDIO_BLOCK: u32 = 256;

/// AC-3 syncframe всегда содержит 6 audio block-ов = 1536 сэмплов.
pub(crate) const AC3_AUDIO_BLOCKS_PER_SYNCFRAME: u32 = 6;

/// Частоты по `fscod` (A/52 таблица 5.6); `fscod == 3` зарезервирован.
const SAMPLE_RATE_BY_FSCOD: [u32; 3] = [48_000, 44_100, 32_000];

/// Уменьшенные частоты E-AC-3 по `fscod2` (A/52 таблица E2.3); `3` зарезервирован.
const REDUCED_SAMPLE_RATE_BY_FSCOD2: [u32; 3] = [24_000, 22_050, 16_000];

/// Число audio block-ов E-AC-3 по `numblkscod` (A/52 таблица E2.2).
const AUDIO_BLOCKS_BY_NUMBLKSCOD: [u32; 4] = [1, 2, 3, 6];

/// Номинальный битрейт (кбит/с) по `frmsizecod / 2` (A/52 таблица 5.18).
const NOMINAL_BITRATE_KBPS_BY_HALF_FRMSIZECOD: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// Размер 16-битного слова, в котором A/52 выражает длину AC-3 кадра.
const BYTES_PER_WORD: usize = 2;

/// Частота AC-3/E-AC-3 по основному `fscod` (0..=2).
pub(crate) fn sample_rate_from_fscod(fscod: u8) -> Result<u32, SyncframeHeaderError> {
    SAMPLE_RATE_BY_FSCOD
        .get(usize::from(fscod))
        .copied()
        .ok_or(SyncframeHeaderError::ReservedSampleRateCode)
}

/// Уменьшенная частота E-AC-3 по `fscod2` (используется, когда `fscod == 3`).
pub(crate) fn reduced_sample_rate_from_fscod2(fscod2: u8) -> Result<u32, SyncframeHeaderError> {
    REDUCED_SAMPLE_RATE_BY_FSCOD2
        .get(usize::from(fscod2))
        .copied()
        .ok_or(SyncframeHeaderError::ReservedSampleRateCode)
}

/// Число audio block-ов E-AC-3 по 2-битному `numblkscod`.
pub(crate) fn audio_blocks_from_numblkscod(numblkscod: u8) -> u32 {
    // numblkscod — 2-битное поле, индекс всегда в пределах таблицы из 4 значений.
    AUDIO_BLOCKS_BY_NUMBLKSCOD
        .get(usize::from(numblkscod))
        .copied()
        .unwrap_or(AC3_AUDIO_BLOCKS_PER_SYNCFRAME)
}

/// Длина AC-3 кадра в байтах по `fscod` и `frmsizecod` (A/52 таблица 5.18).
///
/// Длина = битрейт × 1536 сэмплов / частота / 16 бит, в 16-битных словах.
/// Для 48 и 32 кГц деление точное (2 и 3 слова на кбит/с), для 44.1 кГц —
/// с остатком: чётный `frmsizecod` округляет вниз, нечётный добавляет одно
/// слово, чтобы средний битрейт совпадал с номинальным.
pub(crate) fn ac3_frame_length_bytes(
    sample_rate_hz: u32,
    frame_size_code: u8,
) -> Result<usize, SyncframeHeaderError> {
    let bitrate_kbps = NOMINAL_BITRATE_KBPS_BY_HALF_FRMSIZECOD
        .get(usize::from(frame_size_code / 2))
        .copied()
        .ok_or(SyncframeHeaderError::ReservedFrameSizeCode { frame_size_code })?;
    // 1536 сэмплов × 1000 бит/кбит / 16 бит на слово = 96 000.
    const WORDS_NUMERATOR_PER_KBPS: u32 = 96_000;
    let exact_words_numerator = bitrate_kbps * WORDS_NUMERATOR_PER_KBPS;
    let words_rounded_down = exact_words_numerator / sample_rate_hz;
    let has_remainder = !exact_words_numerator.is_multiple_of(sample_rate_hz);
    let padding_word = u32::from(has_remainder && !frame_size_code.is_multiple_of(2));
    let words = usize::try_from(words_rounded_down + padding_word).unwrap_or(usize::MAX);
    Ok(words * BYTES_PER_WORD)
}
