// Модель строки «правило автозапуска для текущего сайта» в попапе (сессия 12b). Чистая логика без DOM и
// хранилища: адрес вкладки + настройка -> что показать. Рисует её `site-rule-row.ts`.

import { builtInStopListEntries, addressIsInStopList, parseWebAddress } from "../background/auto-handoff/stop-list.ts";
import { siteRuleEntryFor } from "../background/auto-handoff/site-policy.ts";
import type { AutoHandoffSettings } from "../shared/auto-handoff-settings.ts";
import type { SiteRuleChoice } from "../shared/auto-handoff-settings-edits.ts";

/** Порядок и значения вариантов выбора. Значения — контракт для стенда (`select.site-rule-choice`). */
export const SITE_RULE_CHOICES: readonly SiteRuleChoice[] = ["default", "always", "never", "ask"];

/** Подписи вариантов с собственным правилом (английский, пользовательский текст). */
export const SITE_RULE_CHOICE_LABELS: Record<Exclude<SiteRuleChoice, "default">, string> = {
  always: "Always",
  never: "Never",
  ask: "Ask first",
};

/** Заметка: адрес в своём стоп-листе. */
export const OWN_STOP_LIST_NOTE = "This site is in your stop list.";
/** Заметка: адрес во встроенном стоп-листе, а правило сайта не «Always». */
export const BUILT_IN_STOP_LIST_NOTE = "Built-in stop list: auto play stays off here unless you choose Always.";

/** Что показывает строка. */
export interface SiteRuleRowModel {
  /** Ключ, который строка правит: ключ найденного правила (возможно, родителя) или точный ключ сайта вкладки. */
  readonly ruleKey: string;
  /** Что выбрано сейчас: своё правило или `default`. */
  readonly choice: SiteRuleChoice;
  /** Подпись варианта «по умолчанию»: что он означает при текущей общей настройке. */
  readonly defaultLabel: string;
  /** Короткое пояснение про стоп-лист или `undefined`. */
  readonly note: string | undefined;
}

/** Что значит «по умолчанию» при текущей общей настройке. */
export function describeDefaultChoice(settings: AutoHandoffSettings): string {
  if (!settings.enabled) return "Default (off)";
  return settings.unknown_site_rule === "always" ? "Default (always)" : "Default (ask first)";
}

/**
 * Модель строки для адреса вкладки. `undefined` — строку не показываем (адрес не http(s) или его нет).
 * Заметка про встроенный стоп-лист не нужна, если правило «Always»: оно стоп-лист перекрывает.
 * Свой стоп-лист сильнее любого правила, поэтому его заметка показывается всегда.
 */
export function buildSiteRuleRowModel(tabUrl: string | undefined, settings: AutoHandoffSettings): SiteRuleRowModel | undefined {
  if (tabUrl === undefined) return undefined;
  const address = parseWebAddress(tabUrl);
  if (address === undefined) return undefined;
  const entry = siteRuleEntryFor(address.siteKey, settings.site_rules);
  const choice: SiteRuleChoice = entry?.rule ?? "default";
  let note: string | undefined;
  if (addressIsInStopList(address, settings.own_stop_list)) {
    note = OWN_STOP_LIST_NOTE;
  } else if (choice !== "always" && addressIsInStopList(address, builtInStopListEntries())) {
    note = BUILT_IN_STOP_LIST_NOTE;
  }
  return { ruleKey: entry?.siteKey ?? address.siteKey, choice, defaultLabel: describeDefaultChoice(settings), note };
}
