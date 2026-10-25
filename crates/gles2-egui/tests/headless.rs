//! Настоящий EGL без окна (surfaceless): кадр рисуется в FBO, результат читается `glReadPixels`.
//!
//! На машине разработчика — настоящий GPU, в CI — llvmpipe (Mesa EGL ставится в job `tests`).
//! Без EGL тест пропускается с причиной в stderr. В CI переменная `GLES2_EGUI_REQUIRE_EGL=1`
//! превращает пропуск в падение: иначе пропавший пакет Mesa тихо выключил бы все проверки.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

use std::time::Duration;

use gles2_egui::{
    CreateError, Es100ProgramSources, FenceStatus, FrameStart, Gles2Context, GpuCompletionQueue,
    PixelExtent, PresentationError, PresentationSettings, ShaderError, ShaderStage, SurfaceSize,
    WindowAlphaMode,
};

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

fn size(width: u32, height: u32) -> SurfaceSize {
    SurfaceSize::new(width, height).expect("test sizes are non-zero")
}

/// Контекст без окна или пропуск теста с причиной.
fn offscreen_or_skip(test_name: &str, target_size: SurfaceSize) -> Option<Gles2Context> {
    match Gles2Context::offscreen(target_size) {
        Ok(context) => Some(context),
        // Нет libEGL, нет surfaceless-платформы или display не открылся (нет ни GPU, ни
        // llvmpipe) — среда без EGL, а не ошибка нашего кода.
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required ({REQUIRE_EGL_VARIABLE}) but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: offscreen GLES2 context creation failed: {error}"),
    }
}

/// Рисует кадр, залитый цветом, и показывает его.
fn draw_clear_frame(context: &mut Gles2Context, rgba: [f32; 4]) {
    match context.begin_frame().expect("frame starts") {
        FrameStart::Ready(frame) => {
            frame.clear(rgba);
            frame.present().expect("offscreen present succeeds");
        }
        FrameStart::Skipped(reason) => panic!("offscreen frame must not be skipped: {reason:?}"),
    }
}

fn assert_every_pixel(context: &Gles2Context, expected: [u8; 4]) {
    let pixels = context.read_offscreen_pixels().expect("offscreen readback");
    let target = pixels.size();
    for y in 0..target.height() {
        for x in 0..target.width() {
            assert_eq!(pixels.pixel(x, y), Some(expected), "pixel ({x}, {y})");
        }
    }
}

#[test]
fn clear_with_known_color_reads_back_exactly_that_color() {
    let Some(mut context) = offscreen_or_skip("clear_known_color", size(8, 4)) else {
        return;
    };
    // 0.0 и 1.0 представимы точно в 8 битах на любом драйвере.
    draw_clear_frame(&mut context, [1.0, 0.0, 1.0, 1.0]);
    assert_every_pixel(&context, [255, 0, 255, 255]);
}

#[test]
fn second_frame_replaces_the_first_one() {
    let Some(mut context) = offscreen_or_skip("second_frame", size(4, 4)) else {
        return;
    };
    draw_clear_frame(&mut context, [1.0, 0.0, 0.0, 1.0]);
    assert_every_pixel(&context, [255, 0, 0, 255]);
    draw_clear_frame(&mut context, [0.0, 0.0, 1.0, 1.0]);
    assert_every_pixel(&context, [0, 0, 255, 255]);
}

#[test]
fn resized_offscreen_target_reads_back_at_the_new_size() {
    let Some(mut context) = offscreen_or_skip("resized_target", size(4, 4)) else {
        return;
    };
    context
        .resize(PixelExtent {
            width: 6,
            height: 3,
        })
        .expect("offscreen resize");
    draw_clear_frame(&mut context, [0.0, 1.0, 0.0, 1.0]);
    let pixels = context.read_offscreen_pixels().expect("readback");
    assert_eq!(pixels.size(), size(6, 3));
    assert_eq!(pixels.pixel(5, 2), Some([0, 255, 0, 255]));
    assert_eq!(pixels.pixel(6, 0), None);
}

#[test]
fn real_driver_reports_strict_es2_baseline() {
    let Some(context) = offscreen_or_skip("driver_capabilities", size(2, 2)) else {
        return;
    };
    let capabilities = context.capabilities();
    assert!(
        capabilities.tiers().baseline.is_available(),
        "{:?}",
        capabilities.tiers().baseline
    );
    assert!(capabilities.reported_gles_version().major >= 2);
    assert!(capabilities.gl_version_string().starts_with("OpenGL ES "));
    assert!(
        capabilities.max_texture_size() >= 64,
        "ES 2.0 minimum is 64"
    );
    assert!(!capabilities.gl_extensions().is_empty());
    assert!(matches!(
        context.presentation().alpha,
        WindowAlphaMode::Opaque { .. }
    ));
    eprintln!(
        "driver: {} | {} | tiers {:?}",
        capabilities.gl_renderer(),
        capabilities.gl_version_string(),
        capabilities.tiers()
    );
}

#[test]
fn fence_after_a_frame_signals_and_releases_its_payload_once() {
    let Some(mut context) = offscreen_or_skip("fence_release", size(4, 4)) else {
        return;
    };
    draw_clear_frame(&mut context, [0.5, 0.5, 0.5, 1.0]);
    let fence = context.insert_fence().expect("fence is inserted");
    assert_eq!(
        fence.wait(Duration::from_secs(5)).expect("fence wait"),
        FenceStatus::Signaled
    );

    let mut queue = GpuCompletionQueue::new();
    queue.push(
        context.insert_fence().expect("second fence"),
        "decoded-frame",
    );
    // Ждём сигнала второго fence, потом забираем нагрузку.
    let mut released = Vec::new();
    for _ in 0..500 {
        released.extend(queue.drain_completed().released);
        if !released.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(released, vec!["decoded-frame"]);
    assert!(queue.drain_completed().released.is_empty());
    let stats = context.fence_stats();
    assert_eq!(stats.egl_fences_created + stats.fence_fallback_finishes, 2);
}

/// Поток-ожидатель освобождения кадров (сессия 07, решение владельца 32) ждёт fence в потоке, где
/// НИ ОДИН контекст не текущий. EGL_KHR_fence_sync это разрешает (`eglClientWaitSyncKHR` — объект
/// display-а, флаг FLUSH без текущего контекста игнорируется), поэтому fence обязан быть сброшен
/// в конвейер при вставке. Тест ловит fence, который без сброса или на чужом потоке не сработал бы.
#[test]
fn fence_signals_when_waited_from_a_thread_without_current_context() {
    let Some(mut context) = offscreen_or_skip("fence_cross_thread", size(64, 64)) else {
        return;
    };
    draw_clear_frame(&mut context, [0.25, 0.5, 0.75, 1.0]);
    let fence = std::sync::Arc::new(context.insert_fence().expect("fence is inserted"));

    let waited_on_another_thread = std::sync::Arc::clone(&fence);
    let status = std::thread::spawn(move || {
        waited_on_another_thread
            .wait(Duration::from_secs(5))
            .expect("fence wait on a thread without a current context")
    })
    .join()
    .expect("waiter thread finished");

    assert_eq!(status, FenceStatus::Signaled);
    // Поток рендера видит тот же итог: fence один и общий.
    assert_eq!(fence.poll().expect("fence poll"), FenceStatus::Signaled);
}

#[test]
fn es100_program_compiles_and_es3_or_broken_sources_are_typed_errors() {
    let Some(context) = offscreen_or_skip("es100_shaders", size(2, 2)) else {
        return;
    };
    let vertex = "#version 100\nattribute vec2 position;\nvoid main() { gl_Position = vec4(position, 0.0, 1.0); }\n";
    let fragment =
        "#version 100\nprecision mediump float;\nvoid main() { gl_FragColor = vec4(1.0); }\n";
    let program = context
        .compile_es100_program(Es100ProgramSources { vertex, fragment })
        .expect("GLSL ES 1.00 program compiles on every ES 2.0 driver");
    context.delete_program(program).expect("program deletion");

    let es3_fragment = "#version 300 es\nprecision mediump float;\nout vec4 color;\nvoid main() { color = vec4(1.0); }\n";
    assert_eq!(
        context.compile_es100_program(Es100ProgramSources {
            vertex,
            fragment: es3_fragment
        }),
        Err(ShaderError::NotEs100 {
            stage: ShaderStage::Fragment
        })
    );

    let broken_fragment = "#version 100\nvoid main() { gl_FragColor = undefined_value; }\n";
    let broken = context.compile_es100_program(Es100ProgramSources {
        vertex,
        fragment: broken_fragment,
    });
    assert!(
        matches!(
            &broken,
            Err(ShaderError::Compile {
                stage: ShaderStage::Fragment,
                ..
            })
        ),
        "{broken:?}"
    );
}

#[test]
fn offscreen_target_rejects_window_presentation_operations() {
    let Some(mut context) = offscreen_or_skip("offscreen_presentation_ops", size(2, 2)) else {
        return;
    };
    assert_eq!(
        context.reconfigure_presentation(PresentationSettings::default()),
        Err(PresentationError::NotAWindowTarget)
    );
    assert_eq!(
        context.detach_presentation(),
        Err(PresentationError::NotAWindowTarget)
    );
    // Отказ не сломал контекст: кадр рисуется дальше.
    draw_clear_frame(&mut context, [0.0, 0.0, 0.0, 1.0]);
    assert_every_pixel(&context, [0, 0, 0, 255]);
}

#[test]
fn two_contexts_on_the_shared_surfaceless_display_survive_each_other() {
    let Some(mut first) = offscreen_or_skip("shared_display_first", size(2, 2)) else {
        return;
    };
    let Some(mut second) = offscreen_or_skip("shared_display_second", size(2, 2)) else {
        return;
    };
    draw_clear_frame(&mut first, [1.0, 0.0, 0.0, 1.0]);
    draw_clear_frame(&mut second, [0.0, 1.0, 0.0, 1.0]);
    // Как пересоздание рендера: первый уходит, display не завершается под вторым.
    drop(first);
    draw_clear_frame(&mut second, [0.0, 0.0, 1.0, 1.0]);
    assert_every_pixel(&second, [0, 0, 255, 255]);
}
