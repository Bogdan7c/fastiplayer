// Чтение ответа XMLHttpRequest оригинальными геттерами, снятыми на document_start.
//
// Где лежат оригиналы. Обычно — прямо на `XMLHttpRequest.prototype`. Но чужой код, успевший раньше нас,
// может подменить сам конструктор подклассом: так делают скриптлеты блокировщиков рекламы
// (`self.XMLHttpRequest = class extends self.XMLHttpRequest { open(…) { … super.open(…) } get response() { … } }`).
// Тогда `window.XMLHttpRequest.prototype` — прототип подкласса: на нём лежат ЕГО `open`, `response`,
// `responseText`, а родные геттеры остались этажом выше. Раньше мы искали геттеры только на первом
// этаже, не находили `status`, и хук молча не ставился — на живом YouTube с блокировщиком не доходил ни
// один документ субтитров (бэклог п. 78, сессия 16b5).
//
// Поэтому ищем РОДНОЙ прототип: первый объект цепочки, который сам владеет всем нужным набором. Всё
// берём только с него, а не «с первого, у кого нашлось имя»: иначе читатели сняли бы геттеры подкласса
// (блокировщик отдаёт через них подчищенный ответ) и обернули бы его `open` вместо родного. Обёртка на
// родном `open` видит и запросы через подкласс: его `super.open(…)` ищет метод на родном прототипе в
// момент вызова.

/** Окно с XMLHttpRequest; нет — XHR не наблюдается. */
export interface XhrReadersWindow {
  readonly XMLHttpRequest?: { readonly prototype: object };
}

export interface XhrReaders {
  /** Родной прототип XMLHttpRequest: на нём ставится обёртка `open` (см. шапку файла). */
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

/** Геттеры, которыми читается ответ: все должны быть своими у родного прототипа. */
const NATIVE_GETTER_NAMES: readonly string[] = ["status", "responseURL", "responseType", "responseText", "response"];

/** Методы родного прототипа: `open` оборачивает хук, `getResponseHeader` читает заголовки. */
const NATIVE_METHOD_NAMES: readonly string[] = ["open", "getResponseHeader"];

/**
 * Сколько этажей цепочки прототипов осматриваем. У браузера родной прототип — первый этаж, под
 * подклассом блокировщика — второй; запас на несколько вложенных подмен. Предел нужен, чтобы
 * чужая (возможно, зацикленная прокси) цепочка не повесила страницу на document_start.
 */
const MAX_PROTOTYPE_FLOORS = 8;

function getterOf(prototype: object, name: string): Function {
  const getter = Object.getOwnPropertyDescriptor(prototype, name)?.get;
  if (getter === undefined) throw new TypeError(`${name} getter is missing`);
  return getter;
}

/** Объект сам владеет всеми родными геттерами и методами XMLHttpRequest. */
function ownsNativeXhrMembers(candidate: object): boolean {
  const ownsGetters = NATIVE_GETTER_NAMES.every((name) => Object.getOwnPropertyDescriptor(candidate, name)?.get !== undefined);
  return ownsGetters && NATIVE_METHOD_NAMES.every((name) => typeof Object.getOwnPropertyDescriptor(candidate, name)?.value === "function");
}

/**
 * Родной прототип XMLHttpRequest: первый этаж цепочки от `start`, который сам владеет всем набором.
 * `null` — такого нет (окно без XHR или чужая конструкция, которую мы читать не умеем).
 */
function nativeXhrPrototypeAbove(start: object): object | null {
  let candidate: object | null = start;
  for (let floor = 0; candidate !== null && floor < MAX_PROTOTYPE_FLOORS; floor += 1) {
    if (ownsNativeXhrMembers(candidate)) return candidate;
    candidate = Object.getPrototypeOf(candidate) as object | null;
  }
  return null;
}

export function captureXhrReaders(win: XhrReadersWindow): XhrReaders | null {
  if (win.XMLHttpRequest === undefined) return null;
  try {
    const prototype = nativeXhrPrototypeAbove(win.XMLHttpRequest.prototype);
    if (prototype === null) return null;
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
