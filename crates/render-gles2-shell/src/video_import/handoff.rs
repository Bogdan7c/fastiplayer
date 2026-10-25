//! Перенос импорта при пересоздании рендера и владение привязкой освобождения кадров.
//!
//! # Задача
//!
//! Привязка освобождения ([`Gles2FrameReleaseBinding`]) разделена между рендером (публикует
//! fence показанных кадров) и обёрткой провайдера декодера (просит вернуть кадр). При
//! пересоздании рендера внутри семейства новый рендер ОБЯЗАН продолжить с той же привязкой:
//! обёртка, которую уже держит плеер, ссылается именно на неё. Старый рендер при уничтожении
//! обычно отвязывает привязку (`renderer_detaching`) — но у переданной привязки теперь другой
//! владелец, и отвязка сломала бы его (режим «рендер отвязан», забытые fence).
//!
//! Поэтому владение выражено явно: [`OwnedReleaseBinding`] знает, передана ли привязка
//! преемнику ([`HandoffMark`]), и отвязывает её только если нет.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use render_shell_api::VideoImportPlan;

use super::prepared::FrameSource;
use crate::frame_release::{Gles2FrameReleaseBinding, RendererDetachGrace, RendererDetachReport};

/// Метка «привязка передана преемнику»: общая у старого рендера и у переносимого импорта.
#[derive(Debug, Clone, Default)]
pub(crate) struct HandoffMark(Arc<AtomicBool>);

impl HandoffMark {
    /// Преемник принял привязку.
    fn mark_handed_off(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Привязку принял преемник.
    fn is_handed_off(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Привязка освобождения кадров, которой владеет рендер.
pub(crate) struct OwnedReleaseBinding {
    binding: Gles2FrameReleaseBinding,
    handed_off: HandoffMark,
}

impl OwnedReleaseBinding {
    /// Новая привязка нового рендера (без потока-ожидателя: он стартует по требованию).
    pub(crate) fn new() -> Self {
        Self {
            binding: Gles2FrameReleaseBinding::new(),
            handed_off: HandoffMark::default(),
        }
    }

    /// Привязка (публикация fence, потеря контекста, статистика, обёртки декодеров).
    pub(crate) const fn binding(&self) -> &Gles2FrameReleaseBinding {
        &self.binding
    }

    /// Части привязки для переноса преемнику (старый рендер остаётся владельцем, пока
    /// преемник не примет её через [`Self::adopt`]).
    pub(crate) fn parts_for_successor(&self) -> RecreatedImportParts {
        RecreatedImportParts {
            binding: self.binding.clone(),
            predecessor_mark: self.handed_off.clone(),
        }
    }

    /// Принимает привязку предшественника: обёртки декодера, которые держит плеер, продолжают
    /// работать с этим рендером. Старый fence принадлежал старому контексту — он забывается.
    pub(crate) fn adopt(&mut self, parts: RecreatedImportParts) {
        let RecreatedImportParts {
            binding,
            predecessor_mark,
        } = parts;
        binding.resume_after_context_recreated();
        // С этого момента предшественник привязку не отвязывает (см. документацию модуля).
        predecessor_mark.mark_handed_off();
        self.binding = binding;
        self.handed_off = HandoffMark::default();
    }

    /// Штатная отвязка перед уничтожением контекста (`None` — привязку принял преемник).
    pub(crate) fn detach_for_shutdown(&self) -> Option<RendererDetachReport> {
        if self.handed_off.is_handed_off() {
            return None;
        }
        Some(
            self.binding
                .renderer_detaching(RendererDetachGrace::DEFAULT),
        )
    }
}

/// Привязка освобождения и метка её передачи: едут вместе с переносимым импортом.
pub(crate) struct RecreatedImportParts {
    binding: Gles2FrameReleaseBinding,
    predecessor_mark: HandoffMark,
}

/// Содержимое `VideoImportHandoff` этого семейства: активный импорт старого рендера.
pub(crate) struct Gles2ImportHandoff {
    /// План импорта (путь и размер пула декодера).
    pub(crate) plan: VideoImportPlan,
    /// Источник ресурсов кадров (обёрнутый провайдер того же декодера).
    pub(crate) source: FrameSource,
    /// Привязка освобождения старого рендера.
    pub(crate) parts: RecreatedImportParts,
}

#[cfg(test)]
mod tests;
