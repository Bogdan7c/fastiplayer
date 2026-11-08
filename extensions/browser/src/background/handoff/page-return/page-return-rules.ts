// Правило ухода страницы (сессия 11b2, договор 11b1): «сирота» или прежний `tab_gone`. Чистая функция.
//
// Сирота — связь с плеером остаётся, плееру уходит `page_left{reason}`, и зритель потом может вернуть
// страницу play в окне. Только если вернуть её вообще можно:
// - плеер договорился на протокол 1.4 и новее (иначе `page_left` для него нарушение);
// - страница ушла сама: вкладку закрыли или в ней открыли другой адрес. Сломавшийся плеер сайта (решение 237)
//   — не уход: та же страница снова сломалась бы, и плеер честно досматривает скачанное, как раньше;
// - адрес http/https: страницы браузера и файлы с диска заново не открываем;
// - фон знает, какой фильм искать (реклама на экране — не знает) и в каком окне жила страница.
// Остальное — `tab_gone`, плеер ведёт себя как до 1.4.

import { pageSessionIntroducedIn, versionIncludes, type ProtocolVersion, type TabGoneReason } from "../../../protocol/index.ts";
import type { PageReturnTarget } from "../../../shared/handoff-contract.ts";
import type { ReturnablePage } from "./page-return-memory.ts";

/** Как кончить уход страницы. */
export type PageLeaveDecision =
  | { readonly kind: "orphan"; readonly page: ReturnablePage; readonly target: PageReturnTarget }
  | { readonly kind: "tab_gone" };

/** Что сеанс знает в момент ухода. */
export interface PageLeaveFacts {
  readonly reason: TabGoneReason;
  readonly agreedProtocol: ProtocolVersion;
  readonly page: ReturnablePage | undefined;
  readonly target: PageReturnTarget | undefined;
}

/** Причины ухода, после которых страницу можно вернуть. */
const RETURNABLE_REASONS: readonly TabGoneReason[] = ["closed", "navigated_away"];

/** Схемы адреса, которые фон согласен открыть заново. */
const RETURNABLE_SCHEMES: readonly string[] = ["http:", "https:"];

const TAB_GONE: PageLeaveDecision = { kind: "tab_gone" };

export function decidePageLeave(facts: PageLeaveFacts): PageLeaveDecision {
  if (!versionIncludes(facts.agreedProtocol, pageSessionIntroducedIn("page_left"))) return TAB_GONE;
  if (!RETURNABLE_REASONS.includes(facts.reason)) return TAB_GONE;
  if (facts.page === undefined || facts.target === undefined || !isReturnableAddress(facts.page.url)) return TAB_GONE;
  return { kind: "orphan", page: facts.page, target: facts.target };
}

function isReturnableAddress(url: string): boolean {
  try {
    return RETURNABLE_SCHEMES.includes(new URL(url).protocol);
  } catch {
    return false;
  }
}
