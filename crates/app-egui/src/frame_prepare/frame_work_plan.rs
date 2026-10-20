//! Решение «что делать на этом пробуждении UI-потока» (cpu-parity, сессия 04).
//!
//! До сессии 04 каждый redraw делал всё: egui-проход, подготовку видео, submit и present —
//! 60 раз в секунду независимо от того, менялось ли что-то. Теперь на каждом пробуждении
//! сначала собираются факты (ввод, события плеера, анимации, дедлайны, изменение UI-модели,
//! новый видеокадр), а чистая функция [`plan_frame_work`] переводит их в один из трёх планов.
//!
//! Инварианты:
//! - любая причина для egui-прохода даёт `FullPass` (интерфейс никогда не отстаёт);
//! - новый кадр без причин для интерфейса даёт `PresentReusingUi` (видео показывается с той же
//!   задержкой, что и раньше, но без `tessellate`/`update_buffers`);
//! - ничего не изменилось — `Skip`: ни surface, ни present, ни барьеров DMA-BUF.

/// Что делать с этим пробуждением UI-потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameWorkPlan {
    /// Полный проход: egui + видео + submit + present (как было до сессии 04).
    FullPass,
    /// Видео/субтитры изменились, интерфейс — нет: present с удержанными примитивами egui.
    PresentReusingUi,
    /// Ничего не менялось: кадр не берётся, не кодируется и не презентуется.
    Skip,
}

impl FrameWorkPlan {
    /// Стабильное имя для трассы и счётчиков.
    #[must_use]
    pub(crate) const fn metric_name(self) -> &'static str {
        match self {
            Self::FullPass => "full_pass",
            Self::PresentReusingUi => "present_reusing_ui",
            Self::Skip => "skip",
        }
    }

    /// `true`, если в этом кадре будет egui-проход.
    #[must_use]
    pub(crate) const fn runs_ui_pass(self) -> bool {
        matches!(self, Self::FullPass)
    }

    /// `true`, если кадр уходит в renderer (acquire/submit/present).
    #[must_use]
    pub(crate) const fn presents(self) -> bool {
        !matches!(self, Self::Skip)
    }
}

/// Факты, каждый из которых требует полного egui-прохода.
///
/// Порядок полей — это и порядок приоритета в [`UiPassReasons::primary_name`] для трассы.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UiPassReasons {
    /// С прошлого прохода пришли оконные события или ввод (egui их ещё не видел).
    pub(crate) window_input: bool,
    /// В этом пробуждении вычитаны события плеера (состояние, seek, ошибка, scrub).
    pub(crate) player_events: bool,
    /// Приложение само попросило ещё один проход (команда worker-у, действия панелей).
    pub(crate) worker_follow_up: bool,
    /// Другой владелец (субтитры, плейлист, startup) сообщил о видимой мутации.
    pub(crate) visible_mutation_wake: bool,
    /// egui просил перерисовку сейчас или его отложенный запрос уже созрел.
    pub(crate) egui_repaint_due: bool,
    /// Идёт анимация сайдбара или панелей фуллскрина.
    pub(crate) animation_in_flight: bool,
    /// Настал таймер UI-модели (тост, автоскрытие, Undo, секунда/пиксель позиции).
    pub(crate) ui_deadline_due: bool,
    /// Явная UI-модель кадра отличается от нарисованной в прошлый раз.
    pub(crate) ui_model_changed: bool,
    /// Панель телеметрии открыта, и подошёл срок обновить её строки.
    pub(crate) telemetry_refresh_due: bool,
    /// Удержанных примитивов нет (первый кадр, пересозданный renderer, другой размер окна).
    pub(crate) retained_ui_unavailable: bool,
    /// Принудительный проход после отказа shell-а повторить прежний интерфейс.
    pub(crate) forced: bool,
}

impl UiPassReasons {
    /// Хотя бы одна причина для egui-прохода.
    #[must_use]
    pub(crate) const fn any(self) -> bool {
        self.window_input
            || self.player_events
            || self.worker_follow_up
            || self.visible_mutation_wake
            || self.egui_repaint_due
            || self.animation_in_flight
            || self.ui_deadline_due
            || self.ui_model_changed
            || self.telemetry_refresh_due
            || self.retained_ui_unavailable
            || self.forced
    }

    /// Первая по приоритету причина — для трассы кадра (`ui_pass_reason`).
    #[must_use]
    pub(crate) const fn primary_name(self) -> &'static str {
        if self.forced {
            "forced"
        } else if self.retained_ui_unavailable {
            "retained_ui_unavailable"
        } else if self.window_input {
            "window_input"
        } else if self.player_events {
            "player_events"
        } else if self.worker_follow_up {
            "worker_follow_up"
        } else if self.visible_mutation_wake {
            "visible_mutation_wake"
        } else if self.egui_repaint_due {
            "egui_repaint_due"
        } else if self.animation_in_flight {
            "animation_in_flight"
        } else if self.ui_deadline_due {
            "ui_deadline_due"
        } else if self.ui_model_changed {
            "ui_model_changed"
        } else if self.telemetry_refresh_due {
            "telemetry_refresh_due"
        } else {
            "none"
        }
    }
}

/// Факты, каждый из которых требует показать кадр (даже при неизменном интерфейсе).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PresentReasons {
    /// В слоте показа другой кадр, чем на экране (или слот занят worker-ом — считаем новым).
    pub(crate) video_frame_changed: bool,
    /// Идёт seek/scrub или активен scrub-override: кадр решает владелец override-а.
    pub(crate) seek_or_scrub_activity: bool,
    /// Живая смена backend-а держит замороженный кадр: видео-стадия сама решает, что показать.
    pub(crate) backend_swap_hold: bool,
}

impl PresentReasons {
    /// Хотя бы одна причина показать кадр.
    #[must_use]
    pub(crate) const fn any(self) -> bool {
        self.video_frame_changed || self.seek_or_scrub_activity || self.backend_swap_hold
    }

    /// Первая по приоритету причина — для трассы кадра (`present_reason`).
    #[must_use]
    pub(crate) const fn primary_name(self) -> &'static str {
        if self.video_frame_changed {
            "video_frame_changed"
        } else if self.seek_or_scrub_activity {
            "seek_or_scrub_activity"
        } else if self.backend_swap_hold {
            "backend_swap_hold"
        } else {
            "none"
        }
    }
}

/// Переводит собранные факты в план кадра. Чистая функция: тестируется таблицей без окна.
#[must_use]
pub(crate) const fn plan_frame_work(ui: UiPassReasons, present: PresentReasons) -> FrameWorkPlan {
    if ui.any() {
        FrameWorkPlan::FullPass
    } else if present.any() {
        FrameWorkPlan::PresentReusingUi
    } else {
        FrameWorkPlan::Skip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Без единой причины кадр пропускается целиком — это и есть экономия на 24/30 fps.
    #[test]
    fn no_reasons_skip_the_frame() {
        assert_eq!(
            plan_frame_work(UiPassReasons::default(), PresentReasons::default()),
            FrameWorkPlan::Skip
        );
    }

    /// Новый кадр при неизменном интерфейсе показывается без egui-прохода.
    #[test]
    fn new_video_frame_alone_presents_with_retained_ui() {
        let present = PresentReasons {
            video_frame_changed: true,
            ..PresentReasons::default()
        };
        let plan = plan_frame_work(UiPassReasons::default(), present);
        assert_eq!(plan, FrameWorkPlan::PresentReusingUi);
        assert!(plan.presents());
        assert!(!plan.runs_ui_pass());
    }

    /// Каждая причина интерфейса сама по себе даёт полный проход, даже с новым кадром и без.
    #[test]
    fn every_single_ui_reason_forces_a_full_pass() {
        let reasons = [
            UiPassReasons {
                window_input: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                player_events: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                worker_follow_up: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                visible_mutation_wake: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                egui_repaint_due: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                animation_in_flight: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                ui_deadline_due: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                ui_model_changed: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                telemetry_refresh_due: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                retained_ui_unavailable: true,
                ..UiPassReasons::default()
            },
            UiPassReasons {
                forced: true,
                ..UiPassReasons::default()
            },
        ];
        for ui in reasons {
            assert!(ui.any(), "{ui:?}");
            assert_ne!(ui.primary_name(), "none", "{ui:?}");
            for present in [
                PresentReasons::default(),
                PresentReasons {
                    video_frame_changed: true,
                    ..PresentReasons::default()
                },
            ] {
                let plan = plan_frame_work(ui, present);
                assert_eq!(plan, FrameWorkPlan::FullPass, "{ui:?} / {present:?}");
                assert!(plan.runs_ui_pass());
                assert!(plan.presents());
            }
        }
    }

    /// Seek/scrub и живая смена backend-а показывают кадр, даже если identity слота та же.
    #[test]
    fn seek_scrub_and_backend_swap_present_without_ui_pass() {
        for present in [
            PresentReasons {
                seek_or_scrub_activity: true,
                ..PresentReasons::default()
            },
            PresentReasons {
                backend_swap_hold: true,
                ..PresentReasons::default()
            },
        ] {
            assert_eq!(
                plan_frame_work(UiPassReasons::default(), present),
                FrameWorkPlan::PresentReusingUi,
                "{present:?}"
            );
            assert_ne!(present.primary_name(), "none");
        }
    }

    /// Имя причины стабильно: первая по приоритету из нескольких одновременно.
    #[test]
    fn primary_reason_name_follows_priority_order() {
        let ui = UiPassReasons {
            ui_model_changed: true,
            window_input: true,
            animation_in_flight: true,
            ..UiPassReasons::default()
        };
        assert_eq!(ui.primary_name(), "window_input");
        assert_eq!(UiPassReasons::default().primary_name(), "none");
        assert_eq!(FrameWorkPlan::Skip.metric_name(), "skip");
    }
}
