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
// Форма записи общая со своим стоп-листом человека (`normalizeStopListEntry`): `host` или `host/начало-пути`;
// правило совпадения — shared/web-address-match.ts.

import { addressMatchesSiteEntry, type WebAddress } from "../../shared/web-address-match.ts";

// Разбор адреса и сопоставление с записью живут в shared/web-address-match.ts (16b3c: им же пользуется лента одного
// элемента). Здесь — прежние имена, чтобы потребители режима 2 не менялись.
export { parseWebAddress, type WebAddress } from "../../shared/web-address-match.ts";
export { addressMatchesSiteEntry as addressMatchesStopListEntry } from "../../shared/web-address-match.ts";

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

/** Адрес подходит хотя бы одной записи списка. */
export function addressIsInStopList(address: WebAddress, entries: readonly string[]): boolean {
  return entries.some((entry) => addressMatchesSiteEntry(address, entry));
}
