// Мини-язык описания схем сообщений: «кодек» знает, как разобрать значение из
// дерева JSON и как привести значение обратно к виду для записи.
//
// Правила разбора повторяют serde из Rust (error.rs, codec.rs), чтобы виды причин
// совпадали с общими фикстурами:
// - поля обрабатываются в порядке появления во входе, первая же ошибка прерывает
//   разбор: неизвестное поле -> `unknown_field`, повтор -> `duplicate_field`,
//   плохое значение -> ошибка значения; «нет поля» проверяется в самом конце;
// - необязательное поле (`optionalField`, с протокола 1.10) может отсутствовать: тогда его нет
//   и в разобранном объекте. Если ключ есть, значение разбирается строго своим кодеком, и
//   `null` - `wrong_value_type`, как у любого поля (зеркало `strict_shape::optional_field`);
// - у перечислений с полем-меткой (`type`/`kind`) метка сначала ищется по ВСЕМ
//   полям (повтор метки, незнакомое значение), и только потом разбираются поля
//   варианта. Так делает serde: он буферизует содержимое и разбирает его позже;
// - у «пустых» вариантов (`{"type":"attached"}`) лишнее поле — тоже `unknown_field`.
//
// Запись: `encode` расставляет поля в порядке объявления схемы, поэтому одно и то
// же сообщение всегда даёт одни и те же байты (общие фикстуры сверяются байт в байт).
// Он НЕ проверяет значения: полную проверку делает разбор записанного текста
// (см. message-codec.ts), так отправитель не шлёт того, что получатель отвергнет.

import { ProtocolFailure } from "./errors.ts";
import type { JsonObject, JsonValue } from "./json/value.ts";

/** Разбор и приведение к виду для записи для значений типа `T`. */
export interface Codec<T> {
  /** @throws ProtocolFailure на первой ошибке (путь к полю дописывают вложенные кодеки). */
  decode(json: JsonValue): T;
  /** Значение в виде, готовом для `JSON.stringify` (поля в порядке схемы). */
  encode(value: T): unknown;
}

/**
 * Кодек НЕОБЯЗАТЕЛЬНОГО поля (первое - `clock.generation`, протокол 1.10): ключа может не быть.
 *
 * Метка `presence` нужна разбору структуры: только у такого поля отсутствие ключа - не
 * `missing_field`. Типы требуют её ровно у необязательных свойств (`FieldCodecs`), поэтому
 * забыть пометку или навесить её на обязательное поле не выйдет - не соберётся.
 */
export interface OptionalFieldCodec<T> extends Codec<T> {
  readonly presence: "optional";
}

/** Кодек обязательного поля: любой кодек, кроме помеченного необязательным. */
type RequiredFieldCodec<T> = Codec<T> & { readonly presence?: never };

/** Необязательно ли свойство `K` у `T` (`generation?:`). */
type IsOptionalKey<T, K extends keyof T> = Partial<Pick<T, K>> extends Pick<T, K> ? true : false;

/**
 * Набор кодеков полей структуры `T`: ключи в порядке записи. Необязательному свойству нужен
 * `OptionalFieldCodec` (`optionalField(...)`), обязательному - обычный кодек.
 */
export type FieldCodecs<T> = {
  readonly [K in keyof T]-?: IsOptionalKey<T, K> extends true
    ? OptionalFieldCodec<Exclude<T[K], undefined>>
    : RequiredFieldCodec<T[K]>;
};

/**
 * Делает поле необязательным: нет ключа - нет свойства в разобранном объекте (не `undefined`:
 * с `exactOptionalPropertyTypes` это разные формы, и запись обратно должна дать те же байты).
 * Ключ есть - значение разбирает `present` со всеми его правилами; `null` он отвергает сам как
 * значение не того типа. Почему не принимать `null` за «нет»: тогда у смысла «номера нет» было
 * бы две формы на проводе, и `{"generation":null}` не записался бы обратно байт в байт. Так же
 * решено в Rust (`strict_shape::optional_field`). Запись: отсутствующее значение не пишется
 * (`encodeFields` пропускает `undefined`).
 */
export function optionalField<T>(present: Codec<T>): OptionalFieldCodec<T> {
  return {
    presence: "optional",
    decode: (json) => present.decode(json),
    encode: (value) => present.encode(value),
  };
}

/** Помечено ли поле необязательным (`optionalField`). */
function isOptionalField(codec: Codec<unknown>): boolean {
  return (codec as Partial<OptionalFieldCodec<unknown>>).presence === "optional";
}

// ---------------------------------------------------------------- перечисления

/** Перечисление из строковых значений (snake_case). */
export function stringEnum<T extends string>(allowedValues: readonly T[]): Codec<T> {
  const allowed: ReadonlySet<string> = new Set(allowedValues);
  return {
    decode(json) {
      // Значение перечисления — только строка. Внешняя форма serde `{"вариант": null}`
      // (со знакомым или незнакомым ключом) — значение не того типа: форма проверяется
      // раньше значения, как в Rust после сессии 07 (`strict_label!`, фикстуры `shape-*`).
      if (json.kind !== "string") {
        throw new ProtocolFailure("wrong_value_type");
      }
      if (!allowed.has(json.text)) {
        throw new ProtocolFailure("unknown_value");
      }
      return json.text as T;
    },
    encode: (value) => value,
  };
}

// ---------------------------------------------------------------- списки

/** Список значений; длину и уникальность проверяет `validate` сообщения. */
export function listOf<T>(element: Codec<T>): Codec<readonly T[]> {
  return {
    decode(json) {
      if (json.kind !== "array") {
        throw new ProtocolFailure("wrong_value_type");
      }
      return json.items.map((item, index) => {
        try {
          return element.decode(item);
        } catch (failure) {
          throw withPath(failure, `[${index}]`);
        }
      });
    },
    encode: (value) => (Array.isArray(value) ? value.map((item: T) => element.encode(item)) : value),
  };
}

// ---------------------------------------------------------------- структуры

/**
 * Читает пары «поле — значение» по набору кодеков: порядок обработки, повторы и
 * неизвестные поля как у serde с `deny_unknown_fields`. Возвращает объект с
 * полями в порядке объявления схемы.
 */
function decodeFields<T extends object>(
  entries: readonly (readonly [string, JsonValue])[],
  fields: FieldCodecs<T>,
): T {
  const fieldCodecs = fields as unknown as Readonly<Record<string, Codec<unknown>>>;
  const decoded = new Map<string, unknown>();
  for (const [key, value] of entries) {
    const codec = Object.hasOwn(fieldCodecs, key) ? fieldCodecs[key] : undefined;
    if (codec === undefined) {
      throw new ProtocolFailure("unknown_field").prependPath(key);
    }
    if (decoded.has(key)) {
      throw new ProtocolFailure("duplicate_field").prependPath(key);
    }
    try {
      decoded.set(key, codec.decode(value));
    } catch (failure) {
      throw withPath(failure, key);
    }
  }
  const result: Record<string, unknown> = {};
  for (const [key, codec] of Object.entries(fieldCodecs)) {
    if (decoded.has(key)) {
      result[key] = decoded.get(key);
    } else if (!isOptionalField(codec)) {
      throw new ProtocolFailure("missing_field").prependPath(key);
    }
    // Необязательного поля нет во входе - нет и свойства в итоге (а не `key: undefined`).
  }
  return result as T;
}

function encodeFields<T extends object>(value: T, fields: FieldCodecs<T>): Record<string, unknown> {
  const fieldCodecs = fields as unknown as Readonly<Record<string, Codec<unknown>>>;
  const source = value as Readonly<Record<string, unknown>>;
  const written: Record<string, unknown> = {};
  for (const key of Object.keys(fieldCodecs)) {
    const codec = fieldCodecs[key];
    const fieldValue = source[key];
    // Нет поля — не записываем: у необязательного это и есть форма «нет значения», а у
    // обязательного разбор записанного текста честно скажет `missing_field`.
    if (codec !== undefined && fieldValue !== undefined) {
      written[key] = codec.encode(fieldValue);
    }
  }
  return written;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Вложенный объект с фиксированным набором полей. Массив вместо объекта — `wrong_value_type`. */
export function struct<T extends object>(fields: FieldCodecs<T>): Codec<T> {
  return {
    decode(json) {
      if (json.kind !== "object") {
        throw new ProtocolFailure("wrong_value_type");
      }
      return decodeFields(json.entries, fields);
    },
    encode: (value) => (isRecord(value) ? encodeFields(value as T, fields) : value),
  };
}

// ---------------------------------------------------------------- размеченные объединения

/** Поля варианта без поля-метки. */
type VariantFields<Union, Tag extends string, Name extends string> = Omit<
  Extract<Union, { readonly [K in Tag]: Name }>,
  Tag
>;

/** Кодеки полей всех вариантов объединения `Union` с меткой `Tag`. */
export type VariantCodecs<Union extends { readonly [K in Tag]: string }, Tag extends string> = {
  readonly [Name in Union[Tag]]: FieldCodecs<VariantFields<Union, Tag, Name>>;
};

/** Кодек размеченного объединения и список его меток (нужен для проверки `type` до разбора). */
export interface TaggedUnionCodec<T> extends Codec<T> {
  readonly knownTags: readonly string[];
}

/**
 * Перечисление с полем-меткой: `{"type":"hello",...}` или `{"kind":"at",...}`.
 *
 * Метка в итоговом объекте стоит первой, как в Rust.
 */
export function taggedUnion<Union extends { readonly [K in Tag]: string }, Tag extends string>(
  tag: Tag,
  variants: VariantCodecs<Union, Tag>,
): TaggedUnionCodec<Union> {
  const variantFields = variants as unknown as Readonly<Record<string, FieldCodecs<object>>>;
  return {
    knownTags: Object.keys(variantFields),
    decode(json) {
      if (json.kind !== "object") {
        throw new ProtocolFailure("wrong_value_type");
      }
      const { tagName, otherEntries } = splitTag(json, tag, variantFields);
      const fields = variantFields[tagName];
      if (fields === undefined) {
        throw new ProtocolFailure("unknown_value").prependPath(tag);
      }
      return { [tag]: tagName, ...decodeFields(otherEntries, fields) } as unknown as Union;
    },
    encode(value) {
      if (!isRecord(value)) {
        return value;
      }
      const tagValue = value[tag];
      const fields = typeof tagValue === "string" && Object.hasOwn(variantFields, tagValue) ? variantFields[tagValue] : undefined;
      if (fields === undefined) {
        return value;
      }
      return { [tag]: tagValue, ...encodeFields(value as object, fields as FieldCodecs<object>) };
    },
  };
}

/**
 * Первый проход serde: находит метку среди всех полей (повтор метки -> `duplicate_field`,
 * не строка -> `wrong_value_type`, незнакомое значение -> `unknown_value`), остальные
 * поля отдаёт как есть для второго прохода.
 */
function splitTag(
  json: JsonObject,
  tag: string,
  variantFields: Readonly<Record<string, unknown>>,
): { tagName: string; otherEntries: (readonly [string, JsonValue])[] } {
  let tagName: string | undefined;
  const otherEntries: (readonly [string, JsonValue])[] = [];
  for (const entry of json.entries) {
    const [key, value] = entry;
    if (key !== tag) {
      otherEntries.push(entry);
      continue;
    }
    if (tagName !== undefined) {
      throw new ProtocolFailure("duplicate_field").prependPath(tag);
    }
    if (value.kind !== "string") {
      // Метка варианта — только строка. Число (в том числе «индекс варианта», который
      // производный разбор serde когда-то принимал) — значение не того типа, как и
      // в Rust после сессии 07 (`strict_shape.rs`, фикстуры `shape-*`).
      throw new ProtocolFailure("wrong_value_type").prependPath(tag);
    }
    if (!Object.hasOwn(variantFields, value.text)) {
      throw new ProtocolFailure("unknown_value").prependPath(tag);
    }
    tagName = value.text;
  }
  if (tagName === undefined) {
    throw new ProtocolFailure("missing_field").prependPath(tag);
  }
  return { tagName, otherEntries };
}

/** Дописывает имя поля к пути ошибки; чужие исключения пробрасывает как есть. */
function withPath(failure: unknown, segment: string): unknown {
  return failure instanceof ProtocolFailure ? failure.prependPath(segment) : failure;
}
