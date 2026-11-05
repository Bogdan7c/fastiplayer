// Реклама на экране: страница говорит плееру только числа и метки. Зеркало session/ad_state.rs.
//
// Текста со страницы здесь нет (инвариант 1): подписи плашки пишет плеер.

import { ProtocolFailure } from "../errors.ts";
import { type Codec, taggedUnion } from "../schema.ts";
import { safeInteger } from "../numbers.ts";

/** Сколько реклам может быть в одном блоке. */
export const MAX_ADS_IN_POD = 64;

/** Самый длинный отсчёт до пропуска, секунды. */
export const MAX_AD_COUNTDOWN_SECONDS = 3_600;

/** Какая реклама из блока идёт. */
export type AdPod =
  | { readonly kind: "unknown" }
  | { readonly kind: "known"; readonly index: number; readonly count: number };

/**
 * Можно ли пропустить идущую рекламу.
 * `available` — кнопка показана и принимает наш клик; `on_page` — кнопка показана, но нажать её
 * может только человек на странице, плеер лишь подсказывает (YouTube, решения 207, 208).
 */
export type AdSkip =
  | { readonly kind: "unavailable" }
  | { readonly kind: "countdown"; readonly seconds: number }
  | { readonly kind: "available" }
  | { readonly kind: "on_page" };

/** Что сейчас с рекламой на странице. */
export type PageAdState =
  | { readonly kind: "none" }
  | { readonly kind: "showing"; readonly pod: AdPod; readonly skip: AdSkip };

const adPodNumberCodec: Codec<number> = safeInteger({ primitive: "u16", min: 1, max: MAX_ADS_IN_POD });

const adPodCodec: Codec<AdPod> = taggedUnion<AdPod, "kind">("kind", {
  unknown: {},
  known: { index: adPodNumberCodec, count: adPodNumberCodec },
});

const adSkipCodec: Codec<AdSkip> = taggedUnion<AdSkip, "kind">("kind", {
  unavailable: {},
  countdown: { seconds: safeInteger({ primitive: "u16", min: 0, max: MAX_AD_COUNTDOWN_SECONDS }) },
  available: {},
  on_page: {},
});

/** Кодек состояния рекламы. */
export const pageAdStateCodec: Codec<PageAdState> = taggedUnion<PageAdState, "kind">("kind", {
  none: {},
  showing: { pod: adPodCodec, skip: adSkipCodec },
});

/** Правило между полями: номер рекламы не больше размера блока. */
export function validatePageAdState(ad: PageAdState): void {
  if (ad.kind === "showing" && ad.pod.kind === "known" && ad.pod.index > ad.pod.count) {
    throw new ProtocolFailure("number_out_of_range").prependPath("ad");
  }
}
