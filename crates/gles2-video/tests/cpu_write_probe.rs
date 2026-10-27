//! Самопроверка видимости записи CPU на настоящем EGL и настоящем буфере ядра (`udmabuf`).
//!
//! Производители-двойники отличаются только тем, КОГДА запись доходит до памяти: сразу, только
//! на сбросе кэша или никогда. Результат проверки — единственный наблюдаемый итог; рядом
//! проверяется, что проверка не оставила следов в состоянии GL и в объектах контекста.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use std::os::fd::{AsFd, BorrowedFd};

use gles2_video::{
    CPU_WRITE_PROBE_MIN_BUFFER_BYTES, ChromaSubsampling, ColorAdjustment, CpuWriteProbeProducer,
    CpuWriteVisibility, VideoLayerOptions, probe_cpu_write_visibility,
};
use support::state::snapshot;
use support::udmabuf::Udmabuf;
use support::{
    OwnedFrame, assert_close, draw_frame, extent, fill_request, offscreen_or_skip, pixel, renderer,
    size,
};

fn probe_buffer(test_name: &str) -> Option<Udmabuf> {
    match Udmabuf::create(&vec![0_u8; CPU_WRITE_PROBE_MIN_BUFFER_BYTES]) {
        Ok(buffer) => Some(buffer),
        Err(reason) => {
            eprintln!("skip {test_name}: {reason}");
            None
        }
    }
}

/// Запись доходит до памяти сразу и с полным сбросом кэша: GPU видит её без отдельного сброса.
struct WriteThroughProducer(Udmabuf);

impl CpuWriteProbeProducer for WriteThroughProducer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.0.dma_buf.as_fd()
    }
    fn write_all_bytes(&mut self, value: u8) {
        self.0
            .overwrite_contents_as_cpu_producer(&vec![value; CPU_WRITE_PROBE_MIN_BUFFER_BYTES]);
    }
    fn flush_cpu_cache(&mut self) {}
}

/// Настоящая «голая» запись CPU без сброса; сброс — отдельный вызов, как у плеера.
struct PlainWriteProducer(Udmabuf);

impl CpuWriteProbeProducer for PlainWriteProducer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.0.dma_buf.as_fd()
    }
    fn write_all_bytes(&mut self, value: u8) {
        self.0
            .write_without_cache_flush(&vec![value; CPU_WRITE_PROBE_MIN_BUFFER_BYTES]);
    }
    fn flush_cpu_cache(&mut self) {
        self.0.flush_written_cache();
    }
}

/// Запись «застревает» в кэше CPU и доходит до памяти только на сбросе (модель Ironlake).
struct FlushOnlyProducer {
    buffer: Udmabuf,
    pending: Option<u8>,
}

impl CpuWriteProbeProducer for FlushOnlyProducer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.buffer.dma_buf.as_fd()
    }
    fn write_all_bytes(&mut self, value: u8) {
        self.pending = Some(value);
    }
    fn flush_cpu_cache(&mut self) {
        if let Some(value) = self.pending.take() {
            self.buffer
                .overwrite_contents_as_cpu_producer(&vec![value; CPU_WRITE_PROBE_MIN_BUFFER_BYTES]);
        }
    }
}

/// Запись не доходит до памяти никогда (даже после сброса).
struct NeverVisibleProducer(Udmabuf);

impl CpuWriteProbeProducer for NeverVisibleProducer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.0.dma_buf.as_fd()
    }
    fn write_all_bytes(&mut self, _value: u8) {}
    fn flush_cpu_cache(&mut self) {}
}

fn is_cache_coherent_renderer(gl_renderer: &str) -> bool {
    ["llvmpipe", "softpipe", "AMD", "Radeon", "radeonsi"]
        .iter()
        .any(|name| gl_renderer.contains(name))
}

#[test]
fn write_through_producer_is_coherent_without_a_flush() {
    let name = "write_through_producer_is_coherent_without_a_flush";
    let Some(context) = offscreen_or_skip(name, size(16, 16)) else {
        return;
    };
    let Some(buffer) = probe_buffer(name) else {
        return;
    };
    let mut producer = WriteThroughProducer(buffer);
    assert_eq!(
        probe_cpu_write_visibility(&context, &mut producer).unwrap(),
        CpuWriteVisibility::CoherentWithoutCacheFlush
    );
}

/// Настоящая голая запись: на программном рендере и AMD кэш общий, на других GPU допустим сброс,
/// но `NotVisible` на исправной машине означал бы, что путь без копии недоступен.
#[test]
fn plain_cpu_write_is_visible_on_this_machine() {
    let name = "plain_cpu_write_is_visible_on_this_machine";
    let Some(context) = offscreen_or_skip(name, size(16, 16)) else {
        return;
    };
    let Some(buffer) = probe_buffer(name) else {
        return;
    };
    let mut producer = PlainWriteProducer(buffer);
    let visibility = probe_cpu_write_visibility(&context, &mut producer).unwrap();
    let gl_renderer = context.capabilities().gl_renderer().to_owned();
    if is_cache_coherent_renderer(&gl_renderer) {
        assert_eq!(
            visibility,
            CpuWriteVisibility::CoherentWithoutCacheFlush,
            "{gl_renderer}"
        );
    } else {
        assert_ne!(visibility, CpuWriteVisibility::NotVisible, "{gl_renderer}");
    }
}

#[test]
fn producer_that_needs_a_flush_is_reported_as_visible_after_flush() {
    let name = "producer_that_needs_a_flush_is_reported_as_visible_after_flush";
    let Some(context) = offscreen_or_skip(name, size(16, 16)) else {
        return;
    };
    let Some(buffer) = probe_buffer(name) else {
        return;
    };
    let mut producer = FlushOnlyProducer {
        buffer,
        pending: None,
    };
    assert_eq!(
        probe_cpu_write_visibility(&context, &mut producer).unwrap(),
        CpuWriteVisibility::VisibleAfterCacheFlush
    );
}

#[test]
fn producer_whose_writes_never_arrive_is_not_visible() {
    let name = "producer_whose_writes_never_arrive_is_not_visible";
    let Some(context) = offscreen_or_skip(name, size(16, 16)) else {
        return;
    };
    let Some(buffer) = probe_buffer(name) else {
        return;
    };
    let mut producer = NeverVisibleProducer(buffer);
    assert_eq!(
        probe_cpu_write_visibility(&context, &mut producer).unwrap(),
        CpuWriteVisibility::NotVisible
    );
}

/// Проверка вне кадра: ни привязки, ни флаги, ни объекты контекста не изменились, а видео
/// после неё рисуется пиксель в пиксель как до неё.
#[test]
fn probe_leaves_gl_state_objects_and_the_video_picture_untouched() {
    let name = "probe_leaves_gl_state_objects_and_the_video_picture_untouched";
    let target = size(64, 32);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(buffer) = probe_buffer(name) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let host = OwnedFrame::from_pattern(
        ChromaSubsampling::Yuv420,
        extent(32, 16),
        0,
        &support::FramePattern {
            luma: &|x, y| u8::try_from(30 + x * 5 + y * 3).unwrap(),
            chroma_u: &|x, _| u8::try_from(60 + x * 9).unwrap(),
            chroma_v: &|_, y| u8::try_from(200 - y * 11).unwrap(),
        },
    );
    video.upload(&context, &host.upload(1)).unwrap();
    let rects = [support::full_rect(target)];
    let request = fill_request(
        target,
        &rects,
        gles2_video::YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
    );
    let (_, picture_before) = draw_frame(&mut context, &mut video, &request);
    let state_before = snapshot(&context);

    let mut producer = FlushOnlyProducer {
        buffer,
        pending: None,
    };
    probe_cpu_write_visibility(&context, &mut producer).unwrap();

    assert_eq!(
        snapshot(&context),
        state_before,
        "bindings, flags and object counts are restored"
    );
    let stats = context.dma_buf_texture_stats();
    assert_eq!(
        stats.images_created, stats.images_destroyed,
        "no EGLImage leaks"
    );
    assert!(
        stats.images_created > 0,
        "the probe really imported a plane"
    );
    // Тот же кадр (тот же ключ) рисуется заново: картинка прежняя.
    let (_, picture_after) = draw_frame(&mut context, &mut video, &request);
    for y in 0..32 {
        for x in 0..64 {
            assert_close(
                pixel(&picture_after, x, y),
                pixel(&picture_before, x, y),
                0,
                "picture after the probe",
            );
        }
    }
    video.destroy(&context).unwrap();
}

/// Производитель, чей «буфер» не dma-buf (`/dev/null`).
struct NotADmaBufProducer(std::fs::File);

impl CpuWriteProbeProducer for NotADmaBufProducer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
    fn write_all_bytes(&mut self, _value: u8) {}
    fn flush_cpu_cache(&mut self) {}
}

/// Отказ драйвера на плоскости проверки — типизированная ошибка (а не вердикт «не видно»), а
/// состояние GL и объекты контекста возвращены так же, как при успехе.
#[test]
fn driver_rejection_is_an_error_and_still_restores_everything() {
    let name = "driver_rejection_is_an_error_and_still_restores_everything";
    let Some(context) = offscreen_or_skip(name, size(16, 16)) else {
        return;
    };
    let gl_renderer = context.capabilities().gl_renderer();
    if ["llvmpipe", "softpipe"]
        .iter()
        .any(|driver| gl_renderer.contains(driver))
    {
        eprintln!("skip {name}: software driver ({gl_renderer}) does not validate dma-buf fds");
        return;
    }
    let state_before = snapshot(&context);
    let mut producer = NotADmaBufProducer(std::fs::File::open("/dev/null").unwrap());
    let error = probe_cpu_write_visibility(&context, &mut producer).unwrap_err();
    assert!(
        matches!(error, gles2_video::CpuWriteProbeError::Attach(_)),
        "{error:?}"
    );
    assert_eq!(snapshot(&context), state_before);
    let stats = context.dma_buf_texture_stats();
    assert_eq!(stats.images_created, stats.images_destroyed);
}

/// Первые две записи доходят сразу, дальше кэш «залипает» до сброса: два значения подряд видны,
/// но источник НЕ когерентный — проверка обязана потребовать третье значение.
struct GoesStaleAfterTwoWritesProducer {
    buffer: Udmabuf,
    writes: u32,
    pending: Option<u8>,
}

impl CpuWriteProbeProducer for GoesStaleAfterTwoWritesProducer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.buffer.dma_buf.as_fd()
    }
    fn write_all_bytes(&mut self, value: u8) {
        self.writes += 1;
        if self.writes <= 2 {
            self.buffer
                .overwrite_contents_as_cpu_producer(&vec![value; CPU_WRITE_PROBE_MIN_BUFFER_BYTES]);
        } else {
            self.pending = Some(value);
        }
    }
    fn flush_cpu_cache(&mut self) {
        if let Some(value) = self.pending.take() {
            self.buffer
                .overwrite_contents_as_cpu_producer(&vec![value; CPU_WRITE_PROBE_MIN_BUFFER_BYTES]);
        }
    }
}

#[test]
fn two_visible_writes_in_a_row_are_not_enough_for_coherence() {
    let name = "two_visible_writes_in_a_row_are_not_enough_for_coherence";
    let Some(context) = offscreen_or_skip(name, size(16, 16)) else {
        return;
    };
    let Some(buffer) = probe_buffer(name) else {
        return;
    };
    let mut producer = GoesStaleAfterTwoWritesProducer {
        buffer,
        writes: 0,
        pending: None,
    };
    assert_eq!(
        probe_cpu_write_visibility(&context, &mut producer).unwrap(),
        CpuWriteVisibility::VisibleAfterCacheFlush
    );
}
