// Чистое правило памяти выбора (сессия 14b2, решения 284, 287): что сделать с запомненной озвучкой или
// качеством на текущей серии. Без часов, хранилища и браузера — только факты на входе и шаг на выходе.
//
// Правила:
// - на странице уже выбран запомненный пункт — делать нечего;
// - запомненного пункта на странице нет (у этой серии другие озвучки) — ничего не трогаем;
// - озвучку ставим только у начала серии: на многих сайтах (Ashdi) смена озвучки начинает серию заново,
//   и зритель с двадцатой минуты оказался бы в начале (решение 287);
// - качество ставим в любой момент серии: сайт меняет поток с той же секунды (выбор владельца, решение 287);
// - когда серия началась, пока неизвестно (плеер ещё не прислал часы), — ждать, а не гадать.

import type { VariantMenu } from "../../protocol/index.ts";
import type { PageVariantKind } from "../../shared/variant-watch-contract.ts";

/** Выбранный сейчас пункт страницы (из меню, уже отправленного плееру). */
export interface SeenSelection {
  readonly id: string;
  readonly label: string;
  /** Пункт — «Auto» сайта: не выбор человека. */
  readonly automatic: boolean;
}

/** Где мы в серии с точки зрения памяти. */
export type EpisodeTiming =
  /** Позиция ещё неизвестна: плеер не прислал часы. */
  | "position_unknown"
  /** У начала серии: можно менять и то, что начинает серию заново. */
  | "near_start"
  /** Серия уже идёт: менять можно только то, что не сбрасывает позицию. */
  | "under_way";

/** Когда вариант этого вида можно менять. */
export type VariantSwitchRule = "episode_start_only" | "any_time";

/** Озвучка сбрасывает позицию на многих сайтах, качество — нет (решение 287). */
export const VARIANT_SWITCH_RULES: Readonly<Record<PageVariantKind, VariantSwitchRule>> = {
  audio: "episode_start_only",
  quality: "any_time",
};

/** Почему память для этого вида на этой серии закончила, ничего не нажав. */
export type SettledReason = "nothing_remembered" | "already_on_page" | "not_on_page" | "episode_under_way";

/** Шаг памяти для одного вида варианта. */
export type RememberedVariantStep =
  | { readonly kind: "select"; readonly variantId: string }
  | { readonly kind: "settled"; readonly reason: SettledReason }
  /** Пока рано решать: меню без выбранного пункта или позиция неизвестна. */
  | { readonly kind: "wait" };

export interface RememberedVariantFacts {
  readonly variantKind: PageVariantKind;
  /** Запомненная подпись; `undefined` — этот вид ещё не выбирали. */
  readonly rememberedLabel: string | undefined;
  /** Выбранный сейчас пункт; `undefined` — страница ещё не показала выбор этого вида. */
  readonly selected: SeenSelection | undefined;
  readonly menu: VariantMenu;
  readonly timing: EpisodeTiming;
}

export function planRememberedVariant(facts: RememberedVariantFacts): RememberedVariantStep {
  if (facts.rememberedLabel === undefined) return { kind: "settled", reason: "nothing_remembered" };
  if (facts.selected === undefined) return { kind: "wait" };
  if (facts.selected.label === facts.rememberedLabel) return { kind: "settled", reason: "already_on_page" };
  const target = facts.menu.options.find((option) => option.label === facts.rememberedLabel);
  if (target === undefined) return { kind: "settled", reason: "not_on_page" };
  if (VARIANT_SWITCH_RULES[facts.variantKind] === "episode_start_only") {
    if (facts.timing === "position_unknown") return { kind: "wait" };
    if (facts.timing === "under_way") return { kind: "settled", reason: "episode_under_way" };
  }
  return { kind: "select", variantId: target.id };
}

/** Выбранный пункт меню вида (`undefined` — не выбран ни один). */
export function selectionOf(menu: VariantMenu, automaticIds: ReadonlySet<string>): SeenSelection | undefined {
  const selected = menu.options.find((option) => option.selection === "selected");
  return selected === undefined ? undefined : { id: selected.id, label: selected.label, automatic: automaticIds.has(selected.id) };
}
