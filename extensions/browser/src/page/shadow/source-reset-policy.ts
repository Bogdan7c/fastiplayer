// Что значит `emptied` переданного элемента: смена поколения или страница бросила элемент.
//
// `emptied` браузер шлёт, когда страница запускает «алгоритм загрузки» элемента: `load()`,
// новый `src`/`srcObject`, снятие источника. Это всегда намерение страницы, поэтому сам вызов
// идёт в оригинал, а тень лишь решает, что делать с собой:
// - новый источник — MediaSource (blob-адрес MediaSource по реестру A1 или `srcObject` =
//   MediaSource/handle): это новое
//   поколение на том же элементе (реклама YouTube → фильм, следующая серия Tortuga, сторож
//   «зависло → пересоздать»). Тень остаётся и повторяет алгоритм загрузки (позиция 0, пауза).
// - источника нет или он не MSE (обычный файл, HLS-адрес): 09a передаёт только MSE, тень
//   снимается с `handoff_ended{element_released}` и элемент снова целиком принадлежит странице.
//
// Точный ответ «этот blob-адрес — MediaSource» знает реестр MAIN-мира (A1: обёртка
// `URL.createObjectURL` запоминает адреса, созданные для MediaSource). Тень спрашивает его через
// окружение: blob-адрес обычного файла (`Blob`/`File`) — не MSE, тень снимается.

import { hasPrototype } from "./intrinsics.ts";
import { type ElementOriginals, readOriginal } from "./originals.ts";

/** Чем стал элемент после `emptied`. */
export type SourceResetKind = "new_media_source" | "released";

/** Ответ реестра MAIN-мира: адрес создан `URL.createObjectURL` для MediaSource этого документа. */
export type MediaSourceObjectUrlCheck = (url: string) => boolean;

export function classifySourceReset(
  element: HTMLMediaElement,
  originals: ElementOriginals,
  isMediaSourceObjectUrl: MediaSourceObjectUrlCheck,
): SourceResetKind {
  const srcObjectAccessor = originals.media.srcObject;
  if (srcObjectAccessor !== undefined) {
    const sourceObject = readOriginal(srcObjectAccessor, element);
    // `srcObject` главнее атрибута `src`: если он задан, решает только он.
    if (sourceObject !== null && sourceObject !== undefined) {
      return isMediaSourceObject(sourceObject, originals) ? "new_media_source" : "released";
    }
  }
  const sourceUrl = readOriginal(originals.media.src, element);
  if (typeof sourceUrl === "string" && isMediaSourceObjectUrl(sourceUrl)) return "new_media_source";
  return "released";
}

function isMediaSourceObject(sourceObject: unknown, originals: ElementOriginals): boolean {
  if (typeof sourceObject !== "object" || sourceObject === null) return false;
  return originals.mediaSourcePrototypes.some((prototype) => hasPrototype(sourceObject, prototype));
}
