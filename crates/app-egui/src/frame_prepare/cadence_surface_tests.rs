//! Настоящий shell transaction с fake source и настоящим materializer/GPU.
//! Проверка swapchain требует отдельного X11 процесса; pixel proof живёт рядом.

use super::*;
use render_core::RenderViewport;
use render_wgpu_shell::{RenderFrameInput, RenderFrameOutcome, RenderScreenDescriptor, Renderer};
use winit::event_loop::EventLoop;
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::window::Window;

fn input(window: &Window) -> RenderFrameInput<'_> {
    // Текстура нужна текущему UI draw, хотя этот же delta уже пометил её retired.
    // Преждевременный free сломает реальный overlay render на каждом пути теста.
    let texture_id = egui::TextureId::Managed(17);
    let mut mesh = egui::Mesh::with_texture(texture_id);
    let rectangle = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(8.0, 8.0));
    mesh.add_rect_with_uv(
        rectangle,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    RenderFrameInput {
        window,
        egui_paint_jobs: vec![egui::ClippedPrimitive {
            clip_rect: rectangle,
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }],
        egui_textures_delta: egui::TexturesDelta {
            set: vec![(
                texture_id,
                egui::epaint::ImageDelta::full(
                    egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
                    egui::TextureOptions::NEAREST,
                ),
            )],
            free: vec![texture_id],
        },
        screen: RenderScreenDescriptor {
            size_in_pixels: [64, 64],
            pixels_per_point: 1.0,
        },
        video_viewport: RenderViewport::full_surface(64, 64),
        video_exclusion_rects: Vec::new(),
        window_corner_mask: render_wgpu_shell::WindowCornerMask::square(),
    }
}

#[test]
#[ignore = "manual X11/Vulkan surface transaction; run alone"]
#[allow(deprecated)] // Одно тестовое окно без production event loop; API winit ещё поддерживается.
fn acquired_surface_preserves_video_ownership_and_recovers_after_drop_and_error() {
    let mut builder = EventLoop::builder();
    builder.with_x11().with_any_thread(true); // any_thread: Rust test runner.
    let event_loop = builder.build().expect("X11 event loop");
    let window = Arc::new(
        event_loop
            .create_window(
                Window::default_attributes().with_inner_size(winit::dpi::PhysicalSize::new(64, 64)),
            )
            .expect("test window"),
    );
    let mut renderer = Renderer::new(window.clone(), Default::default()).expect("Vulkan shell");
    let settings = renderer.live_settings();
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        renderer.device(),
        renderer.queue(),
        PresentFrameResourceProviderHandle::new(TwoFrames),
    );
    let releases = Arc::new(ReleaseProbe::default());
    let lease = frame(1, Duration::ZERO, &releases);

    // Absent video всё ещё доходит до clear/present; shell не трогает чужой lease.
    assert!(matches!(
        renderer
            .acquire_frame(input(&window))
            .expect("acquire clear")
            .render(None),
        RenderFrameOutcome::Presented(_)
    ));
    assert!(releases.0.lock().expect("releases").is_empty());

    // Отмена полученного surface не делает submit и не мешает следующему acquire.
    drop(
        renderer
            .acquire_frame(input(&window))
            .expect("acquire then abandon"),
    );
    assert!(releases.0.lock().expect("releases").is_empty());
    // WGPU 29 Vulkan discard — no-op: штатный lifecycle восстанавливает swapchain.
    renderer.resize(64, 64);
    let acquired = renderer
        .acquire_frame(input(&window))
        .expect("acquire after abandon");
    let prepared = prepare(lease.clone(), &materializer);
    let mut video = prepared
        .render_input_video_frame()
        .expect("input")
        .expect("video");
    // Ошибка контракта до draw обязана остаться Failed, а не clear/Presented.
    video.metadata.render_width = 0;
    assert!(matches!(
        acquired.render(Some(&video)),
        RenderFrameOutcome::Failed(_)
    ));
    drop(prepared);
    assert!(releases.0.lock().expect("releases").is_empty());

    renderer.resize(64, 64);
    let acquired = renderer
        .acquire_frame(input(&window))
        .expect("acquire after video error recovery");
    let prepared = prepare(lease.clone(), &materializer);
    let video = prepared
        .render_input_video_frame()
        .expect("input")
        .expect("video");
    assert!(matches!(
        acquired.render(Some(&video)),
        RenderFrameOutcome::Presented(_)
    ));
    // Shell принимает только borrowed video input: submission accounting принадлежит app.
    prepared.mark_submitted_to_renderer();
    renderer
        .device()
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .expect("GPU completion");
    drop(prepared);
    assert!(releases.0.lock().expect("releases").is_empty());
    drop(lease);
    let released = releases.0.lock().expect("releases");
    assert_eq!(released.len(), 1);
    assert!(released[0].submitted_to_renderer());
    assert_eq!(released[0].resource_handle(), FrameResourceHandle(1));
    assert_eq!(
        renderer.live_settings(),
        settings,
        "surface transaction must not own settings"
    );
}
