//! Импорт программного планарного кадра (8/10/12 бит) из DMA-BUF в три GL-текстуры и кэш импортов.
//!
//! Владелец состояния «импортированный планарный кадр»: кэш (паспорт → три текстуры) и текущий
//! кадр. Устроен так же, как [`crate::dma_buf_import`] (NV12), но плоскостей три, все `R8`, и
//! текстуры хранятся тем же [`PlaneTextureSet`], что и у кольца загрузок: поэтому кадр рисуется
//! ТОЙ ЖЕ планарной программой и теми же uniform-ами, что кадр после `upload`, а пиксели
//! совпадают с копирующим путём.
//!
//! # Как это работает
//! Каждая плоскость — свой `EGLImage` (`DRM_FORMAT_R8` + `DRM_FORMAT_MOD_LINEAR`, смещение и
//! шаг строк из паспорта), привязанный к `GL_TEXTURE_2D` через `glEGLImageTargetTexture2DOES`
//! (`GL_OES_EGL_image`). Текстура ровно размера видимой плоскости: хвост строки (шаг больше
//! ширины) драйвер пропускает по pitch, шейдеру обрезка не нужна. Контракт с производителем —
//! [`crate::DmaBufSharingContract`]; удаление текстур вытесненных записей безопасно по тем же
//! причинам, что у NV12 (см. документацию [`crate::dma_buf_import`]).

use gles2_egui::{DmaBufPlaneTexture, DrmFourcc, DrmModifier, Gles2Context};

use crate::dma_buf_api::{
    CachedDmaBufFrame, PlanarDmaBufImportOutcome, PlanarDmaBufImportReport, VideoImportError,
};
use crate::dma_buf_cache::{
    DmaBufImportCache, DmaBufImportCacheCapacity, DmaBufImportCacheSnapshot,
};
use crate::dma_buf_planar_frame::{DmaBufPlanarFrame, DmaBufPlanarIdentity, PLANAR_ROLES};
use crate::gl_calls::VideoGl;
use crate::planes::{PlaneRole, VideoFrameKey};
use crate::textures::PlaneTextureSet;

/// Текущий импортированный кадр: ключ вызывающего и паспорт, по которому лежат его текстуры.
#[derive(Debug)]
struct CurrentPlanarImport {
    key: VideoFrameKey,
    identity: DmaBufPlanarIdentity,
}

/// Кэш планарных импортов и текущий импортированный кадр.
#[derive(Debug)]
pub(crate) struct DmaBufPlanarImports {
    cache: DmaBufImportCache<PlaneTextureSet, DmaBufPlanarIdentity>,
    current: Option<CurrentPlanarImport>,
}

impl DmaBufPlanarImports {
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
    pub(crate) fn current_textures(&self) -> Option<&PlaneTextureSet> {
        let current = self.current.as_ref()?;
        self.cache.peek(&current.identity)
    }

    /// Импортированный кадр больше не текущий (текущим стал другой источник); кэш остаётся.
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
        identity: &DmaBufPlanarIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame {
        if self.cache.lookup(identity).is_none() {
            return CachedDmaBufFrame::NotCached;
        }
        self.current = Some(CurrentPlanarImport {
            key,
            identity: identity.clone(),
        });
        CachedDmaBufFrame::Shown
    }

    /// Импортировать кадр (или взять уже лежащий в кэше импорт), сделать его текущим.
    ///
    /// Порядок и причины те же, что у NV12: проверки без побочных эффектов (размер текстуры) —
    /// до кэша; место под запись освобождается ДО импорта; отказ драйвера удаляет созданные
    /// текстуры, в кэш ничего не попадает, счётчик отказов растёт.
    pub(crate) fn import(
        &mut self,
        context: &Gles2Context,
        gl: VideoGl<'_>,
        frame: &DmaBufPlanarFrame<'_>,
        max_texture_size: u32,
    ) -> Result<PlanarDmaBufImportOutcome, VideoImportError> {
        let identity = frame.identity();
        if self.cache.contains(identity) {
            self.cache.touch(identity);
            self.make_current(frame);
            return Ok(PlanarDmaBufImportOutcome::ReusedCachedImport);
        }
        for role in PLANAR_ROLES {
            check_texture_size(role, identity, max_texture_size)?;
        }
        let evicted = self.cache.prepare_for_insert(identity);
        self.delete_evicted(gl, evicted);
        match import_textures(context, gl, frame) {
            Ok((textures, report)) => {
                let evicted = self.cache.insert(identity.clone(), textures);
                self.delete_evicted(gl, evicted);
                self.make_current(frame);
                Ok(PlanarDmaBufImportOutcome::Imported(report))
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
    /// держат память блоков, которых у нового декодера уже нет.
    pub(crate) fn purge(&mut self, gl: VideoGl<'_>) {
        self.current = None;
        for textures in self.cache.drain_all() {
            textures.delete(gl);
        }
    }

    fn make_current(&mut self, frame: &DmaBufPlanarFrame<'_>) {
        self.current = Some(CurrentPlanarImport {
            key: frame.key(),
            identity: frame.identity().clone(),
        });
    }

    /// Удалить вытесненные текстуры; текущий кадр, чьей записи больше нет, перестаёт быть
    /// текущим (рисовать нечем).
    fn delete_evicted(&mut self, gl: VideoGl<'_>, evicted: Vec<PlaneTextureSet>) {
        for textures in evicted {
            textures.delete(gl);
        }
        let current_lost_texture = self
            .current
            .as_ref()
            .is_some_and(|current| !self.cache.contains(&current.identity));
        if current_lost_texture {
            self.current = None;
        }
    }
}

fn check_texture_size(
    role: PlaneRole,
    identity: &DmaBufPlanarIdentity,
    max: u32,
) -> Result<(), VideoImportError> {
    let extent = identity.plane_extent(role);
    if extent.width > max || extent.height > max {
        return Err(VideoImportError::PlanarPlaneTooLarge {
            role,
            width: extent.width,
            height: extent.height,
            max,
        });
    }
    Ok(())
}

/// Создать три текстуры, привязать к ним плоскости и выставить выборку. Любой отказ удаляет
/// созданные текстуры.
fn import_textures(
    context: &Gles2Context,
    gl: VideoGl<'_>,
    frame: &DmaBufPlanarFrame<'_>,
) -> Result<(PlaneTextureSet, PlanarDmaBufImportReport), VideoImportError> {
    let identity = frame.identity();
    // Отказ на второй или третьей плоскости удаляет уже созданные текстуры: объекты не остаются
    // в контексте без владельца.
    let (luma, luma_report) = create_and_attach(context, gl, frame, PlaneRole::Luma)?;
    let (chroma_u, chroma_u_report) = create_and_attach(context, gl, frame, PlaneRole::ChromaU)
        .inspect_err(|_| gl.delete_texture(luma))?;
    let (chroma_v, chroma_v_report) = create_and_attach(context, gl, frame, PlaneRole::ChromaV)
        .inspect_err(|_| {
            gl.delete_texture(luma);
            gl.delete_texture(chroma_u);
        })?;
    let textures = [luma, chroma_u, chroma_v];
    for texture in textures {
        gl.set_imported_texture_sampling(texture);
    }
    let extents = PLANAR_ROLES.map(|role| identity.plane_extent(role));
    Ok((
        PlaneTextureSet::from_imported_planes(textures, identity.layout, extents),
        PlanarDmaBufImportReport {
            luma: luma_report,
            chroma_u: chroma_u_report,
            chroma_v: chroma_v_report,
        },
    ))
}

/// Одна плоскость: текстура и привязка к ней `EGLImage`. Отказ привязки удаляет текстуру.
fn create_and_attach(
    context: &Gles2Context,
    gl: VideoGl<'_>,
    frame: &DmaBufPlanarFrame<'_>,
    role: PlaneRole,
) -> Result<(glow::NativeTexture, gles2_egui::DmaBufTextureAttached), VideoImportError> {
    let identity = frame.identity();
    let extent = identity.plane_extent(role);
    let location = identity.plane_location(role);
    let texture = gl.create_texture().map_err(VideoImportError::Gl)?;
    // 8 бит — байт на отсчёт (`R8`); 10/12 бит — пара байтов LE (`GR88`: младший в R, старший
    // в G), её собирает программа с `BYTE_PAIRS_IN_RED_GREEN`.
    let fourcc = if identity.layout.depth.is_high_bit() {
        DrmFourcc::GR88
    } else {
        DrmFourcc::R8
    };
    let request = DmaBufPlaneTexture {
        fourcc,
        width: extent.width,
        height: extent.height,
        fd: frame.dma_buf(),
        offset: location.offset,
        pitch: location.pitch,
        modifier: DrmModifier::LINEAR,
    };
    match context.attach_dma_buf_plane_to_texture(texture, &request) {
        Ok(report) => Ok((texture, report)),
        Err(source) => {
            gl.delete_texture(texture);
            Err(VideoImportError::PlanarAttach { role, source })
        }
    }
}
