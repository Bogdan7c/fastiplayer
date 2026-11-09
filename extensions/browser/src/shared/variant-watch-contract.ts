// Договор наблюдения за вариантами страницы между фоном и фреймами (сессия 14b, решения 198, 279, 282).
//
// Как у читателей рекламы (09d): фон берёт наблюдение в аренду (`arm_variant_watch` всем фреймам вкладки),
// пока сеанс жив, и продлевает её раз в секунду; до передачи фреймы молчат (решение 198). Фрейм, где адаптер
// узнал страницу, доносит описание её меню (`variant_watch_report`) при изменении и на каждом продлении:
// молчание фон считает уходом меню. Команды плеера фон отдаёт ровно тому фрейму, что прислал меню
// (`variant_command`), и ждёт исход.
//
// Описание — сырой текст страницы. Здесь проверяется только форма и грубые пределы размера; подписи для
// протокола нормализует фон (`background/handoff/variant-menu-normalize.ts`).

import type { PageVariantDescription, PageVariantKind, PageVariantOption, VariantActionOutcome } from "../adapters/capabilities/variants/contract.ts";

export type { PageVariantDescription, PageVariantKind, PageVariantOption, VariantActionOutcome };

export const ARM_VARIANT_WATCH_KIND = "arm_variant_watch";
export const VARIANT_WATCH_REPORT_KIND = "variant_watch_report";
export const VARIANT_COMMAND_KIND = "variant_command";

/** Наибольший срок аренды, который фрейм примет: защита от «вечной» аренды при ошибке фона. */
export const MAX_VARIANT_WATCH_LEASE_MS = 60_000;

/** Грубые пределы донесения (форма, а не протокол): пункты, длина ключей, подписей и паспорта адаптера. */
export const MAX_REPORTED_VARIANT_OPTIONS = 256;
export const MAX_REPORTED_VARIANT_ID_LENGTH = 256;
export const MAX_REPORTED_VARIANT_LABEL_LENGTH = 1_024;
export const MAX_REPORTED_KEY_LENGTH = 512;
export const MAX_REPORTED_ADAPTER_LENGTH = 80;

/** Фон → все фреймы вкладки: наблюдать за вариантами ещё `lease_ms`. */
export interface ArmVariantWatchRequest {
  readonly kind: typeof ARM_VARIANT_WATCH_KIND;
  readonly lease_ms: number;
}

/**
 * Фрейм → фон: что предлагает плеер сайта в этом фрейме. `description: null` — меню больше нет (адаптер
 * упал, страница сменилась). `adapter` — паспорт `id@version` для журнала разработчика.
 * Вкладку и фрейм фон берёт из отправителя, а не из сообщения.
 */
export interface VariantWatchReport {
  readonly kind: typeof VARIANT_WATCH_REPORT_KIND;
  readonly adapter: string;
  readonly description: PageVariantDescription | null;
}

/** Что сделать на странице по просьбе плеера. */
export type VariantCommand =
  | { readonly kind: "select"; readonly variant_kind: PageVariantKind; readonly variant_id: string }
  | { readonly kind: "next" };

/** Фон → один фрейм: исполнить команду. Ответ фрейма — `VariantActionOutcome`. */
export interface VariantCommandRequest {
  readonly kind: typeof VARIANT_COMMAND_KIND;
  readonly command: VariantCommand;
}
