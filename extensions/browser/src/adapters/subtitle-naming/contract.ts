// Договор адаптера именования документа субтитров по его адресу (решение 276).
//
// Адаптер — чистая функция: по частям адреса возвращает, как назвать дорожку, или `null`
// («не мой адрес»). Ни сети, ни DOM, ни состояния: его легко тестировать настоящими адресами сайта.

import type { PageSubtitleNaming } from "../../shared/page-subtitle-contract.ts";
import type { AddressParts } from "./address-parts.ts";

export interface SubtitleNamingAdapter {
  /** Имя адаптера — для тестов и диагностики. */
  readonly name: string;
  /** Как назвать дорожку по адресу; `null` — адрес не этого сайта/формы. */
  nameByAddress(parts: AddressParts): PageSubtitleNaming | null;
}
