//! Приём drag & drop из внешних приложений на Wayland (`wl_data_device`).
//!
//! # Владение состоянием
//!
//! [`ExternalDragReceiver`] принадлежит `WinitState` и хранит:
//! * менеджер data device (если композитор его не предоставляет — приём тихо
//!   отключён, остальной winit работает как раньше);
//! * по одному `wl_data_device` на каждый seat;
//! * текущий жест ([`ActiveDrag`]) — между `enter` и `drop` / `leave`.
//!
//! # Границы
//!
//! Композитор вызывает [`DataDeviceHandler`] (реализован для `WinitState` ниже).
//! Наружу результат уходит двумя путями:
//! * новый канал: `external_drag::queue::publish` → `ApplicationHandler::external_drag_event`;
//! * legacy: `WindowEvent::DroppedFile` для каждого `file://` URI, чтобы egui
//!   вёл себя одинаково на X11 и Wayland. Legacy `HoveredFile` на Wayland не
//!   отправляется: содержимое жеста читается только в момент броска.
//!
//! # Инварианты
//!
//! * Событийный цикл никогда не блокируется: данные при броске читаются через
//!   calloop по готовности пайпа, источником может быть и наш собственный процесс.
//! * Объём данных ограничен `MAX_DROP_PAYLOAD_BYTES`, ожидание — `DROP_READ_TIMEOUT`.
//! * Любая ошибка логируется (`tracing::warn`) и завершает жест событием `Left`;
//!   паник нет.
//! * Принимаем только действие «копировать»: действие «переместить» заставило бы
//!   источник (например Dolphin) удалить исходные файлы после броска.
//! * После `drop` нового `Left` не бывает: финалом жеста становится `Dropped`
//!   (или `Left`, если чтение данных провалилось).

use std::cell::RefCell;
use std::io::{ErrorKind, Read};
use std::rc::Rc;
use std::time::Duration;

use ahash::AHashMap;
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, ReadPipe, WritePipe};
use sctk::reexports::calloop::generic::NoIoDrop;
use sctk::reexports::calloop::timer::{TimeoutAction, Timer};
use sctk::reexports::calloop::{PostAction, RegistrationToken};
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use tracing::{debug, warn};

use super::state::WinitState;
use super::{make_wid, WindowId};
use crate::dpi::{LogicalPosition, PhysicalPosition};
use crate::event::WindowEvent;
use crate::platform::external_drag::ExternalDragEvent;
use crate::platform_impl::external_drag::mime::{choose_mime, PayloadFormat};
use crate::platform_impl::external_drag::payload::{
    append_bounded, file_uri_to_path, payload_from_bytes, PayloadTooLarge,
};
use crate::platform_impl::external_drag::{queue, MAX_DROP_PAYLOAD_BYTES};
use crate::window::WindowId as RootWindowId;

/// Сколько ждать завершения передачи данных после броска, прежде чем сдаться.
const DROP_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Размер одного куска чтения из пайпа источника.
const DROP_READ_CHUNK_BYTES: usize = 64 * 1024;

/// Единственное действие, которое мы принимаем (см. инварианты модуля).
const ACCEPTED_DND_ACTION: DndAction = DndAction::Copy;

/// Текущий жест перетаскивания над одним из наших окон.
#[derive(Debug, Clone)]
struct ActiveDrag {
    /// Окно, над которым идёт жест.
    window_id: WindowId,
    /// Как разбирать данные при броске.
    format: PayloadFormat,
    /// Точное имя типа, которое просим у источника.
    offered_name: String,
    /// Последняя известная позиция (на момент броска Wayland позицию не повторяет).
    last_position: PhysicalPosition<f64>,
    /// Действие, выбранное композитором (`wl_data_offer.action`); до первого
    /// события пустое — композитор ещё ничего не согласовал с источником.
    selected_action: DndAction,
}

impl ActiveDrag {
    /// Запоминает действие, которое композитор согласовал с источником.
    fn record_selected_action(&mut self, action: DndAction) {
        self.selected_action = action;
    }

    /// Можно ли сообщать источнику `wl_data_offer.finish`.
    ///
    /// Протокол: `finish` при выбранном действии `none` — ошибка `invalid_finish`
    /// (композитор рвёт соединение). В этом случае offer только уничтожаем.
    fn may_send_finish(&self) -> bool {
        !self.selected_action.is_empty()
    }
}

/// Владелец приёма drag & drop на Wayland.
#[derive(Debug, Default)]
pub struct ExternalDragReceiver {
    /// `None`, если композитор не предоставляет `wl_data_device_manager`.
    manager: Option<DataDeviceManagerState>,
    /// Data device по идентификатору seat-а; хранение нужно, чтобы device жил
    /// и корректно освобождался при удалении seat-а.
    devices: AHashMap<ObjectId, DataDevice>,
    /// Жест, который сейчас идёт (между `enter` и `drop` / `leave`).
    active: Option<ActiveDrag>,
}

impl ExternalDragReceiver {
    /// Забирает незавершённый жест, который вытесняется новым `enter`.
    ///
    /// Читающих источников у такого жеста ещё нет: чтение стартует только на
    /// `drop`, а `drop` забирает `active` себе. Поэтому чистить нечего, но
    /// приложение обязано получить `Left` за вытесненный жест.
    fn take_superseded_gesture(&mut self) -> Option<ActiveDrag> {
        self.active.take()
    }

    /// Привязывает менеджер data device; при отсутствии — предупреждение и отключённый приём.
    pub fn new(globals: &GlobalList, queue_handle: &QueueHandle<WinitState>) -> Self {
        let manager = match DataDeviceManagerState::bind(globals, queue_handle) {
            Ok(manager) => Some(manager),
            Err(error) => {
                warn!("wl_data_device_manager недоступен, drag & drop из других приложений отключён: {error}");
                None
            },
        };
        Self { manager, devices: AHashMap::default(), active: None }
    }

    /// Создаёт data device для seat-а (повторный вызов для того же seat-а ничего не делает).
    pub fn seat_added(&mut self, queue_handle: &QueueHandle<WinitState>, seat: &WlSeat) {
        let Some(manager) = self.manager.as_ref() else { return };
        self.devices
            .entry(seat.id())
            .or_insert_with(|| manager.get_data_device(queue_handle, seat));
    }

    /// Освобождает data device удалённого seat-а.
    pub fn seat_removed(&mut self, seat: &WlSeat) {
        self.devices.remove(&seat.id());
    }
}

/// Переводит координаты поверхности (логические) в физические пиксели окна.
fn surface_position_to_physical(x: f64, y: f64, scale_factor: f64) -> PhysicalPosition<f64> {
    LogicalPosition::new(x, y).to_physical(scale_factor)
}

/// Чем закончилось чтение данных после броска.
#[derive(Debug)]
enum DropReadOutcome {
    /// Источник закрыл пайп: данные получены полностью.
    Complete,
    /// Данные превысили лимит.
    TooLarge(PayloadTooLarge),
    /// Ошибка чтения из пайпа.
    ReadFailed(std::io::Error),
    /// Источник не уложился в `DROP_READ_TIMEOUT`.
    TimedOut,
}

/// Кто именно завершил чтение (второй источник calloop надо снять).
#[derive(Debug, Clone, Copy)]
enum SettledBy {
    Reader,
    Timeout,
}

/// Состояние чтения данных одного броска; делится между читателем пайпа и таймером.
struct DropRead {
    /// Предложение данных; после чтения его надо завершить и уничтожить.
    offer: DragOffer,
    /// Жест, которому принадлежит бросок.
    active: ActiveDrag,
    /// Накопленные байты (ограничены `MAX_DROP_PAYLOAD_BYTES`).
    received: Vec<u8>,
    /// Токен источника-читателя в calloop.
    reader_token: Option<RegistrationToken>,
    /// Токен таймера ожидания в calloop.
    timeout_token: Option<RegistrationToken>,
    /// Защита от двойного завершения (читатель и таймер могут сработать подряд).
    settled: bool,
}

type SharedDropRead = Rc<RefCell<DropRead>>;

impl WinitState {
    /// Масштаб окна; `None`, если окна нет (жест над чужой поверхностью) или mutex отравлен.
    fn external_drag_scale_factor(&mut self, window_id: WindowId) -> Option<f64> {
        let window = self.windows.get_mut().get(&window_id)?;
        window.lock().map(|window| window.scale_factor()).ok()
    }

    /// Публикует событие жеста в очередь приложения и будит цикл.
    fn publish_external_drag(&mut self, window_id: WindowId, event: ExternalDragEvent) {
        queue::publish(RootWindowId(window_id), event);
        // Без этого флага цикл посчитает пробуждение «пустым» и не вызовет приложение.
        self.dispatched_events = true;
    }

    /// Вход курсора с данными: решает, принимаем ли жест, и объявляет `Entered`.
    fn begin_external_drag(
        &mut self,
        data_device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        // Новый жест вытесняет незавершённый (композитор не должен так делать):
        // приложению нужно честно закрыть старый жест событием `Left`.
        if let Some(superseded) = self.external_drag.take_superseded_gesture() {
            self.publish_external_drag(superseded.window_id, ExternalDragEvent::Left);
        }

        let window_id = make_wid(surface);
        let Some(scale_factor) = self.external_drag_scale_factor(window_id) else {
            debug!("drag & drop над поверхностью, не принадлежащей окну winit: игнорируем");
            return;
        };
        let Some(offer) = data_device.data::<DataDeviceData>().and_then(DataDeviceData::drag_offer)
        else {
            return;
        };

        let offered_types = offer.with_mime_types(|types| types.to_vec());
        let Some(choice) = choose_mime(&offered_types) else {
            // Нет понятного формата: честно отказываем, чтобы композитор показал «нельзя».
            offer.accept_mime_type(offer.serial, None);
            offer.set_actions(DndAction::empty(), DndAction::empty());
            return;
        };

        offer.accept_mime_type(offer.serial, Some(choice.offered_name.to_owned()));
        offer.set_actions(ACCEPTED_DND_ACTION, ACCEPTED_DND_ACTION);

        let position = surface_position_to_physical(x, y, scale_factor);
        self.external_drag.active = Some(ActiveDrag {
            window_id,
            format: choice.format,
            offered_name: choice.offered_name.to_owned(),
            last_position: position,
            selected_action: DndAction::empty(),
        });
        self.publish_external_drag(window_id, ExternalDragEvent::Entered { position });
    }

    /// Движение курсора во время принятого жеста.
    fn move_external_drag(&mut self, x: f64, y: f64) {
        let Some(window_id) = self.external_drag.active.as_ref().map(|active| active.window_id)
        else {
            return;
        };
        let Some(scale_factor) = self.external_drag_scale_factor(window_id) else {
            return;
        };
        let position = surface_position_to_physical(x, y, scale_factor);
        if let Some(active) = self.external_drag.active.as_mut() {
            active.last_position = position;
        }
        self.publish_external_drag(window_id, ExternalDragEvent::Moved { position });
    }

    /// Курсор ушёл без броска.
    fn leave_external_drag(&mut self) {
        if let Some(active) = self.external_drag.active.take() {
            self.publish_external_drag(active.window_id, ExternalDragEvent::Left);
        }
    }

    /// Бросок: запускает неблокирующее чтение данных.
    fn drop_external_drag(&mut self, connection: &Connection, data_device: &WlDataDevice) {
        let offer = data_device.data::<DataDeviceData>().and_then(DataDeviceData::drag_offer);
        let Some(active) = self.external_drag.active.take() else {
            // Жест мы не принимали (чужой формат / чужое окно): просто освобождаем offer.
            if let Some(offer) = offer {
                offer.destroy();
            }
            return;
        };
        let Some(offer) = offer else {
            self.publish_external_drag(active.window_id, ExternalDragEvent::Left);
            return;
        };

        let pipe = match offer.receive(active.offered_name.clone()) {
            Ok(pipe) => pipe,
            Err(error) => {
                warn!("drag & drop: не удалось запросить данные у источника: {error}");
                self.abort_external_drag(&offer, &active);
                return;
            },
        };
        // Запрос `receive` должен дойти до источника до того, как мы начнём ждать пайп.
        if let Err(error) = connection.flush() {
            warn!("drag & drop: не удалось отправить запрос данных композитору: {error}");
        }
        self.watch_drop_pipe(pipe, offer, active);
    }

    /// Подписывает пайп и таймер в calloop; ошибки регистрации завершают жест.
    fn watch_drop_pipe(&mut self, pipe: ReadPipe, offer: DragOffer, active: ActiveDrag) {
        let session: SharedDropRead = Rc::new(RefCell::new(DropRead {
            offer: offer.clone(),
            active: active.clone(),
            received: Vec::new(),
            reader_token: None,
            timeout_token: None,
            settled: false,
        }));

        let reader_session = Rc::clone(&session);
        let reader_token =
            self.loop_handle.insert_source(pipe, move |(), file, state: &mut WinitState| {
                read_drop_chunk(state, &reader_session, file)
            });
        match reader_token {
            Ok(token) => session.borrow_mut().reader_token = Some(token),
            Err(error) => {
                warn!("drag & drop: не удалось зарегистрировать чтение данных: {}", error.error);
                self.abort_external_drag(&offer, &active);
                return;
            },
        }

        let timer_session = Rc::clone(&session);
        let timer_token = self.loop_handle.insert_source(
            Timer::from_duration(DROP_READ_TIMEOUT),
            move |_deadline, _, state: &mut WinitState| {
                settle_drop_read(
                    state,
                    &timer_session,
                    DropReadOutcome::TimedOut,
                    SettledBy::Timeout,
                );
                TimeoutAction::Drop
            },
        );
        match timer_token {
            Ok(token) => session.borrow_mut().timeout_token = Some(token),
            Err(error) => {
                warn!("drag & drop: не удалось поставить таймер ожидания данных: {}", error.error);
                if let Some(reader_token) = session.borrow_mut().reader_token.take() {
                    self.loop_handle.remove(reader_token);
                }
                self.abort_external_drag(&offer, &active);
            },
        }
    }

    /// Завершает жест неудачей: `Left` приложению и освобождение offer-а.
    fn abort_external_drag(&mut self, offer: &DragOffer, active: &ActiveDrag) {
        self.publish_external_drag(active.window_id, ExternalDragEvent::Left);
        offer.destroy();
    }

    /// Успешный бросок: legacy `DroppedFile`, событие `Dropped` и завершение offer-а.
    fn finish_external_drop(&mut self, offer: &DragOffer, active: &ActiveDrag, received: &[u8]) {
        let payload = payload_from_bytes(active.format, received);
        if payload.is_empty() {
            warn!("drag & drop: источник передал данные, из которых не получилось извлечь ни URI, ни текст");
            self.abort_external_drag(offer, active);
            return;
        }

        // Legacy-события для приложений на штатном API (egui): только настоящие файлы.
        for path in payload.uris().iter().filter_map(|uri| file_uri_to_path(uri)) {
            self.events_sink.push_window_event(WindowEvent::DroppedFile(path), active.window_id);
        }
        self.publish_external_drag(
            active.window_id,
            ExternalDragEvent::Dropped { position: active.last_position, payload },
        );
        // Протокол: после получения данных получатель сообщает `finish`, затем уничтожает
        // offer. Но `finish` при выбранном действии `none` — `invalid_finish`.
        if active.may_send_finish() {
            offer.finish();
        } else {
            debug!("drag & drop: композитор не выбрал действие, `finish` пропущен");
        }
        offer.destroy();
    }
}

/// Читает один кусок из пайпа источника (вызывается calloop по готовности пайпа).
fn read_drop_chunk(
    state: &mut WinitState,
    session: &SharedDropRead,
    file: &mut NoIoDrop<std::fs::File>,
) -> PostAction {
    let mut chunk = [0_u8; DROP_READ_CHUNK_BYTES];
    // SAFETY: файл внутри `NoIoDrop` только читается и не заменяется и не закрывается
    // здесь — это единственное требование `get_mut`. Чтение по готовности не блокирует.
    let read_result = unsafe { file.get_mut() }.read(&mut chunk);

    let outcome = match read_result {
        Ok(0) => Some(DropReadOutcome::Complete),
        Ok(read_bytes) => {
            let mut session = session.borrow_mut();
            append_bounded(&mut session.received, &chunk[..read_bytes], MAX_DROP_PAYLOAD_BYTES)
                .err()
                .map(DropReadOutcome::TooLarge)
        },
        Err(error) if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
            None
        },
        Err(error) => Some(DropReadOutcome::ReadFailed(error)),
    };

    match outcome {
        None => PostAction::Continue,
        Some(outcome) => {
            settle_drop_read(state, session, outcome, SettledBy::Reader);
            PostAction::Remove
        },
    }
}

/// Единственная точка завершения чтения: снимает второй источник и сообщает итог.
fn settle_drop_read(
    state: &mut WinitState,
    session: &SharedDropRead,
    outcome: DropReadOutcome,
    settled_by: SettledBy,
) {
    let (offer, active, received, other_source) = {
        let mut session = session.borrow_mut();
        if session.settled {
            return;
        }
        session.settled = true;
        // Сам сработавший источник снимется возвращаемым `Remove` / `TimeoutAction::Drop`.
        let other_source = match settled_by {
            SettledBy::Reader => session.timeout_token.take(),
            SettledBy::Timeout => session.reader_token.take(),
        };
        (
            session.offer.clone(),
            session.active.clone(),
            std::mem::take(&mut session.received),
            other_source,
        )
    };
    if let Some(token) = other_source {
        state.loop_handle.remove(token);
    }

    match outcome {
        DropReadOutcome::Complete => state.finish_external_drop(&offer, &active, &received),
        DropReadOutcome::TooLarge(error) => {
            warn!(
                "drag & drop: данные превысили лимит {MAX_DROP_PAYLOAD_BYTES} байт ({} байт), бросок отменён",
                error.attempted_bytes
            );
            state.abort_external_drag(&offer, &active);
        },
        DropReadOutcome::ReadFailed(error) => {
            warn!("drag & drop: ошибка чтения данных источника, бросок отменён: {error}");
            state.abort_external_drag(&offer, &active);
        },
        DropReadOutcome::TimedOut => {
            warn!(
                "drag & drop: источник не передал данные за {DROP_READ_TIMEOUT:?}, бросок отменён"
            );
            state.abort_external_drag(&offer, &active);
        },
    }
}

impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        self.begin_external_drag(data_device, x, y, surface);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.leave_external_drag();
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        self.move_external_drag(x, y);
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        // Буфер обмена (selection) не наша зона: им занимается smithay-clipboard.
    }

    fn drop_performed(
        &mut self,
        connection: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
    ) {
        self.drop_external_drag(connection, data_device);
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
        // Мы всегда принимаем только копирование, набор действий источника не важен.
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        action: DndAction,
    ) {
        // Запоминаем действие в жесте: от него зависит, можно ли слать `finish`.
        // Событие чужого жеста (другая поверхность) игнорируем.
        let offer_window_id = make_wid(&offer.surface);
        if let Some(active) = self.external_drag.active.as_mut() {
            if active.window_id == offer_window_id {
                active.record_selected_action(action);
            }
        }
    }
}

/// Источники данных мы не создаём (drag наружу не поддерживается), но делегат sctk
/// требует реализацию трейта; все методы намеренно пустые.
impl DataSourceHandler for WinitState {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

sctk::delegate_data_device!(WinitState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_position_is_scaled_to_physical_pixels() {
        let position = surface_position_to_physical(10.0, 20.5, 2.0);

        assert_eq!(position, PhysicalPosition::new(20.0, 41.0));
    }

    #[test]
    fn surface_position_supports_fractional_scale() {
        let position = surface_position_to_physical(100.0, 40.0, 1.25);

        assert_eq!(position, PhysicalPosition::new(125.0, 50.0));
    }

    fn sample_drag() -> ActiveDrag {
        ActiveDrag {
            window_id: WindowId(1),
            format: PayloadFormat::UriList,
            offered_name: "text/uri-list".to_owned(),
            last_position: PhysicalPosition::new(0.0, 0.0),
            selected_action: DndAction::empty(),
        }
    }

    #[test]
    fn finish_is_forbidden_until_compositor_selects_action() {
        let mut drag = sample_drag();
        assert!(!drag.may_send_finish());

        drag.record_selected_action(DndAction::Copy);
        assert!(drag.may_send_finish());

        // Композитор может снова выбрать `none` (например курсор ушёл с цели).
        drag.record_selected_action(DndAction::empty());
        assert!(!drag.may_send_finish());
    }

    #[test]
    fn new_enter_hands_back_superseded_gesture_for_left_event() {
        let mut receiver = ExternalDragReceiver::default();
        assert!(receiver.take_superseded_gesture().is_none());

        receiver.active = Some(sample_drag());
        let superseded = receiver.take_superseded_gesture();

        assert_eq!(superseded.map(|drag| drag.window_id), Some(WindowId(1)));
        assert!(receiver.active.is_none());
    }

    #[test]
    fn only_copy_action_is_accepted() {
        // Move привёл бы к удалению исходных файлов источником после броска.
        assert_eq!(ACCEPTED_DND_ACTION, DndAction::Copy);
        assert!(!ACCEPTED_DND_ACTION.contains(DndAction::Move));
    }
}
