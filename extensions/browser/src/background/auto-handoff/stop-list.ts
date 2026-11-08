// Встроенный стоп-лист режима 2 «автозапуск» (сессия 12a, решение 15) и сопоставление адреса с записью.
//
// Стоп-лист — сайты, где «человек нажал play со звуком» почти никогда не значит «отдай это в плеер»:
// созвоны, банки и платежи. Список — данные, сгруппированные по смыслу, чтобы его было легко править: добавить
// сайт — дописать строку в нужную группу, ничего больше менять не надо.
//
// Соцсетей с лентой здесь нет (решение 259, после живой вехи M2): владелец убрал их из встроенного списка.
// Беззвучные ролики ленты режим 2 и так не берёт (фильтр звука), короткие — фильтр длины; кому соцсеть мешает,
// добавляет её в свой стоп-лист или ставит сайту «Never».
//
// Форма записи общая со своим стоп-листом человека (`normalizeStopListEntry`): `host` или `host/начало-пути`.
// - хост совпадает, если адрес вкладки на этом хосте или на его поддомене (`www.` и порт не мешают);
// - путь, если он есть в записи, совпадает на границе сегмента: `/shorts` и `/shorts/abc` — да, `/shortsx` — нет.

import { normalizeStopListEntry, siteKeyOfHost } from "../../shared/auto-handoff-settings.ts";

/** Группа встроенного стоп-листа: название для людей и записи. */
export interface StopListGroup {
  readonly id: "calls" | "banks_and_payments";
  /** Для журнала разработчика и, позже, страницы настроек (сессия 12b). */
  readonly title: string;
  readonly entries: readonly string[];
}

/** Встроенный стоп-лист, одобренный владельцем. Записи в той же форме, что и своя (`host` или `host/путь`). */
export const BUILT_IN_STOP_LIST_GROUPS: readonly StopListGroup[] = [
  {
    id: "calls",
    title: "Calls and messengers",
    entries: [
      "meet.google.com",
      "zoom.us",
      "teams.microsoft.com",
      "teams.live.com",
      "discord.com",
      "web.whatsapp.com",
      "web.telegram.org",
      "messenger.com",
      "whereby.com",
      "meet.jit.si",
      "webex.com",
      "app.slack.com",
    ],
  },
  {
    id: "banks_and_payments",
    title: "Banks and payments",
    entries: [
      "privat24.ua",
      "privatbank.ua",
      "monobank.ua",
      "online.oschadbank.ua",
      "ukrsibbank.com",
      "raiffeisen.ua",
      "pumb.ua",
      "paypal.com",
      "wise.com",
      "revolut.com",
    ],
  },
];

/** Все записи встроенного стоп-листа одним списком (порядок групп сохраняется). */
export function builtInStopListEntries(): readonly string[] {
  return BUILT_IN_STOP_LIST_GROUPS.flatMap((group) => group.entries);
}

/** Адрес вкладки, разобранный для сопоставления: только http(s). */
export interface WebAddress {
  /** Ключ сайта: хост строчными без `www.` (см. `siteKeyOfHost`). */
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
  const siteKey = siteKeyOfHost(parsed.hostname);
  return siteKey === null ? undefined : { siteKey, path: parsed.pathname.toLowerCase() };
}

/** Путь адреса лежит под путём записи на границе сегмента: `/shorts` и `/shorts/abc` — да, `/shortsx` — нет. */
function pathIsUnder(path: string, entryPath: string): boolean {
  return path === entryPath || path.startsWith(`${entryPath}/`);
}

/** Адрес подходит одной записи. Битая запись (не `host[/путь]`) не подходит ни к чему. */
export function addressMatchesStopListEntry(address: WebAddress, rawEntry: string): boolean {
  const entry = normalizeStopListEntry(rawEntry);
  if (entry === null) return false;
  const slash = entry.indexOf("/");
  const entryHost = slash < 0 ? entry : entry.slice(0, slash);
  const entryPath = slash < 0 ? "" : entry.slice(slash);
  const hostMatches = address.siteKey === entryHost || address.siteKey.endsWith(`.${entryHost}`);
  return hostMatches && (entryPath === "" || pathIsUnder(address.path, entryPath));
}

/** Адрес подходит хотя бы одной записи списка. */
export function addressIsInStopList(address: WebAddress, entries: readonly string[]): boolean {
  return entries.some((entry) => addressMatchesStopListEntry(address, entry));
}
