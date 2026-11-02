//! Тесты установщика. Все работают во временном каталоге с фейковым HOME: настоящий
//! домашний каталог, `~/.var`, `~/.mozilla` и `~/.config` не трогаются никогда.

mod allow;
mod chaos;
mod connect;
mod disconnect;
mod flatpak_command;
mod flatpak_permissions;
mod host_copy;
mod inspection;
mod status;
mod support;
