// Реестр адаптеров мира страницы (MAIN): всё, что работает внутри скрипта страницы (`src/page/`).
//
// Сейчас здесь именование документа субтитров по адресу (сессия 13b2, решение 276). Адаптеры сайтов идут
// раньше общего читателя: у YouTube язык и «авто» читаются иначе, чем `?lang=`.
// Новый сайт — папка в `sites/` и строка в `PAGE_WORLD_SITES`.

import type { PageSubtitleNaming } from "../../shared/page-subtitle-contract.ts";
import { parseAddressParts } from "../capabilities/subtitle-naming/address-parts.ts";
import type { SubtitleNamingAdapter } from "../capabilities/subtitle-naming/contract.ts";
import { genericAddressAdapter } from "../capabilities/subtitle-naming/generic-address.ts";
import type { SiteAdapterIdentity } from "../site-identity.ts";
import { YOUTUBE_SITE } from "../sites/youtube/identity.ts";
import { youtubeTimedtextAdapter } from "../sites/youtube/timedtext-naming.ts";

/** Что адаптер сайта умеет в мире страницы. */
export interface PageWorldSiteAdapter {
  readonly identity: SiteAdapterIdentity;
  /** Именование документа субтитров по адресу (сессия 13b2). */
  readonly subtitleNaming?: SubtitleNamingAdapter;
}

export const PAGE_WORLD_SITES: readonly PageWorldSiteAdapter[] = [
  { identity: YOUTUBE_SITE, subtitleNaming: youtubeTimedtextAdapter },
];

/** Порядок важен: первый адаптер, узнавший адрес, называет дорожку. Общий читатель — последним. */
export const SUBTITLE_NAMING_ADAPTERS: readonly SubtitleNamingAdapter[] = [
  ...PAGE_WORLD_SITES.flatMap((site) => (site.subtitleNaming === undefined ? [] : [site.subtitleNaming])),
  genericAddressAdapter,
];

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

export { parseAddressParts, queryValueOf, type AddressParts } from "../capabilities/subtitle-naming/address-parts.ts";
export { languageOrNull, labelOrNull } from "../capabilities/subtitle-naming/language-tag.ts";
export type { SubtitleNamingAdapter } from "../capabilities/subtitle-naming/contract.ts";
