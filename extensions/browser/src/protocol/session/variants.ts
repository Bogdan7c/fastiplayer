// Меню вариантов от адаптера сайта: озвучки, качество, субтитры. Зеркало session/variants.rs.

import { ProtocolFailure } from "../errors.ts";
import { MAX_VARIANT_OPTIONS } from "../limits.ts";
import { type Codec, listOf, stringEnum, struct } from "../schema.ts";
import { variantIdCodec, variantLabelCodec } from "../text.ts";

/** Что переключает меню. */
export type VariantKind = "audio" | "quality" | "subtitles";

/** Выбран ли вариант сейчас. */
export type VariantSelection = "selected" | "available";

/** Один вариант меню. */
export interface VariantOption {
  /** ID придумывает адаптер сайта; плеер вернёт его в `select_variant`. */
  readonly id: string;
  readonly label: string;
  readonly selection: VariantSelection;
}

/** Меню вариантов одного вида. */
export interface VariantMenu {
  readonly kind: VariantKind;
  readonly options: readonly VariantOption[];
}

export const variantKindCodec: Codec<VariantKind> = stringEnum<VariantKind>(["audio", "quality", "subtitles"]);

const variantOptionCodec: Codec<VariantOption> = struct<VariantOption>({
  id: variantIdCodec,
  label: variantLabelCodec,
  selection: stringEnum<VariantSelection>(["selected", "available"]),
});

export const variantMenuCodec: Codec<VariantMenu> = struct<VariantMenu>({
  kind: variantKindCodec,
  options: listOf(variantOptionCodec),
});

/**
 * Проверки меню между полями: не больше 256 вариантов, ID уникальны, выбран не
 * больше одного.
 */
export function validateVariantMenu(menu: VariantMenu): void {
  if (menu.options.length > MAX_VARIANT_OPTIONS) {
    throw new ProtocolFailure("too_many_items").prependPath("menu.options");
  }
  if (new Set(menu.options.map((option) => option.id)).size !== menu.options.length) {
    throw new ProtocolFailure("duplicate_id").prependPath("menu.options");
  }
  const selectedCount = menu.options.filter((option) => option.selection === "selected").length;
  if (selectedCount > 1) {
    throw new ProtocolFailure("conflicting_selection").prependPath("menu.options");
  }
}
