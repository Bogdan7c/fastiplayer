// Реестр адаптеров изолированного мира фрейма: всё, что читает DOM страницы и нажимает её кнопки.
//
// Сюда попадает код, который собирается в контент-скрипт (`src/content/`). Адаптеры мира страницы
// (MAIN) живут в соседнем `page-world.ts`, чтобы ни один бандл не тянул чужой код.
// Новый сайт — папка в `sites/` и строка в `ISOLATED_WORLD_SITES`.

import type { AdOrgans, AdPageView } from "../capabilities/ad-organs/contract.ts";
import type { VariantAdapter, VariantHands } from "../capabilities/variants/contract.ts";
import type { SiteAdapterIdentity } from "../site-identity.ts";
import { BENCH_SERIES_SITE } from "../sites/bench-series/identity.ts";
import { createBenchSeriesVariants } from "../sites/bench-series/variants.ts";
import { createImaOrgans } from "../sites/ima/ad-organs.ts";
import { IMA_SITE } from "../sites/ima/identity.ts";
import { createPlayerJsOrgans } from "../sites/playerjs/ad-organs.ts";
import { PLAYERJS_SITE } from "../sites/playerjs/identity.ts";
import { createYouTubeOrgans } from "../sites/youtube/ad-organs.ts";
import { YOUTUBE_SITE } from "../sites/youtube/identity.ts";

export type {
  AdElement,
  AdFramePlace,
  AdOrgans,
  AdPageView,
  ElementRect,
  PressSkipOutcome,
} from "../capabilities/ad-organs/contract.ts";

/** Что адаптер сайта умеет в изолированном мире. Способности необязательны: у сайта может быть любая их часть. */
export interface IsolatedWorldSiteAdapter {
  readonly identity: SiteAdapterIdentity;
  /** Органы рекламы (сессия 09d). */
  readonly createAdOrgans?: (view: AdPageView) => AdOrgans;
  /** Варианты страницы: озвучки, качества, следующая серия (сессия 14b). */
  readonly createVariants?: (hands: VariantHands) => VariantAdapter;
}

/** Адаптер вариантов вместе с паспортом своего сайта: паспорт нужен руке-функции и журналу разработчика. */
export interface SiteVariantAdapter {
  readonly identity: SiteAdapterIdentity;
  readonly adapter: VariantAdapter;
}

/** Порядок важен: при нажатии пропуска первым спрашивается первый подходящий. Порядок 09d сохранён. */
export const ISOLATED_WORLD_SITES: readonly IsolatedWorldSiteAdapter[] = [
  { identity: YOUTUBE_SITE, createAdOrgans: createYouTubeOrgans },
  { identity: IMA_SITE, createAdOrgans: createImaOrgans },
  { identity: PLAYERJS_SITE, createAdOrgans: createPlayerJsOrgans },
  { identity: BENCH_SERIES_SITE, createVariants: createBenchSeriesVariants },
];

/** Читатели рекламы всех сайтов для документа, который показывает `view`. */
export function createAllAdOrgans(view: AdPageView): readonly AdOrgans[] {
  return ISOLATED_WORLD_SITES.flatMap((site) => (site.createAdOrgans === undefined ? [] : [site.createAdOrgans(view)]));
}

/**
 * Адаптеры вариантов всех сайтов. `handsFor` даёт каждому его руки: рука-функция знает имя своего сайта,
 * поэтому адаптер не может позвать белый список чужого.
 */
export function createAllVariantAdapters(handsFor: (identity: SiteAdapterIdentity) => VariantHands): readonly SiteVariantAdapter[] {
  return ISOLATED_WORLD_SITES.flatMap((site) =>
    site.createVariants === undefined ? [] : [{ identity: site.identity, adapter: site.createVariants(handsFor(site.identity)) }],
  );
}
