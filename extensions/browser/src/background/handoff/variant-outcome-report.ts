// Исход выбора зрителя -> слово протокола 1.8 (`variant_outcome`, сессия 15c2, решение 301).
//
// Адаптер знает пять исходов, протокол - четыре: «такого пункта уже нет» (`no_such_variant`) зрителю в
// окне плеера ничем не отличается от «не сработало», поэтому уходит как `failed`. Отдельное слово
// `no_such_episode` - только «у выбранной озвучки нет текущей серии» (решение 295).

import type { VariantOutcome } from "../../protocol/index.ts";
import type { VariantActionOutcome } from "../../shared/variant-watch-contract.ts";

/** Исход адаптера -> исход протокола. Таблица полная по `VariantActionOutcome`: новый исход без слова не соберётся. */
const PROTOCOL_OUTCOME_OF: Readonly<Record<VariantActionOutcome, VariantOutcome>> = {
  done: "done",
  no_such_episode: "no_such_episode",
  person_only: "person_only",
  no_such_variant: "failed",
  failed: "failed",
};

export function protocolOutcomeOf(outcome: VariantActionOutcome): VariantOutcome {
  return PROTOCOL_OUTCOME_OF[outcome];
}
