// «Рука-функция» адаптера сайта (сессия 14b, решение 282): изолированный мир просит MAIN-мир вызвать
// функцию плеера сайта, MAIN-мир отвечает её результатом. Нужна сайтам, которые не верят кликам из кода
// (YouTube в 09d, решение 207): у их плеера есть свои функции «выбрать качество», «следующее видео».
//
// Путь — личный порт миров (участок 1), фону эти сообщения не уходят никогда.
//
// Безопасность:
// - MAIN-мир исполняет ТОЛЬКО действия из белого списка адаптера (`registry/page-world.ts`). Имя сайта и
//   действия — ключи этого списка, а не код: изолированный мир не может попросить «вызови что угодно»;
// - ответ приходит из мира страницы, а страница — враг (инвариант 1). Поэтому ответ проверяет пограничник
//   изолированного мира теми же строгими правилами, что здесь: форма, глубина, число узлов, длина строк.
//   Подделка ответа даёт адаптеру не больше, чем он и так читает из DOM страницы.

/** Значение, которое может вернуть рука: «JSON без хитростей» — только данные, никаких функций и прототипов. */
export type HandValue = null | boolean | number | string | readonly HandValue[] | { readonly [key: string]: HandValue };

/** Наибольшая глубина вложенности значения: меню вида `[{id, label, selected}]` — это глубина 2. */
export const MAX_HAND_VALUE_DEPTH = 4;

/** Наибольшее число узлов значения (все числа, строки, массивы и объекты вместе). */
export const MAX_HAND_VALUE_NODES = 2_048;

/** Наибольшая длина одной строки значения, в кодовых единицах UTF-16. */
export const MAX_HAND_STRING_LENGTH = 1_024;

/** Наибольшая длина имени сайта и действия: это ключи белого списка, длинных не бывает. */
export const MAX_HAND_NAME_LENGTH = 64;

/** Наибольшая длина аргумента действия (номер варианта, ключ качества). */
export const MAX_HAND_ARGUMENT_LENGTH = 256;

/** Наибольший номер запроса (u32, как у остальных вопросов порта). */
const MAX_HAND_REQUEST_ID = 0xffff_ffff;

/** Изолированный мир → MAIN-мир: вызвать действие `action` адаптера `site` с аргументом. */
export interface AdapterHandCall {
  readonly kind: "adapter_hand_call";
  readonly request_id: number;
  readonly site: string;
  readonly action: string;
  /** Аргумент действия; `null` — без аргумента. */
  readonly argument: string | null;
}

/**
 * Чем кончился вызов:
 * - `done` — функция сайта отработала, `value` — её результат (`null`, если она ничего не вернула);
 * - `no_such_action` — у адаптера нет такого действия или у сайта нет такой функции (страница другая);
 * - `failed` — функция бросила или вернула то, что не укладывается в `HandValue`.
 */
export type AdapterHandOutcome =
  | { readonly kind: "done"; readonly value: HandValue }
  | { readonly kind: "no_such_action" }
  | { readonly kind: "failed" };

/** MAIN-мир → изолированный мир: ответ на вызов с этим номером. */
export interface AdapterHandReply {
  readonly kind: "adapter_hand_reply";
  readonly request_id: number;
  readonly outcome: AdapterHandOutcome;
}

type Fields = Readonly<Record<string, unknown>>;

/** Простой объект ровно с этими собственными полями. */
function exactFields(candidate: unknown, names: readonly string[]): Fields | undefined {
  if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) return undefined;
  const ownNames = Object.keys(candidate);
  if (ownNames.length !== names.length || !names.every((name) => Object.hasOwn(candidate, name))) return undefined;
  return candidate as Fields;
}

function isRequestId(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 1 && value <= MAX_HAND_REQUEST_ID;
}

/** Имя сайта или действия: латиница, цифры, `-` и `_`, без пробелов. */
function isHandName(value: unknown): value is string {
  return typeof value === "string" && value.length >= 1 && value.length <= MAX_HAND_NAME_LENGTH && /^[a-z0-9_-]+$/u.test(value);
}

/**
 * Копия значения в форме `HandValue` или `undefined`, если оно не укладывается в лимиты. Копия, а не
 * проверка «на месте»: у значения страницы могут быть геттеры, прокси и прототипы, а дальше уходит
 * только наш простой объект. Ключи объекта — только собственные перечислимые, без `__proto__`.
 */
export function copyHandValue(candidate: unknown): HandValue | undefined {
  let nodes = 0;
  function copy(value: unknown, depth: number): HandValue | undefined {
    nodes += 1;
    if (nodes > MAX_HAND_VALUE_NODES || depth > MAX_HAND_VALUE_DEPTH) return undefined;
    if (value === null || typeof value === "boolean") return value;
    if (typeof value === "number") return Number.isFinite(value) ? value : undefined;
    if (typeof value === "string") return value.length <= MAX_HAND_STRING_LENGTH ? value : undefined;
    if (Array.isArray(value)) {
      const items: HandValue[] = [];
      for (const item of value as readonly unknown[]) {
        const copied = copy(item, depth + 1);
        if (copied === undefined) return undefined;
        items.push(copied);
      }
      return items;
    }
    if (typeof value !== "object") return undefined;
    // Прототип чужой (класс, Map, Date) — это не данные, а объект страницы: не берём.
    const prototype: unknown = Object.getPrototypeOf(value);
    if (prototype !== null && prototype !== Object.prototype && Object.getPrototypeOf(prototype) !== null) return undefined;
    // Обычный объект безопасен: ключ `__proto__` отвергается ниже, поэтому присваивание не трогает прототип.
    const fields: Record<string, HandValue> = {};
    for (const key of Object.keys(value)) {
      if (key === "__proto__" || key.length > MAX_HAND_NAME_LENGTH) return undefined;
      const copied = copy((value as Fields)[key], depth + 1);
      if (copied === undefined) return undefined;
      fields[key] = copied;
    }
    return fields;
  }
  try {
    return copy(candidate, 0);
  } catch {
    // Геттер или прокси страницы бросил: такого значения у нас нет.
    return undefined;
  }
}

/** Разобрать вызов (MAIN-мир не доверяет форме, см. `page-command-guard.ts`). `undefined` — не по форме. */
export function parseAdapterHandCall(candidate: unknown): AdapterHandCall | undefined {
  const fields = exactFields(candidate, ["kind", "request_id", "site", "action", "argument"]);
  if (fields === undefined || fields["kind"] !== "adapter_hand_call") return undefined;
  const { request_id: requestId, site, action, argument } = fields;
  if (!isRequestId(requestId) || !isHandName(site) || !isHandName(action)) return undefined;
  if (argument !== null && (typeof argument !== "string" || argument.length > MAX_HAND_ARGUMENT_LENGTH)) return undefined;
  return { kind: "adapter_hand_call", request_id: requestId, site, action, argument };
}

/** Похоже ли сообщение порта на ответ руки (по одному виду, без разбора): такие сообщения берёт клиент руки. */
export function looksLikeAdapterHandReply(candidate: unknown): boolean {
  return typeof candidate === "object" && candidate !== null && (candidate as Fields)["kind"] === "adapter_hand_reply";
}

function parseOutcome(candidate: unknown): AdapterHandOutcome | undefined {
  const done = exactFields(candidate, ["kind", "value"]);
  if (done !== undefined) {
    if (done["kind"] !== "done") return undefined;
    const value = copyHandValue(done["value"]);
    return value === undefined ? undefined : { kind: "done", value };
  }
  const bare = exactFields(candidate, ["kind"]);
  if (bare?.["kind"] === "no_such_action") return { kind: "no_such_action" };
  if (bare?.["kind"] === "failed") return { kind: "failed" };
  return undefined;
}

/** Разобрать ответ MAIN-мира (пограничник изолированного мира). `undefined` — не по форме. */
export function parseAdapterHandReply(candidate: unknown): AdapterHandReply | undefined {
  const fields = exactFields(candidate, ["kind", "request_id", "outcome"]);
  if (fields === undefined || fields["kind"] !== "adapter_hand_reply" || !isRequestId(fields["request_id"])) return undefined;
  const outcome = parseOutcome(fields["outcome"]);
  return outcome === undefined ? undefined : { kind: "adapter_hand_reply", request_id: fields["request_id"], outcome };
}
