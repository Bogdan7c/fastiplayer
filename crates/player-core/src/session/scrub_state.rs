use media_core::MediaTime;

use crate::{ScrubGeneration, ScrubUpdateIntent, SeekMode};

/// Preview frame, который был реально показан в рамках конкретного scrub intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleScrubPreview {
    /// Поколение пользовательского scrub, которому принадлежит preview frame.
    generation: ScrubGeneration,

    /// Позиция уже показанного frame-а на media timeline.
    pub(super) position: MediaTime,
}

/// Причина, по которой session не приняла latest scrub update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionScrubUpdateRejection {
    /// Сейчас нет active scrub generation.
    Inactive,

    /// Update относится к generation, который уже не является текущим.
    StaleGeneration,
}

/// Typed результат session-side update boundary.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionScrubUpdateAcceptance {
    /// Update принят и стал latest request-ом текущего scrub-а.
    Accepted {
        /// Intent, который session сохранила как latest request.
        intent: ScrubUpdateIntent,
    },

    /// Update отброшен без изменения scrub state.
    Rejected {
        /// Intent, который не прошёл session boundary.
        intent: ScrubUpdateIntent,

        /// Точная причина отказа для diagnostics и тестов.
        reason: SessionScrubUpdateRejection,
    },
}

/// Причина, по которой visible preview не записан в session scrub state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisiblePreviewRejection {
    /// Сейчас нет active scrub generation.
    Inactive,

    /// Preview относится к устаревшему generation.
    StaleGeneration,
}

/// Typed результат записи реально показанного preview frame-а.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisiblePreviewDecision {
    /// Preview принадлежит текущему generation и записан как visible.
    Recorded {
        /// Preview, который теперь является visible feedback-ом release policy.
        preview: VisibleScrubPreview,
    },

    /// Preview отброшен без изменения visible preview state.
    Rejected {
        /// Generation preview frame-а, который не был принят.
        generation: ScrubGeneration,

        /// Позиция frame-а, которую caller пытался записать.
        position: MediaTime,

        /// Точная причина отказа.
        reason: VisiblePreviewRejection,
    },
}

/// Причина, по которой completed preview не записан в session scrub state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewCompletionRejection {
    /// Сейчас нет active scrub generation.
    Inactive,

    /// Preview completion относится к устаревшему generation.
    StaleGeneration,
}

/// Typed результат отметки завершённого preview seek-а.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewCompletionDecision {
    /// Preview текущего generation завершён и может участвовать в release policy.
    Completed {
        /// Generation завершённого preview.
        generation: ScrubGeneration,
    },

    /// Completion отброшен без изменения completed preview state.
    Rejected {
        /// Generation preview, который не был принят.
        generation: ScrubGeneration,

        /// Точная причина отказа.
        reason: PreviewCompletionRejection,
    },
}

/// Причина, по которой completed preview не был инвалидирован.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletedPreviewInvalidationRejection {
    /// Сейчас нет active scrub generation.
    Inactive,

    /// Invalidation относится к устаревшему generation.
    StaleGeneration,
}

/// Typed результат invalidation-а completed preview marker-а.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletedPreviewInvalidationDecision {
    /// Completed preview marker текущего generation очищен.
    Invalidated {
        /// Generation, для которого marker был очищен.
        generation: ScrubGeneration,
    },

    /// Invalidation отброшен без изменения completed preview state.
    Rejected {
        /// Generation, который caller пытался инвалидировать.
        generation: ScrubGeneration,

        /// Точная причина отказа.
        reason: CompletedPreviewInvalidationRejection,
    },
}

/// Причина, по которой session не может завершить scrub generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionScrubFinishRejection {
    /// Сейчас нет active scrub generation.
    Inactive,

    /// Finish относится к generation, который уже не является текущим.
    StaleGeneration,
}

/// Pure typed decision для EndScrub до мутации state.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionScrubFinishDecision {
    /// Generation является текущим и caller может применить commit policy.
    Current {
        /// Generation, который прошёл проверку актуальности.
        generation: ScrubGeneration,
    },

    /// Finish отброшен без изменения scrub state.
    Rejected {
        /// Generation, который не прошёл проверку.
        generation: ScrubGeneration,

        /// Точная причина отказа.
        reason: SessionScrubFinishRejection,
    },
}

/// Session-side scrub state, который группирует данные только активного пользовательского scrub-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionScrubState {
    /// Приватное состояние, чтобы `PlayerSession` не зависел от layout-а state machine.
    inner: SessionScrubStateInner,
}

/// Приватный layout scrub state; внешняя граница модуля проходит только через methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionScrubStateInner {
    /// Сейчас нет active scrub, поэтому latest/preview данные недоступны.
    Idle,

    /// Данные interactive scrub-а, привязанные к одному пользовательскому generation.
    Active {
        /// Поколение scrub-а, которому принадлежат все вложенные поля.
        generation: ScrubGeneration,

        /// Последний scrub request целиком, чтобы final commit не терял `SeekMode`.
        latest_request: Option<ScrubUpdateIntent>,

        /// Последний preview-кадр этого generation, который уже реально показан.
        visible_preview: Option<VisibleScrubPreview>,

        /// Preview transaction, который уже завершился для текущего generation.
        completed_preview_generation: Option<ScrubGeneration>,
    },
}

impl SessionScrubState {
    /// Открывает новый active scrub и выбрасывает state предыдущего generation.
    pub(super) fn begin(&mut self, generation: ScrubGeneration) {
        self.inner = SessionScrubStateInner::Active {
            generation,
            latest_request: None,
            visible_preview: None,
            completed_preview_generation: None,
        };
    }

    /// Принимает latest target только для текущего active generation.
    pub(super) fn accept_update(
        &mut self,
        intent: ScrubUpdateIntent,
    ) -> SessionScrubUpdateAcceptance {
        match &mut self.inner {
            SessionScrubStateInner::Active {
                generation,
                latest_request,
                ..
            } if *generation == intent.generation => {
                *latest_request = Some(intent);
                SessionScrubUpdateAcceptance::Accepted { intent }
            }
            SessionScrubStateInner::Active { .. } => SessionScrubUpdateAcceptance::Rejected {
                intent,
                reason: SessionScrubUpdateRejection::StaleGeneration,
            },
            SessionScrubStateInner::Idle => SessionScrubUpdateAcceptance::Rejected {
                intent,
                reason: SessionScrubUpdateRejection::Inactive,
            },
        }
    }

    /// Запоминает реально показанный preview только для текущего active generation.
    pub(super) fn note_visible_preview(
        &mut self,
        generation: ScrubGeneration,
        position: MediaTime,
    ) -> VisiblePreviewDecision {
        match &mut self.inner {
            SessionScrubStateInner::Active {
                generation: active_generation,
                visible_preview,
                ..
            } if *active_generation == generation => {
                let preview = VisibleScrubPreview {
                    generation,
                    position,
                };
                *visible_preview = Some(preview);
                VisiblePreviewDecision::Recorded { preview }
            }
            SessionScrubStateInner::Active { .. } => VisiblePreviewDecision::Rejected {
                generation,
                position,
                reason: VisiblePreviewRejection::StaleGeneration,
            },
            SessionScrubStateInner::Idle => VisiblePreviewDecision::Rejected {
                generation,
                position,
                reason: VisiblePreviewRejection::Inactive,
            },
        }
    }

    /// Отмечает завершённый preview только для текущего active generation.
    pub(super) fn complete_preview(
        &mut self,
        generation: ScrubGeneration,
    ) -> PreviewCompletionDecision {
        match &mut self.inner {
            SessionScrubStateInner::Active {
                generation: active_generation,
                completed_preview_generation,
                ..
            } if *active_generation == generation => {
                *completed_preview_generation = Some(generation);
                PreviewCompletionDecision::Completed { generation }
            }
            SessionScrubStateInner::Active { .. } => PreviewCompletionDecision::Rejected {
                generation,
                reason: PreviewCompletionRejection::StaleGeneration,
            },
            SessionScrubStateInner::Idle => PreviewCompletionDecision::Rejected {
                generation,
                reason: PreviewCompletionRejection::Inactive,
            },
        }
    }

    /// Проверяет EndScrub без мутации, чтобы commit policy видела старый state.
    pub(super) fn finish_decision(
        &self,
        generation: ScrubGeneration,
    ) -> SessionScrubFinishDecision {
        match &self.inner {
            SessionScrubStateInner::Active {
                generation: active_generation,
                ..
            } if *active_generation == generation => {
                SessionScrubFinishDecision::Current { generation }
            }
            SessionScrubStateInner::Active { .. } => SessionScrubFinishDecision::Rejected {
                generation,
                reason: SessionScrubFinishRejection::StaleGeneration,
            },
            SessionScrubStateInner::Idle => SessionScrubFinishDecision::Rejected {
                generation,
                reason: SessionScrubFinishRejection::Inactive,
            },
        }
    }

    /// Закрывает active scrub после commit policy, если caller пришёл с актуальным generation.
    pub(super) fn close_current(
        &mut self,
        generation: ScrubGeneration,
    ) -> SessionScrubFinishDecision {
        let decision = self.finish_decision(generation);
        if let SessionScrubFinishDecision::Current { .. } = decision {
            self.inner = SessionScrubStateInner::Idle;
        }
        decision
    }

    /// Полностью очищает scrub state для media reset, final complete и timeout путей.
    pub(super) fn clear(&mut self) {
        self.inner = SessionScrubStateInner::Idle;
    }

    /// Возвращает текущий active generation без раскрытия внутреннего enum layout-а.
    pub(super) const fn current_generation(&self) -> Option<ScrubGeneration> {
        match &self.inner {
            SessionScrubStateInner::Idle => None,
            SessionScrubStateInner::Active { generation, .. } => Some(*generation),
        }
    }

    /// Возвращает latest request только если он относится к запрошенному generation.
    pub(super) fn latest_request_for(
        &self,
        generation: ScrubGeneration,
    ) -> Option<ScrubUpdateIntent> {
        match &self.inner {
            SessionScrubStateInner::Active {
                generation: active_generation,
                latest_request,
                ..
            } if *active_generation == generation => {
                (*latest_request).filter(|latest_intent| latest_intent.generation == generation)
            }
            SessionScrubStateInner::Active { .. } | SessionScrubStateInner::Idle => None,
        }
    }

    /// Возвращает `SeekMode` latest request-а для final commit-а без потери public intent-а.
    pub(super) fn latest_seek_mode(&self, generation: ScrubGeneration) -> SeekMode {
        self.latest_request_for(generation)
            .map(|latest_intent| latest_intent.request.mode)
            .unwrap_or_default()
    }

    /// Возвращает visible preview только если он принадлежит текущему active generation.
    pub(super) fn visible_preview_for(
        &self,
        generation: ScrubGeneration,
    ) -> Option<VisibleScrubPreview> {
        match &self.inner {
            SessionScrubStateInner::Active {
                generation: active_generation,
                visible_preview,
                ..
            } if *active_generation == generation => {
                (*visible_preview).filter(|preview| preview.generation == generation)
            }
            SessionScrubStateInner::Active { .. } | SessionScrubStateInner::Idle => None,
        }
    }

    /// Проверяет, что completed preview относится к текущему active generation.
    pub(super) fn completed_preview_for(&self, generation: ScrubGeneration) -> bool {
        matches!(
            &self.inner,
            SessionScrubStateInner::Active {
                generation: active_generation,
                completed_preview_generation: Some(completed_generation),
                ..
            } if *active_generation == generation && *completed_generation == generation
        )
    }

    /// Инвалидирует completed preview текущего generation перед новым preview attempt-ом.
    pub(super) fn invalidate_completed_preview(
        &mut self,
        generation: ScrubGeneration,
    ) -> CompletedPreviewInvalidationDecision {
        match &mut self.inner {
            SessionScrubStateInner::Active {
                generation: active_generation,
                completed_preview_generation,
                ..
            } if *active_generation == generation => {
                *completed_preview_generation = None;
                CompletedPreviewInvalidationDecision::Invalidated { generation }
            }
            SessionScrubStateInner::Active { .. } => {
                CompletedPreviewInvalidationDecision::Rejected {
                    generation,
                    reason: CompletedPreviewInvalidationRejection::StaleGeneration,
                }
            }
            SessionScrubStateInner::Idle => CompletedPreviewInvalidationDecision::Rejected {
                generation,
                reason: CompletedPreviewInvalidationRejection::Inactive,
            },
        }
    }

    /// Убирает preview candidates, когда final transaction заменяет live preview path.
    pub(super) fn prepare_final_commit(&mut self) {
        if let SessionScrubStateInner::Active {
            visible_preview,
            completed_preview_generation,
            ..
        } = &mut self.inner
        {
            *visible_preview = None;
            *completed_preview_generation = None;
        }
    }
}

impl Default for SessionScrubState {
    /// Новый session scrub state стартует без active generation.
    fn default() -> Self {
        Self {
            inner: SessionScrubStateInner::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use media_core::MediaTime;

    use crate::{
        ScrubGeneration, ScrubUpdateIntent, SeekMode, SeekRequest, SeekTarget,
        session::scrub_state::{
            CompletedPreviewInvalidationDecision, CompletedPreviewInvalidationRejection,
            PreviewCompletionDecision, PreviewCompletionRejection, SessionScrubFinishDecision,
            SessionScrubFinishRejection, SessionScrubState, SessionScrubUpdateAcceptance,
            SessionScrubUpdateRejection, VisiblePreviewDecision, VisiblePreviewRejection,
            VisibleScrubPreview,
        },
    };

    /// Собирает абсолютный scrub request с явным seek mode для проверки boundary state.
    fn session_scrub_request(seconds: u64, mode: SeekMode) -> SeekRequest {
        SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(seconds)),
            mode,
        }
    }

    /// Собирает tagged scrub update для unit-тестов session-side scrub state.
    fn session_scrub_intent(
        generation: ScrubGeneration,
        seconds: u64,
        mode: SeekMode,
    ) -> ScrubUpdateIntent {
        ScrubUpdateIntent::new(generation, session_scrub_request(seconds, mode))
    }

    #[test]
    fn begin_creates_active_generation_and_clears_previous_state() {
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();
        let first_intent = session_scrub_intent(first_generation, 7, SeekMode::Accurate);
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(first_generation);
        assert_eq!(
            scrub_state.accept_update(first_intent),
            SessionScrubUpdateAcceptance::Accepted {
                intent: first_intent,
            }
        );
        assert_eq!(
            scrub_state.note_visible_preview(first_generation, MediaTime::from_secs(6)),
            VisiblePreviewDecision::Recorded {
                preview: VisibleScrubPreview {
                    generation: first_generation,
                    position: MediaTime::from_secs(6),
                },
            }
        );
        assert_eq!(
            scrub_state.complete_preview(first_generation),
            PreviewCompletionDecision::Completed {
                generation: first_generation,
            }
        );

        scrub_state.begin(second_generation);

        assert_eq!(scrub_state.current_generation(), Some(second_generation));
        assert_eq!(scrub_state.latest_request_for(first_generation), None);
        assert_eq!(scrub_state.latest_request_for(second_generation), None);
        assert_eq!(scrub_state.visible_preview_for(first_generation), None);
        assert!(!scrub_state.completed_preview_for(first_generation));
        assert!(!scrub_state.completed_preview_for(second_generation));
    }

    #[test]
    fn accept_update_outside_active_returns_inactive_rejection() {
        let generation = ScrubGeneration::default().next();
        let intent = session_scrub_intent(generation, 5, SeekMode::Accurate);
        let mut scrub_state = SessionScrubState::default();

        assert_eq!(
            scrub_state.accept_update(intent),
            SessionScrubUpdateAcceptance::Rejected {
                intent,
                reason: SessionScrubUpdateRejection::Inactive,
            }
        );

        assert_eq!(scrub_state.current_generation(), None);
        assert_eq!(scrub_state.latest_request_for(generation), None);
    }

    #[test]
    fn accept_update_rejects_stale_generation_without_changing_latest_request() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let current_intent = session_scrub_intent(current_generation, 5, SeekMode::Accurate);
        let stale_intent = session_scrub_intent(stale_generation, 9, SeekMode::KeyframeBefore);
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);
        assert_eq!(
            scrub_state.accept_update(current_intent),
            SessionScrubUpdateAcceptance::Accepted {
                intent: current_intent,
            }
        );

        assert_eq!(
            scrub_state.accept_update(stale_intent),
            SessionScrubUpdateAcceptance::Rejected {
                intent: stale_intent,
                reason: SessionScrubUpdateRejection::StaleGeneration,
            }
        );

        assert_eq!(
            scrub_state.latest_request_for(current_generation),
            Some(current_intent)
        );
        assert_eq!(scrub_state.latest_request_for(stale_generation), None);
        assert_eq!(scrub_state.visible_preview_for(stale_generation), None);
        assert!(!scrub_state.completed_preview_for(stale_generation));
        assert_eq!(scrub_state.current_generation(), Some(current_generation));
    }

    #[test]
    fn latest_request_preserves_seek_mode() {
        let generation = ScrubGeneration::default().next();
        let intent = session_scrub_intent(generation, 11, SeekMode::KeyframeBefore);
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(generation);
        assert_eq!(
            scrub_state.accept_update(intent),
            SessionScrubUpdateAcceptance::Accepted { intent }
        );

        assert_eq!(scrub_state.latest_request_for(generation), Some(intent));
        assert_eq!(
            scrub_state.latest_seek_mode(generation),
            SeekMode::KeyframeBefore
        );
    }

    #[test]
    fn note_visible_preview_rejects_stale_generation_without_changing_preview() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);
        assert_eq!(
            scrub_state.note_visible_preview(current_generation, MediaTime::from_secs(3)),
            VisiblePreviewDecision::Recorded {
                preview: VisibleScrubPreview {
                    generation: current_generation,
                    position: MediaTime::from_secs(3),
                },
            }
        );

        assert_eq!(
            scrub_state.note_visible_preview(stale_generation, MediaTime::from_secs(13)),
            VisiblePreviewDecision::Rejected {
                generation: stale_generation,
                position: MediaTime::from_secs(13),
                reason: VisiblePreviewRejection::StaleGeneration,
            }
        );

        assert_eq!(
            scrub_state.visible_preview_for(current_generation),
            Some(VisibleScrubPreview {
                generation: current_generation,
                position: MediaTime::from_secs(3),
            })
        );
        assert_eq!(scrub_state.visible_preview_for(stale_generation), None);
    }

    #[test]
    fn visible_preview_belongs_only_to_current_generation() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);
        assert_eq!(
            scrub_state.note_visible_preview(current_generation, MediaTime::from_secs(3)),
            VisiblePreviewDecision::Recorded {
                preview: VisibleScrubPreview {
                    generation: current_generation,
                    position: MediaTime::from_secs(3),
                },
            }
        );

        scrub_state.begin(stale_generation);

        assert_eq!(scrub_state.visible_preview_for(current_generation), None);
        assert_eq!(scrub_state.visible_preview_for(stale_generation), None);
    }

    #[test]
    fn complete_preview_rejects_stale_generation_without_changing_completed_preview() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);

        assert_eq!(
            scrub_state.complete_preview(current_generation),
            PreviewCompletionDecision::Completed {
                generation: current_generation,
            }
        );
        assert_eq!(
            scrub_state.complete_preview(stale_generation),
            PreviewCompletionDecision::Rejected {
                generation: stale_generation,
                reason: PreviewCompletionRejection::StaleGeneration,
            }
        );
        assert!(scrub_state.completed_preview_for(current_generation));
        assert!(!scrub_state.completed_preview_for(stale_generation));
    }

    #[test]
    fn completed_preview_belongs_only_to_current_generation() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);

        assert_eq!(
            scrub_state.complete_preview(current_generation),
            PreviewCompletionDecision::Completed {
                generation: current_generation,
            }
        );
        assert!(scrub_state.completed_preview_for(current_generation));

        scrub_state.begin(stale_generation);

        assert!(!scrub_state.completed_preview_for(current_generation));
        assert!(!scrub_state.completed_preview_for(stale_generation));
    }

    #[test]
    fn finish_decision_rejects_stale_generation_without_closing_active_state() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);

        assert_eq!(
            scrub_state.finish_decision(stale_generation),
            SessionScrubFinishDecision::Rejected {
                generation: stale_generation,
                reason: SessionScrubFinishRejection::StaleGeneration,
            }
        );
        assert_eq!(
            scrub_state.close_current(stale_generation),
            SessionScrubFinishDecision::Rejected {
                generation: stale_generation,
                reason: SessionScrubFinishRejection::StaleGeneration,
            }
        );
        assert_eq!(scrub_state.current_generation(), Some(current_generation));
    }

    #[test]
    fn close_current_generation_closes_active_state() {
        let generation = ScrubGeneration::default().next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(generation);

        assert_eq!(
            scrub_state.finish_decision(generation),
            SessionScrubFinishDecision::Current { generation }
        );
        assert_eq!(
            scrub_state.close_current(generation),
            SessionScrubFinishDecision::Current { generation }
        );
        assert_eq!(scrub_state.current_generation(), None);
    }

    #[test]
    fn invalidate_completed_preview_current_generation_clears_only_completion() {
        let generation = ScrubGeneration::default().next();
        let intent = session_scrub_intent(generation, 17, SeekMode::KeyframeBefore);
        let visible_preview = VisibleScrubPreview {
            generation,
            position: MediaTime::from_secs(16),
        };
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(generation);
        assert_eq!(
            scrub_state.accept_update(intent),
            SessionScrubUpdateAcceptance::Accepted { intent }
        );
        assert_eq!(
            scrub_state.note_visible_preview(generation, visible_preview.position),
            VisiblePreviewDecision::Recorded {
                preview: visible_preview,
            }
        );
        assert_eq!(
            scrub_state.complete_preview(generation),
            PreviewCompletionDecision::Completed { generation }
        );

        assert_eq!(
            scrub_state.invalidate_completed_preview(generation),
            CompletedPreviewInvalidationDecision::Invalidated { generation }
        );

        assert_eq!(scrub_state.current_generation(), Some(generation));
        assert_eq!(scrub_state.latest_request_for(generation), Some(intent));
        assert_eq!(
            scrub_state.visible_preview_for(generation),
            Some(visible_preview)
        );
        assert!(!scrub_state.completed_preview_for(generation));
    }

    #[test]
    fn invalidate_completed_preview_rejects_stale_generation_without_change() {
        let current_generation = ScrubGeneration::default().next();
        let stale_generation = current_generation.next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(current_generation);
        assert_eq!(
            scrub_state.complete_preview(current_generation),
            PreviewCompletionDecision::Completed {
                generation: current_generation,
            }
        );

        assert_eq!(
            scrub_state.invalidate_completed_preview(stale_generation),
            CompletedPreviewInvalidationDecision::Rejected {
                generation: stale_generation,
                reason: CompletedPreviewInvalidationRejection::StaleGeneration,
            }
        );

        assert_eq!(scrub_state.current_generation(), Some(current_generation));
        assert!(scrub_state.completed_preview_for(current_generation));
    }

    #[test]
    fn clear_fully_resets_scrub_state() {
        let generation = ScrubGeneration::default().next();
        let mut scrub_state = SessionScrubState::default();

        scrub_state.begin(generation);
        let intent = session_scrub_intent(generation, 19, SeekMode::Accurate);
        assert_eq!(
            scrub_state.accept_update(intent),
            SessionScrubUpdateAcceptance::Accepted { intent }
        );
        assert_eq!(
            scrub_state.note_visible_preview(generation, MediaTime::from_secs(18)),
            VisiblePreviewDecision::Recorded {
                preview: VisibleScrubPreview {
                    generation,
                    position: MediaTime::from_secs(18),
                },
            }
        );
        assert_eq!(
            scrub_state.complete_preview(generation),
            PreviewCompletionDecision::Completed { generation }
        );

        scrub_state.clear();

        assert_eq!(scrub_state.current_generation(), None);
        assert_eq!(scrub_state.latest_request_for(generation), None);
        assert_eq!(scrub_state.visible_preview_for(generation), None);
        assert!(!scrub_state.completed_preview_for(generation));
    }
}
