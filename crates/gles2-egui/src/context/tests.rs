//! Сессия EGL на fake-драйвере: порядок разрушения, общий display, ошибки создания, отпуск и
//! возврат окна. Без GPU и без окна.

use super::driver::{
    ConfigId, ContextId, CurrentBinding, EglErrorCode, EglVersion, NativeDisplay,
    PlatformWindowTarget, ResetNotificationRequest, SurfaceSize,
};
use super::egl_objects::{EglSession, ResetNotification, SessionRequest, SessionTarget};
use super::platform::NativeWindow;
use super::safety::{SystemEgl, SystemEglLoadError};
use crate::damage::BufferAge;
use crate::error::CreateError;
use crate::frame::PresentKind;
use crate::presentation::{
    DetachOutcome, FramebufferAlpha, ReattachOutcome, SwapIntervalOutcome, VsyncMode,
};
use crate::test_support::fake_egl::{FakeBehavior, FakeCall, FakeEgl, fake_config};

const WAYLAND_DISPLAY: NativeDisplay = NativeDisplay::Wayland {
    display_address: 0xD15,
};

fn window_size() -> SurfaceSize {
    match SurfaceSize::new(640, 360) {
        Some(size) => size,
        None => panic!("640×360 is a valid size"),
    }
}

fn wayland_request() -> SessionRequest {
    SessionRequest {
        native_display: WAYLAND_DISPLAY,
        target: SessionTarget::Window {
            window: NativeWindow::Wayland {
                surface_address: 0x5EF,
            },
            size: window_size(),
        },
    }
}

fn open_or_panic(
    registry: &std::sync::Arc<super::display_registry::DisplayRegistry>,
    request: SessionRequest,
) -> EglSession {
    match EglSession::open(registry, request) {
        Ok(session) => session,
        Err(error) => panic!("fake session must open: {error}"),
    }
}

/// Позиция первого вызова, подходящего под условие.
fn position(journal: &[FakeCall], predicate: impl Fn(&FakeCall) -> bool) -> usize {
    match journal.iter().position(predicate) {
        Some(index) => index,
        None => panic!("call not found in journal: {journal:#?}"),
    }
}

#[test]
fn wayland_session_tears_down_surface_then_window_then_context_then_display() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    fake.clear_journal();

    drop(session);

    let journal = fake.journal();
    let release = position(&journal, |call| {
        *call == FakeCall::MakeCurrent(CurrentBinding::Nothing)
    });
    let surface = position(&journal, |call| matches!(call, FakeCall::DestroySurface(_)));
    let window = position(&journal, |call| {
        matches!(call, FakeCall::DestroyWaylandEglWindow(_))
    });
    let context = position(&journal, |call| matches!(call, FakeCall::DestroyContext(_)));
    let display = position(&journal, |call| matches!(call, FakeCall::Terminate(_)));
    assert!(
        release < surface && surface < window && window < context && context < display,
        "teardown order is wrong: {journal:#?}"
    );
    fake.assert_nothing_alive();
}

#[test]
fn two_sessions_on_one_native_display_terminate_it_once_after_the_last() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let first = open_or_panic(&registry, wayland_request());
    let second = open_or_panic(&registry, wayland_request());
    assert_eq!(registry.lease_count(WAYLAND_DISPLAY), 2);
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::Initialize(_))),
        1,
        "the shared display is initialized once"
    );

    // Как при пересоздании рендера (02a): старый уходит, новый на том же окне живёт дальше.
    drop(first);
    assert_eq!(fake.count(|call| matches!(call, FakeCall::Terminate(_))), 0);
    assert!(
        second.make_current().is_ok(),
        "survivor keeps a usable display"
    );

    drop(second);
    assert_eq!(fake.count(|call| matches!(call, FakeCall::Terminate(_))), 1);
    assert_eq!(registry.lease_count(WAYLAND_DISPLAY), 0);
    fake.assert_nothing_alive();
}

#[test]
fn reopening_after_the_last_session_initializes_the_display_again() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    drop(open_or_panic(&registry, wayland_request()));
    let reopened = open_or_panic(&registry, wayland_request());
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::Initialize(_))),
        2
    );
    drop(reopened);
    fake.assert_nothing_alive();
}

#[test]
fn failed_window_surface_releases_everything_already_created() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        window_surface_error: Some(EglErrorCode::BadNativeWindow),
        ..FakeBehavior::default()
    });
    let result = EglSession::open(&registry, wayland_request());
    assert!(matches!(
        result,
        Err(CreateError::SurfaceCreation(error)) if error.code == EglErrorCode::BadNativeWindow
    ));
    // `wl_egl_window`, контекст и display не утекли.
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::DestroyWaylandEglWindow(_))),
        1
    );
    fake.assert_nothing_alive();
}

#[test]
fn missing_gles2_config_is_a_typed_error_without_leaks() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        configs: Vec::new(),
        ..FakeBehavior::default()
    });
    let result = EglSession::open(&registry, wayland_request());
    assert!(matches!(result, Err(CreateError::NoGles2Config)));
    fake.assert_nothing_alive();
}

#[test]
fn missing_platform_extension_fails_before_opening_a_display() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        client_extensions: "EGL_EXT_client_extensions EGL_KHR_platform_x11".to_owned(),
        ..FakeBehavior::default()
    });
    let result = EglSession::open(&registry, wayland_request());
    assert!(matches!(
        result,
        Err(CreateError::PlatformExtensionMissing {
            extension: "EGL_KHR_platform_wayland"
        })
    ));
    assert!(fake.journal().is_empty(), "no driver call before the check");
}

#[test]
fn egl_display_older_than_1_5_is_rejected_and_terminated() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        display_version: EglVersion { major: 1, minor: 4 },
        ..FakeBehavior::default()
    });
    let result = EglSession::open(&registry, wayland_request());
    assert!(matches!(
        result,
        Err(CreateError::EglVersionUnsupported {
            found: EglVersion { major: 1, minor: 4 }
        })
    ));
    fake.assert_nothing_alive();
}

#[test]
fn rejected_robust_context_falls_back_to_a_plain_one() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        robust_context_error: Some(EglErrorCode::BadAttribute),
        ..FakeBehavior::default()
    });
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(session.reset_notification(), ResetNotification::Unavailable);
    assert_eq!(
        fake.count(|call| matches!(
            call,
            FakeCall::CreateContext {
                reset_notification: ResetNotificationRequest::NotRequested,
                ..
            }
        )),
        1
    );
    drop(session);
    fake.assert_nothing_alive();
}

#[test]
fn robust_context_is_requested_when_the_extension_exists() {
    let (_fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(
        session.reset_notification(),
        ResetNotification::LoseContextOnReset
    );
}

#[test]
fn context_creation_failure_is_reported_as_such() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        display_extensions: "EGL_KHR_fence_sync".to_owned(),
        context_error: Some(EglErrorCode::BadConfig),
        ..FakeBehavior::default()
    });
    let result = EglSession::open(&registry, wayland_request());
    assert!(matches!(
        result,
        Err(CreateError::ContextCreation(error)) if error.code == EglErrorCode::BadConfig
    ));
    fake.assert_nothing_alive();
}

#[test]
fn context_lost_on_first_make_current_is_context_lost_not_a_driver_error() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        make_current_error: Some(EglErrorCode::ContextLost),
        ..FakeBehavior::default()
    });
    let result = EglSession::open(&registry, wayland_request());
    assert!(matches!(result, Err(CreateError::ContextLost(_))));
    fake.assert_nothing_alive();
}

#[test]
fn window_session_selects_config_without_alpha_even_when_alpha_config_exists() {
    // Конфиги по умолчанию: 1 без альфы, 2 с альфой. Окно непрозрачное — нужен первый.
    let (_fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(session.config().description.id, ConfigId(1));
    assert_eq!(session.config().alpha, FramebufferAlpha::Absent);
}

#[test]
fn window_session_with_only_alpha_configs_reports_framebuffer_alpha() {
    let (_fake, registry) = FakeEgl::with_registry(FakeBehavior {
        configs: vec![fake_config(7, 8, 8, 0x22)],
        ..FakeBehavior::default()
    });
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(session.config().description.id, ConfigId(7));
    assert_eq!(session.config().alpha, FramebufferAlpha::Present);
}

#[test]
fn x11_session_passes_window_xid_and_matches_visual() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        configs: vec![fake_config(1, 8, 8, 0x20), fake_config(2, 8, 8, 0x21)],
        ..FakeBehavior::default()
    });
    let session = open_or_panic(
        &registry,
        SessionRequest {
            native_display: NativeDisplay::X11 {
                display_address: Some(0xABC),
            },
            target: SessionTarget::Window {
                window: NativeWindow::X11 {
                    window_xid: 0x0420_0007,
                    visual_id: Some(0x21),
                },
                size: window_size(),
            },
        },
    );
    assert_eq!(session.config().description.native_visual_id, 0x21);
    assert_eq!(
        fake.count(|call| matches!(
            call,
            FakeCall::CreateWindowSurface(
                _,
                PlatformWindowTarget::X11 {
                    window_xid: 0x0420_0007
                }
            )
        )),
        1
    );
    // У X11 нет `wl_egl_window`.
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::CreateWaylandEglWindow(_))),
        0
    );
}

#[test]
fn detach_destroys_only_the_surface_and_reattach_creates_a_new_one() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let mut session = open_or_panic(&registry, wayland_request());
    fake.clear_journal();

    assert!(matches!(
        session.detach_window(),
        Ok(DetachOutcome::Detached)
    ));
    let journal = fake.journal();
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::DestroySurface(_))),
        1
    );
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::DestroyContext(_))),
        0,
        "context and GL objects survive detach"
    );
    // Контекст снят с surface ДО её разрушения и остался текущим без surface.
    let unbind = position(&journal, |call| {
        matches!(
            call,
            FakeCall::MakeCurrent(CurrentBinding::Surfaceless { .. })
        )
    });
    let destroy = position(&journal, |call| matches!(call, FakeCall::DestroySurface(_)));
    assert!(unbind < destroy, "{journal:#?}");
    assert!(!session.has_window_surface());
    assert!(session.can_be_current_without_surface());

    // Повторный detach — no-op, второго разрушения нет (fake упал бы).
    assert!(matches!(
        session.detach_window(),
        Ok(DetachOutcome::AlreadyDetached)
    ));
    assert_eq!(
        session.swap_buffers(),
        Ok(false),
        "nothing to present while detached"
    );

    assert!(matches!(
        session.reattach_window(window_size()),
        Ok(ReattachOutcome::Reattached)
    ));
    assert!(session.has_window_surface());
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::CreateWindowSurface(..))),
        1
    );
    assert!(matches!(
        session.reattach_window(window_size()),
        Ok(ReattachOutcome::AlreadyAttached)
    ));
    assert_eq!(session.swap_buffers(), Ok(true));

    drop(session);
    fake.assert_nothing_alive();
}

#[test]
fn lost_context_still_releases_the_window_for_the_next_renderer() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let mut session = open_or_panic(&registry, wayland_request());
    // После сброса GPU любая привязка контекста отвечает EGL_CONTEXT_LOST.
    fake.update_behavior(|behavior| behavior.make_current_error = Some(EglErrorCode::ContextLost));
    fake.clear_journal();

    assert_eq!(
        session.detach_window_after_context_loss(),
        DetachOutcome::Detached
    );
    let journal = fake.journal();
    let unbind = position(&journal, |call| {
        *call == FakeCall::MakeCurrent(CurrentBinding::Nothing)
    });
    let destroy = position(&journal, |call| matches!(call, FakeCall::DestroySurface(_)));
    assert!(unbind < destroy, "{journal:#?}");
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::DestroyWaylandEglWindow(_))),
        1,
        "the window is free for a new surface"
    );
    assert_eq!(
        session.detach_window_after_context_loss(),
        DetachOutcome::AlreadyDetached
    );
    drop(session);
    fake.assert_nothing_alive();
}

#[test]
fn detached_window_without_surfaceless_context_unbinds_the_context() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        display_extensions: "EGL_KHR_fence_sync".to_owned(),
        ..FakeBehavior::default()
    });
    let mut session = open_or_panic(&registry, wayland_request());
    fake.clear_journal();
    assert!(matches!(
        session.detach_window(),
        Ok(DetachOutcome::Detached)
    ));
    assert!(
        fake.journal()
            .contains(&FakeCall::MakeCurrent(CurrentBinding::Nothing))
    );
    assert!(!session.can_be_current_without_surface());
}

#[test]
fn vsync_is_applied_on_the_window_surface_and_clamped_by_config() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(
        session.apply_vsync(VsyncMode::WaitForVblank),
        Ok(Some(SwapIntervalOutcome::AsRequested { interval: 1 }))
    );
    assert_eq!(
        session.apply_vsync(VsyncMode::Immediate),
        Ok(Some(SwapIntervalOutcome::AsRequested { interval: 0 }))
    );
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::SwapInterval(_))),
        2
    );
}

#[test]
fn resize_reaches_the_wayland_egl_window() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    let Some(bigger) = SurfaceSize::new(1280, 720) else {
        panic!("valid size");
    };
    session.resize_window(bigger);
    assert_eq!(
        fake.count(
            |call| matches!(call, FakeCall::ResizeWaylandEglWindow(_, size) if *size == bigger)
        ),
        1
    );
}

#[test]
fn offscreen_without_surfaceless_context_binds_a_pbuffer() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior {
        display_extensions: "EGL_KHR_fence_sync".to_owned(),
        ..FakeBehavior::default()
    });
    let session = open_or_panic(
        &registry,
        SessionRequest {
            native_display: NativeDisplay::Surfaceless,
            target: SessionTarget::Offscreen,
        },
    );
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::CreatePbufferSurface(_))),
        1
    );
    assert!(
        fake.journal()
            .iter()
            .any(|call| matches!(call, FakeCall::MakeCurrent(CurrentBinding::Surface { .. })))
    );
    drop(session);
    fake.assert_nothing_alive();
}

#[test]
fn offscreen_with_surfaceless_context_creates_no_surface() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(
        &registry,
        SessionRequest {
            native_display: NativeDisplay::Surfaceless,
            target: SessionTarget::Offscreen,
        },
    );
    assert_eq!(
        fake.count(|call| matches!(
            call,
            FakeCall::CreatePbufferSurface(_) | FakeCall::CreateWindowSurface(..)
        )),
        0
    );
    let context = fake.journal().iter().find_map(|call| match call {
        FakeCall::CreateContext { context, .. } => Some(*context),
        _ => None,
    });
    let Some(context) = context else {
        panic!("context created");
    };
    assert!(
        fake.journal()
            .contains(&FakeCall::MakeCurrent(CurrentBinding::Surfaceless {
                context: ContextId(context.0)
            }))
    );
    drop(session);
}

#[test]
fn missing_libegl_is_a_typed_error() {
    let result = SystemEgl::load_from_library_name("libfastiplayer-missing-egl.so.1");
    assert!(matches!(
        result,
        Err(SystemEglLoadError::LibraryUnavailable { .. })
    ));
}

/// Настоящий offscreen-контекст или пропуск с причиной (как в `tests/headless.rs`).
fn real_offscreen_or_skip(test_name: &str) -> Option<super::Gles2Context> {
    let Some(size) = SurfaceSize::new(4, 4) else {
        panic!("4×4 is a valid size");
    };
    match super::Gles2Context::offscreen(size) {
        Ok(context) => Some(context),
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os("GLES2_EGUI_REQUIRE_EGL").is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: offscreen context creation failed: {error}"),
    }
}

#[test]
fn lost_context_answers_context_lost_everywhere_and_drops_without_gl() {
    use crate::error::Gles2Error;
    use crate::loss::{ContextLoss, ContextLossDetection, ContextLossReason};

    let Some(mut context) = real_offscreen_or_skip("lost_context_everywhere") else {
        return;
    };
    let loss = ContextLoss {
        reason: ContextLossReason::GuiltyReset,
        detection: ContextLossDetection::ResetStatusQuery,
    };
    context.simulate_context_loss(loss);

    assert_eq!(context.context_loss(), Some(loss));
    assert!(matches!(
        context.begin_frame(),
        Err(Gles2Error::ContextLost(reported)) if reported == loss
    ));
    assert!(matches!(
        context.insert_fence(),
        Err(Gles2Error::ContextLost(reported)) if reported == loss
    ));
    assert!(matches!(
        context.gl(),
        Err(super::GlAccessError::ContextLost(reported)) if reported == loss
    ));
    assert!(matches!(
        context.resize(super::PixelExtent {
            width: 8,
            height: 8
        }),
        Err(Gles2Error::ContextLost(_))
    ));
    assert!(matches!(
        context.read_offscreen_pixels(),
        Err(super::ReadbackError::GlUnavailable(
            super::GlAccessError::ContextLost(_)
        ))
    ));
    assert_eq!(context.check_context_health(), Err(loss));
    // Разрушение после потери не трогает GL (объекты отдаются драйверу) и не паникует.
    drop(context);
}

// ---- Частичный показ: возраст буфера и показ с повреждениями на fake-драйвере ----

fn behavior_with_extensions(extra: &str) -> FakeBehavior {
    let base = FakeBehavior::default();
    FakeBehavior {
        display_extensions: format!("{} {extra}", base.display_extensions),
        ..base
    }
}

fn damage_rect(x: i32, y: i32, width: i32, height: i32) -> super::driver::DamageRect {
    super::driver::DamageRect {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn buffer_age_query_reaches_the_driver_and_maps_zero_to_undefined() {
    let mut behavior = behavior_with_extensions("EGL_EXT_buffer_age");
    behavior.buffer_age_script = [0, 1, 3].into();
    let (fake, registry) = FakeEgl::with_registry(behavior);
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(session.query_buffer_age(), Ok(BufferAge::Undefined));
    assert_eq!(session.query_buffer_age(), Ok(BufferAge::Frames(1)));
    assert_eq!(session.query_buffer_age(), Ok(BufferAge::Frames(3)));
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::QueryBufferAge(_))),
        3
    );
}

#[test]
fn missing_buffer_age_extension_never_asks_the_driver() {
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(session.query_buffer_age(), Ok(BufferAge::Unsupported));
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::QueryBufferAge(_))),
        0
    );
}

#[test]
fn buffer_age_is_unsupported_while_the_window_is_detached() {
    let mut behavior = behavior_with_extensions("EGL_EXT_buffer_age");
    behavior.default_buffer_age = 2;
    let (_fake, registry) = FakeEgl::with_registry(behavior);
    let mut session = open_or_panic(&registry, wayland_request());
    assert!(matches!(
        session.detach_window(),
        Ok(DetachOutcome::Detached)
    ));
    assert_eq!(session.query_buffer_age(), Ok(BufferAge::Unsupported));
}

#[test]
fn buffer_age_driver_error_is_typed() {
    let mut behavior = behavior_with_extensions("EGL_EXT_buffer_age");
    behavior.buffer_age_error = Some(EglErrorCode::BadSurface);
    let (_fake, registry) = FakeEgl::with_registry(behavior);
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(
        session.query_buffer_age(),
        Err(super::driver::EglCallError::new(
            super::driver::EglOperation::QueryBufferAge,
            EglErrorCode::BadSurface
        ))
    );
}

#[test]
fn damage_swap_sends_bottom_left_rects_through_the_advertised_extension() {
    for (extension_name, expected) in [
        (
            "EGL_KHR_swap_buffers_with_damage",
            super::driver::DamageSwapExtension::Khr,
        ),
        (
            "EGL_EXT_swap_buffers_with_damage",
            super::driver::DamageSwapExtension::Ext,
        ),
    ] {
        let (fake, registry) = FakeEgl::with_registry(behavior_with_extensions(extension_name));
        let session = open_or_panic(&registry, wayland_request());
        let rects = [damage_rect(10, 20, 30, 40)];
        assert_eq!(
            session.swap_buffers_with_damage(&rects),
            Ok(Some(PresentKind::WithDamage))
        );
        let swaps = fake.damage_swaps();
        assert_eq!(swaps.len(), 1, "{extension_name}");
        assert_eq!(swaps[0].extension, expected);
        assert_eq!(swaps[0].rects, rects.to_vec());
        assert_eq!(
            fake.count(|call| matches!(call, FakeCall::SwapBuffers(_))),
            0,
            "a damage swap replaces the plain swap"
        );
    }
}

#[test]
fn khr_is_preferred_when_both_damage_extensions_exist() {
    let (fake, registry) = FakeEgl::with_registry(behavior_with_extensions(
        "EGL_EXT_swap_buffers_with_damage EGL_KHR_swap_buffers_with_damage",
    ));
    let session = open_or_panic(&registry, wayland_request());
    assert!(
        session
            .swap_buffers_with_damage(&[damage_rect(0, 0, 1, 1)])
            .is_ok()
    );
    assert_eq!(
        fake.damage_swaps()[0].extension,
        super::driver::DamageSwapExtension::Khr
    );
}

#[test]
fn damage_swap_without_extension_or_without_rects_is_a_plain_swap() {
    // Расширения нет: тот же вызов превращается в обычный показ, прямоугольники не теряют кадр.
    let (fake, registry) = FakeEgl::with_registry(FakeBehavior::default());
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(
        session.swap_buffers_with_damage(&[damage_rect(1, 2, 3, 4)]),
        Ok(Some(PresentKind::Plain))
    );
    assert!(fake.damage_swaps().is_empty());
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::SwapBuffers(_))),
        1
    );

    // Расширение есть, но список пуст: по спецификации это «вся surface», то есть обычный показ.
    let (fake, registry) =
        FakeEgl::with_registry(behavior_with_extensions("EGL_KHR_swap_buffers_with_damage"));
    let session = open_or_panic(&registry, wayland_request());
    assert_eq!(
        session.swap_buffers_with_damage(&[]),
        Ok(Some(PresentKind::Plain))
    );
    assert!(fake.damage_swaps().is_empty());
}

#[test]
fn damage_swap_has_nothing_to_present_while_detached_and_reports_driver_errors() {
    let mut behavior = behavior_with_extensions("EGL_KHR_swap_buffers_with_damage");
    behavior.damage_swap_error = Some(EglErrorCode::BadSurface);
    let (_fake, registry) = FakeEgl::with_registry(behavior);
    let mut session = open_or_panic(&registry, wayland_request());
    assert_eq!(
        session.swap_buffers_with_damage(&[damage_rect(0, 0, 1, 1)]),
        Err(super::driver::EglCallError::new(
            super::driver::EglOperation::SwapBuffersWithDamage,
            EglErrorCode::BadSurface
        ))
    );
    assert!(matches!(
        session.detach_window(),
        Ok(DetachOutcome::Detached)
    ));
    assert_eq!(
        session.swap_buffers_with_damage(&[damage_rect(0, 0, 1, 1)]),
        Ok(None)
    );
}
