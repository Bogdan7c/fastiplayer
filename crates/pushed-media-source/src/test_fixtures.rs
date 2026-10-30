//! Фикстуры crate-а для модульных тестов: fMP4, разрезанный на init и фрагменты.

/// H.264 64×64, 0,8 с: init + 4 фрагмента по 200 мс (5 кадров по 40 мс).
pub(crate) const VIDEO_H264_64: &[u8] = include_bytes!("../fixtures/video-h264-64.mp4");
/// То же время в 96×96 — другой init (смена качества).
pub(crate) const VIDEO_H264_96: &[u8] = include_bytes!("../fixtures/video-h264-96.mp4");

/// fMP4, разрезанный по заголовкам боксов верхнего уровня.
pub(crate) struct SplitMp4 {
    /// `ftyp` + `moov`.
    pub(crate) init: Vec<u8>,
    /// `moof` + `mdat` по порядку.
    pub(crate) fragments: Vec<Vec<u8>>,
}

/// Режет fMP4 на init и фрагменты (независимо от сканера источника).
pub(crate) fn split_mp4(file: &[u8]) -> SplitMp4 {
    let mut init = Vec::new();
    let mut fragments: Vec<Vec<u8>> = Vec::new();
    let mut position = 0;
    while position < file.len() {
        let size_bytes: [u8; 4] = file[position..position + 4].try_into().expect("size");
        let size = usize::try_from(u32::from_be_bytes(size_bytes)).expect("box size");
        let bytes = &file[position..position + size];
        match &file[position + 4..position + 8] {
            b"ftyp" | b"moov" => init.extend_from_slice(bytes),
            b"moof" => fragments.push(bytes.to_vec()),
            b"mdat" => fragments
                .last_mut()
                .expect("moof before mdat")
                .extend_from_slice(bytes),
            other => panic!("unexpected box {other:?}"),
        }
        position += size;
    }
    SplitMp4 { init, fragments }
}

/// H.264 64×64, 0,8 с, ключевой кадр каждые 10 кадров, фрагменты по 200 мс:
/// фрагменты 1 и 3 начинаются НЕ с ключевого кадра.
pub(crate) const VIDEO_H264_64_MIDGOP: &[u8] =
    include_bytes!("../fixtures/video-h264-64-midgop.mp4");

/// H.264 64×64, 4 с, 25 к/с: init + 10 фрагментов по 0,4 с, каждый с ключевого кадра.
pub(crate) const VIDEO_H264_64_4S: &[u8] = include_bytes!("../fixtures/video-h264-64-4s.mp4");
/// AAC моно 48 кГц, 0,8 с: init + фрагменты по ~0,2 с.
pub(crate) const AUDIO_AAC: &[u8] = include_bytes!("../fixtures/audio-aac.mp4");

/// AAC моно 48 кГц, 4 с (синус): init + 10 фрагментов по 19 AAC-кадров
/// (19 × 1024 sample = 405,33 мс) — звук фрагмента N начинается в N × 405,33 мс.
pub(crate) const AUDIO_AAC_4S: &[u8] = include_bytes!("../fixtures/audio-aac-4s.mp4");
