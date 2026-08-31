/// Фактический способ, которым web-media вошло в native playback pipeline.
///
/// Тип описывает архитектурный путь, а не конкретного provider-а, протокол или
/// физический locator. Поэтому добавление нового HTTP client-а либо extractor-а
/// не требует расширять этот enum именем реализации.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaIngressKind {
    /// Стабильный progressive/media resource открыт напрямую.
    DirectResource,
    /// Корневой adaptive manifest открыт native provider-ом.
    NativeManifest,
    /// Страница либо provider document разрешены через extractor boundary.
    ExtractorBacked,
}

/// Точный presentation-kind без неявного `bool` или угадывания по duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaPresentationKind {
    /// Конечная presentation с VOD lifecycle.
    Vod,
    /// Обновляемая presentation с live/DVR lifecycle.
    Live,
}

#[cfg(test)]
mod tests {
    use super::{WebMediaIngressKind, WebMediaPresentationKind};

    #[test]
    fn ingress_and_presentation_values_roundtrip_without_provider_state() {
        // Проверяем все допустимые ingress identities без строкового provider ID.
        for ingress in [
            WebMediaIngressKind::DirectResource,
            WebMediaIngressKind::NativeManifest,
            WebMediaIngressKind::ExtractorBacked,
        ] {
            // Copy-roundtrip не должен менять точный semantic kind.
            let roundtripped = ingress;
            assert_eq!(roundtripped, ingress);
        }

        // VOD и live обязаны оставаться разными exact variants.
        assert_ne!(
            WebMediaPresentationKind::Vod,
            WebMediaPresentationKind::Live
        );
    }
}
