//! Fastiplayer: когда потоку буфера обмена нужно слушать общий Wayland-сокет.
//!
//! # Почему не всегда
//!
//! `Clipboard::new` получает `wl_display` чужого соединения (egui-winit отдаёт
//! соединение окна winit) и заводит на нём свою очередь событий. Сокет у
//! соединения один: libwayland не умеет будить поток только событиями его
//! очереди — готовность сокета на чтение видна каждому, кто его опрашивает.
//! Upstream-поток опрашивал сокет всегда и просыпался на каждом событии окна
//! (frame callback, освобождение буфера) — около 100 раз в секунду при видео,
//! хотя его собственных событий в эти моменты не было.
//!
//! # Когда слушать обязательно
//!
//! Пока приложение владеет выделением (скопировало текст), любое другое
//! приложение может вставить его: композитор пришлёт `send` в нашу очередь, и
//! ответить нужно сразу — иначе вставка у соседа зависнет. Поэтому сокет
//! слушается ровно пока живы наши источники выделения (до `cancelled`).
//!
//! # Когда можно спать
//!
//! Остальные события (фокус клавиатуры, serial-ы, текущее выделение) нужны только
//! в момент команды `load`/`store`. Их всё равно читает из сокета поток окна и
//! раскладывает по очередям; рабочий поток догоняет свою очередь roundtrip-ом
//! перед каждой командой и разбирает накопленное раз в
//! [`IDLE_QUEUE_DRAIN_INTERVAL`], чтобы очередь не росла (наш `wl_pointer`
//! получает движения мыши над окном).

use std::io;
use std::time::Duration;

/// Как часто спящий поток разбирает накопившиеся события своей очереди.
///
/// Это не задержка команд (команда сама догоняет очередь roundtrip-ом), а
/// только предел памяти: за секунду движения мыши набирается не больше пары
/// тысяч мелких событий. Одна побудка в секунду вместо ~100 при видео.
pub const IDLE_QUEUE_DRAIN_INTERVAL: Duration = Duration::from_secs(1);

/// Владеет ли приложение сейчас выделением (обычным или primary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOwnership {
    /// Своих источников выделения нет: вставлять у нас нечего.
    NotOwned,
    /// Есть живой источник: другие клиенты могут прислать `send` в любой момент.
    Owned,
}

/// Режим опроса общего сокета рабочим потоком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketWatch {
    /// Сокет не опрашивается; очередь разбирается по команде и по таймеру.
    Sleeping,
    /// Сокет опрашивается как в upstream: события разбираются сразу.
    Listening,
}

impl SocketWatch {
    /// Какой режим нужен при данном владении выделением.
    pub fn required_for(ownership: SelectionOwnership) -> Self {
        match ownership {
            SelectionOwnership::NotOwned => Self::Sleeping,
            SelectionOwnership::Owned => Self::Listening,
        }
    }
}

/// Включение и выключение опроса сокета в цикле событий.
///
/// В рабочем потоке это calloop-регистрация источника сокета; в тестах —
/// подделка со счётчиками.
pub trait SocketWatchSwitch {
    /// Начать опрашивать сокет.
    fn start_listening(&mut self) -> io::Result<()>;
    /// Перестать опрашивать сокет.
    fn stop_listening(&mut self) -> io::Result<()>;
}

/// Хранит текущий режим и переключает регистрацию только при его смене.
#[derive(Debug)]
pub struct SocketWatchController {
    current: SocketWatch,
}

impl SocketWatchController {
    /// Начальное состояние: источник сокета зарегистрирован и сразу выключен.
    pub fn sleeping() -> Self {
        Self { current: SocketWatch::Sleeping }
    }

    /// Текущий режим опроса (нужен только тестам: рабочий поток лишь вызывает `sync`).
    #[cfg(test)]
    pub fn current(&self) -> SocketWatch {
        self.current
    }

    /// Приводит опрос сокета к режиму, нужному при `ownership`.
    ///
    /// Если переключение не удалось, режим не меняется и ошибка возвращается
    /// вызывающему: рабочий поток тогда завершается (как upstream при сбое
    /// цикла событий), а `load` дальше честно отвечает «clipboard is dead».
    pub fn sync(
        &mut self,
        ownership: SelectionOwnership,
        switch: &mut impl SocketWatchSwitch,
    ) -> io::Result<()> {
        let required = SocketWatch::required_for(ownership);
        if required == self.current {
            return Ok(());
        }
        match required {
            SocketWatch::Listening => switch.start_listening()?,
            SocketWatch::Sleeping => switch.stop_listening()?,
        }
        self.current = required;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Подделка переключателя: считает вызовы и умеет отказывать.
    #[derive(Default)]
    struct RecordingSwitch {
        starts: usize,
        stops: usize,
        fail_next: bool,
    }

    impl RecordingSwitch {
        fn take_failure(&mut self) -> io::Result<()> {
            if std::mem::take(&mut self.fail_next) {
                return Err(io::Error::other("epoll registration failed"));
            }
            Ok(())
        }
    }

    impl SocketWatchSwitch for RecordingSwitch {
        fn start_listening(&mut self) -> io::Result<()> {
            self.take_failure()?;
            self.starts += 1;
            Ok(())
        }

        fn stop_listening(&mut self) -> io::Result<()> {
            self.take_failure()?;
            self.stops += 1;
            Ok(())
        }
    }

    #[test]
    fn without_owned_selection_the_socket_is_never_watched() {
        let mut controller = SocketWatchController::sleeping();
        let mut switch = RecordingSwitch::default();
        for _ in 0..3 {
            controller.sync(SelectionOwnership::NotOwned, &mut switch).unwrap();
        }
        assert_eq!(controller.current(), SocketWatch::Sleeping);
        assert_eq!((switch.starts, switch.stops), (0, 0));
    }

    #[test]
    fn copy_starts_watching_once_and_cancel_stops_once() {
        let mut controller = SocketWatchController::sleeping();
        let mut switch = RecordingSwitch::default();

        // Копирование из приложения: источник появился — слушаем ровно один раз.
        controller.sync(SelectionOwnership::Owned, &mut switch).unwrap();
        controller.sync(SelectionOwnership::Owned, &mut switch).unwrap();
        assert_eq!(controller.current(), SocketWatch::Listening);
        assert_eq!((switch.starts, switch.stops), (1, 0));

        // Другой клиент забрал выделение (`cancelled`): снова спим.
        controller.sync(SelectionOwnership::NotOwned, &mut switch).unwrap();
        controller.sync(SelectionOwnership::NotOwned, &mut switch).unwrap();
        assert_eq!(controller.current(), SocketWatch::Sleeping);
        assert_eq!((switch.starts, switch.stops), (1, 1));
    }

    #[test]
    fn failed_switch_keeps_the_previous_mode_and_reports_the_error() {
        let mut controller = SocketWatchController::sleeping();
        let mut switch = RecordingSwitch { fail_next: true, ..RecordingSwitch::default() };

        let error = controller.sync(SelectionOwnership::Owned, &mut switch).unwrap_err();
        assert_eq!(error.to_string(), "epoll registration failed");
        assert_eq!(controller.current(), SocketWatch::Sleeping);
        assert_eq!((switch.starts, switch.stops), (0, 0));

        // Следующая попытка снова пробует включить опрос.
        controller.sync(SelectionOwnership::Owned, &mut switch).unwrap();
        assert_eq!(controller.current(), SocketWatch::Listening);
        assert_eq!(switch.starts, 1);
    }
}
