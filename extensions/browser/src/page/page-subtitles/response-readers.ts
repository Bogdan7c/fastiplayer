// Чтение ответа `fetch` оригинальными методами, снятыми на document_start.
//
// Страница подменяет `Response.prototype.clone`, `Headers.prototype.get`, геттеры `url`/`status`/…
// и читателей потока — все они снимаются заранее. Нет какой-то части в окне (старый браузер, фейк
// теста) — читателей нет вовсе (`null`), `fetch` не наблюдается.

import type { SubtitleResponseCandidate } from "./response-filter.ts";

/** Окно с типами Fetch/Streams. */
export interface ResponseReadersWindow {
  readonly Response?: { readonly prototype: object };
  readonly Headers?: { readonly prototype: object };
  readonly ReadableStream?: { readonly prototype: object };
  readonly ReadableStreamDefaultReader?: { readonly prototype: object };
}

/** Один шаг чтения потока. */
export interface BodyStep {
  readonly done: boolean;
  readonly value: unknown;
}

/** Читатель тела копии ответа. */
export interface BodyReader {
  read(): Promise<BodyStep>;
  /** Отказаться от остатка (потолок превышен). Ошибки отмены глотаются здесь же. */
  cancel(): void;
}

export interface ResponseReaders {
  /** Что известно об ответе до чтения тела; бросает на чужом объекте. */
  describe(response: object): SubtitleResponseCandidate;
  /** Копия ответа и читатель её тела; бросает, если тело уже занято/нет. Исходный ответ остаётся странице целым. */
  openClonedBody(response: object): BodyReader;
}

function methodOf(prototype: object, name: string): Function {
  const method = Object.getOwnPropertyDescriptor(prototype, name)?.value as unknown;
  if (typeof method !== "function") throw new TypeError(`${name} is missing`);
  return method;
}

function getterOf(prototype: object, name: string): Function {
  const getter = Object.getOwnPropertyDescriptor(prototype, name)?.get;
  if (getter === undefined) throw new TypeError(`${name} getter is missing`);
  return getter;
}

/** Дождаться промиса и проглотить его отказ: нигде не вешаем `.catch` на промисы браузера (страница может подменить). */
async function settle(promise: unknown): Promise<void> {
  try {
    await promise;
  } catch {
    // Отмена потока не удалась — читать больше нечего, это не ошибка для страницы.
  }
}

/** Снять читателей ответа; `null` — окружение их не даёт. */
export function captureResponseReaders(win: ResponseReadersWindow): ResponseReaders | null {
  if (win.Response === undefined || win.Headers === undefined || win.ReadableStream === undefined || win.ReadableStreamDefaultReader === undefined) return null;
  try {
    const responseProto = win.Response.prototype;
    const url = getterOf(responseProto, "url");
    const status = getterOf(responseProto, "status");
    const type = getterOf(responseProto, "type");
    const headers = getterOf(responseProto, "headers");
    const body = getterOf(responseProto, "body");
    const clone = methodOf(responseProto, "clone");
    const headerGet = methodOf(win.Headers.prototype, "get");
    const getReader = methodOf(win.ReadableStream.prototype, "getReader");
    const readerRead = methodOf(win.ReadableStreamDefaultReader.prototype, "read");
    const readerCancel = methodOf(win.ReadableStreamDefaultReader.prototype, "cancel");
    const headerValue = (response: object, name: string): string | null => {
      const value: unknown = Reflect.apply(headerGet, Reflect.apply(headers, response, []), [name]);
      return typeof value === "string" ? value : null;
    };
    return {
      describe(response) {
        return {
          url: String(Reflect.apply(url, response, [])),
          status: Number(Reflect.apply(status, response, [])),
          responseType: String(Reflect.apply(type, response, [])),
          contentType: headerValue(response, "content-type"),
          contentLength: headerValue(response, "content-length"),
        };
      },
      openClonedBody(response) {
        const copy = Reflect.apply(clone, response, []) as object;
        const stream: unknown = Reflect.apply(body, copy, []);
        if (typeof stream !== "object" || stream === null) throw new TypeError("the response has no body");
        const reader = Reflect.apply(getReader, stream, []) as object;
        return {
          read: () => Reflect.apply(readerRead, reader, []) as Promise<BodyStep>,
          cancel: () => void settle(Reflect.apply(readerCancel, reader, []) as unknown),
        };
      },
    };
  } catch {
    return null;
  }
}
