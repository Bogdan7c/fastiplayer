// Отпечаток сериала для памяти выбора (решение 281): по хранилищу браузера нельзя прочитать, какие сериалы
// смотрел человек.
//
// Отпечаток = SHA-256 от «метка формы + ключ сайта + ключ сериала адаптера», строчные шестнадцатеричные знаки.
// Честная оговорка (её же пишет `manifest/PERMISSIONS.md`): отпечаток необратим, но тот, кто уже знает сайт
// и сериал, может посчитать его сам и проверить догадку. Это защита от «прочитать историю», а не тайна.
//
// Ключ сайта — хост вкладки строчными без `www.` и порта (`siteKeyOfHost`), как у правил режима 2: разные
// адреса одного сайта дают один сериал.

import { parseWebAddress } from "../auto-handoff/stop-list.ts";
import type { SeriesFingerprint } from "../../shared/choice-memory-contract.ts";

/**
 * Метка формы отпечатка. Меняется только вместе со способом счёта: старые записи тогда просто не находятся
 * и со временем вытесняются, а не путаются с новыми.
 */
export const SERIES_FINGERPRINT_LABEL = "fastiplayer-choice-memory:v1";

/** Счёт SHA-256 (в фоне — `crypto.subtle`; в тестах можно подставить свой). */
export type Sha256Digest = (bytes: Uint8Array<ArrayBuffer>) => Promise<ArrayBuffer>;

export const browserSha256: Sha256Digest = (bytes) => crypto.subtle.digest("SHA-256", bytes);

/** Ключ сайта вкладки; `null` — это не веб-страница (служебная страница, файл), памяти у неё нет. */
export function pageSiteKeyOf(tabUrl: string | undefined): string | null {
  if (tabUrl === undefined) return null;
  return parseWebAddress(tabUrl)?.siteKey ?? null;
}

/** Отпечаток сериала `seriesKey` на сайте `siteKey`. */
export async function seriesFingerprintOf(siteKey: string, seriesKey: string, digest: Sha256Digest = browserSha256): Promise<SeriesFingerprint> {
  // Перевод строки не встречается ни в ключе сайта (только буквы, цифры, точки, дефис), ни в метке: границы
  // частей однозначны, «a» + «b\nc» не совпадёт с «a\nb» + «c».
  const text = `${SERIES_FINGERPRINT_LABEL}\n${siteKey}\n${seriesKey}`;
  const hash = new Uint8Array(await digest(new TextEncoder().encode(text)));
  return Array.from(hash, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
