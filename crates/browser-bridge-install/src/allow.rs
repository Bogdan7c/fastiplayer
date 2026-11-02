//! Кнопка «Allow»: выдать Flatpak-браузеру доступ к имени браузерного режима на шине.
//!
//! Выполняется только по явному клику пользователя (решение 158) и только для
//! установленной Flatpak-цели, которую вообще можно подключить.

use thiserror::Error;

use crate::catalog::BrowserTarget;
use crate::flatpak::{FlatpakError, FlatpakTool};

/// Почему доступ выдать нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AllowError {
    /// Такой цели нет в каталоге.
    #[error("Unknown browser")]
    UnknownTarget,
    /// Цель не Flatpak: доступ выдаётся только песочнице Flatpak.
    #[error("Bus access can only be granted to a Flatpak browser")]
    NotAFlatpakTarget,
    /// Flatpak-Firefox не поддерживается, выдавать нечего.
    #[error("Flatpak builds of Firefox are not supported yet")]
    FlatpakFirefoxFamilyUnsupported,
    /// Приложение не установлено.
    #[error("The Flatpak application is not installed")]
    NotInstalled,
    /// Утилита `flatpak` не справилась.
    #[error("flatpak: {0}")]
    Tool(FlatpakError),
}

/// Выдаёт доступ. Статус после выдачи пересчитывает фасад.
pub(crate) fn grant_bus_access(
    flatpak: &dyn FlatpakTool,
    target: &BrowserTarget,
) -> Result<(), AllowError> {
    let app_id = target
        .flatpak_app_id()
        .ok_or(AllowError::NotAFlatpakTarget)?;
    if target.is_flatpak_firefox_family() {
        return Err(AllowError::FlatpakFirefoxFamilyUnsupported);
    }
    let installed = flatpak.installed_apps().map_err(AllowError::Tool)?;
    if !installed.iter().any(|installed_id| installed_id == app_id) {
        return Err(AllowError::NotInstalled);
    }
    flatpak
        .grant_bridge_bus_access(app_id)
        .map_err(AllowError::Tool)
}
