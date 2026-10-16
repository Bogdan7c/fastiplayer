//! Устойчивая идентичность web-источника для памяти выбора дорожек.
//!
//! Ключ «того же видео» строится из ИСХОДНОЙ ссылки, которой пользователь открыл источник:
//! страница для extractor-а, корневой манифест для native HLS/DASH/HDS/Smooth, сам ресурс для
//! direct. Временные child-endpoint-ы (сегменты, подписанные медиа-ссылки) сюда не попадают —
//! они меняются от запуска к запуску и сломали бы память.

use super::{WebMediaSourceAdapter, WebMediaSourceIntent};

impl WebMediaSourceIntent {
    /// Исходная ссылка источника — только для необратимого отпечатка в памяти дорожек.
    ///
    /// Секретный материал: наружу не логируется и в файл не пишется (хэшируется в `VideoKey`).
    pub(crate) fn original_url_for_track_memory(&self) -> &str {
        match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => locator.expose_secret_for_persistence(),
            WebMediaSourceAdapter::NativeHls { source, .. } => {
                source.target().expose_secret_for_request()
            }
            WebMediaSourceAdapter::NativeDash { source, .. } => {
                source.target().expose_secret_for_request()
            }
            WebMediaSourceAdapter::NativeHds { source, .. } => {
                source.target().expose_secret_for_request()
            }
            WebMediaSourceAdapter::NativeSmooth { source, .. } => {
                source.target().expose_secret_for_request()
            }
            WebMediaSourceAdapter::Extractor { locator, .. } => {
                locator.expose_secret_for_persistence()
            }
        }
    }
}
