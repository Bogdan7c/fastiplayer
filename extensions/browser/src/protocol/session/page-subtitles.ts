// Субтитры страницы (протокол 1.5, сессия 13b1, решения 270-274). Зеркало
// session/page_subtitles.rs.
//
// Весь договор в одном месте:
// 1. Экстеншен скачал (или собрал со страницы) документ дорожки и шлёт заголовок
//    `subtitle_track_offered`: номер дорожки, язык, подпись, формат, откуда документ, была ли
//    дорожка включена на странице, точная длина документа `byte_length`.
// 2. Затем - байты документа двоичными рамками вида 3 (`data-frame.ts`): номер дорожки и
//    смещение внутри документа, подряд с 0, ровно `byte_length` байт. Документ готов, когда
//    пришёл последний байт; отдельного «готово» нет. Нарезку делает
//    `splitPageSubtitleDocument` (`page-subtitle-sender.ts`).
// 3. Тот же номер дорожки ещё раз - замена документа (прежний, даже недокачанный, забыт).
// Сообщение не привязано к поколению потока. Порядок проверяет сторож плеера (Rust): рамка
// для непредложенной или готовой дорожки, разрыв смещения, выход за длину, больше 64 дорожек,
// общая версия ниже 1.5, страница ушла - нарушение (4003).
//
// Необязательные язык и подпись - по конвенции протокола метки, а не отсутствующие поля:
// `{"kind":"unknown"}` / `{"kind":"known","tag":"en"}` и `{"kind":"none"}` / `{"kind":"text","text":"English"}`.

import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "../limits.ts";
import { safeInteger } from "../numbers.ts";
import { type Codec, stringEnum, taggedUnion } from "../schema.ts";
import { subtitleLabelCodec, subtitleLanguageTagCodec } from "../text.ts";

/** Номер дорожки субтитров страницы на сеанс, 1..2^32-1. */
export type PageSubtitleTrackId = number;

/** Точная длина документа субтитров, 1..8 МиБ. */
export type PageSubtitleByteLength = number;

/** Язык дорожки: страница его не назвала или назвала тегом BCP-47. */
export type PageSubtitleLanguage =
  | { readonly kind: "unknown" }
  | { readonly kind: "known"; readonly tag: string };

/** Подпись дорожки для человека: её нет (плеер назовёт дорожку сам) или текст страницы. */
export type PageSubtitleLabel =
  | { readonly kind: "none" }
  | { readonly kind: "text"; readonly text: string };

/**
 * Формат документа по мнению экстеншена. Подсказка, а не обещание: плеер узнаёт формат по
 * содержимому и при расхождении верит содержимому.
 */
export type SubtitleFormatHint = "web_vtt" | "srt" | "ttml" | "srv3" | "json3" | "unknown";

/** Все подсказки формата - один список для разбора. */
export const SUBTITLE_FORMAT_HINTS: readonly SubtitleFormatHint[] = ["web_vtt", "srt", "ttml", "srv3", "json3", "unknown"];

/** Откуда документ: страница сама скачала файл, `<track src>` у видео или реплики `TextTrack.cues`. */
export type PageSubtitleOrigin = "page_download" | "track_element" | "text_track_cues";

/** Все источники документа - один список для разбора. */
export const PAGE_SUBTITLE_ORIGINS: readonly PageSubtitleOrigin[] = ["page_download", "track_element", "text_track_cues"];

/** Была ли дорожка включена на странице (решение 271: «скачала - значит включено»). */
export type PageSubtitleDisplay = "showing" | "not_showing";

/** Оба состояния показа - один список для разбора. */
export const PAGE_SUBTITLE_DISPLAYS: readonly PageSubtitleDisplay[] = ["showing", "not_showing"];

/** Поля сообщения `subtitle_track_offered` без `type` (их собирает отправитель). */
export interface PageSubtitleOffer {
  readonly track: PageSubtitleTrackId;
  readonly language: PageSubtitleLanguage;
  readonly label: PageSubtitleLabel;
  readonly format_hint: SubtitleFormatHint;
  readonly origin: PageSubtitleOrigin;
  readonly page_display: PageSubtitleDisplay;
  readonly byte_length: PageSubtitleByteLength;
}

/** Номер дорожки: u32 от 1 (ноль - признак забытого поля). */
export const pageSubtitleTrackIdCodec: Codec<PageSubtitleTrackId> = safeInteger({ primitive: "u32", min: 1 });

/** Длина документа: 1..8 МиБ (пустого документа не бывает). */
export const pageSubtitleByteLengthCodec: Codec<PageSubtitleByteLength> = safeInteger({
  primitive: "u64",
  min: 1,
  max: MAX_PAGE_SUBTITLE_DOCUMENT_BYTES,
});

export const pageSubtitleLanguageCodec: Codec<PageSubtitleLanguage> = taggedUnion<PageSubtitleLanguage, "kind">("kind", {
  unknown: {},
  known: { tag: subtitleLanguageTagCodec },
});

export const pageSubtitleLabelCodec: Codec<PageSubtitleLabel> = taggedUnion<PageSubtitleLabel, "kind">("kind", {
  none: {},
  text: { text: subtitleLabelCodec },
});

export const subtitleFormatHintCodec: Codec<SubtitleFormatHint> = stringEnum<SubtitleFormatHint>(SUBTITLE_FORMAT_HINTS);

export const pageSubtitleOriginCodec: Codec<PageSubtitleOrigin> = stringEnum<PageSubtitleOrigin>(PAGE_SUBTITLE_ORIGINS);

export const pageSubtitleDisplayCodec: Codec<PageSubtitleDisplay> = stringEnum<PageSubtitleDisplay>(PAGE_SUBTITLE_DISPLAYS);
