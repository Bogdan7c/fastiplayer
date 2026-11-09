// Внутренний договор субтитров страницы (сессия 13b2, решения 20, 54, 270–272, 275, 276).
//
// Кто что делает:
// - MAIN-мир (`src/page/page-subtitles/`) ловит документы субтитров, которые скачал плеер сайта
//   (`fetch`/XHR), собирает WebVTT из реплик `TextTrack` без файла и находит `<track src>` у
//   переданного видео. До передачи всё это лежит на его складе, после `handoff_began{started}`
//   склад уходит сразу за журналом MSE, а новое — по мере поимки.
// - Изолированный мир пропускает события только строгой формы (`page-subtitle-validation.ts`).
// - Фон (`src/background/handoff/page-subtitles/`) даёт дорожкам номера сеанса, держит договор
//   плеера (64 дорожки, 32 МиБ), сам качает `<track src>` (решения 272, 275) и шлёт протоколом 1.5
//   только плееру, который договорился на 1.5.
//
// Адреса документов плееру не уходят никогда: `subtitle_track_source.url` нужен только фону.

import type { PageSubtitleDisplay, SubtitleFormatHint } from "../protocol/session/page-subtitles.ts";
import type { ReferrerPolicyName } from "./referrer-policy.ts";

/**
 * Ключ дорожки внутри одного документа страницы, 1..2^32-1. Его выдаёт MAIN-мир; номер дорожки
 * сеанса (`PageSubtitleTrackId`) по паре «подключение документа + ключ» даёт фон. Тот же ключ
 * ещё раз — замена документа этой дорожки (растущая `TextTrack`, перекачка того же документа).
 */
export type PageSubtitleKey = number;

/**
 * Как страница назвала дорожку. Сырые значения страницы: фон сам приводит их к правилам протокола
 * (неверный тег языка — «язык не назван», слишком длинная подпись — обрезка по символам).
 * - `language` — `srclang`, `TextTrack.language` или язык из адреса документа; `null` — не назван;
 * - `label` — `label` дорожки; `null` — подписи нет, плеер назовёт дорожку по языку;
 * - `auto_generated` — автосубтитры (адаптер timedtext YouTube, решение 276): фон допишет
 *   к названию языка «(auto-generated)», чтобы ручные и авто не слились в два «English».
 */
export interface PageSubtitleNaming {
  readonly language: string | null;
  readonly label: string | null;
  readonly auto_generated: boolean;
}

/** Откуда готовый документ: плеер сайта скачал его сам или MAIN-мир собрал WebVTT из реплик. */
export const PAGE_SUBTITLE_DOCUMENT_ORIGINS = ["page_download", "text_track_cues"] as const;
export type PageSubtitleDocumentOrigin = (typeof PAGE_SUBTITLE_DOCUMENT_ORIGINS)[number];

/** Наибольшая длина сырого `language` от страницы, символов (тег протокола — до 35 байт). */
export const MAX_PAGE_SUBTITLE_LANGUAGE_CHARS = 64;

/** Наибольшая длина сырой подписи от страницы, символов (подпись протокола — до 256 байт). */
export const MAX_PAGE_SUBTITLE_LABEL_CHARS = 512;

/**
 * Готовый документ дорожки (`page_download` или `text_track_cues`). `bytes` — точная копия
 * (byteOffset 0, длина = buffer.byteLength), уходит с transfer, как у события `bytes`; 1 байт…8 МиБ.
 * `format_hint` — что опознал MAIN-мир по содержимому (`sniffSubtitleDocument`); плеер всё равно
 * верит содержимому. `display` — «скачала = включено» (271) для `page_download`, `mode` для реплик.
 */
export interface PageSubtitleDocumentEvent {
  readonly kind: "subtitle_document";
  readonly key: PageSubtitleKey;
  readonly naming: PageSubtitleNaming;
  readonly format_hint: SubtitleFormatHint;
  readonly origin: PageSubtitleDocumentOrigin;
  readonly display: PageSubtitleDisplay;
  readonly bytes: Uint8Array;
}

/**
 * Объявленная дорожка `<track src>` переданного видео: документ качает фон через сеть браузера
 * (куки, заслон локальных адресов, Referer правилом) — и для выключенных (272), и для включённых
 * (275). `url` — абсолютный канонический http(s) (`HTMLTrackElement.src`); факты Referer — как у
 * `file_source` (решение 202). `display` — `mode === "showing"`.
 */
export interface PageSubtitleTrackSourceEvent {
  readonly kind: "subtitle_track_source";
  readonly key: PageSubtitleKey;
  readonly naming: PageSubtitleNaming;
  readonly display: PageSubtitleDisplay;
  readonly url: string;
  readonly referrer_origin: string | null;
  readonly referrer_policy: ReferrerPolicyName;
}

/** Все события субтитров страницы (входят в `PageEvent`). */
export type PageSubtitleEvent = PageSubtitleDocumentEvent | PageSubtitleTrackSourceEvent;
