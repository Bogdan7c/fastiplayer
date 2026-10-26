//! Тесты пересоздания неоптимального swapchain-а на подделке, ведущей себя как Mesa WSI.
//!
//! `MesaLikeSwapchain` повторяет поведение `wsi_common_wayland.c`: буферы выделены на
//! модификаторах из feedback-а композитора на момент создания swapchain-а; если композитор
//! прислал другой набор (fullscreen → scanout-транш), каждый acquire отвечает `Suboptimal`,
//! пока приложение не пересоздаст swapchain. Кадр «показывается на экране напрямую» только
//! если его буфер выделен на модификаторе, который композитор сейчас предпочитает.

use super::*;

/// Набор модификаторов буфера в терминах теста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufferModifiers {
    /// Модификаторы для рендера (композитор вынужден копировать кадр).
    RenderOptimized,

    /// Модификаторы, которые дисплей показывает напрямую (direct scanout).
    Scanout,
}

/// Буфер кадра подделки: на каких модификаторах он выделен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeFrame {
    modifiers: BufferModifiers,
}

/// Отказ acquire у подделки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeAcquireTimeout;

/// Swapchain, ведущий себя как Mesa Vulkan WSI на Wayland.
struct MesaLikeSwapchain {
    /// Модификаторы, на которых выделены буферы текущего swapchain-а.
    allocated: BufferModifiers,

    /// Модификаторы из последнего feedback-а композитора.
    preferred_by_compositor: BufferModifiers,

    /// Драйвер не умеет выделить буферы на предпочитаемых модификаторах.
    cannot_follow_feedback: bool,

    /// Следующее пересоздание будет отклонено surface-ом.
    reject_next_rebuild: bool,

    /// Следующий acquire закончится таймаутом.
    time_out_next_acquire: bool,

    /// Сколько раз приложение пересоздало swapchain.
    rebuilds: usize,

    /// Сколько раз приложение просило буфер.
    acquires: usize,
}

impl MesaLikeSwapchain {
    /// Окно в обычном режиме: композитор просит рендерные модификаторы, swapchain на них.
    const fn windowed() -> Self {
        Self {
            allocated: BufferModifiers::RenderOptimized,
            preferred_by_compositor: BufferModifiers::RenderOptimized,
            cannot_follow_feedback: false,
            reject_next_rebuild: false,
            time_out_next_acquire: false,
            rebuilds: 0,
            acquires: 0,
        }
    }

    /// Композитор прислал новый feedback (KWin: окно стало / перестало быть fullscreen).
    const fn compositor_sends_feedback(&mut self, modifiers: BufferModifiers) {
        self.preferred_by_compositor = modifiers;
    }

    /// Перенастройка surface по другой причине (resize): Mesa создаёт swapchain по feedback-у.
    fn reconfigure_for_resize(&mut self) {
        if !self.cannot_follow_feedback {
            self.allocated = self.preferred_by_compositor;
        }
    }
}

impl SwapchainPort for MesaLikeSwapchain {
    type Frame = FakeFrame;
    type AcquireError = FakeAcquireTimeout;

    fn rebuild_swapchain(&mut self) -> Result<(), String> {
        if self.reject_next_rebuild {
            self.reject_next_rebuild = false;
            return Err("validation: surface rejected configuration".to_owned());
        }
        self.rebuilds += 1;
        if !self.cannot_follow_feedback {
            self.allocated = self.preferred_by_compositor;
        }
        Ok(())
    }

    fn acquire_image(&mut self) -> Result<(FakeFrame, AcquiredSwapchainFit), FakeAcquireTimeout> {
        self.acquires += 1;
        if self.time_out_next_acquire {
            self.time_out_next_acquire = false;
            return Err(FakeAcquireTimeout);
        }
        let fit = if self.allocated == self.preferred_by_compositor {
            AcquiredSwapchainFit::Optimal
        } else {
            AcquiredSwapchainFit::Suboptimal
        };
        Ok((
            FakeFrame {
                modifiers: self.allocated,
            },
            fit,
        ))
    }
}

/// Получает `count` кадров подряд и возвращает модификаторы буфера каждого.
fn present_frames(
    recovery: &mut SuboptimalSwapchainRecovery,
    swapchain: &mut MesaLikeSwapchain,
    count: usize,
) -> Vec<BufferModifiers> {
    (0..count)
        .map(|_| {
            recovery
                .acquire(swapchain)
                .expect("frame acquired")
                .modifiers
        })
        .collect()
}

#[test]
fn fullscreen_scanout_feedback_is_adopted_from_the_next_frame() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    present_frames(&mut recovery, &mut swapchain, 3);

    // Окно ушло в fullscreen: KWin предлагает scanout-модификаторы.
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    let frames = present_frames(&mut recovery, &mut swapchain, 6);

    // Первый кадр ещё на старом буфере (он рабочий и показывается), дальше — scanout.
    assert_eq!(frames[0], BufferModifiers::RenderOptimized);
    assert!(
        frames[1..]
            .iter()
            .all(|modifiers| *modifiers == BufferModifiers::Scanout)
    );
    // Ровно одно пересоздание: не каждый кадр.
    assert_eq!(swapchain.rebuilds, 1);
}

#[test]
fn optimal_swapchain_is_never_rebuilt() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();

    let frames = present_frames(&mut recovery, &mut swapchain, 20);

    assert_eq!(swapchain.rebuilds, 0);
    assert!(
        frames
            .iter()
            .all(|modifiers| *modifiers == BufferModifiers::RenderOptimized)
    );
}

#[test]
fn leaving_fullscreen_rebuilds_back_to_render_modifiers() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    present_frames(&mut recovery, &mut swapchain, 4);

    swapchain.compositor_sends_feedback(BufferModifiers::RenderOptimized);
    let frames = present_frames(&mut recovery, &mut swapchain, 4);

    assert_eq!(frames.last(), Some(&BufferModifiers::RenderOptimized));
    assert_eq!(swapchain.rebuilds, 2);
}

#[test]
fn driver_that_cannot_follow_feedback_stops_after_fruitless_limit() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.cannot_follow_feedback = true;
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);

    let frames = present_frames(&mut recovery, &mut swapchain, 30);

    // Кадры не теряются, но пересоздание не крутится каждый кадр.
    assert_eq!(frames.len(), 30);
    assert_eq!(swapchain.rebuilds, usize::from(MAX_FRUITLESS_REBUILDS));
    assert_eq!(swapchain.acquires, 30);
}

#[test]
fn successful_rebuild_restores_the_full_fruitless_budget() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.cannot_follow_feedback = true;
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    // Две бесплодные попытки (на одну меньше предела).
    present_frames(&mut recovery, &mut swapchain, 3);
    assert_eq!(swapchain.rebuilds, 2);

    // Драйвер «поумнел»: третья попытка удаётся и обнуляет счётчик.
    swapchain.cannot_follow_feedback = false;
    present_frames(&mut recovery, &mut swapchain, 2);
    assert_eq!(swapchain.rebuilds, 3);

    // Новый неудовлетворимый feedback снова получает полный бюджет попыток.
    swapchain.cannot_follow_feedback = true;
    swapchain.compositor_sends_feedback(BufferModifiers::RenderOptimized);
    present_frames(&mut recovery, &mut swapchain, 20);
    assert_eq!(swapchain.rebuilds, 3 + usize::from(MAX_FRUITLESS_REBUILDS));
}

#[test]
fn surface_reconfiguration_rearms_exhausted_recovery() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.cannot_follow_feedback = true;
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    present_frames(&mut recovery, &mut swapchain, 10);
    assert_eq!(swapchain.rebuilds, usize::from(MAX_FRUITLESS_REBUILDS));

    // Resize с прежним неудовлетворимым feedback-ом, затем драйвер снова может.
    swapchain.reconfigure_for_resize();
    recovery.surface_reconfigured();
    swapchain.cannot_follow_feedback = false;
    let frames = present_frames(&mut recovery, &mut swapchain, 3);

    assert_eq!(frames.last(), Some(&BufferModifiers::Scanout));
    assert_eq!(swapchain.rebuilds, usize::from(MAX_FRUITLESS_REBUILDS) + 1);
}

#[test]
fn reconfiguration_elsewhere_cancels_a_pending_rebuild() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    present_frames(&mut recovery, &mut swapchain, 1);

    // Resize успел пересоздать swapchain по свежему feedback-у до следующего кадра.
    swapchain.reconfigure_for_resize();
    recovery.surface_reconfigured();
    let frames = present_frames(&mut recovery, &mut swapchain, 3);

    assert_eq!(swapchain.rebuilds, 0);
    assert!(
        frames
            .iter()
            .all(|modifiers| *modifiers == BufferModifiers::Scanout)
    );
}

#[test]
fn rejected_rebuild_is_reported_without_acquiring_and_not_retried_blindly() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    present_frames(&mut recovery, &mut swapchain, 1);
    swapchain.reject_next_rebuild = true;

    let failure = recovery
        .acquire(&mut swapchain)
        .expect_err("rebuild rejected");

    assert!(matches!(
        failure,
        SwapchainAcquireFailure::RebuildRejected { ref message } if message.contains("rejected")
    ));
    // После отказа буфер не запрашивался: surface без рабочей конфигурации.
    assert_eq!(swapchain.acquires, 1);
    // Вызывающий восстановил surface (resize-подобная настройка) — дальше всё штатно.
    swapchain.reconfigure_for_resize();
    let frames = present_frames(&mut recovery, &mut swapchain, 2);
    assert_eq!(
        frames,
        vec![BufferModifiers::Scanout, BufferModifiers::Scanout]
    );
    assert_eq!(swapchain.rebuilds, 0);
}

#[test]
fn acquire_failure_passes_through_and_keeps_the_scheduled_check() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut swapchain = MesaLikeSwapchain::windowed();
    swapchain.compositor_sends_feedback(BufferModifiers::Scanout);
    present_frames(&mut recovery, &mut swapchain, 1);
    swapchain.time_out_next_acquire = true;

    let failure = recovery
        .acquire(&mut swapchain)
        .expect_err("acquire timed out");

    assert!(matches!(
        failure,
        SwapchainAcquireFailure::Acquire(FakeAcquireTimeout)
    ));
    // Пересоздание успело пройти до таймаута; следующий кадр уже на scanout без повтора.
    assert_eq!(swapchain.rebuilds, 1);
    let frames = present_frames(&mut recovery, &mut swapchain, 2);
    assert_eq!(
        frames,
        vec![BufferModifiers::Scanout, BufferModifiers::Scanout]
    );
    assert_eq!(swapchain.rebuilds, 1);
}

#[test]
fn rebuild_is_due_exactly_once_per_suboptimal_image() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    assert_eq!(
        recovery.take_rebuild_before_acquire(),
        SwapchainRebuild::NotNeeded
    );

    assert_eq!(
        recovery.record_acquired_image(AcquiredSwapchainFit::Suboptimal),
        SuboptimalVerdict::RebuildScheduled
    );

    assert_eq!(
        recovery.take_rebuild_before_acquire(),
        SwapchainRebuild::Due
    );
    assert_eq!(
        recovery.take_rebuild_before_acquire(),
        SwapchainRebuild::NotNeeded
    );
}

#[test]
fn verdicts_report_exhaustion_once_then_accept() {
    let mut recovery = SuboptimalSwapchainRecovery::new();
    let mut verdicts = Vec::new();
    for _ in 0..6 {
        recovery.take_rebuild_before_acquire();
        verdicts.push(recovery.record_acquired_image(AcquiredSwapchainFit::Suboptimal));
    }

    assert_eq!(
        verdicts,
        vec![
            SuboptimalVerdict::RebuildScheduled,
            SuboptimalVerdict::RebuildScheduled,
            SuboptimalVerdict::RebuildScheduled,
            SuboptimalVerdict::RebuildsExhausted,
            SuboptimalVerdict::AcceptedAfterExhaustion,
            SuboptimalVerdict::AcceptedAfterExhaustion,
        ]
    );
}
