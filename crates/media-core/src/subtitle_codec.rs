//! Идентификаторы кодеков субтитров на границе demuxer → потребитель.
//!
//! `TrackInfo::codec_id` для дорожек `TrackKind::Subtitle` всегда равен одной из
//! констант этого модуля, а не сырой строке контейнера: потребители сравнивают
//! значения без «магических» строк и не зависят от того, какой контейнер дорожку отдал.

/// Текстовые реплики UTF-8 (SRT-подобные; в MKV `S_TEXT/UTF8`).
pub const TEXT_UTF8: &str = "S_TEXT/UTF8";
/// Advanced SubStation Alpha.
pub const ASS: &str = "S_TEXT/ASS";
/// SubStation Alpha.
pub const SSA: &str = "S_TEXT/SSA";
/// WebVTT.
pub const WEBVTT: &str = "S_TEXT/WEBVTT";
/// MP4 timed text (`mov_text`, `tx3g`).
pub const MOV_TEXT: &str = "S_TEXT/MOV_TEXT";
/// Blu-ray PGS (растровые).
pub const HDMV_PGS: &str = "S_HDMV/PGS";
/// DVD VobSub (растровые).
pub const VOBSUB: &str = "S_VOBSUB";
/// DVB subtitles (растровые).
pub const DVBSUB: &str = "S_DVBSUB";
/// Blu-ray TextST.
pub const HDMV_TEXTST: &str = "S_HDMV/TEXTST";
/// Ogg Kate.
pub const KATE: &str = "S_KATE";
/// Растровые субтитры в виде BMP-кадров.
pub const BMP: &str = "S_IMAGE/BMP";
/// Кодек субтитров, который контейнер назвал, но demuxer не распознал.
pub const UNKNOWN: &str = "S_UNKNOWN";

/// Семейство кодека субтитров с точки зрения возможности показать его текстом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleCodecFamily {
    /// Реплики приходят текстом (SRT, ASS/SSA, WebVTT, mov_text) — подходят для текстового рендера.
    TextRenderable,
    /// Реплики приходят картинками (PGS, VobSub, DVB, BMP).
    Bitmap,
    /// Кодек не распознан либо не имеет поддерживаемого рендера (TextST, Kate, unknown).
    Unsupported,
}

/// Относит `codec_id` дорожки субтитров к семейству.
#[must_use]
pub fn classify(codec_id: &str) -> SubtitleCodecFamily {
    match codec_id {
        TEXT_UTF8 | ASS | SSA | WEBVTT | MOV_TEXT => SubtitleCodecFamily::TextRenderable,
        HDMV_PGS | VOBSUB | DVBSUB | BMP => SubtitleCodecFamily::Bitmap,
        _ => SubtitleCodecFamily::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_separates_text_bitmap_and_unsupported() {
        assert_eq!(classify(ASS), SubtitleCodecFamily::TextRenderable);
        assert_eq!(classify(MOV_TEXT), SubtitleCodecFamily::TextRenderable);
        assert_eq!(classify(HDMV_PGS), SubtitleCodecFamily::Bitmap);
        assert_eq!(classify(VOBSUB), SubtitleCodecFamily::Bitmap);
        assert_eq!(classify(KATE), SubtitleCodecFamily::Unsupported);
        assert_eq!(classify(UNKNOWN), SubtitleCodecFamily::Unsupported);
        assert_eq!(classify("garbage"), SubtitleCodecFamily::Unsupported);
    }
}
