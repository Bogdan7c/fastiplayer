//! «Сторож CDN»: отдаёт медиа только с кукой логина и своим Referer. Считает отказы по причинам.

use std::sync::atomic::{AtomicU64, Ordering};

/// Имя и значение куки, которую ставит `/cdn/login`.
pub(super) const CDN_COOKIE_NAME: &str = "bench_cdn";
const CDN_COOKIE_VALUE: &str = "1";

/// Сколько запросов отклонено и по какой причине.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CdnRejections {
    /// Нет куки `bench_cdn=1`.
    pub missing_cookie: u64,
    /// Кука есть, но `Referer` отсутствует или не с origin этого же сервера.
    pub foreign_referer: u64,
}

/// Решение сторожа по одному запросу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CdnVerdict {
    Allow,
    MissingCookie,
    ForeignReferer,
}

#[derive(Default)]
pub(super) struct CdnGuard {
    served: AtomicU64,
    missing_cookie: AtomicU64,
    foreign_referer: AtomicU64,
}

impl CdnGuard {
    /// Проверяет запрос и записывает результат в счётчики.
    ///
    /// Порядок: сперва кука, потом Referer. Если не так ни то ни другое, засчитывается только
    /// `missing_cookie` — один отказ считается один раз.
    pub(super) fn check(
        &self,
        cookie_header: Option<&str>,
        referer: Option<&str>,
        own_origin: &str,
    ) -> CdnVerdict {
        let verdict = if !has_cdn_cookie(cookie_header) {
            CdnVerdict::MissingCookie
        } else if !referer_is_from(referer, own_origin) {
            CdnVerdict::ForeignReferer
        } else {
            CdnVerdict::Allow
        };
        let counter = match verdict {
            CdnVerdict::Allow => &self.served,
            CdnVerdict::MissingCookie => &self.missing_cookie,
            CdnVerdict::ForeignReferer => &self.foreign_referer,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        verdict
    }

    pub(super) fn served(&self) -> u64 {
        self.served.load(Ordering::Relaxed)
    }

    pub(super) fn rejections(&self) -> CdnRejections {
        CdnRejections {
            missing_cookie: self.missing_cookie.load(Ordering::Relaxed),
            foreign_referer: self.foreign_referer.load(Ordering::Relaxed),
        }
    }
}

/// Значение для `Set-Cookie` в `/cdn/login`. Путь `/cdn` — кука не уходит на обычные `/media/`.
pub(super) fn login_cookie_header() -> String {
    format!("{CDN_COOKIE_NAME}={CDN_COOKIE_VALUE}; Path=/cdn")
}

fn has_cdn_cookie(cookie_header: Option<&str>) -> bool {
    let expected = format!("{CDN_COOKIE_NAME}={CDN_COOKIE_VALUE}");
    cookie_header.is_some_and(|header| header.split(';').any(|pair| pair.trim() == expected))
}

/// Referer «с нашего origin»: это сам origin либо origin и дальше `/`.
/// Просто `starts_with` ошибся бы на соседнем порту (`:1234` и `:12345`).
fn referer_is_from(referer: Option<&str>, own_origin: &str) -> bool {
    referer.is_some_and(|value| {
        value
            .strip_prefix(own_origin)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}
