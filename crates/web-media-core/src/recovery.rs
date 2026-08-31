use crate::ingress::WebMediaIngressKind;

/// Provider-neutral recovery action без locator-а, endpoint-а или runtime handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaRecoveryStrategy {
    /// Повторно открыть исходный стабильный progressive resource.
    ReopenStableResource,
    /// Обновить исходный root manifest и semantic-rematch-ить active selection.
    RefreshRootManifestAndRematch,
    /// Повторить extraction исходной страницы и semantic-rematch-ить selection.
    FreshExtractionAndRematch,
    /// Завершить playback typed-ошибкой: reconstructible owner отсутствует.
    TerminalUnreconstructibleEndpoint,
}

/// Гарантия lineage, которую recovery strategy способна сохранить.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaRecoveryContinuity {
    /// Recovery остаётся внутри исходного ingress boundary.
    PreserveIngress(WebMediaIngressKind),
    /// Recovery невозможен без выдумывания нового owner-а либо locator-а.
    Terminal,
}

impl WebMediaRecoveryStrategy {
    /// Возвращает каноническую recovery strategy для reconstructible ingress.
    pub const fn for_reconstructible_ingress(ingress: WebMediaIngressKind) -> Self {
        match ingress {
            WebMediaIngressKind::DirectResource => Self::ReopenStableResource,
            WebMediaIngressKind::NativeManifest => Self::RefreshRootManifestAndRematch,
            WebMediaIngressKind::ExtractorBacked => Self::FreshExtractionAndRematch,
        }
    }

    /// Возвращает terminal strategy для endpoint-а без reconstructible owner-а.
    pub const fn terminal_unreconstructible_endpoint() -> Self {
        Self::TerminalUnreconstructibleEndpoint
    }

    /// Описывает, какой ingress lineage сохраняет recovery.
    pub const fn continuity(self) -> WebMediaRecoveryContinuity {
        match self {
            Self::ReopenStableResource => {
                WebMediaRecoveryContinuity::PreserveIngress(WebMediaIngressKind::DirectResource)
            }
            Self::RefreshRootManifestAndRematch => {
                WebMediaRecoveryContinuity::PreserveIngress(WebMediaIngressKind::NativeManifest)
            }
            Self::FreshExtractionAndRematch => {
                WebMediaRecoveryContinuity::PreserveIngress(WebMediaIngressKind::ExtractorBacked)
            }
            Self::TerminalUnreconstructibleEndpoint => WebMediaRecoveryContinuity::Terminal,
        }
    }

    /// Сообщает, обязан ли recovery выполнить semantic rematch selection.
    pub const fn requires_semantic_rematch(self) -> bool {
        matches!(
            self,
            Self::RefreshRootManifestAndRematch | Self::FreshExtractionAndRematch
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{WebMediaRecoveryContinuity, WebMediaRecoveryStrategy};
    use crate::WebMediaIngressKind;

    #[test]
    fn recovery_strategy_preserves_owner_and_never_promotes_direct_to_extractor() {
        // Каждая reconstructible strategy должна сохранять исходный ingress owner.
        for ingress in [
            WebMediaIngressKind::DirectResource,
            WebMediaIngressKind::NativeManifest,
            WebMediaIngressKind::ExtractorBacked,
        ] {
            let strategy = WebMediaRecoveryStrategy::for_reconstructible_ingress(ingress);
            assert_eq!(
                strategy.continuity(),
                WebMediaRecoveryContinuity::PreserveIngress(ingress)
            );
        }

        // Direct resource reopen не должен внезапно требовать extractor rematch.
        let direct = WebMediaRecoveryStrategy::for_reconstructible_ingress(
            WebMediaIngressKind::DirectResource,
        );
        assert!(!direct.requires_semantic_rematch());

        // Manifest и extractor recovery обязаны повторно доказать semantic selection.
        assert!(
            WebMediaRecoveryStrategy::for_reconstructible_ingress(
                WebMediaIngressKind::NativeManifest
            )
            .requires_semantic_rematch()
        );
        assert!(
            WebMediaRecoveryStrategy::for_reconstructible_ingress(
                WebMediaIngressKind::ExtractorBacked
            )
            .requires_semantic_rematch()
        );

        // Временный endpoint без owner-а остаётся terminal, а не меняет ingress.
        assert_eq!(
            WebMediaRecoveryStrategy::terminal_unreconstructible_endpoint().continuity(),
            WebMediaRecoveryContinuity::Terminal
        );
    }
}
