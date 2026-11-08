// Политика сайта для режима 2 «автозапуск» (сессия 12a): можно ли вообще передавать видео с этой вкладки.
// Чистая функция «адрес вкладки + настройка → решение», без браузера и времени.
//
// Порядок проверок — часть правила (первое сработавшее решает), поэтому он записан одним списком
// `POLICY_STEPS`: переставить правило — переставить строку.
// 1. не http(s)                      -> пропустить (служебные страницы, файлы);
// 2. свой стоп-лист человека         -> пропустить (слово человека сильнее его же правила сайта);
// 3. своё правило сайта              -> always: передавать, never: пропустить, ask: пропустить (спросить — сессия 12b);
//    правило ищется от точного хоста вверх по родителям: правило для example.com действует на video.example.com;
// 4. встроенный стоп-лист            -> пропустить (одобрен владельцем, но слабее своего правила `always`);
// 5. режим включён                   -> `unknown_site_rule`: always — передавать, ask — пропустить (до 12b);
// 6. режим выключен                  -> пропустить.

import type { AutoHandoffSettings, SiteRule } from "../../shared/auto-handoff-settings.ts";
import { addressIsInStopList, builtInStopListEntries, parseWebAddress, type WebAddress } from "./stop-list.ts";

/** Почему сайт пропущен. Слова — для журнала разработчика и тестов. */
export type SitePolicySkipReason =
  | "not_web_page"
  | "own_stop_list"
  | "site_never"
  /** Правило `ask`: спросить человека на заглушке умеет только сессия 12b; пока ведёт себя как «не сейчас». */
  | "site_ask_pending_12b"
  | "built_in_stop_list"
  | "mode_off";

export type SitePolicy = { readonly kind: "hand_off" } | { readonly kind: "skip"; readonly reason: SitePolicySkipReason };

const HAND_OFF: SitePolicy = { kind: "hand_off" };
const skip = (reason: SitePolicySkipReason): SitePolicy => ({ kind: "skip", reason });

/** Шаг политики: решение или `undefined` («это не моё дело, дальше»). */
type PolicyStep = (address: WebAddress, settings: AutoHandoffSettings) => SitePolicy | undefined;

/**
 * Своё правило сайта: от точного хоста вверх по родителям (`a.b.example.com` -> `b.example.com` -> `example.com`).
 * До голого домена верхнего уровня не поднимаемся: правило на `com` бессмысленно.
 */
export function siteRuleFor(siteKey: string, rules: Readonly<Record<string, SiteRule>>): SiteRule | undefined {
  let candidate = siteKey;
  for (;;) {
    const rule = Object.hasOwn(rules, candidate) ? rules[candidate] : undefined;
    if (rule !== undefined) return rule;
    const dot = candidate.indexOf(".");
    if (dot < 0 || candidate.indexOf(".", dot + 1) < 0) return undefined;
    candidate = candidate.slice(dot + 1);
  }
}

const ownStopListStep: PolicyStep = (address, settings) => (addressIsInStopList(address, settings.own_stop_list) ? skip("own_stop_list") : undefined);

const ownSiteRuleStep: PolicyStep = (address, settings) => {
  switch (siteRuleFor(address.siteKey, settings.site_rules)) {
    case "always":
      return HAND_OFF;
    case "never":
      return skip("site_never");
    case "ask":
      return skip("site_ask_pending_12b");
    case undefined:
      return undefined;
  }
};

const builtInStopListStep: PolicyStep = (address) => (addressIsInStopList(address, builtInStopListEntries()) ? skip("built_in_stop_list") : undefined);

const generalModeStep: PolicyStep = (_address, settings) => {
  if (!settings.enabled) return skip("mode_off");
  return settings.unknown_site_rule === "always" ? HAND_OFF : skip("site_ask_pending_12b");
};

/** Порядок шагов; последний всегда отвечает. */
const POLICY_STEPS: readonly PolicyStep[] = [ownStopListStep, ownSiteRuleStep, builtInStopListStep, generalModeStep];

/** Решение по сайту для адреса вкладки. `tabUrl` — как его отдал браузер (может быть не адресом вовсе). */
export function sitePolicyOf(tabUrl: string, settings: AutoHandoffSettings): SitePolicy {
  const address = parseWebAddress(tabUrl);
  if (address === undefined) return skip("not_web_page");
  for (const step of POLICY_STEPS) {
    const decision = step(address, settings);
    if (decision !== undefined) return decision;
  }
  return skip("mode_off");
}
