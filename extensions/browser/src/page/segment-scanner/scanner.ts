// Потоковый «сканер границ» байтов одного SourceBuffer (решение 176, «память без копий»).
//
// Зачем: журнал держит ссылки на куски `appendBuffer` под потолком памяти и выкидывает старое.
// Резать можно только там, откуда поток снова понятен плееру: начало init-сегмента или начало
// медиасегмента. Сканер находит эти места, читая ТОЛЬКО заголовки верхнего уровня (размер и тип
// бокса MP4, ID и размер элемента WebM) и перепрыгивая тела — то есть почти без работы процессора.
//
// Куски страниц режутся где угодно (YouTube режет посреди бокса), поэтому сканер — автомат,
// который помнит, сколько байтов тела осталось перепрыгнуть и недочитанный заголовок (≤ 16 байт).
// Итог не зависит от того, как поток порезан на куски (это закрепляют тесты).
//
// Смещения меток — от начала потока этого SourceBuffer (сумма всех байтов, прошедших через сканер).

import { Fmp4Scanner } from "./fmp4-scanner.ts";
import { WebmScanner } from "./webm-scanner.ts";

/** Что начинается (или кончается) в этом месте потока. */
export type SegmentMarkerKind =
  /** Начало init-сегмента (`ftyp`/`moov` в MP4, EBML-заголовок в WebM). Отсюда поток можно начать. */
  | "init_start"
  /** Конец init-сегмента (после `moov`; в WebM — начало первого `Cluster`). */
  | "init_end"
  /** Начало медиасегмента (`styp`/`moof`/`sidx`/`emsg`/`prft`, `Cluster`). Отсюда поток можно продолжить после init. */
  | "media_start";

export interface SegmentMarker {
  readonly kind: SegmentMarkerKind;
  /** Смещение от начала потока SourceBuffer. */
  readonly offset: number;
}

/** Итог разбора одного куска. */
export type ScanOutcome =
  | { readonly kind: "scanned"; readonly markers: readonly SegmentMarker[] }
  /** Поток не похож на свой формат или разбор сбился. Дальше сканер ничего не находит. */
  | { readonly kind: "failed"; readonly reason: string };

export interface SegmentScanner {
  /** Разобрать очередной кусок потока (кусок не копируется и не меняется). */
  scan(chunk: Uint8Array): ScanOutcome;
  /**
   * `abort()` страницы: браузер сбрасывает разборщик, и следующий кусок начинается с нового
   * бокса/элемента. Сканер забывает недочитанное, но помнит, в какой части потока стоит.
   */
  restartAtNextAppend(): void;
}

/** Контейнер, который сканер понимает. */
export type ScannableContainer = "fmp4" | "webm";

/** MIME (`video/mp4; codecs="…"`) → контейнер. `null` — формат сканер не понимает (MP3, ADTS, TS…). */
export function containerOfMimeType(mimeType: string): ScannableContainer | null {
  const essence = (mimeType.split(";")[0] ?? "").trim().toLowerCase();
  switch (essence) {
    case "video/mp4":
    case "audio/mp4":
      return "fmp4";
    case "video/webm":
    case "audio/webm":
    case "video/x-matroska":
    case "audio/x-matroska":
      return "webm";
    default:
      return null;
  }
}

/**
 * Сканер для потока с этим MIME, начиная со смещения `streamOffset` (после `changeType` поток
 * SourceBuffer продолжается, а формат может смениться). `null` — формат не поддержан.
 */
export function createSegmentScanner(mimeType: string, streamOffset: number): SegmentScanner | null {
  switch (containerOfMimeType(mimeType)) {
    case "fmp4":
      return new Fmp4Scanner(streamOffset);
    case "webm":
      return new WebmScanner(streamOffset);
    case null:
      return null;
  }
}
