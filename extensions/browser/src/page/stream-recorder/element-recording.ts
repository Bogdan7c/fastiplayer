// Запись действий СТРАНИЦЫ над видеоэлементом: источник, play/pause/перемотка/скорость/load.
//
// Как отличаем вызов страницы от нашего: обёртки ставятся на прототипы ПОСЛЕ теневых переопределений
// (`shadow/prototype-overrides.ts`), то есть снаружи них — через прототип ходит страница. Сама тень и
// мост управляют настоящим элементом только оригиналами, снятыми на document_start
// (`shadow/originals.ts`), в обход прототипа, поэтому наши вызовы в запись не попадают.
// Условие: ставить ПОСЛЕ `captureShadowEnvironment`.

import { hasPrototype } from "../shadow/intrinsics.ts";
import { isObject, wrapMethod, type OddityReporter } from "../prototype-wrap.ts";
import { jsonNumber, type JsonValue } from "./format.ts";
import type { StreamRecorder } from "./recorder.ts";
import { wrapSetterRaw } from "./wrap-extras.ts";

/** Окно с элементами (в тестах — фейк). */
export interface ElementRecordingScope {
  readonly HTMLMediaElement?: { readonly prototype: object };
  readonly Element?: { readonly prototype: object };
  readonly MediaSource?: { readonly prototype: object };
  readonly MediaSourceHandle?: { readonly prototype: object };
}

/** Виды источника элемента по формату. */
type ElementSourceKind = "media_source_url" | "src_object_media_source" | "src_object_handle" | "src_object_other" | "src_attribute" | "none";

/** Установить запись действий страницы над элементом. */
export function installElementRecording(scope: ElementRecordingScope, recorder: StreamRecorder, reportOddity: OddityReporter): boolean {
  const mediaPrototype = scope.HTMLMediaElement?.prototype;
  if (mediaPrototype === undefined) return false;
  const isMediaElement = (candidate: unknown): candidate is object => isObject(candidate) && hasPrototype(candidate, mediaPrototype);
  const isMediaSource = (candidate: unknown): boolean => scope.MediaSource !== undefined && isObject(candidate) && hasPrototype(candidate, scope.MediaSource.prototype);
  const isHandle = (candidate: unknown): boolean => scope.MediaSourceHandle !== undefined && isObject(candidate) && hasPrototype(candidate, scope.MediaSourceHandle.prototype);

  /** Записать смену источника; `fields` — доп. поля (идентификатор MediaSource, если он известен). */
  const recordSource = (element: object, kind: ElementSourceKind, ms?: string): void => {
    const fields: { [field: string]: JsonValue } = { el: recorder.elementIdOf(element), kind };
    if (ms !== undefined) fields["ms"] = ms;
    recorder.record("element_source", fields);
  };

  /** Что значит строка в `src`: blob-адрес нашего MediaSource, пусто (источник снят) или другой адрес. */
  const recordSrcValue = (element: object, value: unknown): void => {
    const text = String(value);
    if (text === "") {
      recordSource(element, "none");
      return;
    }
    const ms = recorder.mediaSourceOfObjectUrl(text);
    if (ms !== undefined) recordSource(element, "media_source_url", ms);
    else recordSource(element, "src_attribute");
  };

  wrapSetterRaw(mediaPrototype, "src", reportOddity, (element, value) => {
    if (isMediaElement(element)) recordSrcValue(element, value);
  });
  wrapSetterRaw(mediaPrototype, "srcObject", reportOddity, (element, value) => {
    if (!isMediaElement(element)) return;
    if (value === null || value === undefined) recordSource(element, "none");
    else if (isMediaSource(value)) {
      const ms = recorder.knownMediaSourceId(value as object);
      recordSource(element, "src_object_media_source", ms);
    } else if (isHandle(value)) recordSource(element, "src_object_handle");
    else recordSource(element, "src_object_other");
  });
  // Атрибут `src` меняют и через setAttribute/removeAttribute (родной `src` YouTube/Twitch не трогают).
  const elementPrototype = scope.Element?.prototype;
  if (elementPrototype !== undefined) {
    wrapMethod(elementPrototype, "setAttribute", reportOddity, (element, args) => {
      if (isMediaElement(element) && String(args[0]).toLowerCase() === "src") recordSrcValue(element, args[1]);
    });
    wrapMethod(elementPrototype, "removeAttribute", reportOddity, (element, args) => {
      if (isMediaElement(element) && String(args[0]).toLowerCase() === "src") recordSource(element, "none");
    });
  }

  const elementCall = (name: string, op: string): void => {
    wrapMethod(mediaPrototype, name, reportOddity, (element) => {
      if (isMediaElement(element)) recorder.record(op, { el: recorder.elementIdOf(element) });
    });
  };
  elementCall("play", "element_play");
  elementCall("pause", "element_pause");
  elementCall("load", "element_load");
  // Перемотка: запись `currentTime` и `fastSeek` (Firefox/Safari) — одно и то же действие страницы.
  wrapSetterRaw(mediaPrototype, "currentTime", reportOddity, (element, value) => {
    if (isMediaElement(element)) recorder.record("element_seek", { el: recorder.elementIdOf(element), value: jsonNumber(Number(value)) });
  });
  wrapMethod(mediaPrototype, "fastSeek", reportOddity, (element, args) => {
    if (isMediaElement(element)) recorder.record("element_seek", { el: recorder.elementIdOf(element), value: jsonNumber(Number(args[0])) });
  });
  wrapSetterRaw(mediaPrototype, "playbackRate", reportOddity, (element, value) => {
    if (isMediaElement(element)) recorder.record("element_rate", { el: recorder.elementIdOf(element), value: jsonNumber(Number(value)) });
  });
  return true;
}
