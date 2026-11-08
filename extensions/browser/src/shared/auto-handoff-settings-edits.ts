// Правки настройки режима 2 «автозапуск» (сессия 12b, решения 254–258): поставить или снять правило сайта,
// добавить или убрать запись своего стоп-листа.
//
// Чистые функции «настройка + намерение → новая настройка или честный отказ», без хранилища и браузера.
// Ими пользуются все, кто пишет настройку: фон (выбор человека на предложении «Ask»), попап (правило текущего
// сайта) и страница настроек (списки). Поэтому пределы и нормализация проверяются в одном месте, а не в
// каждом окне по-своему.
//
// Запись в хранилище — дело вызывающего: он читает настройку, применяет правку и пишет целиком
// (`writeAutoHandoffSettings`). Правка, которая ничего не меняет, возвращает `unchanged`: писать не нужно.

import {
  MAX_OWN_STOP_LIST_ENTRIES,
  MAX_SITE_RULES,
  normalizeStopListEntry,
  siteKeyOfHost,
  type AutoHandoffSettings,
  type SiteRule,
} from "./auto-handoff-settings.ts";

/**
 * Что человек выбрал для сайта: одно из правил или `default` — «своего правила нет», сайт живёт по общей
 * настройке (`enabled` и `unknown_site_rule`).
 */
export type SiteRuleChoice = SiteRule | "default";

/** Почему правку нельзя применить. Слова — для текста ошибки в окне и для тестов. */
export type SettingsEditRejection =
  /** Ключ сайта не разобрать (не хост). */
  | "bad_site"
  /** Своих правил сайтов уже `MAX_SITE_RULES`: новое правило не поместится. */
  | "too_many_site_rules"
  /** Запись стоп-листа не разобрать (`normalizeStopListEntry` вернул `null`). */
  | "bad_stop_list_entry"
  /** Свой стоп-лист уже полон (`MAX_OWN_STOP_LIST_ENTRIES`). */
  | "stop_list_full";

/** Итог правки. `changed` несёт новую настройку; `unchanged` — писать нечего; `rejected` — причина. */
export type SettingsEdit =
  | { readonly kind: "changed"; readonly settings: AutoHandoffSettings }
  | { readonly kind: "unchanged" }
  | { readonly kind: "rejected"; readonly reason: SettingsEditRejection };

const UNCHANGED: SettingsEdit = { kind: "unchanged" };
const reject = (reason: SettingsEditRejection): SettingsEdit => ({ kind: "rejected", reason });
const changed = (settings: AutoHandoffSettings): SettingsEdit => ({ kind: "changed", settings });

/**
 * Поставить правило сайта или снять своё (`default`). Ключ приводится `siteKeyOfHost` (`www.` и регистр не
 * мешают). Своё правило сайта ставится только на этот ключ: правила родителей не трогаются.
 */
export function withSiteRuleChoice(settings: AutoHandoffSettings, site: string, choice: SiteRuleChoice): SettingsEdit {
  const siteKey = siteKeyOfHost(site);
  if (siteKey === null) return reject("bad_site");
  const current = Object.hasOwn(settings.site_rules, siteKey) ? settings.site_rules[siteKey] : undefined;
  if (choice === "default") {
    if (current === undefined) return UNCHANGED;
    // Копия без ключа: исходная настройка неизменяема (её могут держать другие читатели).
    const { [siteKey]: _removed, ...rest } = settings.site_rules;
    return changed({ ...settings, site_rules: rest });
  }
  if (current === choice) return UNCHANGED;
  // Новый ключ сверх предела не пишем: при чтении он всё равно отбросился бы, и человек решил бы, что правило есть.
  if (current === undefined && Object.keys(settings.site_rules).length >= MAX_SITE_RULES) return reject("too_many_site_rules");
  return changed({ ...settings, site_rules: { ...settings.site_rules, [siteKey]: choice } });
}

/**
 * Добавить запись в свой стоп-лист. Запись приводится к единому виду (`https://`, `www.`, регистр и
 * завершающий `/` не мешают); такая уже есть — `unchanged`.
 */
export function withOwnStopListEntryAdded(settings: AutoHandoffSettings, rawEntry: string): SettingsEdit {
  const entry = normalizeStopListEntry(rawEntry);
  if (entry === null) return reject("bad_stop_list_entry");
  if (settings.own_stop_list.includes(entry)) return UNCHANGED;
  if (settings.own_stop_list.length >= MAX_OWN_STOP_LIST_ENTRIES) return reject("stop_list_full");
  return changed({ ...settings, own_stop_list: [...settings.own_stop_list, entry] });
}

/** Убрать запись своего стоп-листа (в любой записи, как её ввёл бы человек). Нет такой — `unchanged`. */
export function withOwnStopListEntryRemoved(settings: AutoHandoffSettings, rawEntry: string): SettingsEdit {
  const entry = normalizeStopListEntry(rawEntry);
  if (entry === null || !settings.own_stop_list.includes(entry)) return UNCHANGED;
  return changed({ ...settings, own_stop_list: settings.own_stop_list.filter((existing) => existing !== entry) });
}
