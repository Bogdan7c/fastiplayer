/// Тонкая egui-specific граница поверх `egui_wgpu::Renderer`.
///
/// Shell остаётся владельцем swapchain lifecycle, а этот модуль владеет только
/// порядком обновления egui texture atlas/buffers и overlay render pass-ом.
///
/// С сессии 04 cpu-parity compositor ещё и удерживает примитивы последней загрузки
/// (`RetainedEguiFrame`): если интерфейс не менялся, а видео — да, кадр рисуется поверх
/// нового видео **теми же** примитивами без `tessellate`/`update_buffers` на стороне app.
/// Это допустимо по контракту egui-wgpu: `Renderer::render` читает vertex/index буферы
/// последнего `update_buffers`, а `ScreenDescriptor` обязан совпадать (uniform размера
/// экрана пишется только в `update_buffers`).
use crate::EguiFrameContent;

pub(crate) struct EguiCompositor {
    /// Непосредственный renderer из egui-wgpu.
    renderer: egui_wgpu::Renderer,

    /// Примитивы и размер экрана последнего `update_buffers`; ровно они лежат в буферах
    /// renderer-а. `None` до первого свежего кадра (после создания или пересоздания shell-а).
    retained: Option<RetainedEguiFrame>,
}

/// Примитивы, для которых буферы `egui_wgpu::Renderer` сейчас актуальны.
struct RetainedEguiFrame {
    /// Tessellated примитивы последней загрузки.
    paint_jobs: Vec<egui::epaint::ClippedPrimitive>,

    /// Размер экрана и scale, с которыми они были загружены (uniform renderer-а).
    screen_descriptor: egui_wgpu::ScreenDescriptor,
}

/// Содержимое egui между первой стадией кадра (upload текстур / проверка удержанного) и
/// `update_buffers`. Shell создаёт encoder между ними, поэтому стадии разделены.
pub(crate) enum PendingEguiContent {
    /// Свежие примитивы и их delta текстур (delta освобождается после submit-а/отказа).
    Fresh {
        paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
        textures_delta: egui::TexturesDelta,
    },
    /// Удержанные примитивы прошлого свежего кадра уже проверены на совместимость.
    Reused,
}

/// Почему удержанный egui-кадр нельзя нарисовать повторно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedEguiFrameUnavailable {
    /// Ещё не было ни одного свежего кадра (или renderer пересоздан).
    NothingRetained,

    /// Размер экрана/scale отличается от того, с которым примитивы загружались.
    ScreenMismatch,
}

impl EguiCompositor {
    /// Создаёт egui compositor для выбранного swapchain format.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let renderer = egui_wgpu::Renderer::new(
            device,
            surface_format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: None,
                msaa_samples: 1,
                ..Default::default()
            },
        );

        Self {
            renderer,
            retained: None,
        }
    }

    /// Загружает новые и изменённые egui-текстуры до записи command buffer-а кадра.
    ///
    /// Retired-текстуры здесь намеренно не освобождаются: текущие paint jobs всё ещё
    /// могут ссылаться на них. Владелец frame submission обязан вызвать
    /// [`Self::free_retired_textures`] только после submit-а либо отказа от кадра.
    pub fn upload_changed_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures_delta: &egui::TexturesDelta,
    ) {
        // egui 0.36 группирует дельты по текстуре: внутри одного id порядок дельт
        // важен (частичные обновления поверх целой загрузки), между разными id — нет,
        // как и в эталонной интеграции `egui_wgpu::winit`.
        for (id, image_deltas) in &textures_delta.set {
            for image_delta in image_deltas {
                self.renderer
                    .update_texture(device, queue, *id, image_delta);
            }
        }
    }

    /// Освобождает текстуры, которые egui больше не использует.
    ///
    /// Метод является отдельной boundary-операцией, чтобы порядок
    /// `upload -> prepare -> render -> submit -> free` был виден в callsite.
    ///
    /// Это последний шаг жизни `TexturesDelta` кадра, поэтому метод забирает её
    /// по значению. Вызывающий обязан до этого применить `set` через
    /// [`Self::upload_changed_textures`].
    pub fn free_retired_textures(&mut self, mut textures_delta: egui::TexturesDelta) {
        for id in &textures_delta.free {
            self.renderer.free_texture(id);
        }
        // egui 0.36 в debug-сборке паникует, если `TexturesDelta` уничтожается
        // с непустыми `set`/`free` (защита от потерянных обновлений текстур).
        // Здесь обе части уже применены: `set` — при upload в начале кадра,
        // `free` — циклом выше. Очищаем явно, как требует egui.
        textures_delta.clear();
    }

    /// Первая стадия кадра: свежему содержимому загружает текстуры, для повторного проверяет,
    /// что удержанные примитивы есть и загружены для того же размера экрана.
    pub fn begin_frame_content(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        egui: EguiFrameContent,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) -> Result<PendingEguiContent, RetainedEguiFrameUnavailable> {
        match egui {
            EguiFrameContent::Fresh {
                paint_jobs,
                textures_delta,
            } => {
                self.upload_changed_textures(device, queue, &textures_delta);
                Ok(PendingEguiContent::Fresh {
                    paint_jobs,
                    textures_delta,
                })
            }
            EguiFrameContent::ReusePrevious => {
                self.retained_frame_availability(screen_descriptor)?;
                Ok(PendingEguiContent::Reused)
            }
        }
    }

    /// Вторая стадия: свежее содержимое уходит в буферы и удерживается, повторное — ничего не
    /// пишет. Возвращает callback command buffers и delta, которую надо освободить после submit-а.
    pub fn prepare_pending_content(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        pending: PendingEguiContent,
        screen_descriptor: egui_wgpu::ScreenDescriptor,
    ) -> (Vec<wgpu::CommandBuffer>, Option<egui::TexturesDelta>) {
        match pending {
            PendingEguiContent::Fresh {
                paint_jobs,
                textures_delta,
            } => (
                self.prepare_fresh_frame(device, queue, encoder, paint_jobs, screen_descriptor),
                Some(textures_delta),
            ),
            PendingEguiContent::Reused => (Vec::new(), None),
        }
    }

    /// Освобождает retired-текстуры свежего кадра; у повторного кадра delta нет.
    pub fn free_retired_textures_if_any(&mut self, textures_delta: Option<egui::TexturesDelta>) {
        if let Some(textures_delta) = textures_delta {
            self.free_retired_textures(textures_delta);
        }
    }

    /// Загружает свежие примитивы в vertex/index buffers и удерживает их для повторных кадров.
    ///
    /// `egui-wgpu` подготавливает пользовательские paint callbacks отдельно от
    /// основного encoder-а. Владелец frame submission обязан отправить эти buffers
    /// в ту же очередь перед command buffer-ом основного кадра.
    pub fn prepare_fresh_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
        screen_descriptor: egui_wgpu::ScreenDescriptor,
    ) -> Vec<wgpu::CommandBuffer> {
        let callback_command_buffers =
            self.renderer
                .update_buffers(device, queue, encoder, &paint_jobs, &screen_descriptor);
        // Буферы теперь соответствуют именно этим примитивам: прошлые удерживать нельзя.
        self.retained = Some(RetainedEguiFrame {
            paint_jobs,
            screen_descriptor,
        });
        callback_command_buffers
    }

    /// Можно ли нарисовать интерфейс повторно без новой загрузки буферов.
    pub fn retained_frame_availability(
        &self,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) -> Result<(), RetainedEguiFrameUnavailable> {
        let Some(retained) = self.retained.as_ref() else {
            return Err(RetainedEguiFrameUnavailable::NothingRetained);
        };
        if !screen_descriptors_match(&retained.screen_descriptor, screen_descriptor) {
            return Err(RetainedEguiFrameUnavailable::ScreenMismatch);
        }
        Ok(())
    }

    /// Рендерит удержанные egui primitives поверх уже нарисованного video target-а.
    ///
    /// Вызывается после [`Self::begin_frame_content`] того же кадра: свежий кадр уже загружен
    /// и удержан, повторный — проверен. Удержанного кадра не может не быть; если это всё же
    /// случилось (ошибка программы, не окружения), pass не открывается, а в лог идёт `error!`.
    pub fn render_retained_overlay(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) {
        let Some(retained) = self.retained.as_ref() else {
            tracing::error!("Retained egui frame vanished before the overlay pass; UI not drawn");
            return;
        };
        let egui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui overlay pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        self.renderer.render(
            &mut egui_pass.forget_lifetime(),
            &retained.paint_jobs,
            &retained.screen_descriptor,
        );
    }
}

/// Точное совпадение размера и scale: egui-wgpu пишет их в uniform только при изменении,
/// а повторный кадр `update_buffers` не вызывает.
fn screen_descriptors_match(
    retained: &egui_wgpu::ScreenDescriptor,
    requested: &egui_wgpu::ScreenDescriptor,
) -> bool {
    retained.size_in_pixels == requested.size_in_pixels
        && retained.pixels_per_point.to_bits() == requested.pixels_per_point.to_bits()
}

#[cfg(test)]
mod tests;
