//! Видео из НАСТОЯЩЕГО FFmpeg-декодера доходит до показа в GLES2-рендере (сессия 09 плана
//! gles2-renderer): production-цепочка плеера целиком, кроме окна и приложения.
//!
//! FFmpeg decoder thread (кадры в блоках нашего `get_buffer2`, stride больше видимой
//! ширины) → `prepare_video_import` (обёртка освобождения по fence) → `install_video_import` →
//! аренда кадра → `materialize` → `acquire_surface_frame` → `render_frame` → `Presented` с
//! ожидаемым pts и пикселями. У 8-битных потоков каждый пиксель каждого кадра сверяется с
//! CPU-формулой цвета по байтам того же дескриптора; все кадры возвращаются декодеру ровно по
//! одному разу по fence.
//!
//! - `harness` — общая цепочка и проверки путей «копия» / «без копии»;
//! - `vp9` — VP9 8 и 10 бит (сессии 09 и 12);
//! - `av1` — AV1 через libdav1d с `DR1` из патча `ffmpeg-vendor` (сессия 13): 8 и 10 бит и
//!   кадры с зерном плёнки без CPU-копии.
//!
//! Фикстуры — закоммиченные маленькие синтетические IVF из `crates/video-ffmpeg` (генерация —
//! `tests/fixtures/direct-frame-buffers/generate.sh` там же, источник `testsrc2`).
//!
//! Без EGL тест пропускается с причиной; `GLES2_EGUI_REQUIRE_EGL=1` превращает пропуск в
//! падение. Аппаратный VA-API путь здесь не гоняется (нужна видеокарта и файл владельца): его
//! покрывают `dma_buf_frame_to_screen` (настоящие буферы ядра `udmabuf`) и стенд
//! `video-vaapi/examples/gles2_dmabuf_bench.rs`, а живую приёмку делает главный агент.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "integration test: panics are test assertions; pixel math on tiny frames"
)]

mod av1;
mod harness;
mod vp9;
