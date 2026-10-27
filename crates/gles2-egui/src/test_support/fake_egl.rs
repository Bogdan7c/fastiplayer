//! Fake-драйвер EGL для тестов: журнал вызовов, учёт живых объектов, программируемые отказы.
//!
//! Повторное разрушение любого объекта или разрушение чужого/несуществующего — паника теста:
//! так проверяется инвариант «каждый `destroy_*` ровно один раз».

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::context::display_registry::DisplayRegistry;
use crate::context::dma_buf_texture::DmaBufImageAttributes;
use crate::context::driver::{
    ConfigDescription, ConfigId, ConfigQuery, ContextId, ContextRequest, CurrentBinding,
    DamageRect, DamageSwapExtension, DisplayId, DisplayString, DmaBufModifier, EglCallError,
    EglDriver, EglErrorCode, EglOperation, EglVersion, ImageId, NativeDisplay,
    PlatformWindowTarget, ResetNotificationRequest, SurfaceId, SurfaceSize, SyncId, SyncWaitStatus,
    WaylandEglWindowId,
};

/// Один вызов драйвера в журнале.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FakeCall {
    GetPlatformDisplay(NativeDisplay),
    Initialize(DisplayId),
    Terminate(DisplayId),
    CreateContext {
        context: ContextId,
        reset_notification: ResetNotificationRequest,
    },
    DestroyContext(ContextId),
    CreateWaylandEglWindow(WaylandEglWindowId),
    ResizeWaylandEglWindow(WaylandEglWindowId, SurfaceSize),
    DestroyWaylandEglWindow(WaylandEglWindowId),
    CreateWindowSurface(SurfaceId, PlatformWindowTarget),
    CreatePbufferSurface(SurfaceId),
    DestroySurface(SurfaceId),
    MakeCurrent(CurrentBinding),
    SwapBuffers(SurfaceId),
    QueryBufferAge(SurfaceId),
    SwapBuffersWithDamage(SurfaceId, DamageSwapExtension),
    SwapInterval(i32),
    CreateSync(SyncId),
    ClientWaitSync(SyncId),
    DestroySync(SyncId),
    CreateImage(ImageId),
    DestroyImage(ImageId),
}

/// Один показ с повреждениями (прямоугольники не влезают в `Copy`-журнал).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DamageSwapRecord {
    pub(crate) extension: DamageSwapExtension,
    pub(crate) rects: Vec<DamageRect>,
}

/// Программируемое поведение драйвера.
#[derive(Debug, Clone)]
pub(crate) struct FakeBehavior {
    pub(crate) client_extensions: String,
    pub(crate) display_extensions: String,
    pub(crate) display_version: EglVersion,
    pub(crate) configs: Vec<ConfigDescription>,
    pub(crate) robust_context_error: Option<EglErrorCode>,
    pub(crate) context_error: Option<EglErrorCode>,
    pub(crate) window_surface_error: Option<EglErrorCode>,
    pub(crate) make_current_error: Option<EglErrorCode>,
    pub(crate) swap_error: Option<EglErrorCode>,
    /// Возрасты буфера по порядку запросов; когда список кончился, отвечает `default_buffer_age`.
    pub(crate) buffer_age_script: std::collections::VecDeque<u32>,
    pub(crate) default_buffer_age: u32,
    pub(crate) buffer_age_error: Option<EglErrorCode>,
    pub(crate) damage_swap_error: Option<EglErrorCode>,
    pub(crate) wait_error: Option<EglErrorCode>,
    pub(crate) image_create_error: Option<EglErrorCode>,
    pub(crate) image_destroy_error: Option<EglErrorCode>,
    pub(crate) new_syncs_pending: bool,
    pub(crate) dma_buf_formats: Result<Vec<(u32, Vec<DmaBufModifier>)>, EglErrorCode>,
}

impl Default for FakeBehavior {
    fn default() -> Self {
        Self {
            client_extensions: "EGL_EXT_client_extensions EGL_KHR_platform_wayland \
                                EGL_KHR_platform_x11 EGL_MESA_platform_surfaceless"
                .to_owned(),
            display_extensions: "EGL_KHR_fence_sync EGL_KHR_surfaceless_context \
                                 EGL_EXT_create_context_robustness"
                .to_owned(),
            display_version: EglVersion { major: 1, minor: 5 },
            configs: vec![fake_config(1, 8, 0, 0x21), fake_config(2, 8, 8, 0x22)],
            robust_context_error: None,
            context_error: None,
            window_surface_error: None,
            make_current_error: None,
            swap_error: None,
            buffer_age_script: std::collections::VecDeque::new(),
            default_buffer_age: 0,
            buffer_age_error: None,
            damage_swap_error: None,
            wait_error: None,
            image_create_error: None,
            image_destroy_error: None,
            new_syncs_pending: false,
            dma_buf_formats: Ok(Vec::new()),
        }
    }
}

/// Конфиг fake-драйвера.
pub(crate) fn fake_config(
    id: usize,
    rgb_bits: i32,
    alpha_bits: i32,
    visual: i64,
) -> ConfigDescription {
    ConfigDescription {
        id: ConfigId(id),
        red_bits: rgb_bits,
        green_bits: rgb_bits,
        blue_bits: rgb_bits,
        alpha_bits,
        native_visual_id: visual,
        min_swap_interval: 0,
        max_swap_interval: 1,
    }
}

#[derive(Default)]
struct FakeState {
    behavior: FakeBehavior,
    journal: Vec<FakeCall>,
    next_handle: usize,
    displays_by_native: HashMap<NativeDisplay, DisplayId>,
    initialized_displays: BTreeSet<usize>,
    live_contexts: BTreeSet<usize>,
    live_surfaces: BTreeSet<usize>,
    live_wayland_windows: BTreeSet<usize>,
    live_syncs: BTreeSet<usize>,
    live_images: BTreeSet<usize>,
    /// Списки атрибутов всех удачных и неудачных `eglCreateImage` по порядку вызовов.
    image_attribute_lists: Vec<Vec<usize>>,
    pending_syncs: BTreeSet<usize>,
    /// Параметры всех показов с повреждениями по порядку вызовов.
    damage_swaps: Vec<DamageSwapRecord>,
}

impl FakeState {
    fn next(&mut self) -> usize {
        // Дескрипторы начинаются с 0x1000, чтобы не совпадать с номерами конфигов.
        self.next_handle += 1;
        0x1000 + self.next_handle
    }
}

/// Fake-драйвер.
pub(crate) struct FakeEgl {
    state: Mutex<FakeState>,
}

impl FakeEgl {
    /// Драйвер с поведением и реестр поверх него.
    pub(crate) fn with_registry(behavior: FakeBehavior) -> (Arc<Self>, Arc<DisplayRegistry>) {
        let fake = Arc::new(Self {
            state: Mutex::new(FakeState {
                behavior,
                ..FakeState::default()
            }),
        });
        let driver: Arc<dyn EglDriver> = fake.clone();
        (fake, DisplayRegistry::new(driver))
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Журнал вызовов.
    pub(crate) fn journal(&self) -> Vec<FakeCall> {
        self.lock().journal.clone()
    }

    /// Очистить журнал (оставить живые объекты).
    pub(crate) fn clear_journal(&self) {
        self.lock().journal.clear();
    }

    /// Поменять поведение на ходу.
    pub(crate) fn update_behavior(&self, update: impl FnOnce(&mut FakeBehavior)) {
        update(&mut self.lock().behavior);
    }

    /// Все ожидающие sync становятся сигнальными.
    pub(crate) fn signal_all_syncs(&self) {
        self.lock().pending_syncs.clear();
    }

    /// Ничего не утекло: все объекты разрушены, display завершены.
    pub(crate) fn assert_nothing_alive(&self) {
        let state = self.lock();
        assert!(state.initialized_displays.is_empty(), "displays alive");
        assert!(state.live_contexts.is_empty(), "contexts alive");
        assert!(state.live_surfaces.is_empty(), "surfaces alive");
        assert!(
            state.live_wayland_windows.is_empty(),
            "wl_egl_windows alive"
        );
        assert!(state.live_syncs.is_empty(), "syncs alive");
        assert!(state.live_images.is_empty(), "EGLImages alive");
    }

    /// Списки атрибутов, с которыми вызывали `eglCreateImage`.
    pub(crate) fn image_attribute_lists(&self) -> Vec<Vec<usize>> {
        self.lock().image_attribute_lists.clone()
    }

    /// Число живых `EGLImage`.
    pub(crate) fn live_image_count(&self) -> usize {
        self.lock().live_images.len()
    }

    /// Число вызовов, подходящих под условие.
    /// Показы с повреждениями по порядку.
    pub(crate) fn damage_swaps(&self) -> Vec<DamageSwapRecord> {
        self.lock().damage_swaps.clone()
    }

    pub(crate) fn count(&self, predicate: impl Fn(&FakeCall) -> bool) -> usize {
        self.lock()
            .journal
            .iter()
            .filter(|call| predicate(call))
            .count()
    }
}

fn failure(operation: EglOperation, code: Option<EglErrorCode>) -> Result<(), EglCallError> {
    match code {
        Some(code) => Err(EglCallError::new(operation, code)),
        None => Ok(()),
    }
}

fn remove_live(set: &mut BTreeSet<usize>, handle: usize, what: &str) {
    assert!(
        set.remove(&handle),
        "{what} {handle:#x} destroyed twice or never created"
    );
}

impl EglDriver for FakeEgl {
    fn client_extensions(&self) -> String {
        self.lock().behavior.client_extensions.clone()
    }

    fn get_platform_display(&self, native: NativeDisplay) -> Result<DisplayId, EglCallError> {
        let mut state = self.lock();
        state.journal.push(FakeCall::GetPlatformDisplay(native));
        // Как Mesa: один нативный display — один EGLDisplay на весь процесс.
        if let Some(display) = state.displays_by_native.get(&native).copied() {
            return Ok(display);
        }
        let display = DisplayId(state.next());
        state.displays_by_native.insert(native, display);
        Ok(display)
    }

    fn initialize(&self, display: DisplayId) -> Result<EglVersion, EglCallError> {
        let mut state = self.lock();
        state.journal.push(FakeCall::Initialize(display));
        state.initialized_displays.insert(display.0);
        Ok(state.behavior.display_version)
    }

    fn terminate(&self, display: DisplayId) -> Result<(), EglCallError> {
        let mut state = self.lock();
        state.journal.push(FakeCall::Terminate(display));
        remove_live(&mut state.initialized_displays, display.0, "display");
        // EGL 1.5, 3.2: eglTerminate разрушает всё, что не текущее. Если что-то живо, это
        // ошибка порядка в нашем коде.
        assert!(
            state.live_contexts.is_empty() && state.live_surfaces.is_empty(),
            "eglTerminate while contexts/surfaces are alive"
        );
        Ok(())
    }

    fn query_display_string(
        &self,
        display: DisplayId,
        which: DisplayString,
    ) -> Result<String, EglCallError> {
        let state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        Ok(match which {
            DisplayString::Vendor => "Fake EGL".to_owned(),
            DisplayString::Extensions => state.behavior.display_extensions.clone(),
        })
    }

    fn bind_gles_api(&self) -> Result<(), EglCallError> {
        Ok(())
    }

    fn matching_configs(
        &self,
        display: DisplayId,
        _query: ConfigQuery,
    ) -> Result<Vec<ConfigDescription>, EglCallError> {
        let state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        Ok(state.behavior.configs.clone())
    }

    fn create_context(
        &self,
        display: DisplayId,
        _config: ConfigId,
        request: ContextRequest,
    ) -> Result<ContextId, EglCallError> {
        let mut state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        let error = match request.reset_notification {
            ResetNotificationRequest::LoseContextOnReset => state.behavior.robust_context_error,
            ResetNotificationRequest::NotRequested => state.behavior.context_error,
        };
        failure(EglOperation::CreateContext, error)?;
        let context = ContextId(state.next());
        state.live_contexts.insert(context.0);
        state.journal.push(FakeCall::CreateContext {
            context,
            reset_notification: request.reset_notification,
        });
        Ok(context)
    }

    fn destroy_context(&self, _display: DisplayId, context: ContextId) -> Result<(), EglCallError> {
        let mut state = self.lock();
        state.journal.push(FakeCall::DestroyContext(context));
        remove_live(&mut state.live_contexts, context.0, "context");
        Ok(())
    }

    fn create_wayland_egl_window(
        &self,
        _wl_surface_address: usize,
        _size: SurfaceSize,
    ) -> Result<WaylandEglWindowId, EglCallError> {
        let mut state = self.lock();
        let window = WaylandEglWindowId(state.next());
        state.live_wayland_windows.insert(window.0);
        state.journal.push(FakeCall::CreateWaylandEglWindow(window));
        Ok(window)
    }

    fn resize_wayland_egl_window(&self, window: WaylandEglWindowId, size: SurfaceSize) {
        let mut state = self.lock();
        assert!(state.live_wayland_windows.contains(&window.0));
        state
            .journal
            .push(FakeCall::ResizeWaylandEglWindow(window, size));
    }

    fn destroy_wayland_egl_window(&self, window: WaylandEglWindowId) {
        let mut state = self.lock();
        state
            .journal
            .push(FakeCall::DestroyWaylandEglWindow(window));
        remove_live(&mut state.live_wayland_windows, window.0, "wl_egl_window");
    }

    fn create_window_surface(
        &self,
        display: DisplayId,
        _config: ConfigId,
        target: PlatformWindowTarget,
    ) -> Result<SurfaceId, EglCallError> {
        let mut state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        failure(
            EglOperation::CreateWindowSurface,
            state.behavior.window_surface_error,
        )?;
        if let PlatformWindowTarget::Wayland(window) = target {
            assert!(state.live_wayland_windows.contains(&window.0));
        }
        let surface = SurfaceId(state.next());
        state.live_surfaces.insert(surface.0);
        state
            .journal
            .push(FakeCall::CreateWindowSurface(surface, target));
        Ok(surface)
    }

    fn create_pbuffer_surface(
        &self,
        display: DisplayId,
        _config: ConfigId,
        _size: SurfaceSize,
    ) -> Result<SurfaceId, EglCallError> {
        let mut state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        let surface = SurfaceId(state.next());
        state.live_surfaces.insert(surface.0);
        state.journal.push(FakeCall::CreatePbufferSurface(surface));
        Ok(surface)
    }

    fn destroy_surface(&self, _display: DisplayId, surface: SurfaceId) -> Result<(), EglCallError> {
        let mut state = self.lock();
        state.journal.push(FakeCall::DestroySurface(surface));
        remove_live(&mut state.live_surfaces, surface.0, "surface");
        Ok(())
    }

    fn make_current(
        &self,
        _display: DisplayId,
        binding: CurrentBinding,
    ) -> Result<(), EglCallError> {
        let mut state = self.lock();
        state.journal.push(FakeCall::MakeCurrent(binding));
        if binding == CurrentBinding::Nothing {
            return Ok(());
        }
        failure(EglOperation::MakeCurrent, state.behavior.make_current_error)
    }

    fn swap_buffers(&self, _display: DisplayId, surface: SurfaceId) -> Result<(), EglCallError> {
        let mut state = self.lock();
        assert!(state.live_surfaces.contains(&surface.0));
        state.journal.push(FakeCall::SwapBuffers(surface));
        failure(EglOperation::SwapBuffers, state.behavior.swap_error)
    }

    fn query_buffer_age(
        &self,
        _display: DisplayId,
        surface: SurfaceId,
    ) -> Result<u32, EglCallError> {
        let mut state = self.lock();
        assert!(state.live_surfaces.contains(&surface.0));
        state.journal.push(FakeCall::QueryBufferAge(surface));
        failure(
            EglOperation::QueryBufferAge,
            state.behavior.buffer_age_error,
        )?;
        let scripted = state.behavior.buffer_age_script.pop_front();
        Ok(scripted.unwrap_or(state.behavior.default_buffer_age))
    }

    fn swap_buffers_with_damage(
        &self,
        _display: DisplayId,
        surface: SurfaceId,
        extension: DamageSwapExtension,
        rects: &[DamageRect],
    ) -> Result<(), EglCallError> {
        let mut state = self.lock();
        assert!(state.live_surfaces.contains(&surface.0));
        state
            .journal
            .push(FakeCall::SwapBuffersWithDamage(surface, extension));
        state.damage_swaps.push(DamageSwapRecord {
            extension,
            rects: rects.to_vec(),
        });
        failure(
            EglOperation::SwapBuffersWithDamage,
            state.behavior.damage_swap_error,
        )
    }

    fn swap_interval(&self, _display: DisplayId, interval: i32) -> Result<(), EglCallError> {
        self.lock().journal.push(FakeCall::SwapInterval(interval));
        Ok(())
    }

    fn create_fence_sync(&self, display: DisplayId) -> Result<SyncId, EglCallError> {
        let mut state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        let sync = SyncId(state.next());
        state.live_syncs.insert(sync.0);
        if state.behavior.new_syncs_pending {
            state.pending_syncs.insert(sync.0);
        }
        state.journal.push(FakeCall::CreateSync(sync));
        Ok(sync)
    }

    fn client_wait_sync(
        &self,
        display: DisplayId,
        sync: SyncId,
        _timeout: Duration,
    ) -> Result<SyncWaitStatus, EglCallError> {
        let mut state = self.lock();
        assert!(
            state.initialized_displays.contains(&display.0),
            "wait on a terminated display"
        );
        assert!(
            state.live_syncs.contains(&sync.0),
            "wait on a destroyed sync"
        );
        state.journal.push(FakeCall::ClientWaitSync(sync));
        failure(EglOperation::ClientWaitSync, state.behavior.wait_error)?;
        Ok(if state.pending_syncs.contains(&sync.0) {
            SyncWaitStatus::TimedOut
        } else {
            SyncWaitStatus::Signaled
        })
    }

    fn destroy_sync(&self, display: DisplayId, sync: SyncId) -> Result<(), EglCallError> {
        let mut state = self.lock();
        assert!(
            state.initialized_displays.contains(&display.0),
            "sync outlived its display"
        );
        state.journal.push(FakeCall::DestroySync(sync));
        remove_live(&mut state.live_syncs, sync.0, "sync");
        state.pending_syncs.remove(&sync.0);
        Ok(())
    }

    fn query_dma_buf_formats(&self, _display: DisplayId) -> Result<Vec<u32>, EglCallError> {
        let state = self.lock();
        match &state.behavior.dma_buf_formats {
            Ok(formats) => Ok(formats.iter().map(|(fourcc, _)| *fourcc).collect()),
            Err(code) => Err(EglCallError::new(EglOperation::QueryDmaBuf, *code)),
        }
    }

    fn query_dma_buf_modifiers(
        &self,
        _display: DisplayId,
        fourcc: u32,
    ) -> Result<Vec<DmaBufModifier>, EglCallError> {
        let state = self.lock();
        match &state.behavior.dma_buf_formats {
            Ok(formats) => Ok(formats
                .iter()
                .find(|(candidate, _)| *candidate == fourcc)
                .map(|(_, modifiers)| modifiers.clone())
                .unwrap_or_default()),
            Err(code) => Err(EglCallError::new(EglOperation::QueryDmaBuf, *code)),
        }
    }

    fn create_dma_buf_image(
        &self,
        display: DisplayId,
        attributes: &DmaBufImageAttributes,
    ) -> Result<ImageId, EglCallError> {
        let mut state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        state
            .image_attribute_lists
            .push(attributes.as_slice().to_vec());
        failure(EglOperation::CreateImage, state.behavior.image_create_error)?;
        let image = ImageId(state.next());
        state.live_images.insert(image.0);
        state.journal.push(FakeCall::CreateImage(image));
        Ok(image)
    }

    fn destroy_image(&self, display: DisplayId, image: ImageId) -> Result<(), EglCallError> {
        let mut state = self.lock();
        assert!(state.initialized_displays.contains(&display.0));
        state.journal.push(FakeCall::DestroyImage(image));
        remove_live(&mut state.live_images, image.0, "EGLImage");
        failure(
            EglOperation::DestroyImage,
            state.behavior.image_destroy_error,
        )
    }
}
