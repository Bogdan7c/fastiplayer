//! Кто «наш» экстеншен: разрешённые ID и имя посредника для браузеров (сессия 05).
//!
//! Это часть договора трёх сторон:
//! - посредник `fastiplayer-browser-host` пускает только эти ID (инвариант 3 —
//!   «посредник тонкий и недоверчивый»);
//! - установщик (сессия 06) пишет их в файл подключения браузера
//!   (`allowed_origins` у Chrome, `allowed_extensions` у Firefox), и браузер сам не
//!   даст чужому экстеншену запустить посредника;
//! - экстеншен (сессия 07) обязан иметь ровно эти ID: публичный ключ Chrome и ID
//!   Firefox лежат в `fixtures/extension-identity.json`, тест сверяет их с
//!   константами ниже, чтобы стороны не разошлись.
//!
//! Список вшит в код, а не читается из файла (решение владельца 05): посредник
//! не доверяет ничему на диске, а подменить бинарь — уже значит владеть машиной.
//!
//! Как браузер называет экстеншен:
//! - Chrome и его форки — 32 буквы `a`–`p`, производные от публичного ключа
//!   экстеншена (ключ `key` в манифесте); посреднику первым аргументом приходит
//!   `chrome-extension://<ID>/`;
//! - Firefox — ID из `browser_specific_settings.gecko.id` манифеста; посреднику он
//!   приходит вторым аргументом, после пути к файлу подключения.

/// Имя посредника для браузеров: его передаёт `runtime.connectNative()`, им же
/// называется файл подключения (`<имя>.json`).
///
/// Chrome разрешает в имени только строчные буквы, цифры, `_` и точки, поэтому
/// имя пишется строчными в отличие от app ID `io.github.Bogdan7c.Fastiplayer`.
pub const NATIVE_HOST_NAME: &str = "io.github.bogdan7c.fastiplayer.bridge";

/// Семейство браузеров: у каждого свой вид ID экстеншена и свой способ передать
/// его посреднику.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserFamily {
    /// Chrome, Chromium и форки на движке Chromium.
    Chromium,
    /// Firefox и его форки.
    Firefox,
}

/// Откуда взялся ID: от этого зависит, когда он появляется в списке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionChannel {
    /// Сборка разработчика: распакованный экстеншен с нашим ключом `key`
    /// (Chrome) или нашим `gecko.id` (Firefox). На этом ID работают тесты и стенд.
    Development,
    /// ID, выданный магазином (Chrome Web Store назначает свой ID при первой
    /// загрузке; добавляется в сессии 24).
    Store,
}

/// Один разрешённый экстеншен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AllowedExtension {
    /// Для какого семейства браузеров этот ID.
    pub browser: BrowserFamily,
    /// ID ровно в том виде, в каком его передаёт браузер (без `chrome-extension://`).
    pub extension_id: &'static str,
    /// Сборка разработчика или магазин.
    pub channel: ExtensionChannel,
}

/// ID Chrome сборки разработчика: SHA-256 публичного ключа из
/// `fixtures/extension-identity.json`, первые 16 байт, каждая шестнадцатеричная
/// цифра `0`–`f` записана буквой `a`–`p` (так Chrome выводит ID из `key`).
pub const CHROMIUM_DEVELOPMENT_EXTENSION_ID: &str = "mcikhhmaenodlfkafgehbkikilmpnpji";

/// ID Firefox: мы выбираем его сами, он одинаков для сборки разработчика и
/// AMO (магазин принимает ID из манифеста и закрепляет его навсегда).
pub const FIREFOX_EXTENSION_ID: &str = "fastiplayer-bridge@bogdan7c.github.io";

/// Все экстеншены, которым посредник откроет дверь.
pub const ALLOWED_EXTENSIONS: &[AllowedExtension] = &[
    AllowedExtension {
        browser: BrowserFamily::Chromium,
        extension_id: CHROMIUM_DEVELOPMENT_EXTENSION_ID,
        channel: ExtensionChannel::Development,
    },
    AllowedExtension {
        browser: BrowserFamily::Firefox,
        extension_id: FIREFOX_EXTENSION_ID,
        channel: ExtensionChannel::Development,
    },
];

/// Разрешён ли этот ID для этого семейства браузеров.
///
/// Сравнение точное, с учётом регистра: браузер передаёт ID ровно так, как он
/// записан в манифесте, а «похожий» ID — это уже другой экстеншен.
#[must_use]
pub fn is_allowed_extension(browser: BrowserFamily, extension_id: &str) -> bool {
    ALLOWED_EXTENSIONS
        .iter()
        .any(|allowed| allowed.browser == browser && allowed.extension_id == extension_id)
}

#[cfg(test)]
mod tests;
