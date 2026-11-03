//! Метка «дешёвых» видео-кодеков для `hello` (решения 43, 189, 190): что плеер просит у сайтов.
//!
//! Сайту можно сказать только «нет», а не «порядок». Поэтому первый кодек из настройки
//! `player.preferred_video_codec_order` работает как ПОТОЛОК: экстеншен просит браузер отказать
//! во всём, что сайты обычно ставят выше него. Допущение (так ранжируют YouTube и большинство
//! плееров): AV1 > VP9 > HEVC > H.264. Записи VP8 протокольного семейства не имеют и при поиске
//! первого кодека пропускаются.
//!
//! Итоговая метка — пересечение потолка и набора профиля рендера (явный OpenGL ES → только H.264).
//! Если в пересечении все четыре семейства, метка ПУСТА («ограничений нет»).
//!
//! Метка собирается один раз на запуск процесса браузерного режима: смена настройки действует
//! с ближайшего нового процесса. Автоопределение железа (Vulkan без аппаратного VP9/AV1) —
//! сессия 13 (решение 190).

use std::collections::BTreeSet;

use browser_bridge_protocol::VideoCodecFamily;
use fastiplayer_config::{RenderProfile, VideoCodec};

/// Семейства от самого «жадного» (как ранжируют сайты) к самому скромному.
const SITE_RANKING: [VideoCodecFamily; 4] = [
    VideoCodecFamily::Av1,
    VideoCodecFamily::Vp9,
    VideoCodecFamily::Hevc,
    VideoCodecFamily::H264,
];

/// Протокольное семейство кодека настройки; `None` — у VP8 семейства нет.
const fn family_of(codec: VideoCodec) -> Option<VideoCodecFamily> {
    match codec {
        VideoCodec::Av1 => Some(VideoCodecFamily::Av1),
        VideoCodec::Vp9 => Some(VideoCodecFamily::Vp9),
        VideoCodec::H265 => Some(VideoCodecFamily::Hevc),
        VideoCodec::H264 => Some(VideoCodecFamily::H264),
        VideoCodec::Vp8 => None,
    }
}

/// Семейства, не выше потолка настройки (все четыре, если в списке нет ни одного пригодного).
fn ceiling_families(preferred_order: &[VideoCodec]) -> BTreeSet<VideoCodecFamily> {
    let Some(ceiling) = preferred_order.iter().copied().find_map(family_of) else {
        return SITE_RANKING.into_iter().collect();
    };
    let rank = SITE_RANKING
        .iter()
        .position(|family| *family == ceiling)
        .unwrap_or(0);
    SITE_RANKING[rank..].iter().copied().collect()
}

/// Семейства, разрешённые профилем рендера без пробы видеокарты: явный OpenGL ES тянет дёшево
/// только H.264; остальные профили ограничений не знают (`Auto` — до пробы GPU неизвестно).
fn render_profile_families(render_profile: RenderProfile) -> BTreeSet<VideoCodecFamily> {
    match render_profile {
        RenderProfile::OpenGles => BTreeSet::from([VideoCodecFamily::H264]),
        RenderProfile::Vulkan | RenderProfile::Auto => SITE_RANKING.into_iter().collect(),
    }
}

/// Метка для `hello`: пересечение потолка настройки и профиля рендера; все четыре семейства → пусто.
pub(super) fn cheap_video_codecs(
    render_profile: RenderProfile,
    preferred_order: &[VideoCodec],
) -> BTreeSet<VideoCodecFamily> {
    let label: BTreeSet<VideoCodecFamily> = ceiling_families(preferred_order)
        .intersection(&render_profile_families(render_profile))
        .copied()
        .collect();
    if label.len() == SITE_RANKING.len() {
        BTreeSet::new()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use VideoCodec::{Av1, H264, H265, Vp8, Vp9};
    use VideoCodecFamily as Family;

    fn label(render_profile: RenderProfile, order: &[VideoCodec]) -> Vec<VideoCodecFamily> {
        cheap_video_codecs(render_profile, order)
            .into_iter()
            .collect()
    }

    #[test]
    fn the_first_codec_is_a_ceiling_over_the_site_ranking() {
        let auto = RenderProfile::Auto;
        // AV1 первым — ограничений нет, метка пуста.
        assert_eq!(label(auto, &[Av1, Vp9, H264]), vec![]);
        // VP9 первым (умолчание) — отказать только AV1.
        assert_eq!(
            label(auto, &[Vp9, Av1, H264, H265, Vp8]),
            vec![Family::H264, Family::Hevc, Family::Vp9]
        );
        assert_eq!(label(auto, &[H265]), vec![Family::H264, Family::Hevc]);
        assert_eq!(label(auto, &[H264, Vp9, Av1]), vec![Family::H264]);
    }

    #[test]
    fn vp8_entries_are_skipped_when_looking_for_the_first_codec() {
        assert_eq!(label(RenderProfile::Auto, &[Vp8, H264]), vec![Family::H264]);
        // Только VP8 — потолка нет, метка пуста.
        assert_eq!(label(RenderProfile::Auto, &[Vp8]), vec![]);
        assert_eq!(label(RenderProfile::Auto, &[]), vec![]);
    }

    #[test]
    fn explicit_opengles_limits_the_label_to_h264_whatever_the_list_says() {
        assert_eq!(label(RenderProfile::OpenGles, &[Av1]), vec![Family::H264]);
        assert_eq!(
            label(RenderProfile::OpenGles, &[Vp9, Av1]),
            vec![Family::H264]
        );
        assert_eq!(label(RenderProfile::OpenGles, &[H264]), vec![Family::H264]);
    }

    #[test]
    fn vulkan_and_auto_profiles_add_no_restriction_of_their_own() {
        for profile in [RenderProfile::Vulkan, RenderProfile::Auto] {
            assert_eq!(label(profile, &[H265]), vec![Family::H264, Family::Hevc]);
            assert_eq!(label(profile, &[Av1]), vec![]);
        }
    }
}
