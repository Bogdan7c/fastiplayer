//! Разбор прав Flatpak-приложения: может ли оно звонить на имя браузерного режима.
//!
//! Вход — текст `flatpak info --show-permissions` (формат keyfile). Нас интересуют
//! две вещи:
//! - `[Context]` `sockets=` содержит `session-bus` (без `!`): приложение видит всю
//!   сессионную шину;
//! - `[Session Bus Policy]` есть имя с уровнем `talk` или `own`, которое покрывает
//!   `io.github.Bogdan7c.Fastiplayer.BrowserSession`.
//!
//! Имя в политике покрывает целевое, если оно равно ему или это маска `X.*`: по
//! правилам D-Bus маска `X.*` покрывает `X` и всё вида `X.что-угодно`. Уровень `see`
//! звонить не позволяет и не считается.

use desktop_integration::BROWSER_SESSION_BUS_NAME;

/// Маска, которую выдаёт кнопка «Allow»: покрывает имя службы и соседей по
/// приложению (`io.github.Bogdan7c.Fastiplayer.*`).
pub const BRIDGE_TALK_NAME_MASK: &str = "io.github.Bogdan7c.Fastiplayer.*";

/// Что песочница может на сессионной шине в отношении нашего имени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatpakBusAccess {
    /// Вся сессионная шина (`sockets=session-bus`).
    FullSessionBus,
    /// Доступ именно к нашему имени (`talk`/`own` на точное имя или маску).
    TalkNameGranted,
    /// Доступа нет.
    Missing,
}

impl FlatpakBusAccess {
    /// Достаточно ли доступа, чтобы посредник дозвонился до плеера.
    pub fn allows_bridge(self) -> bool {
        !matches!(self, Self::Missing)
    }
}

/// Разбирает текст прав. Незнакомые строки и секции пропускаются: формат расширяется.
pub fn parse_bus_access(permissions_text: &str) -> FlatpakBusAccess {
    let mut section = "";
    let mut has_session_bus_socket = false;
    let mut talk_name_granted = false;
    for raw_line in permissions_text.lines() {
        let line = raw_line.trim();
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = header;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (section, key.trim()) {
            ("Context", "sockets") => {
                apply_socket_list(value, &mut has_session_bus_socket);
            }
            ("Session Bus Policy", name)
                if grants_talk(value.trim()) && name_covers_bridge(name) =>
            {
                talk_name_granted = true;
            }
            _ => {}
        }
    }
    if has_session_bus_socket {
        FlatpakBusAccess::FullSessionBus
    } else if talk_name_granted {
        FlatpakBusAccess::TalkNameGranted
    } else {
        FlatpakBusAccess::Missing
    }
}

/// Применяет список `a;b;!c;` по порядку: `session-bus` включает, `!session-bus` выключает.
fn apply_socket_list(list: &str, has_session_bus_socket: &mut bool) {
    for entry in list.split(';').map(str::trim) {
        match entry {
            "session-bus" => *has_session_bus_socket = true,
            "!session-bus" => *has_session_bus_socket = false,
            _ => {}
        }
    }
}

/// Позволяет ли уровень доступа звонить.
fn grants_talk(level: &str) -> bool {
    matches!(level, "talk" | "own")
}

/// Покрывает ли имя из политики имя службы браузерного режима.
fn name_covers_bridge(policy_name: &str) -> bool {
    match policy_name.strip_suffix(".*") {
        Some(prefix) => {
            BROWSER_SESSION_BUS_NAME == prefix
                || BROWSER_SESSION_BUS_NAME
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        }
        None => policy_name == BROWSER_SESSION_BUS_NAME,
    }
}
