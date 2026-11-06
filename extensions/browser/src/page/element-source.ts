// Чем сейчас играет видеоэлемент — с точки зрения «можно ли его передать» (сессия 09b).
//
// Единственное место MAIN-мира, где решается форма источника: из него берут и описание
// кандидатов для фона (`VideoCandidates.describe`), и ветка `begin_handoff` контроллера. Так фон
// и контроллер не расходятся в том, что считать файлом, а что — неподдержанной формой.
//
// Порядок проверки — как браузер выбирает источник:
//  1. живой MediaSource (наш хук его видел, есть SourceBuffer, не `closed`) → MSE;
//  2. `srcObject` (главнее атрибута `src`): MediaStream, Blob — неподдержанные формы;
//     `MediaSourceHandle` (MSE в выделенном воркере, 09b-C) — по реестру воркеров: живой MediaSource
//     обёрнутого воркера → MSE воркера, ещё без SourceBuffer или закрытый → «пока нечего», не
//     сопоставленный ни с одним обёрнутым воркером → `untracked_worker_media_source`; прочее
//     (MediaSource главного потока без SourceBuffer) — «пока нечего передавать»;
//  3. выбранный адрес `currentSrc` (он учитывает и дочерние `<source>`): http(s) → файл,
//     m3u8 → нативный HLS (решение 185), blob обычного Blob → `blob_file`, `data:` → `data_url`,
//     прочие схемы → `other_scheme`;
//  4. иначе — источника нет.
// Адрес читается ОРИГИНАЛЬНЫМИ геттерами (страница могла подменить свойства), разбирается без
// `URL` страницы (его тоже могли подменить) — только строковыми операциями.

import type { CandidateMedia, UnsupportedMediaForm } from "../shared/handoff-contract.ts";
import { MAX_FILE_SOURCE_URL_LENGTH } from "../shared/page-message-validation.ts";
import type { DomReaders } from "./dom-readers.ts";
import type { MediaSourceRecord } from "./mse-tracker.ts";
import type { ScriptArrival } from "./script-arrival.ts";
import type { WorkerHandleResolution, WorkerMediaSourceRef } from "./worker-capture/index.ts";

/** Источник элемента, с тем, что нужно ветке передачи. */
export type ElementSource =
  | { readonly kind: "media_source"; readonly record: MediaSourceRecord }
  /** MediaSource выделенного воркера: его журнал и живой поток — по личному каналу воркера. */
  | { readonly kind: "worker_media_source"; readonly mediaSource: WorkerMediaSourceRef }
  /** Абсолютный http(s)-адрес файла (уже проверенной длины). */
  | { readonly kind: "file"; readonly url: string }
  | { readonly kind: "unsupported"; readonly form: UnsupportedMediaForm }
  | { readonly kind: "none" };

/** Что нужно классификатору снаружи. */
export interface ElementSourceReaders {
  readonly dom: DomReaders;
  /** Живой MediaSource элемента (`VideoCandidates.liveMediaSourceOf`). */
  readonly liveMediaSourceOf: (element: Element) => MediaSourceRecord | null;
  /** Адрес создан `URL.createObjectURL` для MediaSource этого документа (реестр). */
  readonly isMediaSourceObjectUrl: (url: string) => boolean;
  /** Что стоит за объектом `srcObject` (`null` — это не `MediaSourceHandle`), реестр воркеров. */
  readonly resolveWorkerHandle: (candidate: unknown) => WorkerHandleResolution | null;
  /** Когда встал наш скрипт: от этого зависит, как понимать незнакомый blob-адрес (сессия 10d). */
  readonly scriptArrival: ScriptArrival;
}

/** MIME-типы HLS, которыми сайты помечают `<source type>` (без параметров, строчными). */
const HLS_MIME_TYPES: readonly string[] = ["application/vnd.apple.mpegurl", "application/x-mpegurl", "audio/mpegurl", "audio/x-mpegurl"];

/** Расширение плейлиста HLS в пути адреса. */
const HLS_PLAYLIST_EXTENSION = ".m3u8";

/** Схемы адреса (строчными, с двоеточием). */
const HTTP_SCHEMES: readonly string[] = ["http:", "https:"];
const BLOB_SCHEME = "blob:";
const DATA_SCHEME = "data:";

/** Источник элемента по правилу выше. Чтения могут бросить (чужой «бренд») — ловит вызывающий. */
export function elementSourceOf(element: Element, readers: ElementSourceReaders): ElementSource {
  const record = readers.liveMediaSourceOf(element);
  if (record !== null) return { kind: "media_source", record };
  const sourceObject = readers.dom.srcObjectOf(element);
  if (sourceObject !== null && sourceObject !== undefined) {
    if (readers.dom.isMediaStream(sourceObject)) return { kind: "unsupported", form: "media_stream" };
    if (readers.dom.isBlob(sourceObject)) return { kind: "unsupported", form: "blob_file" };
    return workerHandleSource(readers.resolveWorkerHandle(sourceObject));
  }
  const url = readers.dom.currentSrcOf(element);
  if (url === "") return { kind: "none" };
  return classifyMediaUrl(url, readers.dom.sourceTypeFor(element, url), readers.isMediaSourceObjectUrl, readers.scriptArrival);
}

/**
 * Форма по выбранному адресу. `sourceType` — атрибут `type` выбранного `<source>` (`null` — нет).
 * Чистая функция: проверяется тестами без DOM.
 */
export function classifyMediaUrl(
  url: string,
  sourceType: string | null,
  isMediaSourceObjectUrl: (url: string) => boolean,
  scriptArrival: ScriptArrival = "at_document_start",
): ElementSource {
  const scheme = schemeOf(url);
  if (scheme === BLOB_SCHEME) {
    // Blob-адрес MediaSource, который ещё не живой (нет SourceBuffer) или уже `closed`: пока нечего.
    if (isMediaSourceObjectUrl(url)) return { kind: "none" };
    // Незнакомый blob при позднем скрипте: это почти наверняка MediaSource, созданный до хука. Отличить его от
    // обычного Blob нельзя, поэтому честно просим перезагрузку: после неё обычный Blob получит свой отказ.
    return { kind: "unsupported", form: scriptArrival === "after_page_started" ? "started_before_script" : "blob_file" };
  }
  if (scheme === DATA_SCHEME) return { kind: "unsupported", form: "data_url" };
  if (scheme === undefined || !HTTP_SCHEMES.includes(scheme)) return { kind: "unsupported", form: "other_scheme" };
  if (isHlsPlaylist(url, sourceType)) return { kind: "unsupported", form: "native_hls" };
  // Адрес длиннее предела договора изолированный мир отверг бы, и передача встала бы без файла.
  // Такой адрес — уже не «файл, который можно отдать»: честно считаем его неподдержанным.
  if (url.length > MAX_FILE_SOURCE_URL_LENGTH) return { kind: "unsupported", form: "other_scheme" };
  return { kind: "file", url };
}

/** Форма `srcObject`-handle по ответу реестра воркеров (`null` — не handle: «пока нечего»). */
function workerHandleSource(resolution: WorkerHandleResolution | null): ElementSource {
  switch (resolution?.kind) {
    case "live":
      return { kind: "worker_media_source", mediaSource: resolution.mediaSource };
    case "untracked":
      return { kind: "unsupported", form: "untracked_worker_media_source" };
    case "not_live":
    case undefined:
      return { kind: "none" };
  }
}

/**
 * Описание источника для фона (без записи MediaSource и адреса — они фону не нужны). MSE воркера
 * для фона — такое же MSE: выбор видео и поколения у него те же (решение 63, поправка 01).
 */
export function candidateMediaOf(source: ElementSource): CandidateMedia {
  switch (source.kind) {
    case "media_source":
      return { kind: "media_source", in_worker: false };
    case "worker_media_source":
      return { kind: "media_source", in_worker: true };
    case "file":
      return { kind: "file" };
    case "unsupported":
      return { kind: "unsupported", form: source.form };
    case "none":
      return { kind: "none" };
  }
}

/** Схема адреса строчными с двоеточием (`undefined` — схемы нет). */
function schemeOf(url: string): string | undefined {
  const match = /^([a-z][a-z0-9+.-]*:)/i.exec(url);
  return match?.[1]?.toLowerCase();
}

/** Плейлист HLS: путь кончается на `.m3u8` или выбранный `<source>` помечен типом HLS. */
function isHlsPlaylist(url: string, sourceType: string | null): boolean {
  const path = url.split(/[?#]/, 1)[0] ?? "";
  if (path.toLowerCase().endsWith(HLS_PLAYLIST_EXTENSION)) return true;
  if (sourceType === null) return false;
  const essence = (sourceType.split(";", 1)[0] ?? "").trim().toLowerCase();
  return HLS_MIME_TYPES.includes(essence);
}
