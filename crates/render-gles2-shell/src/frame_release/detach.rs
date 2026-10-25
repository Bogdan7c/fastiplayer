//! Штатная отвязка рендера от привязки освобождения: `renderer_detaching`.
//!
//! # Зачем (корневая причина падения при выходе, найдена на T410)
//!
//! `GpuFence` держит `Arc<DisplayLease>`: пока жив хоть один fence, `eglTerminate` не вызывается.
//! Привязка хранит последний fence (`last_fence`) и fence в очереди ожидания, поэтому без явной
//! отвязки ПОСЛЕДНИЙ fence умирал в потоке-ожидателе (`gles2-frame-release`) при уничтожении
//! общего ядра, и именно этот поток делал `eglDestroySync` и финальный `eglTerminate` — уже
//! после уничтожения контекста и одновременно с `exit()` главного потока. Mesa (`libgallium`
//! на Intel Ironlake/crocus) в это время гонялась со своими atexit-обработчиками, и процесс
//! падал с `corrupted double-linked list` / SIGSEGV. Доказательство (T410): та же сборка с
//! принудительным сбросом `last_fence` в главном потоке — 10 из 10 без падений; без сброса — 10
//! из 10 падений; без сброса, но с паузой 500 мс перед выходом — 0 падений (гонка с `exit()`).
//!
//! # Инвариант
//!
//! **Ни один `GpuFence` не переживает свой рендер.** Все fence привязки разрушаются в потоке
//! рендера, пока контекст ещё жив, а поток-ожидатель после отвязки не держит ни одного fence и
//! не трогает EGL.
//!
//! # Контракт с рендером (сессия 09)
//!
//! Рендер ОБЯЗАН вызвать [`Gles2FrameReleaseBinding::renderer_detaching`] в своём потоке после
//! `VideoLayerRenderer::destroy` и ДО уничтожения `Gles2Context`. Если не вызвать: кадры всё
//! равно вернутся (поток-ожидатель доработает очередь, а `Drop` последнего клона привязки
//! сбросит `last_fence` в своём потоке), но fence очереди могут быть разрушены уже в потоке
//! ожидателя после уничтожения контекста, то есть вернётся исходная гонка с завершением процесса.

use std::time::{Duration, Instant};

use gles2_egui::FenceStatus;

use super::binding::Gles2FrameReleaseBinding;
use super::shared::{BindingMode, ReleaseCore, WaitingRelease};

/// Предел ожидания, пока поток-ожидатель отпустит клон записи. Он не держит запись дольше
/// одного слайса ожидания fence (50 мс) плюс возврат кадра, поэтому 500 мс — с большим запасом.
const WAITER_RELEASE_LIMIT: Duration = Duration::from_millis(500);

/// Сколько `renderer_detaching` готов ждать, пока GPU пройдёт fence ожидающих кадров, прежде
/// чем вернуть их декодеру «как есть».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererDetachGrace(Duration);

impl RendererDetachGrace {
    /// Отсрочка по умолчанию: GPU обычно заканчивает кадр за единицы миллисекунд, а за 250 мс
    /// успевает вся очередь пула декодера.
    pub const DEFAULT: Self = Self(Duration::from_millis(250));

    /// Явная отсрочка. `Duration::ZERO` — не ждать вовсе: сработавшие fence считаются
    /// «после fence», остальные кадры возвращаются сразу.
    #[must_use]
    pub const fn new(duration: Duration) -> Self {
        Self(duration)
    }

    /// Длительность отсрочки.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }
}

impl Default for RendererDetachGrace {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Итог [`Gles2FrameReleaseBinding::renderer_detaching`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RendererDetachReport {
    /// Привязка уже была отвязана: вызов ничего не сделал.
    pub already_detached: bool,
    /// Кадры, возвращённые декодеру этим вызовом после того, как GPU прошёл их fence.
    pub released_after_fence: usize,
    /// Кадры, возвращённые этим вызовом по истечении отсрочки, пока fence ещё не сработал.
    pub released_unfinished: usize,
    /// Кадры, возвращённые этим вызовом из-за отказа ожидания fence.
    pub released_after_fence_failure: usize,
    /// Поток-ожидатель не отпустил свой fence за предел ожидания (500 мс): последняя ссылка на
    /// fence может оказаться в его потоке (аномалия, пишется в лог как ошибка).
    pub waiter_still_holds_fence: bool,
}

impl Gles2FrameReleaseBinding {
    /// Рендер штатно отвязывается: вызывать в потоке рендера ПЕРЕД уничтожением `Gles2Context`.
    ///
    /// Что делает, по шагам:
    /// 1. переводит привязку в режим «рендер отвязан»: новые fence не принимаются, новые запросы
    ///    освобождения возвращаются декодеру сразу, поток-ожидатель новых записей не получит;
    /// 2. забирает `last_fence` и очередь ожидания, дожидается fence каждого ожидающего кадра в
    ///    пределах `grace` и возвращает кадр декодеру (ровно один раз, не раньше fence, кроме
    ///    случая «отсрочка вышла» — он считается отдельно в отчёте и статистике);
    /// 3. дожидается, пока поток-ожидатель отпустит запись, которую он ждал в этот момент;
    /// 4. разрушает все забранные fence здесь, в вызывающем потоке, пока контекст жив.
    ///
    /// Повторный вызов — no-op (`already_detached`). Привязку можно вернуть в работу через
    /// [`Self::resume_after_context_recreated`]. Клоны привязки в провайдере и декодере остаются
    /// рабочими: они по-прежнему возвращают кадры (теперь сразу).
    pub fn renderer_detaching(&self, grace: RendererDetachGrace) -> RendererDetachReport {
        let core = self.core();
        let Some(detached) = take_fences_for_detach(core) else {
            return RendererDetachReport {
                already_detached: true,
                ..RendererDetachReport::default()
            };
        };
        let deadline = Instant::now() + grace.duration();
        let mut report = RendererDetachReport::default();
        for entry in &detached.waiting {
            release_waiting_entry(core, entry, deadline, &mut report);
        }
        // Шаг 3 ДО шага 4: пока ожидатель держит клон fence, наша ссылка не последняя, и
        // разрушение `EGLSync` случилось бы в его потоке.
        if !core.wait_until_waiter_releases_entry(WAITER_RELEASE_LIMIT) {
            report.waiter_still_holds_fence = true;
            log::error!(
                "поток освобождения кадров GLES2 не отпустил fence за {WAITER_RELEASE_LIMIT:?}: \
                 EGLSync может быть разрушен в его потоке"
            );
        }
        drop(detached);
        report
    }
}

/// Забранные у привязки fence: живут, пока вызывающий не закончил, и умирают в его потоке.
struct DetachedFences {
    waiting: std::collections::VecDeque<WaitingRelease>,
    // Поле держит ссылку на fence последнего кадра до конца отвязки; читать его не нужно.
    _last_fence: Option<std::sync::Arc<dyn super::fence::PresentedFrameFence>>,
}

/// Шаг 1–2: меняет режим и забирает fence под блокировкой. `None` — уже отвязана.
fn take_fences_for_detach(core: &ReleaseCore) -> Option<DetachedFences> {
    let mut state = core.lock_state();
    if state.mode == BindingMode::RendererDetached {
        return None;
    }
    state.mode = BindingMode::RendererDetached;
    Some(DetachedFences {
        waiting: std::mem::take(&mut state.waiting),
        _last_fence: state.last_fence.take(),
    })
}

/// Ждёт fence одной записи до `deadline` и возвращает её кадр декодеру (если его ещё не вернули
/// другим путём — гонку с потоком-ожидателем решает CAS в `release_once`).
fn release_waiting_entry(
    core: &ReleaseCore,
    entry: &WaitingRelease,
    deadline: Instant,
    report: &mut RendererDetachReport,
) {
    if entry.release.is_released() {
        return;
    }
    // `wait(ZERO)` у истёкшего срока работает как неблокирующая проверка: уже сработавший
    // fence всё равно считается «после fence».
    let remaining = deadline.saturating_duration_since(Instant::now());
    let counters = &core.counters;
    match entry.fence.wait(remaining) {
        Ok(FenceStatus::Signaled) => {
            if entry
                .release
                .release_once(&counters.released_at_renderer_detach_after_fence)
            {
                report.released_after_fence += 1;
            }
        }
        Ok(FenceStatus::Pending) => {
            if entry
                .release
                .release_once(&counters.released_at_renderer_detach_unfinished)
            {
                report.released_unfinished += 1;
            }
        }
        Err(error) => {
            log::warn!(
                "ожидание fence при отвязке рендера GLES2 отказало ({error}); кадр возвращён декодеру"
            );
            if entry.release.release_once(&counters.fence_wait_failures) {
                report.released_after_fence_failure += 1;
            }
        }
    }
}
