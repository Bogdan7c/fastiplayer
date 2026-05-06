use std::collections::VecDeque;

use cros_codecs::Resolution;
use cros_codecs::libva::VA_RT_FORMAT_YUV420;
use cros_codecs::video_frame::VideoFrame;

use crate::internal_vaapi_frame::InternalVaapiFrame;

/// Пул lightweight descriptors для internal VA surfaces.
///
/// Используется как источник выходных буферов для cros-codecs VA-API decoder.
/// Сами surfaces создаёт VA driver в [`InternalVaapiFrame::to_native_handle`].
pub struct DmaFramePool {
    /// Текущее разрешение пула (coded resolution).
    resolution: Resolution,
    /// VA RT format для output surfaces текущего decoder sequence.
    rt_format: u32,
    /// Свободные кадры, готовые к выделению.
    free_frames: VecDeque<InternalVaapiFrame>,
}

impl DmaFramePool {
    /// Создаёт новый пул с `count` кадрами заданного разрешения.
    ///
    /// # Аргументы
    /// * `width` — ширина кадра в пикселях.
    /// * `height` — высота кадра в пикселях.
    /// * `count` — количество кадров в пуле.
    ///
    /// # Ошибки
    /// Возвращает ошибку если не удалось создать internal frame descriptors.
    pub fn new(width: u32, height: u32, count: usize) -> anyhow::Result<Self> {
        Self::new_with_rt_format(width, height, count, VA_RT_FORMAT_YUV420)
    }

    /// Создаёт новый пул с явным VA RT format для output surfaces.
    pub fn new_with_rt_format(
        width: u32,
        height: u32,
        count: usize,
        rt_format: u32,
    ) -> anyhow::Result<Self> {
        Self::new_for_resolution(width, height, count, rt_format)
    }

    /// Создаёт новый пул для заданного разрешения.
    fn new_for_resolution(
        width: u32,
        height: u32,
        count: usize,
        rt_format: u32,
    ) -> anyhow::Result<Self> {
        let resolution = Resolution::from((width, height));
        let mut free_frames = VecDeque::with_capacity(count);

        for i in 0..count {
            free_frames.push_back(Self::allocate_frame(resolution, rt_format, i)?);
        }

        tracing::info!(
            width = width,
            height = height,
            count = count,
            rt_format = rt_format,
            "DmaFramePool created for internal VA surfaces"
        );

        Ok(Self {
            resolution,
            rt_format,
            free_frames,
        })
    }

    /// Создаёт один internal VA output frame descriptor.
    fn allocate_frame(
        resolution: Resolution,
        rt_format: u32,
        frame_index: usize,
    ) -> anyhow::Result<InternalVaapiFrame> {
        tracing::trace!(
            frame_index,
            ?resolution,
            rt_format,
            "Internal VA frame descriptor allocated"
        );
        Ok(InternalVaapiFrame::new(resolution, rt_format))
    }

    /// Выделяет свободный кадр из пула.
    ///
    /// Возвращает `None` если пул пуст.
    pub fn alloc(&mut self) -> Option<InternalVaapiFrame> {
        self.free_frames.pop_front()
    }

    /// Выделяет свободный кадр или создаёт новый при необходимости.
    ///
    /// Если пул пуст (все кадры заняты decoder'ом как reference frames),
    /// выделяет новый descriptor on-the-fly. Это fallback для ситуаций,
    /// когда пул исчерпан, но decoder всё ещё нужны выходные буферы.
    ///
    /// # Примечание
    /// On-demand allocation медленнее reuse из пула, но гарантирует
    /// что decoder не остановится из-за нехватки буферов.
    pub fn alloc_or_allocate(&mut self) -> Option<InternalVaapiFrame> {
        // Сначала пытаемся взять готовый кадр из пула.
        if let Some(frame) = self.alloc() {
            return Some(frame);
        }

        // Pool exhausted — выделяем новый descriptor с тем же разрешением.
        Self::allocate_frame(self.resolution, self.rt_format, self.free_frames.len()).ok()
    }

    /// Возвращает кадр в пул для повторного использования.
    ///
    /// Если разрешение возвращаемого кадра не совпадает с текущим разрешением пула
    /// (например, после `FormatChanged`), кадр отбрасывается — он больше не валиден
    /// для текущего decoder'а.
    pub fn return_frame(&mut self, frame: InternalVaapiFrame) {
        // Проверяем что разрешение кадра совпадает с текущим разрешением пула.
        // После FormatChanged старые кадры могут вернуться с неправильным разрешением.
        if frame.resolution() != self.resolution || frame.rt_format() != self.rt_format {
            tracing::debug!(
                frame_res = ?frame.resolution(),
                pool_res = ?self.resolution,
                frame_rt_format = frame.rt_format(),
                pool_rt_format = self.rt_format,
                "Dropping frame with mismatched pool parameters after format change"
            );
            return;
        }
        self.free_frames.push_back(frame);
    }

    /// Пересоздаёт пул для нового разрешения.
    ///
    /// Старые кадры удаляются (drop).
    pub fn resize(&mut self, width: u32, height: u32, count: usize) -> anyhow::Result<()> {
        self.resize_with_rt_format(width, height, count, self.rt_format)
    }

    /// Пересоздаёт пул для нового разрешения и нового VA RT format.
    pub fn resize_with_rt_format(
        &mut self,
        width: u32,
        height: u32,
        count: usize,
        rt_format: u32,
    ) -> anyhow::Result<()> {
        self.free_frames.clear();
        let new_pool = Self::new_for_resolution(width, height, count, rt_format)?;
        self.resolution = new_pool.resolution;
        self.rt_format = new_pool.rt_format;
        self.free_frames = new_pool.free_frames;
        Ok(())
    }

    /// Возвращает текущее разрешение пула.
    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// Возвращает количество свободных кадров.
    pub fn num_free(&self) -> usize {
        self.free_frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тест: создание пула, alloc/return цикл.
    #[test]
    fn test_pool_alloc_return() {
        let mut pool = DmaFramePool::new(1920, 1080, 4).expect("pool creation failed");
        assert_eq!(pool.num_free(), 4);

        // Выделяем все кадры.
        let mut frames = Vec::new();
        for _ in 0..4 {
            frames.push(pool.alloc().expect("alloc failed"));
        }
        assert_eq!(pool.num_free(), 0);
        assert!(pool.alloc().is_none());

        // Возвращаем кадры.
        for frame in frames {
            pool.return_frame(frame);
        }
        assert_eq!(pool.num_free(), 4);
    }

    /// Тест: исчерпание пула возвращает None.
    #[test]
    fn test_pool_exhaustion() {
        let mut pool = DmaFramePool::new(320, 240, 2).expect("pool creation failed");

        let _f1 = pool.alloc().expect("first alloc");
        let _f2 = pool.alloc().expect("second alloc");
        assert!(pool.alloc().is_none(), "pool should be exhausted");
    }

    /// Тест: resize изменяет разрешение.
    #[test]
    fn test_pool_resize() {
        let mut pool = DmaFramePool::new(1920, 1080, 4).expect("pool creation failed");
        assert_eq!(pool.resolution().width, 1920);
        assert_eq!(pool.resolution().height, 1080);

        pool.resize(1280, 720, 2).expect("resize failed");
        assert_eq!(pool.resolution().width, 1280);
        assert_eq!(pool.resolution().height, 720);
        assert_eq!(pool.num_free(), 2);
    }
}
