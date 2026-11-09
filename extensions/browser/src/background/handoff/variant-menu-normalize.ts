// Описание меню страницы → меню протокола (сессия 14b, инвариант 1 «страница — враг»).
//
// Адаптер приносит сырой текст страницы. Протокол строг и негодное меню не чинит, а рвёт сеанс
// (`page_message_rejected`), поэтому всё приводится к его правилам ДО отправки:
// - id — непрозрачный ключ 1–64 байта алфавита base64url (`isValidVariantId`); негодный — пункт выбрасывается;
// - повтор id — второй пункт выбрасывается;
// - подпись чистится и режется до 256 байт (`sanitizeVariantLabel`); пустая — пункт выбрасывается;
// - пунктов не больше 256; «выбран» — не больше одного (первый).

import {
  isValidVariantId,
  MAX_VARIANT_OPTIONS,
  sanitizeVariantLabel,
  type NextEpisodeAvailability,
  type VariantMenu,
  type VariantOption,
} from "../../protocol/index.ts";
import type { PageVariantDescription, PageVariantKind, PageVariantOption } from "../../shared/variant-watch-contract.ts";

/** Меню обоих видов и «есть ли следующая серия» — то, что уходит плееру. */
export interface NormalizedVariantMenus {
  readonly audio: VariantMenu;
  readonly quality: VariantMenu;
  readonly nextEpisode: NextEpisodeAvailability;
}

function normalizeMenu(kind: PageVariantKind, options: readonly PageVariantOption[]): VariantMenu {
  const normalized: VariantOption[] = [];
  const seenIds = new Set<string>();
  let selectedSeen = false;
  for (const option of options) {
    if (normalized.length >= MAX_VARIANT_OPTIONS) break;
    if (seenIds.has(option.id) || !isValidVariantId(option.id)) continue;
    const label = sanitizeVariantLabel(option.label);
    if (label === "") continue;
    seenIds.add(option.id);
    const selected: boolean = option.selected && !selectedSeen;
    selectedSeen ||= selected;
    normalized.push({ id: option.id, label, selection: selected ? "selected" : "available" });
  }
  return { kind, options: normalized };
}

/** Меню для плеера; `null` (меню нет) — оба меню пустые и следующей серии нет. */
export function normalizeVariantMenus(description: PageVariantDescription | null): NormalizedVariantMenus {
  return {
    audio: normalizeMenu("audio", description?.audio ?? []),
    quality: normalizeMenu("quality", description?.quality ?? []),
    nextEpisode: description?.nextEpisode === true ? "available" : "unavailable",
  };
}
