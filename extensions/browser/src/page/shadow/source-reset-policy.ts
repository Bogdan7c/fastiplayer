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
// Источники из дочерних `<source>` (атрибута `src` у элемента нет, сессия 09b): решает первый
// `<source>` с непустым `src` — blob-адрес MediaSource → `media_source`, иначе `not_media_source`.
// Так браузер и выбирает (по порядку детей); проверку `type` браузер делает позже асинхронно, но
// для «это всё ещё MSE или страница забрала видео» достаточно адреса.

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
  if (sourceAttributeIsEmpty(element, originals)) {
    const childUrl = firstSourceChildUrl(element, originals);
    if (childUrl === null) return "no_source";
    return isMediaSourceObjectUrl(childUrl) ? "media_source" : "not_media_source";
  }
  const sourceUrl = readOriginal(originals.media.src, element);
  return typeof sourceUrl === "string" && isMediaSourceObjectUrl(sourceUrl) ? "media_source" : "not_media_source";
}

/**
 * Объявлен ли источник элемента в разметке: непустой атрибут `src` или дочерний `<source>` с
 * адресом. Нет — значит, файл элемент получил мимо DOM: видео-документ Firefox (прямая ссылка на
 * `.mp4`) грузит файл каналом самого документа, и ни атрибута, ни `<source>` у его `<video>` нет
 * (дефект 4 живой проверки 09b). `srcObject` здесь не смотрится — это не разметка.
 */
export function hasSourceDeclaredInMarkup(element: HTMLMediaElement, originals: ElementOriginals): boolean {
  return !sourceAttributeIsEmpty(element, originals) || firstSourceChildUrl(element, originals) !== null;
}

/** Имя дочернего элемента-источника (`localName` HTML-элементов — строчными). */
const SOURCE_CHILD_NAME = "source";

/**
 * Атрибут `src` первого дочернего `<source>` с непустым адресом (`null` — таких нет или DOM без
 * нужных оригиналов). Blob-адрес в атрибуте всегда абсолютный, поэтому сверка с реестром идёт по
 * атрибуту как есть.
 */
function firstSourceChildUrl(element: HTMLMediaElement, originals: ElementOriginals): string | null {
  const { elementChildren, localName, getAttribute } = originals;
  if (elementChildren === undefined || localName === undefined || getAttribute === undefined) return null;
  const children = readOriginal(elementChildren, element) as { readonly length: number; readonly [index: number]: unknown } | null;
  if (children === null || typeof children !== "object") return null;
  for (let index = 0; index < children.length; index += 1) {
    const child = children[index];
    if (readOriginal(localName, child) !== SOURCE_CHILD_NAME) continue;
    const url = callOriginal(getAttribute, child, [SOURCE_ATTRIBUTE]);
    if (typeof url === "string" && url !== "") return url;
  }
  return null;
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
