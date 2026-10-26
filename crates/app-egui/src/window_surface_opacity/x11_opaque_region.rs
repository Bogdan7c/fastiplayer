//! X11-путь подсказки непрозрачности: свойство EWMH `_NET_WM_OPAQUE_REGION`.
//!
//! Спецификация EWMH: `_NET_WM_OPAQUE_REGION, x, y, width, height, CARDINAL[][4]/32` — список
//! прямоугольников в координатах окна, где пиксели содержимого полностью непрозрачны. KWin
//! (X11 и XWayland) читает свойство при появлении окна и по каждому `PropertyNotify`
//! (`X11Window::getWmOpaqueRegion`), а для окна с ARGB visual (`hasAlpha()`, глубина 32)
//! считает непрозрачной только `opaqueRegion() & shape()` (`SurfaceItemXwayland::opaque`).
//! Наше окно ARGB (прозрачность выбрана при создании), поэтому без свойства композитор
//! рисует всё, что под ним, на каждом кадре.
//!
//! Снятие подсказки — удаление свойства, а не пустой список: это ровно состояние нового окна
//! (свойства нет — непрозрачных областей не объявлено), а KWin на `PropertyNotify` удаления
//! перечитывает свойство и очищает регион так же, как для пустого списка.
//!
//! Свойство пишется через собственное соединение `x11rb` (чистый Rust, без `unsafe`), а не
//! через Xlib-соединение winit: идентификаторы окон X11 общие для всего сервера, и свойство
//! окна вправе менять любой клиент. Своё соединение даёт синхронную проверку каждого запроса
//! (`check()`), тогда как ошибки на Xlib-соединении winit ушли бы в его асинхронный обработчик.
//! Дисплей — `$DISPLAY`, тот же, что открывает winit (`XOpenDisplay(NULL)`).

use x11rb::errors::{ConnectError, ConnectionError, ReplyError};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::x11_utils::X11Error;

use super::SurfacePixelSize;

/// Имя свойства EWMH с непрозрачным регионом окна.
const OPAQUE_REGION_PROPERTY: &[u8] = b"_NET_WM_OPAQUE_REGION";

/// Идентификатор окна X11 (XID), общий для всех клиентов сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct X11WindowId(u32);

impl X11WindowId {
    /// Окно с данным XID.
    pub(super) const fn new(xid: u32) -> Self {
        Self(xid)
    }
}

/// Отказ X11 принять подсказку.
#[derive(Debug, thiserror::Error)]
pub(super) enum X11OpaqueRegionError {
    /// Не удалось открыть своё соединение с X-сервером из `$DISPLAY`.
    #[error("cannot open an X11 connection to $DISPLAY: {0}")]
    Connect(#[source] ConnectError),
    /// Соединение оборвалось; писатель больше не пробует (см. [`X11OpaqueRegionWriter`]).
    #[error("X11 connection failed: {0}")]
    Connection(#[source] ConnectionError),
    /// Сервер отверг запрос (например, окно уже уничтожено); соединение живо.
    #[error("X server rejected the opaque region request: {0:?}")]
    Rejected(X11Error),
    /// Писатель отключён прошлым отказом соединения: X11 ничего не отправлялось.
    #[error("X11 opaque region writer is disabled after an earlier connection failure")]
    Disabled,
}

impl From<ReplyError> for X11OpaqueRegionError {
    fn from(error: ReplyError) -> Self {
        match error {
            ReplyError::ConnectionError(error) => Self::Connection(error),
            ReplyError::X11Error(error) => Self::Rejected(error),
        }
    }
}

impl From<ConnectionError> for X11OpaqueRegionError {
    fn from(error: ConnectionError) -> Self {
        Self::Connection(error)
    }
}

impl X11OpaqueRegionError {
    /// Отказ самого соединения: повторять бесполезно, писатель отключается.
    const fn disables_writer(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Connection(_))
    }
}

/// Значение свойства для непрозрачного окна целиком: один прямоугольник `x, y, width, height`
/// в координатах окна (начало — левый верхний угол клиентской области).
pub(super) const fn whole_window_opaque_region(size: SurfacePixelSize) -> [u32; 4] {
    [0, 0, size.width, size.height]
}

/// Открытое соединение и атом свойства (атом запрашивается один раз на соединение).
struct OpaqueRegionConnection {
    /// Своё соединение с X-сервером.
    connection: RustConnection,
    /// Атом `_NET_WM_OPAQUE_REGION` на этом сервере.
    opaque_region_atom: Atom,
}

impl OpaqueRegionConnection {
    /// Открывает соединение с `$DISPLAY` и получает атом свойства.
    fn open() -> Result<Self, X11OpaqueRegionError> {
        // `None` — дисплей из `$DISPLAY`, как у winit.
        let (connection, _default_screen) =
            x11rb::connect(None).map_err(X11OpaqueRegionError::Connect)?;
        let opaque_region_atom = connection
            .intern_atom(/* only_if_exists */ false, OPAQUE_REGION_PROPERTY)?
            .reply()?
            .atom;
        Ok(Self {
            connection,
            opaque_region_atom,
        })
    }

    /// Заменяет регион окна одним прямоугольником на всё окно.
    fn replace_with_whole_window(
        &self,
        window: X11WindowId,
        size: SurfacePixelSize,
    ) -> Result<(), X11OpaqueRegionError> {
        self.connection
            .change_property32(
                PropMode::REPLACE,
                window.0,
                self.opaque_region_atom,
                AtomEnum::CARDINAL,
                &whole_window_opaque_region(size),
            )?
            // `check()` ждёт ответа сервера: запрос гарантированно отправлен, ошибка видна сразу.
            .check()?;
        Ok(())
    }

    /// Удаляет свойство: непрозрачных областей у окна больше не объявлено.
    fn delete(&self, window: X11WindowId) -> Result<(), X11OpaqueRegionError> {
        self.connection
            .delete_property(window.0, self.opaque_region_atom)?
            .check()?;
        Ok(())
    }
}

/// Состояние соединения писателя.
enum WriterConnection {
    /// Ещё ни разу не понадобилось: соединение открывается при первой подсказке.
    NotOpened,
    /// Соединение открыто и исправно (в куче: буферы `RustConnection` занимают сотни байт).
    Open(Box<OpaqueRegionConnection>),
    /// Соединение не открылось или оборвалось: писатель больше не обращается к X11.
    Disabled,
}

/// Пишет `_NET_WM_OPAQUE_REGION` окну через своё соединение с X-сервером.
///
/// Повторы: отказ *соединения* (не открылось, оборвалось) отключает писателя навсегда. Если
/// новое соединение с тем же `$DISPLAY` не открылось, пока winit работает, причина в окружении
/// (авторизация, лимит клиентов) и следующая попытка повторит её — но уже блокирующим
/// подключением в UI-потоке на каждой смене формы окна. Цена отказа — прежнее поведение
/// (композитор рисует то, что под окном). Отказ *запроса* при живом соединении писателя не
/// отключает: следующее изменение подсказки пробует снова.
pub(super) struct X11OpaqueRegionWriter {
    /// Соединение открывается лениво: в обычном окне со скруглением X11 не трогаем вовсе.
    connection: WriterConnection,
}

impl X11OpaqueRegionWriter {
    /// Писатель без соединения.
    pub(super) const fn new() -> Self {
        Self {
            connection: WriterConnection::NotOpened,
        }
    }

    /// Объявляет окно непрозрачным целиком при данном размере.
    pub(super) fn declare_whole_window_opaque(
        &mut self,
        window: X11WindowId,
        size: SurfacePixelSize,
    ) -> Result<(), X11OpaqueRegionError> {
        self.with_connection(|connection| connection.replace_with_whole_window(window, size))
    }

    /// Снимает объявление непрозрачности (удаляет свойство).
    pub(super) fn clear_opaque_region(
        &mut self,
        window: X11WindowId,
    ) -> Result<(), X11OpaqueRegionError> {
        self.with_connection(|connection| connection.delete(window))
    }

    /// Выполняет запрос на открытом соединении; отказ соединения отключает писателя.
    fn with_connection(
        &mut self,
        request: impl FnOnce(&OpaqueRegionConnection) -> Result<(), X11OpaqueRegionError>,
    ) -> Result<(), X11OpaqueRegionError> {
        if matches!(self.connection, WriterConnection::NotOpened) {
            self.connection = match OpaqueRegionConnection::open() {
                Ok(connection) => WriterConnection::Open(Box::new(connection)),
                Err(error) => {
                    self.connection = WriterConnection::Disabled;
                    return Err(error);
                }
            };
        }
        let WriterConnection::Open(connection) = &self.connection else {
            return Err(X11OpaqueRegionError::Disabled);
        };
        let result = request(connection);
        if let Err(error) = &result
            && error.disables_writer()
        {
            self.connection = WriterConnection::Disabled;
        }
        result
    }
}

#[cfg(test)]
mod tests;
