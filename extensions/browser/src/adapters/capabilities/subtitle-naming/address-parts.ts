// Разбор адреса документа субтитров на части — без глобальных `URL` и `URLSearchParams`.
//
// Код работает в MAIN-мире, где страница может подменить `URL`, `URLSearchParams` и `decodeURIComponent`
// раньше или позже нас. Поэтому разбор написан вручную на строковых операциях языка: они от страницы
// не зависят. Адрес приходит из `Response.url` / `XMLHttpRequest.responseURL` / `HTMLTrackElement.src`,
// то есть уже абсолютный и каноничный; относительные и чужие схемы нам не нужны (`null`).

/** Части абсолютного http(s)-адреса. */
export interface AddressParts {
  /** Имя хоста строчными, без логина и порта. */
  readonly host: string;
  /** Путь как в адресе (с ведущим `/`, без запроса и фрагмента); пустой путь — `/`. */
  readonly path: string;
  /** Параметры запроса в порядке адреса: имена строчными, значения декодированы (при ошибке — как есть). */
  readonly query: readonly (readonly [name: string, value: string])[];
}

/** http(s)://[логин@]хост[:порт][путь][?запрос][#фрагмент] */
const ABSOLUTE_ADDRESS_PATTERN = /^https?:\/\/([^/?#]*)([^?#]*)(?:\?([^#]*))?/i;

/** Декодировать компонент запроса; битая последовательность `%` не должна ронять разбор. */
function decodeQueryComponent(raw: string): string {
  const spaced = raw.split("+").join(" ");
  try {
    return decodeURIComponent(spaced);
  } catch {
    return spaced;
  }
}

/** Хост без логина и порта. */
function hostOf(authority: string): string {
  const withoutUser = authority.slice(authority.lastIndexOf("@") + 1);
  // IPv6 в квадратных скобках двоеточия внутри себя держит: порт отрезаем только после `]`.
  const portStart = withoutUser.lastIndexOf(":");
  const hasPort = portStart > withoutUser.lastIndexOf("]");
  return (hasPort ? withoutUser.slice(0, portStart) : withoutUser).toLowerCase();
}

/** Разобрать абсолютный http(s)-адрес; `null` — не он. */
export function parseAddressParts(address: string): AddressParts | null {
  const match = ABSOLUTE_ADDRESS_PATTERN.exec(address);
  if (match === null) return null;
  const host = hostOf(match[1] ?? "");
  if (host === "") return null;
  const path = match[2] === undefined || match[2] === "" ? "/" : match[2];
  const query: (readonly [string, string])[] = [];
  for (const pair of (match[3] ?? "").split("&")) {
    if (pair === "") continue;
    const separator = pair.indexOf("=");
    const name = separator === -1 ? pair : pair.slice(0, separator);
    const value = separator === -1 ? "" : pair.slice(separator + 1);
    query.push([decodeQueryComponent(name).toLowerCase(), decodeQueryComponent(value)]);
  }
  return { host, path, query };
}

/** Значение первого параметра запроса с таким именем (имя — строчными); `null` — параметра нет. */
export function queryValueOf(parts: AddressParts, name: string): string | null {
  for (const [candidate, value] of parts.query) {
    if (candidate === name) return value;
  }
  return null;
}
