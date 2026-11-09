// Именование документа субтитров по адресу (сессия 13b2, решение 276).
//
// Адаптеры сайтов идут раньше общего читателя: у YouTube язык и «авто» читаются иначе, чем `?lang=`.
// Новый сайт — один файл-адаптер и строка в `SUBTITLE_NAMING_ADAPTERS`.

import type { PageSubtitleNaming } from "../../shared/page-subtitle-contract.ts";
import { parseAddressParts } from "./address-parts.ts";
import type { SubtitleNamingAdapter } from "./contract.ts";
import { genericAddressAdapter } from "./generic-address.ts";
import { youtubeTimedtextAdapter } from "./youtube-timedtext.ts";

/** Порядок важен: первый адаптер, узнавший адрес, называет дорожку. Общий читатель — последним. */
export const SUBTITLE_NAMING_ADAPTERS: readonly SubtitleNamingAdapter[] = [youtubeTimedtextAdapter, genericAddressAdapter];

/** Дорожка, о которой по адресу ничего не известно. */
const UNNAMED: PageSubtitleNaming = { language: null, label: null, auto_generated: false };

/** Как назвать дорожку документа, скачанного с `address`. Не бросает: неразборчивый адрес — «не названа». */
export function nameSubtitleDocumentByAddress(address: string): PageSubtitleNaming {
  const parts = parseAddressParts(address);
  if (parts === null) return UNNAMED;
  for (const adapter of SUBTITLE_NAMING_ADAPTERS) {
    const naming = adapter.nameByAddress(parts);
    if (naming !== null) return naming;
  }
  return UNNAMED;
}

export { parseAddressParts, queryValueOf, type AddressParts } from "./address-parts.ts";
export { languageOrNull, labelOrNull } from "./language-tag.ts";
export type { SubtitleNamingAdapter } from "./contract.ts";
