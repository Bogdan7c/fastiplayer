//! Метка «дешёвых» видео-кодеков для `hello` (решения 43, 189): что плеер просит у сайтов.
//!
//! Сайту можно сказать только «нет», а не «порядок». Поэтому первый кодек из настройки
//! `player.preferred_video_codec_order` работает как ПОТОЛОК: экстеншен просит браузер отказать
//! во всём, что сайты обычно ставят выше него. Допущение (так ранжируют YouTube и большинство
//! плееров): AV1 > VP9 > HEVC > H.264. Записи VP8 протокольного семейства не имеют и при поиске
//! первого кодека пропускаются.
//!
//! Итоговая метка — пересечение потолка и набора рендера окна: окно на GLES2 (явный профиль или
//! `Auto`, выбравший GLES2 на машине без аппаратного Vulkan, — сессия 13a) тянет дёшево только
//! H.264. Если в пересечении все четыре семейства, метка ПУСТА («ограничений нет»).
//!
//! Метка собирается один раз на запуск процесса браузерного режима: смена настройки действует
//! с ближайшего нового процесса. Определение аппаратного декода по железу (решение 190)
//! владелец отложил (решение 264): без него метка не трогает машины, где видеокарта не умеет
//! VP9/AV1.

use std::collections::BTreeSet;

use browser_bridge_protocol::VideoCodecFamily;
use fastiplayer_config::{RenderProfile, VideoCodec};
use render_shell_api::RendererFamily;

use crate::render_backend::WindowRendererForecast;

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

/// Чем будет рисовать видео процесс браузерного режима — вход метки со стороны рендера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VideoRenderPath {
    /// Окно на этом семействе (прогноз той же выборки, что у окна).
    Window(RendererFamily),

    /// Ограничений со стороны рендера нет: годного рендера нет (окно не откроется) или
    /// процесс безоконный, а профиль настроек не просит GLES2 явно.
    Unrestricted,
}

impl VideoRenderPath {
    /// Путь по прогнозу рендера окна.
    pub(super) const fn from_window_forecast(forecast: WindowRendererForecast) -> Self {
        match forecast {
            WindowRendererForecast::Family(family) => Self::Window(family),
            // Окна не будет — играть нечем, ограничивать сайт незачем.
            WindowRendererForecast::NoUsableRenderer => Self::Unrestricted,
        }
    }

    /// Путь безоконного процесса (тестовый режим): проб нет, известен только профиль.
    pub(super) const fn from_profile_without_window(profile: RenderProfile) -> Self {
        match profile {
            RenderProfile::OpenGles => Self::Window(RendererFamily::Gles2),
            RenderProfile::Vulkan | RenderProfile::Auto => Self::Unrestricted,
        }
    }

    /// Семейства, которые этот путь показывает дёшево: GLES2 — только H.264 (решение 43).
    fn families(self) -> BTreeSet<VideoCodecFamily> {
        match self {
            Self::Window(RendererFamily::Gles2) => BTreeSet::from([VideoCodecFamily::H264]),
            Self::Window(RendererFamily::WgpuVulkan) | Self::Unrestricted => {
                SITE_RANKING.into_iter().collect()
            }
        }
    }
}

/// Метка для `hello`: пересечение потолка настройки и пути рендера; все четыре семейства → пусто.
pub(super) fn cheap_video_codecs(
    render_path: VideoRenderPath,
    preferred_order: &[VideoCodec],
) -> BTreeSet<VideoCodecFamily> {
    let label: BTreeSet<VideoCodecFamily> = ceiling_families(preferred_order)
        .intersection(&render_path.families())
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

    const WGPU: VideoRenderPath = VideoRenderPath::Window(RendererFamily::WgpuVulkan);
    const GLES2: VideoRenderPath = VideoRenderPath::Window(RendererFamily::Gles2);

    fn label(render_path: VideoRenderPath, order: &[VideoCodec]) -> Vec<VideoCodecFamily> {
        cheap_video_codecs(render_path, order).into_iter().collect()
    }

    #[test]
    fn the_first_codec_is_a_ceiling_over_the_site_ranking() {
        // AV1 первым — ограничений нет, метка пуста.
        assert_eq!(label(WGPU, &[Av1, Vp9, H264]), vec![]);
        // VP9 первым (умолчание) — отказать только AV1.
        assert_eq!(
            label(WGPU, &[Vp9, Av1, H264, H265, Vp8]),
            vec![Family::H264, Family::Hevc, Family::Vp9]
        );
        assert_eq!(label(WGPU, &[H265]), vec![Family::H264, Family::Hevc]);
        assert_eq!(label(WGPU, &[H264, Vp9, Av1]), vec![Family::H264]);
    }

    #[test]
    fn vp8_entries_are_skipped_when_looking_for_the_first_codec() {
        assert_eq!(label(WGPU, &[Vp8, H264]), vec![Family::H264]);
        // Только VP8 — потолка нет, метка пуста.
        assert_eq!(label(WGPU, &[Vp8]), vec![]);
        assert_eq!(label(WGPU, &[]), vec![]);
    }

    /// Окно на GLES2 — только H.264, что бы ни говорил список (решение 43). Сюда попадает и
    /// `Auto`, который на машине без аппаратного Vulkan встаёт на GLES2 (дыра 09b).
    #[test]
    fn a_gles2_window_limits_the_label_to_h264_whatever_the_list_says() {
        assert_eq!(label(GLES2, &[Av1]), vec![Family::H264]);
        assert_eq!(label(GLES2, &[Vp9, Av1]), vec![Family::H264]);
        assert_eq!(label(GLES2, &[H264]), vec![Family::H264]);
    }

    #[test]
    fn a_wgpu_window_and_the_unrestricted_path_add_nothing_of_their_own() {
        for path in [WGPU, VideoRenderPath::Unrestricted] {
            assert_eq!(label(path, &[H265]), vec![Family::H264, Family::Hevc]);
            assert_eq!(label(path, &[Av1]), vec![]);
        }
    }

    /// Прогноз окна переводится в путь рендера без потерь; «окна не будет» не ограничивает сайт.
    #[test]
    fn the_window_forecast_maps_to_the_render_path() {
        assert_eq!(
            VideoRenderPath::from_window_forecast(WindowRendererForecast::Family(
                RendererFamily::Gles2
            )),
            GLES2
        );
        assert_eq!(
            VideoRenderPath::from_window_forecast(WindowRendererForecast::NoUsableRenderer),
            VideoRenderPath::Unrestricted
        );
    }

    /// Безоконный процесс: проб нет, GLES2 — только по явному профилю.
    #[test]
    fn without_a_window_only_the_explicit_opengles_profile_restricts() {
        assert_eq!(
            VideoRenderPath::from_profile_without_window(RenderProfile::OpenGles),
            GLES2
        );
        for profile in [RenderProfile::Auto, RenderProfile::Vulkan] {
            assert_eq!(
                VideoRenderPath::from_profile_without_window(profile),
                VideoRenderPath::Unrestricted
            );
        }
    }
}
