// Сопоставление адреса вкладки со списком сайтов вида `host` или `host/начало-пути` (вынесено из
// background/auto-handoff/stop-list.ts в сессии 16b3c: тем же правилом пользуется лента одного элемента,
// background/handoff/feed/single-element-feeds.ts, а модулю передачи незачем зависеть от режима 2).
//
// Правило записи (форма общая со стоп-листом человека, `normalizeStopListEntry`):
// - хост совпадает, если адрес вкладки на этом хосте или на его поддомене (`www.` и порт не мешают);
// - путь, если он есть в записи, совпадает на границе сегмента: `/shorts` и `/shorts/abc` — да, `/shortsx` — нет.
// Поведение не менялось: stop-list.ts отдаёт эти же функции под прежними именами.

import { normalizeStopListEntry } from "./auto-handoff-settings.ts";
import { siteKeyOfHost } from "./site-key.ts";

/** Адрес вкладки, разобранный для сопоставления: только http(s). */
export interface WebAddress {
  /** Ключ сайта: хост строчными без `www.` и без порта (см. `siteKeyOfHost`). */
  readonly siteKey: string;
  /** Путь строчными, начинается с `/`. */
  readonly path: string;
}

/** Разобрать адрес вкладки. `undefined` — это не веб-страница (служебная страница браузера, файл, мусор). */
export function parseWebAddress(tabUrl: string): WebAddress | undefined {
  let parsed: URL;
  try {
    parsed = new URL(tabUrl);
  } catch {
    return undefined;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return undefined;
  // `hostname` — без порта: стенд на `127.0.0.1:<порт>` совпадает с записью `127.0.0.1/…`.
  const siteKey = siteKeyOfHost(parsed.hostname);
  return siteKey === null ? undefined : { siteKey, path: parsed.pathname.toLowerCase() };
}

/** Путь адреса лежит под путём записи на границе сегмента: `/shorts` и `/shorts/abc` — да, `/shortsx` — нет. */
function pathIsUnder(path: string, entryPath: string): boolean {
  return path === entryPath || path.startsWith(`${entryPath}/`);
}

/** Адрес подходит одной записи `host[/путь]`. Битая запись не подходит ни к чему. */
export function addressMatchesSiteEntry(address: WebAddress, rawEntry: string): boolean {
  const entry = normalizeStopListEntry(rawEntry);
  if (entry === null) return false;
  const slash = entry.indexOf("/");
  const entryHost = slash < 0 ? entry : entry.slice(0, slash);
  const entryPath = slash < 0 ? "" : entry.slice(slash);
  const hostMatches = address.siteKey === entryHost || address.siteKey.endsWith(`.${entryHost}`);
  return hostMatches && (entryPath === "" || pathIsUnder(address.path, entryPath));
}
