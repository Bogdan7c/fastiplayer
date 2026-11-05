// Поиск `MediaSourceHandle` в данных сообщения воркера (сторона MAIN-мира, сессия 09b).
//
// Сайт кладёт handle куда угодно: `{ type: "handle", handle }`, `[handle]`, `{ arg: { handle } }`.
// Обход ограничен глубиной и числом просмотренных значений: сообщение воркера может нести огромные
// массивы (статистика, кадры), а этот обход идёт на каждом сообщении, на потоке страницы.
//
// Безопасность чтения: данные пришли структурным клонированием — это простые объекты и массивы
// этого мира с собственными свойствами-данными; читаются только собственные перечислимые ключи,
// поэтому геттеры, которые страница могла повесить на `Object.prototype`, не вызываются. `Map`,
// `Set`, двоичные данные и прочие объекты-хозяева не обходятся (handle в них — честно «не нашли»).
//
// Цена (замер 09c, на сообщение; для сравнения — структурная копия того же сообщения, её браузер делает
// и без нас): маленькое сообщение 0,8 мкс при копии 1,3 мкс; 300 ключей 10 мкс при копии 52 мкс; массив
// 1 млн элементов 3 мкс при копии 18 мс. 5 000 сообщений в секунду по 300 ключей — 5 % ядра при 26 % на
// копии. Единственное, что не ограничивается пределами ниже, — список ключей объекта: движок строит
// его целиком (и `Object.keys`, и `for…in`), объект в 1 млн ключей — 170 мс при копии 630 мс. Это всё
// равно меньше копии и бьёт только по вкладке самой страницы, поэтому отдельного предела нет.

/** Пределы обхода (именованные, чтобы тесты и ревью видели, откуда числа). */
export interface HandleScanLimits {
  /** Глубина вложенности: 0 — само сообщение. */
  readonly maxDepth: number;
  /** Сколько значений всего просмотреть. */
  readonly maxValues: number;
}

/**
 * Пределы по умолчанию. Handle у настоящих плееров лежит на глубине 1–2 (Twitch, MDN-пример);
 * 4 уровня и 256 значений покрывают это с запасом и стоят микросекунды на сообщение.
 */
export const DEFAULT_HANDLE_SCAN_LIMITS: HandleScanLimits = { maxDepth: 4, maxValues: 256 };

/** Что нашёл обход. */
export type HandleScan =
  | { readonly kind: "none" }
  | { readonly kind: "one"; readonly handle: object }
  /** Больше одного handle в сообщении: сопоставить с MediaSource воркера однозначно нельзя. */
  | { readonly kind: "several" };

/** Проверки типа, снятые на document_start (без `instanceof` страницы). */
export interface HandleScanReaders {
  readonly isMediaSourceHandle: (candidate: unknown) => boolean;
  /** Простой объект или массив, который можно обходить (не вид на байты, не хозяин). */
  readonly isTraversable: (candidate: object) => boolean;
}

export function scanForMediaSourceHandles(message: unknown, readers: HandleScanReaders, limits: HandleScanLimits = DEFAULT_HANDLE_SCAN_LIMITS): HandleScan {
  let found: object | null = null;
  // Очередь обхода в ширину; длина ограничена `maxValues`, поэтому огромный массив сайта не копируется.
  const queue: { readonly value: unknown; readonly depth: number }[] = [{ value: message, depth: 0 }];
  for (let position = 0; position < queue.length; position += 1) {
    const entry = queue[position];
    if (entry === undefined) break;
    const { value, depth } = entry;
    if (typeof value !== "object" || value === null) continue;
    if (readers.isMediaSourceHandle(value)) {
      if (found !== null) return { kind: "several" };
      found = value;
      continue;
    }
    if (depth >= limits.maxDepth || !readers.isTraversable(value)) continue;
    const room = limits.maxValues - queue.length;
    for (const child of childrenOf(value, room)) queue.push({ value: child, depth: depth + 1 });
  }
  return found === null ? { kind: "none" } : { kind: "one", handle: found };
}

/** До `room` дочерних значений: у массива — по индексам (без списка ключей), у объекта — по своим ключам. */
function childrenOf(container: object, room: number): unknown[] {
  if (room <= 0) return [];
  const children: unknown[] = [];
  if (Array.isArray(container)) {
    const length = Math.min(container.length, room);
    for (let index = 0; index < length; index += 1) children.push(container[index]);
    return children;
  }
  for (const key of Object.keys(container)) {
    if (children.length >= room) break;
    children.push((container as Readonly<Record<string, unknown>>)[key]);
  }
  return children;
}
