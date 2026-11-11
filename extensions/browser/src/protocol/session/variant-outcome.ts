// Исход выбора на странице (протокол 1.8, сессия 15c2, решение 301). Зеркало
// session/variant_outcome.rs.
//
// Страница -> плеер `variant_outcome{request, outcome}`: чем кончился выбор ЗРИТЕЛЯ (озвучка, качество,
// субтитры или Next из окна плеера). Выбор памяти выбора экстеншена не сообщается.
// `no_such_episode` значит ТОЛЬКО «у выбранной озвучки нет текущей серии» (решение 295); всё остальное
// («пункта нет в меню», «нет следующей серии», «кнопки не сработали», «адаптер упал») - `failed`.
// Сообщение не привязано к поколению потока. Форма строгая: лишнее поле и незнакомая метка - отказ.

import { type Codec, stringEnum, taggedUnion } from "../schema.ts";
import { variantIdCodec } from "../text.ts";
import { type VariantKind, variantKindCodec } from "./variants.ts";

/** О каком запросе зрителя сообщается исход: выбор пункта меню или «следующая серия». */
export type VariantRequest =
  | { readonly kind: "select_variant"; readonly menu: VariantKind; readonly variant_id: string }
  | { readonly kind: "next_episode" };

/** Чем кончился запрос на странице. */
export type VariantOutcome = "done" | "no_such_episode" | "person_only" | "failed";

/** Все исходы - один список для разбора. */
export const VARIANT_OUTCOMES: readonly VariantOutcome[] = ["done", "no_such_episode", "person_only", "failed"];

export const variantRequestCodec: Codec<VariantRequest> = taggedUnion<VariantRequest, "kind">("kind", {
  select_variant: { menu: variantKindCodec, variant_id: variantIdCodec },
  next_episode: {},
});

export const variantOutcomeCodec: Codec<VariantOutcome> = stringEnum<VariantOutcome>(VARIANT_OUTCOMES);
