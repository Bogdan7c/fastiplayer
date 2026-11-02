// Что плеер узнаёт о странице (инвариант 2: без адреса и кук): чистый заголовок и домен.

import { MAX_SITE_DOMAIN_BYTES, sanitizePageTitle, type PageIdentity } from "../../protocol/index.ts";

/**
 * Домен, который пройдёт строгую проверку протокола: нижний регистр, цифры, `-`, `.` и `[`, `]`, `:`
 * для IPv6. `URL.hostname` уже переводит IDN в punycode и опускает регистр.
 */
const PROTOCOL_DOMAIN_PATTERN = /^[a-z0-9.\-[\]:]+$/;

/** Подпись домена, когда у адреса его нет (file://, about:blank) или он не проходит проверку. */
export const UNKNOWN_SITE_DOMAIN = "unknown";

export function siteDomainOf(url: string | undefined): string {
  if (url === undefined) return UNKNOWN_SITE_DOMAIN;
  let hostname: string;
  try {
    hostname = new URL(url).hostname;
  } catch {
    return UNKNOWN_SITE_DOMAIN;
  }
  const valid = hostname.length > 0 && hostname.length <= MAX_SITE_DOMAIN_BYTES && PROTOCOL_DOMAIN_PATTERN.test(hostname);
  return valid ? hostname : UNKNOWN_SITE_DOMAIN;
}

/** Страница вкладки для `open_session`: заголовок чистится `sanitizePageTitle`, домен — по правилу выше. */
export function pageIdentityOf(tab: { readonly title?: string | undefined; readonly url?: string | undefined }): PageIdentity {
  return { title: sanitizePageTitle(tab.title ?? ""), domain: siteDomainOf(tab.url) };
}
