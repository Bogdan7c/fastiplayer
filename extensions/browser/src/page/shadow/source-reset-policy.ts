// Какой источник сейчас у переданного элемента — новое поколение, «пусто» или чужой поток.
//
// Тень спрашивает это в двух местах:
// - сразу после «алгоритма загрузки» (`load()`, запись `src`/`srcObject` через наши
//   переопределения, доверенное `emptied`) — это всегда намерение страницы, сам вызов уже ушёл
//   в оригинал, а тень лишь решает, что делать с собой;
// - на тиках часов, пока элемент без источника (сторож `empty-source-watch.ts`): атрибут `src`,
//   поставленный через `setAttribute` элементу в NETWORK_EMPTY, не даёт `emptied`, и иначе новый
//   источник прошёл бы мимо тени.
//
// Вердикты:
// - `media_source` — MediaSource (blob-адрес MediaSource по реестру A1 или `srcObject` =
//   MediaSource/handle): новое поколение на том же элементе (реклама YouTube → фильм, следующая
//   серия, сторож плеера «зависло → пересоздать»). Тень остаётся.
// - `no_source` — `srcObject` пуст И атрибута `src` нет или он пустой. Это ПЕРЕХОДНОЕ состояние
//   (Д-1): YouTube перед новым MediaSource делает `srcObject = null`, hls.js/Shaka/dash.js —
//   `removeAttribute("src"); load()`. Тень остаётся и ждёт допуск (решение владельца, 179).
// - `not_media_source` — обычный файл, HLS-адрес, blob обычного `Blob`/`File`: 09a передаёт
//   только MSE, страница честно забрала видео — тень снимается сразу (`element_released`).
//
// Точный ответ «этот blob-адрес — MediaSource» знает реестр MAIN-мира (A1: обёртка
// `URL.createObjectURL` запоминает адреса, созданные для MediaSource). Тень спрашивает его через
// окружение.
// Известное ограничение: источники из дочерних `<source>` (без атрибута `src`) здесь выглядят как
// «пусто» и снимаются по допуску — передача `<source>` и `<video src>` — сессия 09b.

import { hasPrototype } from "./intrinsics.ts";
import { callOriginal, type ElementOriginals, readOriginal } from "./originals.ts";

/** Что сейчас стоит источником у элемента. */
export type SourceVerdict = "media_source" | "no_source" | "not_media_source";

/** Ответ реестра MAIN-мира: адрес создан `URL.createObjectURL` для MediaSource этого документа. */
export type MediaSourceObjectUrlCheck = (url: string) => boolean;

/** Имя атрибута-источника медиаэлемента. */
const SOURCE_ATTRIBUTE = "src";

export function classifyElementSource(
  element: HTMLMediaElement,
  originals: ElementOriginals,
  isMediaSourceObjectUrl: MediaSourceObjectUrlCheck,
): SourceVerdict {
  const srcObjectAccessor = originals.media.srcObject;
  if (srcObjectAccessor !== undefined) {
    const sourceObject = readOriginal(srcObjectAccessor, element);
    // `srcObject` главнее атрибута `src`: если он задан, решает только он.
    if (sourceObject !== null && sourceObject !== undefined) {
      return isMediaSourceObject(sourceObject, originals) ? "media_source" : "not_media_source";
    }
  }
  if (sourceAttributeIsEmpty(element, originals)) return "no_source";
  const sourceUrl = readOriginal(originals.media.src, element);
  return typeof sourceUrl === "string" && isMediaSourceObjectUrl(sourceUrl) ? "media_source" : "not_media_source";
}

/** Атрибута `src` нет или он пустой (читается оригинальным `getAttribute`). */
function sourceAttributeIsEmpty(element: HTMLMediaElement, originals: ElementOriginals): boolean {
  if (originals.getAttribute === undefined) {
    // Без `getAttribute` (не бывает в живых браузерах) остаётся геттер: "" — атрибута нет;
    // пустой атрибут (адрес документа) тогда честно считается чужим источником.
    return readOriginal(originals.media.src, element) === "";
  }
  const attribute = callOriginal(originals.getAttribute, element, [SOURCE_ATTRIBUTE]);
  return attribute === null || attribute === "";
}

function isMediaSourceObject(sourceObject: unknown, originals: ElementOriginals): boolean {
  if (typeof sourceObject !== "object" || sourceObject === null) return false;
  return originals.mediaSourcePrototypes.some((prototype) => hasPrototype(sourceObject, prototype));
}
