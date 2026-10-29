//! Возможности сторон, о которых они сообщают в `Hello`.
//!
//! Возможности — список меток-строк, а не поля `Hello`: форма `Hello` не
//! меняется никогда (`crate::version`), а новые возможности появляются с каждой
//! новой фичей. Незнакомая метка — **единственное**, что протокол пропускает
//! молча: она лишь значит «собеседник умеет то, чем мы пользоваться не умеем»,
//! и ничего из-за пропуска не теряется.

use std::collections::BTreeSet;

use crate::text::CapabilityToken;

/// Необязательная фича протокола, которую включают обе стороны.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptionalFeature {
    /// Геометрия главного видео для режима 3 «Наклейка» (`results/00a.md`).
    ///
    /// **Заготовка:** метка зарезервирована, сообщения появятся в сессиях 17–19
    /// новой `minor` версией. До этого ни одна сторона её не объявляет.
    PageGeometry,
}

impl OptionalFeature {
    const ALL: [Self; 1] = [Self::PageGeometry];

    /// Метка в списке возможностей.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::PageGeometry => "feature.page_geometry",
        }
    }
}

/// Семейство видеокодеков для политики «дешёвый кодек» (решение 43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VideoCodecFamily {
    /// H.264 / AVC (`avc1`, `avc3`).
    H264,
    /// H.265 / HEVC (`hvc1`, `hev1`).
    Hevc,
    /// VP9 (`vp09`, `vp9`).
    Vp9,
    /// AV1 (`av01`).
    Av1,
}

impl VideoCodecFamily {
    const ALL: [Self; 4] = [Self::H264, Self::Hevc, Self::Vp9, Self::Av1];

    /// Метка «плееру этот кодек дёшев» в списке возможностей плеера.
    #[must_use]
    pub const fn cheap_codec_token(self) -> &'static str {
        match self {
            Self::H264 => "cheap_codec.h264",
            Self::Hevc => "cheap_codec.hevc",
            Self::Vp9 => "cheap_codec.vp9",
            Self::Av1 => "cheap_codec.av1",
        }
    }
}

/// Разобранные возможности одной стороны.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerCapabilities {
    /// Необязательные фичи, которые сторона умеет.
    pub features: BTreeSet<OptionalFeature>,
    /// Кодеки, которые плееру дёшево играть (у экстеншена — пусто).
    ///
    /// Экстеншен по этому списку говорит сайту «остальное не поддерживаю»
    /// (`auto` в переключателе решения 43). Пустой список — ограничений нет.
    pub cheap_video_codecs: BTreeSet<VideoCodecFamily>,
}

/// Итог разбора меток: знакомое и сколько незнакомых пропущено.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCapabilities {
    /// Знакомые возможности.
    pub capabilities: PeerCapabilities,
    /// Сколько незнакомых меток пропущено (для диагностики).
    pub ignored_unknown_tokens: usize,
}

impl PeerCapabilities {
    /// Разбирает метки из `Hello`.
    #[must_use]
    pub fn from_tokens(tokens: &[CapabilityToken]) -> ParsedCapabilities {
        let mut capabilities = Self::default();
        let mut ignored_unknown_tokens = 0;
        for token in tokens {
            let text = token.as_str();
            if let Some(feature) = OptionalFeature::ALL
                .into_iter()
                .find(|feature| feature.token() == text)
            {
                capabilities.features.insert(feature);
            } else if let Some(codec) = VideoCodecFamily::ALL
                .into_iter()
                .find(|codec| codec.cheap_codec_token() == text)
            {
                capabilities.cheap_video_codecs.insert(codec);
            } else {
                ignored_unknown_tokens += 1;
            }
        }
        ParsedCapabilities {
            capabilities,
            ignored_unknown_tokens,
        }
    }

    /// Метки для `Hello` в устойчивом порядке: сначала фичи, потом кодеки.
    #[must_use]
    pub fn to_tokens(&self) -> Vec<CapabilityToken> {
        let feature_tokens = self.features.iter().map(|feature| feature.token());
        let codec_tokens = self
            .cheap_video_codecs
            .iter()
            .map(|codec| codec.cheap_codec_token());
        feature_tokens
            .chain(codec_tokens)
            // Все метки — константы из этого файла, они всегда проходят проверку;
            // filter_map лишь избавляет от паники на невозможной ошибке.
            .filter_map(|token| CapabilityToken::new(token).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(texts: &[&str]) -> Vec<CapabilityToken> {
        texts
            .iter()
            .filter_map(|text| CapabilityToken::new(*text).ok())
            .collect()
    }

    #[test]
    fn known_tokens_are_parsed_and_unknown_ones_are_only_counted() {
        let parsed = PeerCapabilities::from_tokens(&tokens(&[
            "cheap_codec.h264",
            "feature.page_geometry",
            "feature.from_the_future",
            "cheap_codec.av1",
        ]));
        assert_eq!(
            parsed.capabilities.cheap_video_codecs,
            BTreeSet::from([VideoCodecFamily::H264, VideoCodecFamily::Av1])
        );
        assert_eq!(
            parsed.capabilities.features,
            BTreeSet::from([OptionalFeature::PageGeometry])
        );
        assert_eq!(parsed.ignored_unknown_tokens, 1);
    }

    #[test]
    fn every_constant_token_is_a_valid_capability_token_and_round_trips() {
        let all = PeerCapabilities {
            features: OptionalFeature::ALL.into_iter().collect(),
            cheap_video_codecs: VideoCodecFamily::ALL.into_iter().collect(),
        };
        let written = all.to_tokens();
        assert_eq!(
            written.len(),
            OptionalFeature::ALL.len() + VideoCodecFamily::ALL.len()
        );
        let parsed = PeerCapabilities::from_tokens(&written);
        assert_eq!(parsed.capabilities, all);
        assert_eq!(parsed.ignored_unknown_tokens, 0);
    }
}
