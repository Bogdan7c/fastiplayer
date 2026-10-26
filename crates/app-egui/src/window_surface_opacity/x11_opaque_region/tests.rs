//! Значение `_NET_WM_OPAQUE_REGION` и запись свойства настоящему окну X-сервера.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, CreateWindowAux, GetPropertyReply, WindowClass};
use x11rb::rust_connection::RustConnection;

use super::*;

/// Переменная окружения: X-сервер обязателен, его отсутствие — провал, а не пропуск теста.
const REQUIRE_X11_VARIABLE: &str = "FASTIPLAYER_REQUIRE_X11";

/// Сколько 32-битных значений читать из свойства: с запасом больше одного прямоугольника.
const PROPERTY_READ_LIMIT_CARDINALS: u32 = 64;

const fn pixels(width: u32, height: u32) -> SurfacePixelSize {
    SurfacePixelSize { width, height }
}

#[test]
fn whole_window_region_is_one_rectangle_from_the_window_origin() {
    // EWMH: CARDINAL[][4]/32 — x, y, ширина, высота в координатах окна.
    assert_eq!(
        whole_window_opaque_region(pixels(1920, 1080)),
        [0, 0, 1920, 1080]
    );
    assert_eq!(
        whole_window_opaque_region(pixels(2560, 1440)),
        [0, 0, 2560, 1440]
    );
}

#[test]
fn only_connection_failures_disable_the_writer() {
    // Оборванное соединение: повторять бесполезно, писатель отключается.
    assert!(X11OpaqueRegionError::Connection(ConnectionError::UnknownError).disables_writer());
    // Уже отключённый писатель ничего не отправлял — это не новый отказ соединения.
    // Отказ запроса сервером (`Rejected`) проверяет тест с настоящим окном ниже.
    assert!(!X11OpaqueRegionError::Disabled.disables_writer());
}

/// Соединение теста и корневое окно экрана по умолчанию, либо пропуск без X-сервера.
fn test_connection_or_skip(test_name: &str) -> Option<(RustConnection, u32)> {
    match x11rb::connect(None) {
        Ok((connection, screen_number)) => {
            let root = connection.setup().roots[screen_number].root;
            Some((connection, root))
        }
        Err(error) => {
            assert!(
                std::env::var_os(REQUIRE_X11_VARIABLE).is_none(),
                "{test_name}: X server is required ({REQUIRE_X11_VARIABLE}) but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no X server in $DISPLAY: {error}");
            None
        }
    }
}

/// Читает свойство окна со стороны другого клиента (как это делает композитор).
fn read_opaque_region(connection: &RustConnection, window: u32) -> GetPropertyReply {
    let atom = connection
        .intern_atom(/* only_if_exists */ false, OPAQUE_REGION_PROPERTY)
        .unwrap_or_else(|error| panic!("intern _NET_WM_OPAQUE_REGION request: {error}"))
        .reply()
        .unwrap_or_else(|error| panic!("intern _NET_WM_OPAQUE_REGION reply: {error}"));
    connection
        .get_property(
            /* delete */ false,
            window,
            atom.atom,
            AtomEnum::CARDINAL,
            0,
            PROPERTY_READ_LIMIT_CARDINALS,
        )
        .unwrap_or_else(|error| panic!("get_property request: {error}"))
        .reply()
        .unwrap_or_else(|error| panic!("get_property reply: {error}"))
}

/// Значения свойства как 32-битные CARDINAL.
fn cardinals(reply: &GetPropertyReply) -> Vec<u32> {
    reply
        .value32()
        .map(Iterator::collect)
        .unwrap_or_else(|| panic!("property format is {}, expected 32", reply.format))
}

#[test]
fn writer_declares_and_clears_the_region_on_a_real_x11_window() {
    let test_name = "writer_declares_and_clears_the_region_on_a_real_x11_window";
    let Some((connection, root)) = test_connection_or_skip(test_name) else {
        return;
    };
    // Невидимое (не отображённое) окно другого клиента — как окно winit для нашего писателя.
    let window = connection
        .generate_id()
        .unwrap_or_else(|error| panic!("generate window id: {error}"));
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            640,
            360,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )
        .unwrap_or_else(|error| panic!("create_window request: {error}"))
        .check()
        .unwrap_or_else(|error| panic!("create_window: {error}"));

    let mut writer = X11OpaqueRegionWriter::new();
    let window_id = X11WindowId::new(window);

    // Новое окно: свойства нет.
    assert_eq!(
        read_opaque_region(&connection, window).type_,
        u32::from(AtomEnum::NONE)
    );

    writer
        .declare_whole_window_opaque(window_id, pixels(1920, 1080))
        .unwrap_or_else(|error| panic!("declare opaque: {error}"));
    let declared = read_opaque_region(&connection, window);
    assert_eq!(declared.type_, u32::from(AtomEnum::CARDINAL));
    assert_eq!(cardinals(&declared), vec![0, 0, 1920, 1080]);

    // Новый размер заменяет прямоугольник, а не дописывает второй.
    writer
        .declare_whole_window_opaque(window_id, pixels(1920, 1040))
        .unwrap_or_else(|error| panic!("redeclare opaque: {error}"));
    assert_eq!(
        cardinals(&read_opaque_region(&connection, window)),
        vec![0, 0, 1920, 1040]
    );

    writer
        .clear_opaque_region(window_id)
        .unwrap_or_else(|error| panic!("clear opaque: {error}"));
    assert_eq!(
        read_opaque_region(&connection, window).type_,
        u32::from(AtomEnum::NONE)
    );

    // Окно уничтожено: сервер отвергает запрос, но соединение писателя остаётся рабочим.
    connection
        .destroy_window(window)
        .unwrap_or_else(|error| panic!("destroy_window request: {error}"))
        .check()
        .unwrap_or_else(|error| panic!("destroy_window: {error}"));
    let rejected = writer.declare_whole_window_opaque(window_id, pixels(640, 360));
    assert!(
        matches!(rejected, Err(X11OpaqueRegionError::Rejected(_))),
        "destroyed window must be rejected by the server, got {rejected:?}"
    );
    assert!(
        matches!(writer.connection, WriterConnection::Open(_)),
        "a rejected request must not disable the writer"
    );
}
