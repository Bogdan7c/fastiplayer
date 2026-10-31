//! Запоминание размера и фуллскрина окна браузерного режима (решения 137–138).
//!
//! Живая геометрия принадлежит окну и меняется немедленно. Этот модуль владеет только
//! «латест-онли + пауза»: последнее наблюдение окна ждёт [`BROWSER_WINDOW_GEOMETRY_DEBOUNCE`]
//! тишины и пишется одной транзакцией по трём ключам `browser_session.window.*`
//! (образец — `sidebar_resize.rs`). Обычное окно модуль не использует: запоминает только
//! браузерное (решение 138), своими ключами.
//!
//! Правила:
//! - оконный размер пишется только из оконного режима; размер в фуллскрине НЕ пишется —
//!   помним последний оконный (выход из фуллскрина вернёт окно к нему);
//! - в транзакцию идут только ключи, которые отличаются от committed, и только
//!   `browser_session.window.*` (больше никакие настройки не трогаются);
//! - выход и закрытие окна дописывают ждущее наблюдение сразу
//!   ([`SettingsRuntime::flush_pending_browser_window_geometry`]).

use fastiplayer_config::{
    MAX_BROWSER_WINDOW_HEIGHT_POINTS, MAX_BROWSER_WINDOW_WIDTH_POINTS,
    MIN_BROWSER_WINDOW_HEIGHT_POINTS, MIN_BROWSER_WINDOW_WIDTH_POINTS,
};

use super::*;

/// Тишина после последнего отличающегося наблюдения перед записью.
pub(super) const BROWSER_WINDOW_GEOMETRY_DEBOUNCE: Duration = Duration::from_millis(500);

/// Id ширины в реестре настроек.
const WIDTH_SETTING_ID: &str = "browser_session.window.inner_width_points";
/// Id высоты в реестре настроек.
const HEIGHT_SETTING_ID: &str = "browser_session.window.inner_height_points";
/// Id полноэкранности в реестре настроек.
const FULLSCREEN_SETTING_ID: &str = "browser_session.window.fullscreen";

/// Внутренний размер окна в логических точках, уже приведённый к пределам схемы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowedSizePoints {
    width: u16,
    height: u16,
}

impl WindowedSizePoints {
    /// Размер из значений окна; всё вне пределов схемы прижимается к ним.
    pub(crate) fn clamped(width: u32, height: u32) -> Self {
        let clamp = |value: u32, min: u16, max: u16| {
            u16::try_from(value).map_or(max, |points| points.clamp(min, max))
        };
        Self {
            width: clamp(
                width,
                MIN_BROWSER_WINDOW_WIDTH_POINTS,
                MAX_BROWSER_WINDOW_WIDTH_POINTS,
            ),
            height: clamp(
                height,
                MIN_BROWSER_WINDOW_HEIGHT_POINTS,
                MAX_BROWSER_WINDOW_HEIGHT_POINTS,
            ),
        }
    }
}

/// Что окно увидело о своей геометрии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserWindowObservation {
    /// Обычное (не полноэкранное) окно такого размера.
    Windowed(WindowedSizePoints),
    /// Окно во весь экран; его размер не интересен — помним оконный.
    Fullscreen,
}

/// Желаемое состояние, ещё не записанное в config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingBrowserWindowGeometry {
    /// Последний оконный размер.
    size: WindowedSizePoints,
    /// Окно во весь экран.
    fullscreen: bool,
    /// Когда писать (монотонные часы).
    persist_at: Instant,
}

/// Итог попытки записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserWindowGeometryFlushOutcome {
    /// Нечего писать или пауза ещё не прошла.
    NoPending,
    /// Записано (или config уже содержал эти значения).
    Succeeded,
    /// Транзакция не прошла; причина — в статусе настроек и журнале.
    Failed,
}

impl BrowserWindowGeometryFlushOutcome {
    /// Любая завершённая попытка требует перерисовки статуса.
    #[must_use]
    pub(crate) fn needs_redraw(self) -> bool {
        self != Self::NoPending
    }
}

impl SettingsRuntime {
    /// Committed-геометрия окна: размер и фуллскрин из config.
    fn committed_browser_window(&self) -> (WindowedSizePoints, bool) {
        let window = self.controller.committed().browser_session.window;
        (
            WindowedSizePoints {
                width: window.inner_width_points,
                height: window.inner_height_points,
            },
            window.fullscreen,
        )
    }

    /// Запоминает последнее наблюдение окна. `true` — ждущее значение изменилось.
    ///
    /// Наблюдение, равное уже ждущему (или committed, если ждущего нет), срок не двигает.
    pub(crate) fn record_browser_window_observation(
        &mut self,
        observation: BrowserWindowObservation,
        now: Instant,
    ) -> bool {
        let (base_size, base_fullscreen) = match self.pending_browser_window_geometry {
            Some(pending) => (pending.size, pending.fullscreen),
            None => self.committed_browser_window(),
        };
        let (size, fullscreen) = match observation {
            BrowserWindowObservation::Windowed(size) => (size, false),
            // Размер в фуллскрине не трогаем: помним оконный.
            BrowserWindowObservation::Fullscreen => (base_size, true),
        };
        if size == base_size && fullscreen == base_fullscreen {
            return false;
        }
        self.pending_browser_window_geometry = Some(PendingBrowserWindowGeometry {
            size,
            fullscreen,
            persist_at: now + BROWSER_WINDOW_GEOMETRY_DEBOUNCE,
        });
        true
    }

    /// Ближайший срок записи для спящего цикла.
    #[must_use]
    pub(crate) fn next_browser_window_geometry_deadline(&self) -> Option<Instant> {
        if self.is_apply_in_flight() {
            // Запись ждёт конец применения настроек (как у ширины сайдбара).
            return None;
        }
        self.pending_browser_window_geometry
            .map(|pending| pending.persist_at)
    }

    /// Пишет, только когда пауза уже прошла.
    pub(crate) fn flush_due_browser_window_geometry<A>(
        &mut self,
        now: Instant,
        runtime_adapter: &mut A,
    ) -> SettingsResult<BrowserWindowGeometryFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match self.pending_browser_window_geometry {
            Some(pending) if now >= pending.persist_at && !self.is_apply_in_flight() => {
                self.flush_pending_browser_window_geometry(runtime_adapter)
            }
            _ => Ok(BrowserWindowGeometryFlushOutcome::NoPending),
        }
    }

    /// Принудительно пишет ждущее значение (выход, закрытие окна).
    pub(crate) fn flush_pending_browser_window_geometry<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<BrowserWindowGeometryFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let Some(pending) = self.pending_browser_window_geometry.take() else {
            return Ok(BrowserWindowGeometryFlushOutcome::NoPending);
        };
        let requests = self.geometry_requests_differing_from_committed(pending);
        if requests.is_empty() {
            return Ok(BrowserWindowGeometryFlushOutcome::Succeeded);
        }
        let outcome =
            self.commit_runtime_settings_with_runtime_adapter(requests, runtime_adapter)?;
        let Some(report) = outcome.report() else {
            return Ok(BrowserWindowGeometryFlushOutcome::Succeeded);
        };
        if matches!(
            report.final_state,
            ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
        ) {
            return Ok(BrowserWindowGeometryFlushOutcome::Succeeded);
        }
        // Транзакция откатилась; окно остаётся таким, какое есть, config не изменился.
        let status = status_from_apply_report(&self.setting_labels(), report);
        self.invalidate_ui_model();
        self.status = status;
        tracing::error!(
            final_state = ?report.final_state,
            "Could not remember the browser window geometry in the settings file"
        );
        Ok(BrowserWindowGeometryFlushOutcome::Failed)
    }

    /// Запросы на запись только тех ключей геометрии, что отличаются от committed.
    fn geometry_requests_differing_from_committed(
        &self,
        pending: PendingBrowserWindowGeometry,
    ) -> Vec<RuntimeSettingCommitRequest> {
        let (committed_size, committed_fullscreen) = self.committed_browser_window();
        let mut requests = Vec::new();
        if pending.size.width != committed_size.width {
            requests.push(RuntimeSettingCommitRequest::new(
                WIDTH_SETTING_ID,
                SettingValue::Integer(i64::from(pending.size.width)),
            ));
        }
        if pending.size.height != committed_size.height {
            requests.push(RuntimeSettingCommitRequest::new(
                HEIGHT_SETTING_ID,
                SettingValue::Integer(i64::from(pending.size.height)),
            ));
        }
        if pending.fullscreen != committed_fullscreen {
            requests.push(RuntimeSettingCommitRequest::new(
                FULLSCREEN_SETTING_ID,
                SettingValue::Bool(pending.fullscreen),
            ));
        }
        requests
    }

    /// Подключает владельца окна буфера для следующего видео (окно браузерного режима).
    pub(crate) fn attach_browser_buffer_window_owner(
        &mut self,
        owner: Arc<dyn BrowserBufferWindowOwner>,
    ) {
        self.route_appliers
            .browser_session
            .attach_buffer_window_owner(owner);
    }
}
