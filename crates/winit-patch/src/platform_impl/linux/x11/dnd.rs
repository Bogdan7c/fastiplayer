use std::io;
use std::os::raw::*;
use std::path::{Path, PathBuf};
use std::str::Utf8Error;
use std::sync::Arc;

use percent_encoding::percent_decode;
use tracing::warn;
use x11rb::protocol::xproto::{self, ConnectionExt};

use super::atoms::AtomName::None as DndNone;
use super::atoms::*;
use super::{util, CookieResultExt, X11Error, XConnection};
use crate::dpi::PhysicalPosition;
use crate::platform::external_drag::{ExternalDragEvent, ExternalDragPayload};
use crate::platform_impl::external_drag::mime::PayloadFormat;
use crate::platform_impl::external_drag::payload::payload_from_bytes;
use crate::platform_impl::external_drag::MAX_DROP_PAYLOAD_BYTES;

/// Атомы типов данных, которые мы умеем принимать (из `XdndTypeList` / `XdndEnter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownTargets {
    pub uri_list: xproto::Atom,
    pub plain_text_utf8: xproto::Atom,
    pub utf8_string: xproto::Atom,
    pub plain_text: xproto::Atom,
}

/// Выбранный тип данных: какой атом просить у источника и как разбирать ответ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedTarget {
    pub atom: xproto::Atom,
    pub format: PayloadFormat,
}

/// Выбирает лучший принимаемый тип из предложенных источником.
///
/// Порядок тот же, что и на Wayland (`external_drag::mime`): `text/uri-list`,
/// затем простой текст (`text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`).
/// `text/x-moz-url` на X11 не запрашиваем: браузеры вместе с ним отдают `text/uri-list`.
pub fn select_target(offered: &[xproto::Atom], known: &KnownTargets) -> Option<SelectedTarget> {
    let ranked = [
        (known.uri_list, PayloadFormat::UriList),
        (known.plain_text_utf8, PayloadFormat::PlainText),
        (known.utf8_string, PayloadFormat::PlainText),
        (known.plain_text, PayloadFormat::PlainText),
    ];
    ranked
        .into_iter()
        .find(|(atom, _)| offered.contains(atom))
        .map(|(atom, format)| SelectedTarget { atom, format })
}

/// Достаёт корневые координаты из упакованного поля `XdndPosition` (`x << 16 | y`).
///
/// Обе координаты — знаковые 16-битные числа в пространстве рабочего стола.
pub fn unpack_root_position(packed: c_long) -> (i32, i32) {
    let root_x = ((packed >> 16) & 0xFFFF) as u16 as i16;
    let root_y = (packed & 0xFFFF) as u16 as i16;
    (i32::from(root_x), i32::from(root_y))
}

#[derive(Debug, Clone, Copy)]
pub enum DndState {
    Accepted,
    Rejected,
}

#[derive(Debug)]
pub enum DndDataParseError {
    EmptyData,
    InvalidUtf8(#[allow(dead_code)] Utf8Error),
    HostnameSpecified(#[allow(dead_code)] String),
    UnexpectedProtocol(#[allow(dead_code)] String),
    UnresolvablePath(#[allow(dead_code)] io::Error),
}

impl From<Utf8Error> for DndDataParseError {
    fn from(e: Utf8Error) -> Self {
        DndDataParseError::InvalidUtf8(e)
    }
}

impl From<io::Error> for DndDataParseError {
    fn from(e: io::Error) -> Self {
        DndDataParseError::UnresolvablePath(e)
    }
}

/// Состояние одного жеста для нового канала `external_drag` (без обращений к X-серверу).
///
/// Контракт: первая позиция даёт `Entered`, последующие — `Moved`; финал — `Dropped`
/// (данные успели прийти) или `Left`. Пока позиции не было, жест для нового канала не начинался.
#[derive(Debug, Default)]
pub struct GestureTracker {
    /// Последняя позиция курсора; `Some` означает, что `Entered` уже опубликован.
    last_position: Option<PhysicalPosition<f64>>,
    /// Содержимое жеста; заполняется, когда пришёл `SelectionNotify`.
    payload: Option<ExternalDragPayload>,
    /// Запрос данных уже провалился (слишком большой объём или ошибка чтения):
    /// повторять `convert_selection` на каждом `XdndPosition` бессмысленно.
    /// Сбрасывается вместе с жестом (`XdndLeave` / `XdndDrop` вызывают `Dnd::reset`).
    payload_unavailable: bool,
}

impl GestureTracker {
    /// Фиксирует новую позицию: первое событие жеста — `Entered`, дальше — `Moved`.
    pub fn advance(&mut self, position: PhysicalPosition<f64>) -> ExternalDragEvent {
        let already_entered = self.last_position.is_some();
        self.last_position = Some(position);
        if already_entered {
            ExternalDragEvent::Moved { position }
        } else {
            ExternalDragEvent::Entered { position }
        }
    }

    /// Запоминает прочитанное содержимое.
    pub fn set_payload(&mut self, payload: ExternalDragPayload) {
        self.payload = Some(payload);
    }

    /// Пришло ли уже содержимое.
    #[cfg(test)]
    pub fn has_payload(&self) -> bool {
        self.payload.is_some()
    }

    /// Запоминает, что содержимое жеста получить не удалось (запрашиваем один раз за жест).
    pub fn mark_payload_unavailable(&mut self) {
        self.payload_unavailable = true;
    }

    /// Нужно ли (ещё раз) просить у источника содержимое: ни данных, ни прежнего провала.
    pub fn should_request_payload(&self) -> bool {
        self.payload.is_none() && !self.payload_unavailable
    }

    /// Итог броска: `Dropped`, если данные успели прийти, иначе `Left`.
    /// `None` — жест для нового канала не начинался.
    pub fn finish_drop(&mut self) -> Option<ExternalDragEvent> {
        let position = self.last_position?;
        Some(match self.payload.take() {
            Some(payload) if !payload.is_empty() => ExternalDragEvent::Dropped { position, payload },
            _ => ExternalDragEvent::Left,
        })
    }

    /// Итог `XdndLeave`: `Left`, если жест уже был объявлен.
    pub fn leave(&self) -> Option<ExternalDragEvent> {
        self.last_position.map(|_| ExternalDragEvent::Left)
    }
}

pub struct Dnd {
    xconn: Arc<XConnection>,
    // Populated by XdndEnter event handler
    pub version: Option<c_long>,
    pub type_list: Option<Vec<xproto::Atom>>,
    // Populated by XdndPosition event handler
    pub source_window: Option<xproto::Window>,
    // Populated by SelectionNotify event handler (triggered by XdndPosition event handler)
    pub result: Option<Result<Vec<PathBuf>, DndDataParseError>>,
    // Тип данных, выбранный при первом XdndPosition (см. `select_target`).
    pub selected_target: Option<SelectedTarget>,
    // Состояние жеста для нового канала `external_drag` (позиция, содержимое).
    pub gesture: GestureTracker,
}

impl Dnd {
    pub fn new(xconn: Arc<XConnection>) -> Result<Self, X11Error> {
        Ok(Dnd {
            xconn,
            version: None,
            type_list: None,
            source_window: None,
            result: None,
            selected_target: None,
            gesture: GestureTracker::default(),
        })
    }

    pub fn reset(&mut self) {
        self.version = None;
        self.type_list = None;
        self.source_window = None;
        self.result = None;
        self.selected_target = None;
        self.gesture = GestureTracker::default();
    }

    /// Атомы принимаемых типов для `select_target`.
    pub fn known_targets(&self) -> KnownTargets {
        let atoms = self.xconn.atoms();
        KnownTargets {
            uri_list: atoms[TextUriList],
            plain_text_utf8: atoms[TextPlainUtf8],
            utf8_string: atoms[UTF8_STRING],
            plain_text: atoms[TextPlain],
        }
    }

    /// Переводит корневые координаты `XdndPosition` в физическую позицию внутри окна.
    ///
    /// При ошибке X-сервера пишет предупреждение и возвращает `None`
    /// (событие позиции тогда пропускается, жест не прерывается).
    pub fn window_position(
        &self,
        window: xproto::Window,
        root: xproto::Window,
        packed_root_position: c_long,
    ) -> Option<PhysicalPosition<f64>> {
        let (root_x, root_y) = unpack_root_position(packed_root_position);
        match self.xconn.translate_coords(window, root) {
            Ok(origin) => Some(PhysicalPosition::new(
                f64::from(root_x - i32::from(origin.dst_x)),
                f64::from(root_y - i32::from(origin.dst_y)),
            )),
            Err(error) => {
                warn!("drag & drop: не удалось перевести координаты в окно: {error}");
                None
            },
        }
    }

    /// Принимает прочитанные байты выбранного типа.
    ///
    /// Заполняет `payload` для нового канала и (только для `text/uri-list`) legacy-`result`
    /// с путями для `HoveredFile` / `DroppedFile`: его поведение осталось прежним.
    /// Данные больше лимита отбрасываются с предупреждением.
    pub fn accept_data(&mut self, bytes: &mut [c_uchar]) {
        let Some(target) = self.selected_target else {
            return;
        };
        if bytes.len() > MAX_DROP_PAYLOAD_BYTES {
            warn!(
                "drag & drop: данные ({} байт) превысили лимит {MAX_DROP_PAYLOAD_BYTES} байт, отброшены",
                bytes.len()
            );
            self.gesture.mark_payload_unavailable();
            return;
        }
        self.gesture.set_payload(payload_from_bytes(target.format, bytes));
        if target.format == PayloadFormat::UriList {
            self.result = Some(self.parse_data(bytes));
        }
    }

    pub unsafe fn send_status(
        &self,
        this_window: xproto::Window,
        target_window: xproto::Window,
        state: DndState,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        let (accepted, action) = match state {
            DndState::Accepted => (1, atoms[XdndActionPrivate]),
            DndState::Rejected => (0, atoms[DndNone]),
        };
        self.xconn
            .send_client_msg(target_window, target_window, atoms[XdndStatus] as _, None, [
                this_window,
                accepted,
                0,
                0,
                action as _,
            ])?
            .ignore_error();

        Ok(())
    }

    pub unsafe fn send_finished(
        &self,
        this_window: xproto::Window,
        target_window: xproto::Window,
        state: DndState,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        let (accepted, action) = match state {
            DndState::Accepted => (1, atoms[XdndActionPrivate]),
            DndState::Rejected => (0, atoms[DndNone]),
        };
        self.xconn
            .send_client_msg(target_window, target_window, atoms[XdndFinished] as _, None, [
                this_window,
                accepted,
                action as _,
                0,
                0,
            ])?
            .ignore_error();

        Ok(())
    }

    pub unsafe fn get_type_list(
        &self,
        source_window: xproto::Window,
    ) -> Result<Vec<xproto::Atom>, util::GetPropertyError> {
        let atoms = self.xconn.atoms();
        self.xconn.get_property(
            source_window,
            atoms[XdndTypeList],
            xproto::Atom::from(xproto::AtomEnum::ATOM),
        )
    }

    pub unsafe fn convert_selection(
        &self,
        window: xproto::Window,
        time: xproto::Timestamp,
        target: xproto::Atom,
    ) {
        let atoms = self.xconn.atoms();
        self.xconn
            .xcb_connection()
            .convert_selection(
                window,
                atoms[XdndSelection],
                target,
                atoms[XdndSelection],
                time,
            )
            .expect_then_ignore_error("Failed to send XdndSelection event")
    }

    pub unsafe fn read_data(
        &self,
        window: xproto::Window,
        target: xproto::Atom,
    ) -> Result<Vec<c_uchar>, util::GetPropertyError> {
        let atoms = self.xconn.atoms();
        self.xconn.get_property(window, atoms[XdndSelection], target)
    }

    pub fn parse_data(&self, data: &mut [c_uchar]) -> Result<Vec<PathBuf>, DndDataParseError> {
        if !data.is_empty() {
            let mut path_list = Vec::new();
            let decoded = percent_decode(data).decode_utf8()?.into_owned();
            for uri in decoded.split("\r\n").filter(|u| !u.is_empty()) {
                // The format is specified as protocol://host/path
                // However, it's typically simply protocol:///path
                let path_str = if uri.starts_with("file://") {
                    let path_str = uri.replace("file://", "");
                    if !path_str.starts_with('/') {
                        // A hostname is specified
                        // Supporting this case is beyond the scope of my mental health
                        return Err(DndDataParseError::HostnameSpecified(path_str));
                    }
                    path_str
                } else {
                    // Only the file protocol is supported
                    return Err(DndDataParseError::UnexpectedProtocol(uri.to_owned()));
                };

                let path = Path::new(&path_str).canonicalize()?;
                path_list.push(path);
            }
            Ok(path_list)
        } else {
            Err(DndDataParseError::EmptyData)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Условные номера атомов: важна только их различимость.
    const KNOWN: KnownTargets = KnownTargets {
        uri_list: 10,
        plain_text_utf8: 20,
        utf8_string: 30,
        plain_text: 40,
    };

    #[test]
    fn target_selection_prefers_uri_list_then_utf8_text_then_bare_text() {
        let all = select_target(&[40, 30, 20, 10], &KNOWN).expect("target");
        assert_eq!(all, SelectedTarget { atom: 10, format: PayloadFormat::UriList });

        let text_only = select_target(&[40, 30, 20], &KNOWN).expect("target");
        assert_eq!(text_only, SelectedTarget { atom: 20, format: PayloadFormat::PlainText });

        let utf8_string = select_target(&[40, 30], &KNOWN).expect("target");
        assert_eq!(utf8_string.atom, 30);

        let bare = select_target(&[40], &KNOWN).expect("target");
        assert_eq!(bare.atom, 40);
    }

    #[test]
    fn target_selection_rejects_unknown_types() {
        assert_eq!(select_target(&[1, 2, 3], &KNOWN), None);
        assert_eq!(select_target(&[], &KNOWN), None);
    }

    #[test]
    fn failed_payload_is_requested_once_per_gesture() {
        let mut gesture = GestureTracker::default();
        gesture.advance(PhysicalPosition::new(1.0, 2.0));
        assert!(gesture.should_request_payload());

        gesture.mark_payload_unavailable();

        // Следующие `XdndPosition` того же жеста не должны снова просить данные.
        assert!(!gesture.should_request_payload());
        // Новый жест (после `Dnd::reset`) начинается с чистого состояния.
        assert!(GestureTracker::default().should_request_payload());
    }

    #[test]
    fn received_payload_is_not_requested_again() {
        let mut gesture = GestureTracker::default();
        gesture.set_payload(ExternalDragPayload::new(vec!["file:///a".to_owned()], None));

        assert!(!gesture.should_request_payload());
    }

    #[test]
    fn gesture_reports_entered_once_then_moved() {
        let mut gesture = GestureTracker::default();

        let first = gesture.advance(PhysicalPosition::new(1.0, 2.0));
        let second = gesture.advance(PhysicalPosition::new(3.0, 4.0));

        assert_eq!(first, ExternalDragEvent::Entered { position: PhysicalPosition::new(1.0, 2.0) });
        assert_eq!(second, ExternalDragEvent::Moved { position: PhysicalPosition::new(3.0, 4.0) });
    }

    #[test]
    fn gesture_drop_carries_last_position_and_payload() {
        let mut gesture = GestureTracker::default();
        gesture.advance(PhysicalPosition::new(1.0, 2.0));
        gesture.advance(PhysicalPosition::new(30.0, 40.0));
        gesture.set_payload(ExternalDragPayload::new(vec!["file:///a".to_owned()], None));

        let event = gesture.finish_drop().expect("event");

        match event {
            ExternalDragEvent::Dropped { position, payload } => {
                assert_eq!(position, PhysicalPosition::new(30.0, 40.0));
                assert_eq!(payload.uris(), ["file:///a"]);
            },
            other => panic!("ожидался Dropped, получено {other:?}"),
        }
        assert!(!gesture.has_payload(), "содержимое отдаётся один раз");
    }

    #[test]
    fn gesture_drop_without_data_degrades_to_left() {
        let mut gesture = GestureTracker::default();
        gesture.advance(PhysicalPosition::new(1.0, 2.0));

        assert_eq!(gesture.finish_drop(), Some(ExternalDragEvent::Left));
    }

    #[test]
    fn gesture_without_position_emits_nothing_on_drop_or_leave() {
        let mut gesture = GestureTracker::default();
        gesture.set_payload(ExternalDragPayload::new(vec!["file:///a".to_owned()], None));

        assert_eq!(gesture.finish_drop(), None);
        assert_eq!(gesture.leave(), None);
    }

    #[test]
    fn gesture_leave_after_enter_reports_left() {
        let mut gesture = GestureTracker::default();
        gesture.advance(PhysicalPosition::new(5.0, 5.0));

        assert_eq!(gesture.leave(), Some(ExternalDragEvent::Left));
    }

    #[test]
    fn root_position_is_unpacked_as_signed_16_bit_pairs() {
        assert_eq!(unpack_root_position((300 << 16) | 120), (300, 120));
        // Отрицательные координаты (второй монитор слева/сверху) приходят в дополнительном коде.
        let negative_x = ((-5_i16 as u16 as c_long) << 16) | 7;
        assert_eq!(unpack_root_position(negative_x), (-5, 7));
        let negative_y = (9 << 16) | (-3_i16 as u16 as c_long);
        assert_eq!(unpack_root_position(negative_y), (9, -3));
    }
}
