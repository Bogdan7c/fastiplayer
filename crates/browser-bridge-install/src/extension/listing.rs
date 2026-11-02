//! Страницы нашего экстеншена в магазинах как значение времени выполнения.
//!
//! В протоколе (`extension_identity`) страницы заданы константами, которые в сессии 24
//! получат настоящие значения. Установщик же работает с [`ExtensionListings`]:
//! так тесты и стенд живой проверки подставляют ID сторонних экстеншенов из
//! настоящих магазинов, не трогая константы.

use browser_bridge_protocol::extension_identity::{
    BrowserFamily, CHROME_WEB_STORE_LISTING, FIREFOX_ADD_ONS_LISTING, StoreListing,
};
use thiserror::Error;

/// Адрес сервера обновлений Chrome Web Store: именно его ждёт поле
/// `external_update_url` в файле внешней установки.
pub const CHROME_WEB_STORE_UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// Длина ID экстеншена Chromium: 32 буквы `a`–`p`.
const CHROMIUM_EXTENSION_ID_LENGTH: usize = 32;

/// Опубликован ли экстеншен в одном магазине.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Ещё нет: ставить нечего.
    NotPublished,
    /// Опубликован: ID в Chrome Web Store или slug на AMO.
    Published(String),
}

/// Почему страницу магазина принять нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ListingError {
    /// ID Chrome Web Store не из 32 букв `a`–`p`. Он становится именем файла
    /// `<ID>.json`, поэтому чужие символы (например `/`) недопустимы.
    #[error("A Chrome Web Store ID must be 32 letters from a to p")]
    InvalidChromeWebStoreId,
    /// Slug AMO пуст или содержит символы вне `A-Za-z0-9._-`.
    #[error(
        "A Firefox Add-ons slug must be non-empty and use only letters, digits, '.', '_' and '-'"
    )]
    InvalidFirefoxSlug,
}

/// Страницы экстеншена в двух магазинах.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionListings {
    chrome_web_store: Listing,
    firefox_add_ons: Listing,
}

impl ExtensionListings {
    /// Значения из протокола (в приложении). Пока константы — `NotPublished`.
    ///
    /// Если константы кто-то заполнил некорректно, это не паника: лог и вид
    /// «не опубликовано» (тест `protocol_listings_are_valid` ловит это при сборке).
    pub fn published() -> Self {
        let from_protocol = Self::new(
            runtime_listing(CHROME_WEB_STORE_LISTING),
            runtime_listing(FIREFOX_ADD_ONS_LISTING),
        );
        from_protocol.unwrap_or_else(|error| {
            tracing::error!(%error, "invalid store listing constant in the protocol crate");
            Self::unpublished()
        })
    }

    /// Ни один магазин не опубликован.
    pub fn unpublished() -> Self {
        Self {
            chrome_web_store: Listing::NotPublished,
            firefox_add_ons: Listing::NotPublished,
        }
    }

    /// Свои значения (тесты, стенд живой проверки с чужими экстеншенами-заменителями).
    pub fn new(chrome_web_store: Listing, firefox_add_ons: Listing) -> Result<Self, ListingError> {
        if let Listing::Published(id) = &chrome_web_store
            && !is_valid_chromium_extension_id(id)
        {
            return Err(ListingError::InvalidChromeWebStoreId);
        }
        if let Listing::Published(slug) = &firefox_add_ons
            && !is_valid_add_ons_slug(slug)
        {
            return Err(ListingError::InvalidFirefoxSlug);
        }
        Ok(Self {
            chrome_web_store,
            firefox_add_ons,
        })
    }

    /// ID в Chrome Web Store, если опубликован.
    pub fn chrome_web_store_id(&self) -> Option<&str> {
        match &self.chrome_web_store {
            Listing::Published(id) => Some(id),
            Listing::NotPublished => None,
        }
    }

    /// Опубликован ли экстеншен для этого семейства браузеров.
    pub fn is_published_for(&self, family: BrowserFamily) -> bool {
        self.store_page_url(family).is_some()
    }

    /// Адрес страницы в магазине семейства или `None`, если не опубликован.
    pub fn store_page_url(&self, family: BrowserFamily) -> Option<String> {
        match (family, &self.chrome_web_store, &self.firefox_add_ons) {
            (BrowserFamily::Chromium, Listing::Published(id), _) => {
                Some(format!("https://chromewebstore.google.com/detail/{id}"))
            }
            (BrowserFamily::Firefox, _, Listing::Published(slug)) => {
                Some(format!("https://addons.mozilla.org/firefox/addon/{slug}/"))
            }
            _ => None,
        }
    }
}

fn runtime_listing(listing: StoreListing) -> Listing {
    match listing {
        StoreListing::NotPublished => Listing::NotPublished,
        StoreListing::Published(value) => Listing::Published(value.to_owned()),
    }
}

fn is_valid_chromium_extension_id(id: &str) -> bool {
    id.len() == CHROMIUM_EXTENSION_ID_LENGTH && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

fn is_valid_add_ons_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
