//! Таблица кодеков субтитров Symphonia → константы `media_core::subtitle_codec`.

use media_core::subtitle_codec;
use symphonia::core::codecs::subtitle::SubtitleCodecId;
use symphonia::core::codecs::subtitle::well_known as symphonia_subtitle_codec;

/// Мапит subtitle codec ids Symphonia в константы `media_core::subtitle_codec`.
///
/// SRT и «UTF-8 text» — один и тот же формат реплик, поэтому сводятся к `TEXT_UTF8`.
/// Нераспознанные (например, SAMI) остаются `None` и публикуются как `subtitle_codec::UNKNOWN`.
pub(super) fn codec_id_from_symphonia_subtitle_codec(
    codec: SubtitleCodecId,
) -> Option<&'static str> {
    match codec {
        symphonia_subtitle_codec::CODEC_ID_TEXT_UTF8 | symphonia_subtitle_codec::CODEC_ID_SRT => {
            Some(subtitle_codec::TEXT_UTF8)
        }
        symphonia_subtitle_codec::CODEC_ID_SSA => Some(subtitle_codec::SSA),
        symphonia_subtitle_codec::CODEC_ID_ASS => Some(subtitle_codec::ASS),
        symphonia_subtitle_codec::CODEC_ID_WEBVTT => Some(subtitle_codec::WEBVTT),
        symphonia_subtitle_codec::CODEC_ID_MOV_TEXT => Some(subtitle_codec::MOV_TEXT),
        symphonia_subtitle_codec::CODEC_ID_HDMV_PGS => Some(subtitle_codec::HDMV_PGS),
        symphonia_subtitle_codec::CODEC_ID_VOBSUB => Some(subtitle_codec::VOBSUB),
        symphonia_subtitle_codec::CODEC_ID_DVBSUB => Some(subtitle_codec::DVBSUB),
        symphonia_subtitle_codec::CODEC_ID_HDMV_TEXTST => Some(subtitle_codec::HDMV_TEXTST),
        symphonia_subtitle_codec::CODEC_ID_KATE => Some(subtitle_codec::KATE),
        symphonia_subtitle_codec::CODEC_ID_BMP => Some(subtitle_codec::BMP),
        _ => None,
    }
}
