//! Импорт NV12 из DMA-BUF в две GL-текстуры и кэш импортов по паспорту буфера.
//!
//! Владелец состояния «импортированный кадр»: кэш (паспорт → пара текстур) и текущий
//! импортированный кадр. [`crate::VideoLayerRenderer`] только вызывает методы этого модуля и
//! решает, какой источник текущий (кольцо загрузок или импорт).
//!
//! # Как это работает
//! Каждая плоскость — свой `EGLImage` (`R8` для яркости, `GR88` для цветности), привязанный к
//! обычной текстуре `GL_TEXTURE_2D` через `glEGLImageTargetTexture2DOES`
//! (`GL_OES_EGL_image`); `EGLImage` живёт только внутри вызова привязки (его делает
//! `gles2-egui`), дальше буфер держит сама текстура. Контракт с производителем кадров — в
//! [`crate::DmaBufSharingContract`].
//!
//! # Удаление текстур вытесненных записей
//! Текстуры убираются из кэша и удаляются сразу, там же, где есть контекст. Это безопасно, даже
//! если команды, читающие текстуру, ещё не выполнены: команды GL выполняются так, будто по
//! порядку, а `glDeleteTextures` не обрывает уже поставленные в очередь рисования — имя
//! становится недействительным сразу, объект живёт, пока на него есть ссылки (спецификация
//! OpenGL ES 2.0.25, приложение C.1 «Object Deletion Behavior», разделы C.1.2 «Automatic
//! Unbinding of Deleted Objects» и C.1.3 «Deleted Object and Object Name Lifetimes»). Память
//! буфера при этом держит ядро (ссылка dma-buf), пока её читает GPU.

use gles2_egui::{DmaBufPlaneTexture, DrmModifier, Gles2Context};

use crate::dma_buf_api::{
    CachedDmaBufFrame, DmaBufImportOutcome, DmaBufImportReport, VideoImportError,
};
use crate::dma_buf_cache::{
    DmaBufImportCache, DmaBufImportCacheCapacity, DmaBufImportCacheSnapshot,
};
use crate::dma_buf_frame::{DmaBufBufferIdentity, DmaBufNv12Frame, DmaBufPlaneRole};
use crate::gl_calls::VideoGl;
use crate::planes::{DmaBufPlane, PlaneExtent, VideoFrameKey};

/// Две текстуры импортированного кадра NV12.
#[derive(Debug)]
pub(crate) struct ImportedNv12 {
    pub(crate) luma: glow::NativeTexture,
    pub(crate) chroma: glow::NativeTexture,
}

impl ImportedNv12 {
    fn delete(self, gl: VideoGl<'_>) {
        gl.delete_texture(self.luma);
        gl.delete_texture(self.chroma);
    }
}

/// Текущий импортированный кадр: ключ вызывающего и паспорт, по которому лежат его текстуры.
#[derive(Debug)]
struct CurrentImport {
    key: VideoFrameKey,
    buffer: DmaBufBufferIdentity,
}

/// Кэш импортов и текущий импортированный кадр.
#[derive(Debug)]
pub(crate) struct DmaBufImports {
    cache: DmaBufImportCache<ImportedNv12>,
    current: Option<CurrentImport>,
}

impl DmaBufImports {
    pub(crate) fn new(capacity: DmaBufImportCacheCapacity) -> Self {
        Self {
            cache: DmaBufImportCache::new(capacity),
            current: None,
        }
    }

    /// Ключ текущего импортированного кадра.
    pub(crate) fn current_key(&self) -> Option<VideoFrameKey> {
        self.current.as_ref().map(|current| current.key)
    }

    /// Текстуры текущего импортированного кадра (для рисования; кэш не освежается).
    pub(crate) fn current_textures(&self) -> Option<&ImportedNv12> {
        let current = self.current.as_ref()?;
        self.cache.peek(&current.buffer)
    }

    /// Импортированный кадр больше не текущий (текущим стал загруженный); кэш остаётся.
    pub(crate) fn clear_current(&mut self) {
        self.current = None;
    }

    pub(crate) fn snapshot(&self) -> DmaBufImportCacheSnapshot {
        self.cache.snapshot()
    }

    /// Показать кадр по паспорту без GL и без дескрипторов. `key` уже не равен ключу текущего
    /// кадра (это проверяет слой выше).
    pub(crate) fn show_cached(
        &mut self,
        buffer: &DmaBufBufferIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame {
        if self.cache.lookup(buffer).is_none() {
            return CachedDmaBufFrame::NotCached;
        }
        self.current = Some(CurrentImport {
            key,
            buffer: buffer.clone(),
        });
        CachedDmaBufFrame::Shown
    }

    /// Импортировать кадр (или взять уже лежащий в кэше импорт), сделать его текущим.
    ///
    /// Порядок и причины:
    /// 1. всё, что может отказать без изменения состояния (размер текстуры), — до кэша;
    /// 2. место под новую запись освобождается ДО импорта (смена формата уничтожает все старые
    ///    записи), чтобы память двух наборов буферов не держалась одновременно;
    /// 3. отказ драйвера: текстуры удалены, в кэш ничего не попало, счётчик отказов вырос.
    pub(crate) fn import(
        &mut self,
        context: &Gles2Context,
        gl: VideoGl<'_>,
        frame: &DmaBufNv12Frame<'_>,
        max_texture_size: u32,
    ) -> Result<DmaBufImportOutcome, VideoImportError> {
        let buffer = frame.buffer();
        if self.cache.contains(buffer) {
            self.cache.touch(buffer);
            self.make_current(frame);
            return Ok(DmaBufImportOutcome::ReusedCachedImport);
        }
        check_texture_size(DmaBufPlaneRole::Luma, frame.luma_extent(), max_texture_size)?;
        check_texture_size(
            DmaBufPlaneRole::Chroma,
            frame.chroma_extent(),
            max_texture_size,
        )?;
        let evicted = self.cache.prepare_for_insert(buffer);
        self.delete_evicted(gl, evicted);
        match import_textures(context, gl, frame) {
            Ok((textures, report)) => {
                let evicted = self.cache.insert(buffer.clone(), textures);
                self.delete_evicted(gl, evicted);
                self.make_current(frame);
                Ok(DmaBufImportOutcome::Imported(report))
            }
            Err(error) => {
                self.cache.record_import_failure();
                Err(error)
            }
        }
    }

    /// Новая ёмкость кэша; лишние записи удаляются.
    pub(crate) fn set_capacity(&mut self, gl: VideoGl<'_>, capacity: DmaBufImportCacheCapacity) {
        let evicted = self.cache.set_capacity(capacity);
        self.delete_evicted(gl, evicted);
    }

    /// Удалить все импорты и забыть текущий кадр (смена декодера, удаление слоя): текстуры
    /// держат память поверхностей, которых у нового декодера уже нет.
    pub(crate) fn purge(&mut self, gl: VideoGl<'_>) {
        self.current = None;
        for textures in self.cache.drain_all() {
            textures.delete(gl);
        }
    }

    fn make_current(&mut self, frame: &DmaBufNv12Frame<'_>) {
        self.current = Some(CurrentImport {
            key: frame.key(),
            buffer: frame.buffer().clone(),
        });
    }

    /// Удалить вытесненные текстуры; текущий кадр, чьей записи больше нет, перестаёт быть
    /// текущим (рисовать нечем).
    fn delete_evicted(&mut self, gl: VideoGl<'_>, evicted: Vec<ImportedNv12>) {
        for textures in evicted {
            textures.delete(gl);
        }
        let current_lost_texture = self
            .current
            .as_ref()
            .is_some_and(|current| !self.cache.contains(&current.buffer));
        if current_lost_texture {
            self.current = None;
        }
    }
}

fn check_texture_size(
    plane: DmaBufPlaneRole,
    extent: PlaneExtent,
    max: u32,
) -> Result<(), VideoImportError> {
    if extent.width > max || extent.height > max {
        return Err(VideoImportError::PlaneTooLarge {
            plane,
            width: extent.width,
            height: extent.height,
            max,
        });
    }
    Ok(())
}

/// Создать две текстуры, привязать к ним плоскости и выставить выборку. Любой отказ удаляет
/// созданные текстуры.
fn import_textures(
    context: &Gles2Context,
    gl: VideoGl<'_>,
    frame: &DmaBufNv12Frame<'_>,
) -> Result<(ImportedNv12, DmaBufImportReport), VideoImportError> {
    let luma = gl.create_texture().map_err(VideoImportError::Gl)?;
    let chroma = gl
        .create_texture()
        .map_err(VideoImportError::Gl)
        .inspect_err(|_| gl.delete_texture(luma))?;
    let textures = ImportedNv12 { luma, chroma };
    let attached = attach_plane(
        context,
        DmaBufPlaneRole::Luma,
        textures.luma,
        frame.luma(),
        frame.luma_offset(),
        frame.luma_extent(),
    )
    .and_then(|luma_report| {
        attach_plane(
            context,
            DmaBufPlaneRole::Chroma,
            textures.chroma,
            frame.chroma(),
            frame.chroma_offset(),
            frame.chroma_extent(),
        )
        .map(|chroma_report| DmaBufImportReport {
            luma: luma_report,
            chroma: chroma_report,
        })
    });
    match attached {
        Ok(report) => {
            gl.set_imported_texture_sampling(textures.luma);
            gl.set_imported_texture_sampling(textures.chroma);
            Ok((textures, report))
        }
        Err(error) => {
            textures.delete(gl);
            Err(error)
        }
    }
}

fn attach_plane(
    context: &Gles2Context,
    role: DmaBufPlaneRole,
    texture: glow::NativeTexture,
    plane: &DmaBufPlane<'_>,
    offset: u32,
    extent: PlaneExtent,
) -> Result<gles2_egui::DmaBufTextureAttached, VideoImportError> {
    let request = DmaBufPlaneTexture {
        fourcc: gles2_egui::DrmFourcc(plane.drm_format),
        width: extent.width,
        height: extent.height,
        fd: plane.fd,
        offset,
        pitch: plane.pitch,
        modifier: DrmModifier(plane.modifier),
    };
    context
        .attach_dma_buf_plane_to_texture(texture, &request)
        .map_err(|source| VideoImportError::Attach {
            plane: role,
            source,
        })
}
