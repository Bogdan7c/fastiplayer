// Описание меню страницы → меню протокола (сессия 14b, инвариант 1 «страница — враг»).
//
// Адаптер приносит сырой текст страницы. Протокол строг и негодное меню не чинит, а рвёт сеанс
// (`page_message_rejected`), поэтому всё приводится к его правилам ДО отправки:
// - id — непрозрачный ключ 1–64 байта алфавита base64url (`isValidVariantId`); негодный — пункт выбрасывается;
// - повтор id — второй пункт выбрасывается;
// - подпись чистится и режется до 256 байт (`sanitizeVariantLabel`); пустая — пункт выбрасывается;
// - пунктов не больше 256; «выбран» — не больше одного (первый).
// Пометка «Auto» сайта (`automatic`, 14b2) плееру не уходит: её читает только память выбора фона.

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
  /** id оставленных пунктов, которые адаптер пометил как автоматический выбор сайта. Только для фона. */
  readonly automatic: Readonly<Record<PageVariantKind, ReadonlySet<string>>>;
}

interface NormalizedMenu {
  readonly menu: VariantMenu;
  readonly automatic: ReadonlySet<string>;
}

function normalizeMenu(kind: PageVariantKind, options: readonly PageVariantOption[]): NormalizedMenu {
  const normalized: VariantOption[] = [];
  const automatic = new Set<string>();
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
    if (option.automatic === true) automatic.add(option.id);
  }
  return { menu: { kind, options: normalized }, automatic };
}

/** Меню для плеера; `null` (меню нет) — оба меню пустые и следующей серии нет. */
export function normalizeVariantMenus(description: PageVariantDescription | null): NormalizedVariantMenus {
  const audio = normalizeMenu("audio", description?.audio ?? []);
  const quality = normalizeMenu("quality", description?.quality ?? []);
  return {
    audio: audio.menu,
    quality: quality.menu,
    nextEpisode: description?.nextEpisode === true ? "available" : "unavailable",
    automatic: { audio: audio.automatic, quality: quality.automatic },
  };
}
