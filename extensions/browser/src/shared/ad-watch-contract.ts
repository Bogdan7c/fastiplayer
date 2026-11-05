// Договор слежения за рекламой (сессия 09d): фон ⇄ изолированный мир фрейма.
//
// Страница в этом разговоре не участвует вообще: наблюдение живёт в ИЗОЛИРОВАННОМ мире (код страницы
// не может подменить наши помощники), а наружу из него уходят только числа и метки (`PageAdState`).
// До передачи вкладки плееру ничего этого нет: ни одного сообщения, ни одного наблюдателя (решение 198).

import type { PageAdState } from "../protocol/index.ts";

/** Вид просьбы фона всем фреймам вкладки: «следи за рекламой ещё `lease_ms`». */
export const ARM_AD_WATCH_KIND = "arm_ad_watch";

/**
 * Аренда наблюдения. Срок вместо команды «снять»: фон может умереть в любой момент, а фрейм не должен
 * остаться с наблюдателем навсегда. Фон продлевает аренду, пока сеанс жив.
 */
export interface ArmAdWatchRequest {
  readonly kind: typeof ARM_AD_WATCH_KIND;
  readonly lease_ms: number;
}

/** Наибольший срок аренды, который фрейм примет. */
export const MAX_AD_WATCH_LEASE_MS = 60_000;

/** Вид донесения фрейма фону о рекламе. */
export const AD_WATCH_REPORT_KIND = "ad_watch_report";

/**
 * Донесение фрейма: `runtime.sendMessage`; вкладку и фрейм фон берёт из отправителя, а не из сообщения.
 * Приходит при каждом изменении и при каждом продлении аренды, пока реклама идёт: фон считает
 * молчание концом рекламы (IMA-фрейм просто удаляется, сказать «конец» ему некому).
 */
export interface AdWatchReport {
  readonly kind: typeof AD_WATCH_REPORT_KIND;
  readonly ad: PageAdState;
}

/** Вид просьбы фона одному фрейму: «нажми кнопку пропуска». */
export const PRESS_AD_SKIP_KIND = "press_ad_skip";

export interface PressAdSkipRequest {
  readonly kind: typeof PRESS_AD_SKIP_KIND;
}

/**
 * Итог нажатия (закрытый набор, ответ фрейма):
 * - `pressed` — кнопка пропуска была на экране, клик выполнен;
 * - `no_button` — кнопки уже нет (команда плеера устарела): не нажато ничего;
 * - `person_only` — кнопка есть, но принимает только настоящую мышь (YouTube): мы не нажимали.
 */
export type PressSkipOutcome = "pressed" | "no_button" | "person_only";
