/// Пользовательская семантическая причина обращения к extractor boundary.
///
/// Reason не содержит свободную строку, URL, provider ID или внутренний retry
/// phase. Внутренние primary/embed/retry attempts могут дополнять этот reason у
/// process owner-а, но не подменять его.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtractorInvocationReason {
    /// Разрешить playable media из страницы либо provider document.
    PageMediaResolution,
    /// Разрешить collection/topology, не открывая media resource.
    CollectionTopologyResolution,
    /// Получить extractor-owned authorization/request material.
    ExtractorOwnedAuthorizationMaterial,
    /// Явно сохранить product semantics для unsupported native profile.
    NativeProfileCompatibilityFallback,
    /// Повторить extraction исходной страницы для recovery.
    ExtractorBackedRecovery,
}

/// Состояние единственного разрешённого pre-install fallback attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaPreInstallFallbackState {
    /// Extractor fallback ещё не выполнялся.
    Available,
    /// Единственный extractor fallback уже был выдан вызывающему коду.
    Consumed,
}

/// Фаза strong-install lifecycle, значимая для extractor fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaFallbackPhase {
    /// `Installed` ещё не достигнут; nested state ограничивает fallback одним разом.
    BeforeInstalled(WebMediaPreInstallFallbackState),
    /// `Installed` уже достигнут; fallback навсегда запрещён.
    Installed,
}

/// Typed событие, для которого classification/open path рассматривает fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaFallbackTrigger {
    /// Content confirmation доказал HTML/provider document.
    ProviderDocument,
    /// Native source требует material, которым владеет extractor.
    ExtractorOwnedAuthorizationMaterial,
    /// Корректный manifest использует явно unsupported native profile.
    UnsupportedNativeProfile,
    /// Операция была отменена.
    Cancellation,
    /// DNS, timeout либо другая transport/network ошибка.
    NetworkFailure,
    /// Manifest синтаксически либо семантически malformed.
    MalformedManifest,
    /// Временный endpoint истёк.
    ExpiredEndpoint,
    /// Downstream временно не принимает работу.
    Backpressure,
    /// Нарушен внутренний contract/invariant.
    InvariantViolation,
    /// Decoder отверг либо не смог обработать media.
    DecoderFailure,
    /// Renderer отверг либо не смог представить media.
    RendererFailure,
}

/// Почему extractor fallback не был разрешён.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaFallbackRejection {
    /// Единственный pre-install fallback уже использован.
    AttemptAlreadyConsumed,
    /// Strong-install barrier уже пройден.
    AfterInstalled,
    /// Cancellation не является сигналом сменить ingress.
    Cancellation,
    /// Network failure не является сигналом сменить ingress.
    NetworkFailure,
    /// Malformed manifest не маскируется extractor-ом.
    MalformedManifest,
    /// Истёкший endpoint восстанавливает его owner, а не fallback registry.
    ExpiredEndpoint,
    /// Backpressure сохраняет текущий ingress и повторяется его owner-ом.
    Backpressure,
    /// Invariant violation остаётся fatal и видимым.
    InvariantViolation,
    /// Decoder failure не возвращается в source classification.
    DecoderFailure,
    /// Renderer failure не возвращается в source classification.
    RendererFailure,
}

/// Результат чистого fallback decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaFallbackOutcome {
    /// Вызвать extractor ровно с этой semantic reason.
    InvokeExtractor(ExtractorInvocationReason),
    /// Сохранить текущий ingress и опубликовать точную причину отказа.
    Rejected(WebMediaFallbackRejection),
}

/// Маленький state owner, который не позволяет выдать два fallback-а либо
/// выполнить fallback после `Installed`.
#[derive(Debug, PartialEq, Eq)]
pub struct WebMediaFallbackGate {
    /// Текущая lifecycle phase остаётся закрытой от обхода инвариантов.
    phase: WebMediaFallbackPhase,
}

impl WebMediaFallbackGate {
    /// Создаёт gate до `Installed` с одним доступным fallback attempt.
    pub const fn before_installed() -> Self {
        Self {
            phase: WebMediaFallbackPhase::BeforeInstalled(
                WebMediaPreInstallFallbackState::Available,
            ),
        }
    }

    /// Возвращает точную phase для diagnostics и lifecycle assertions.
    pub const fn phase(&self) -> WebMediaFallbackPhase {
        self.phase
    }

    /// Закрывает fallback на strong `Installed` barrier.
    pub fn mark_installed(&mut self) {
        self.phase = WebMediaFallbackPhase::Installed;
    }

    /// Проверяет trigger и атомарно расходует attempt только при разрешении.
    pub fn decide(&mut self, trigger: WebMediaFallbackTrigger) -> WebMediaFallbackOutcome {
        match self.phase {
            WebMediaFallbackPhase::Installed => {
                WebMediaFallbackOutcome::Rejected(WebMediaFallbackRejection::AfterInstalled)
            }
            WebMediaFallbackPhase::BeforeInstalled(WebMediaPreInstallFallbackState::Consumed) => {
                WebMediaFallbackOutcome::Rejected(WebMediaFallbackRejection::AttemptAlreadyConsumed)
            }
            WebMediaFallbackPhase::BeforeInstalled(WebMediaPreInstallFallbackState::Available) => {
                match fallback_reason(trigger) {
                    Ok(reason) => {
                        // Attempt расходуется только когда caller действительно получил invoke intent.
                        self.phase = WebMediaFallbackPhase::BeforeInstalled(
                            WebMediaPreInstallFallbackState::Consumed,
                        );
                        WebMediaFallbackOutcome::InvokeExtractor(reason)
                    }
                    Err(rejection) => WebMediaFallbackOutcome::Rejected(rejection),
                }
            }
        }
    }
}

/// Переводит только три разрешённых trigger-а в semantic extractor reason.
fn fallback_reason(
    trigger: WebMediaFallbackTrigger,
) -> Result<ExtractorInvocationReason, WebMediaFallbackRejection> {
    match trigger {
        WebMediaFallbackTrigger::ProviderDocument => {
            Ok(ExtractorInvocationReason::PageMediaResolution)
        }
        WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial => {
            Ok(ExtractorInvocationReason::ExtractorOwnedAuthorizationMaterial)
        }
        WebMediaFallbackTrigger::UnsupportedNativeProfile => {
            Ok(ExtractorInvocationReason::NativeProfileCompatibilityFallback)
        }
        WebMediaFallbackTrigger::Cancellation => Err(WebMediaFallbackRejection::Cancellation),
        WebMediaFallbackTrigger::NetworkFailure => Err(WebMediaFallbackRejection::NetworkFailure),
        WebMediaFallbackTrigger::MalformedManifest => {
            Err(WebMediaFallbackRejection::MalformedManifest)
        }
        WebMediaFallbackTrigger::ExpiredEndpoint => Err(WebMediaFallbackRejection::ExpiredEndpoint),
        WebMediaFallbackTrigger::Backpressure => Err(WebMediaFallbackRejection::Backpressure),
        WebMediaFallbackTrigger::InvariantViolation => {
            Err(WebMediaFallbackRejection::InvariantViolation)
        }
        WebMediaFallbackTrigger::DecoderFailure => Err(WebMediaFallbackRejection::DecoderFailure),
        WebMediaFallbackTrigger::RendererFailure => Err(WebMediaFallbackRejection::RendererFailure),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExtractorInvocationReason, WebMediaFallbackGate, WebMediaFallbackOutcome,
        WebMediaFallbackPhase, WebMediaFallbackRejection, WebMediaFallbackTrigger,
        WebMediaPreInstallFallbackState,
    };

    #[test]
    fn fallback_is_legal_once_before_installed_for_three_explicit_reasons() {
        let legal_cases = [
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

        for (trigger, expected_reason) in legal_cases {
            // Каждый case получает независимый одноразовый gate.
            let mut gate = WebMediaFallbackGate::before_installed();
            assert_eq!(
                gate.decide(trigger),
                WebMediaFallbackOutcome::InvokeExtractor(expected_reason)
            );
            assert_eq!(
                gate.phase(),
                WebMediaFallbackPhase::BeforeInstalled(WebMediaPreInstallFallbackState::Consumed)
            );
            // Второй invoke запрещён независимо от причины.
            assert_eq!(
                gate.decide(WebMediaFallbackTrigger::ProviderDocument),
                WebMediaFallbackOutcome::Rejected(
                    WebMediaFallbackRejection::AttemptAlreadyConsumed
                )
            );
        }
    }

    #[test]
    fn forbidden_trigger_does_not_consume_attempt_and_installed_is_terminal() {
        let mut gate = WebMediaFallbackGate::before_installed();

        // Network failure остаётся network failure и не маскируется extractor-ом.
        assert_eq!(
            gate.decide(WebMediaFallbackTrigger::NetworkFailure),
            WebMediaFallbackOutcome::Rejected(WebMediaFallbackRejection::NetworkFailure)
        );
        assert_eq!(
            gate.phase(),
            WebMediaFallbackPhase::BeforeInstalled(WebMediaPreInstallFallbackState::Available)
        );

        // После Installed даже ранее легальный provider document не даёт fallback.
        gate.mark_installed();
        assert_eq!(
            gate.decide(WebMediaFallbackTrigger::ProviderDocument),
            WebMediaFallbackOutcome::Rejected(WebMediaFallbackRejection::AfterInstalled)
        );
    }
}
