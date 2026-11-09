// Чтение ответа XMLHttpRequest оригинальными геттерами, снятыми на document_start.

/** Окно с XMLHttpRequest; нет — XHR не наблюдается. */
export interface XhrReadersWindow {
  readonly XMLHttpRequest?: { readonly prototype: object };
}

export interface XhrReaders {
  /** Прототип, на котором ставится обёртка `open`. */
  readonly prototype: object;
  status(xhr: object): number;
  /** `responseURL` (пусто, если запрос не дошёл до ответа). */
  responseUrl(xhr: object): string;
  /** `responseType`: `""`, `"text"`, `"json"`, `"arraybuffer"`, `"blob"`, `"document"`. */
  responseType(xhr: object): string;
  /** Заголовок ответа; `null` — нет или скрыт CORS. */
  header(xhr: object, name: string): string | null;
  /** `responseText` — бросает, если `responseType` не текстовый. */
  responseText(xhr: object): string;
  /** `response` — тип зависит от `responseType`. */
  responseValue(xhr: object): unknown;
}

function getterOf(prototype: object, name: string): Function {
  const getter = Object.getOwnPropertyDescriptor(prototype, name)?.get;
  if (getter === undefined) throw new TypeError(`${name} getter is missing`);
  return getter;
}

export function captureXhrReaders(win: XhrReadersWindow): XhrReaders | null {
  if (win.XMLHttpRequest === undefined) return null;
  try {
    const prototype = win.XMLHttpRequest.prototype;
    const status = getterOf(prototype, "status");
    const responseUrl = getterOf(prototype, "responseURL");
    const responseType = getterOf(prototype, "responseType");
    const responseText = getterOf(prototype, "responseText");
    const response = getterOf(prototype, "response");
    const getHeader = Object.getOwnPropertyDescriptor(prototype, "getResponseHeader")?.value as Function | undefined;
    if (typeof getHeader !== "function") return null;
    return {
      prototype,
      status: (xhr) => Number(Reflect.apply(status, xhr, [])),
      responseUrl: (xhr) => String(Reflect.apply(responseUrl, xhr, [])),
      responseType: (xhr) => String(Reflect.apply(responseType, xhr, [])),
      header: (xhr, name) => {
        const value: unknown = Reflect.apply(getHeader, xhr, [name]);
        return typeof value === "string" ? value : null;
      },
      responseText: (xhr) => String(Reflect.apply(responseText, xhr, [])),
      responseValue: (xhr) => Reflect.apply(response, xhr, []) as unknown,
    };
  } catch {
    return null;
  }
}
