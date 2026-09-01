//! Единая app-owned policy boundary для native -> extractor fallback.
//!
//! Protocol owners публикуют только neutral typed trigger. Этот owner единолично
//! хранит lifecycle gate и исходный page locator, поэтому второй fallback и
//! fallback после `Installed` нельзя случайно реализовать в соседнем модуле.

use web_media_core::{
    ExtractorInvocationReason, WebMediaFallbackGate, WebMediaFallbackOutcome,
    WebMediaFallbackRejection, WebMediaFallbackTrigger,
};

/// Общий результат native admission до strong-install barrier-а.
pub(crate) enum NativeWebMediaAttempt<Prepared> {
    /// Native owner полностью подготовил media без extractor-а.
    Prepared(Prepared),
    /// Native owner классифицировал единственный neutral fallback trigger.
    RequiresExtractorFallback(WebMediaFallbackTrigger),
}

/// Разрешённый extractor fallback с исходным page locator и точной причиной.
pub(crate) struct ClaimedNativeExtractorFallback {
    /// Reconstructible page locator существует только у initial admission.
    locator: service_ytdlp::YtDlpMediaLocator,
    /// Product reason передаётся extractor adapter-у без повторной классификации.
    reason: ExtractorInvocationReason,
}

impl ClaimedNativeExtractorFallback {
    /// Передаёт owned locator и reason единственному extractor callsite-у.
    pub(crate) fn into_parts(
        self,
    ) -> (service_ytdlp::YtDlpMediaLocator, ExtractorInvocationReason) {
        (self.locator, self.reason)
    }
}

/// App owner единственного fallback attempt-а для одного native open lifecycle.
pub(crate) struct NativeWebFallbackOwner {
    /// Core state machine атомарно запрещает второй и post-Installed fallback.
    gate: WebMediaFallbackGate,
    /// Locator присутствует только до первого Installed native result-а.
    initial_locator: Option<service_ytdlp::YtDlpMediaLocator>,
}

impl NativeWebFallbackOwner {
    /// Создаёт initial owner с одним доступным pre-Installed fallback-ом.
    pub(crate) fn before_installed(locator: service_ytdlp::YtDlpMediaLocator) -> Self {
        Self {
            gate: WebMediaFallbackGate::before_installed(),
            initial_locator: Some(locator),
        }
    }

    /// Создаёт Installed owner без extractor locator-а и без fallback capability.
    pub(crate) fn installed() -> Self {
        let mut gate = WebMediaFallbackGate::before_installed();
        gate.mark_installed();
        Self {
            gate,
            initial_locator: None,
        }
    }

    /// Атомарно разрешает ровно один trigger и возвращает точную extractor reason.
    pub(crate) fn claim(
        &mut self,
        trigger: WebMediaFallbackTrigger,
    ) -> Result<ClaimedNativeExtractorFallback, WebMediaFallbackRejection> {
        let reason = match self.gate.decide(trigger) {
            WebMediaFallbackOutcome::InvokeExtractor(reason) => reason,
            WebMediaFallbackOutcome::Rejected(rejection) => return Err(rejection),
        };
        let locator = self
            .initial_locator
            .take()
            .ok_or(WebMediaFallbackRejection::InvariantViolation)?;
        Ok(ClaimedNativeExtractorFallback { locator, reason })
    }
}

#[cfg(test)]
mod tests {
    use super::NativeWebFallbackOwner;
    use web_media_core::{
        ExtractorInvocationReason, WebMediaFallbackRejection, WebMediaFallbackTrigger,
    };

    /// Строит secret-safe test locator для pure policy matrix-а без process spawn-а.
    fn test_locator() -> service_ytdlp::YtDlpMediaLocator {
        service_ytdlp::parse_yt_dlp_media_locator("https://example.test/watch?id=secret")
            .expect("valid fallback policy locator")
    }

    #[test]
    fn allowed_pre_installed_matrix_preserves_exact_reason_and_consumes_one_attempt() {
        let cases = [
            (
                WebMediaFallbackTrigger::ProviderDocument,
                ExtractorInvocationReason::PageMediaResolution,
            ),
            (
                WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
                ExtractorInvocationReason::ExtractorOwnedAuthorizationMaterial,
            ),
            (
                WebMediaFallbackTrigger::UnsupportedNativeProfile,
                ExtractorInvocationReason::NativeProfileCompatibilityFallback,
            ),
        ];

        for (trigger, expected_reason) in cases {
            let mut owner = NativeWebFallbackOwner::before_installed(test_locator());
            let (_, actual_reason) = owner
                .claim(trigger)
                .expect("allowlisted pre-Installed trigger должен открыть fallback")
                .into_parts();
            assert_eq!(actual_reason, expected_reason);
            assert!(matches!(
                owner.claim(trigger),
                Err(WebMediaFallbackRejection::AttemptAlreadyConsumed)
            ));
        }
    }

    #[test]
    fn every_forbidden_class_is_terminal_without_consuming_the_allowed_attempt() {
        let cases = [
            (
                WebMediaFallbackTrigger::Cancellation,
                WebMediaFallbackRejection::Cancellation,
            ),
            (
                WebMediaFallbackTrigger::NetworkFailure,
                WebMediaFallbackRejection::NetworkFailure,
            ),
            (
                WebMediaFallbackTrigger::MalformedManifest,
                WebMediaFallbackRejection::MalformedManifest,
            ),
            (
                WebMediaFallbackTrigger::ExpiredEndpoint,
                WebMediaFallbackRejection::ExpiredEndpoint,
            ),
            (
                WebMediaFallbackTrigger::Backpressure,
                WebMediaFallbackRejection::Backpressure,
            ),
            (
                WebMediaFallbackTrigger::InvariantViolation,
                WebMediaFallbackRejection::InvariantViolation,
            ),
            (
                WebMediaFallbackTrigger::DecoderFailure,
                WebMediaFallbackRejection::DecoderFailure,
            ),
            (
                WebMediaFallbackTrigger::RendererFailure,
                WebMediaFallbackRejection::RendererFailure,
            ),
        ];

        for (trigger, expected_rejection) in cases {
            let mut owner = NativeWebFallbackOwner::before_installed(test_locator());
            assert!(matches!(owner.claim(trigger), Err(actual) if actual == expected_rejection));
            assert!(
                owner
                    .claim(WebMediaFallbackTrigger::ProviderDocument)
                    .is_ok(),
                "forbidden trigger не должен расходовать единственный legal attempt"
            );
        }
    }

    #[test]
    fn post_installed_matrix_never_admits_extractor_for_any_trigger() {
        let triggers = [
            WebMediaFallbackTrigger::ProviderDocument,
            WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
            WebMediaFallbackTrigger::UnsupportedNativeProfile,
            WebMediaFallbackTrigger::Cancellation,
            WebMediaFallbackTrigger::NetworkFailure,
            WebMediaFallbackTrigger::MalformedManifest,
            WebMediaFallbackTrigger::ExpiredEndpoint,
            WebMediaFallbackTrigger::Backpressure,
            WebMediaFallbackTrigger::InvariantViolation,
            WebMediaFallbackTrigger::DecoderFailure,
            WebMediaFallbackTrigger::RendererFailure,
        ];

        for trigger in triggers {
            let mut owner = NativeWebFallbackOwner::installed();
            let mut extractor_admissions = 0_u8;
            if owner.claim(trigger).is_ok() {
                extractor_admissions = extractor_admissions.saturating_add(1);
            }
            assert_eq!(extractor_admissions, 0);
            assert!(matches!(
                owner.claim(trigger),
                Err(WebMediaFallbackRejection::AfterInstalled)
            ));
        }
    }
}
