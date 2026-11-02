// Дерево строгого JSON-разбора.
//
// В отличие от `JSON.parse`, дерево сохраняет то, что нужно строгому протоколу:
// порядок ключей, ПОВТОРЫ ключей (JSON.parse молча берёт последний) и исходный
// текст чисел (JSON.parse теряет точность у целых больше 2^53).

/** Объект: пары «ключ — значение» в порядке появления, повторы сохранены. */
export interface JsonObject {
  readonly kind: "object";
  readonly entries: readonly (readonly [key: string, value: JsonValue])[];
  /** Номера пар, чей ключ записан с одиночным суррогатом (см. `ParsedJson`). */
  readonly malformedKeys: ReadonlySet<number>;
}

/** Массив. */
export interface JsonArray {
  readonly kind: "array";
  readonly items: readonly JsonValue[];
}

/** Строка (уже без экранирования). */
export interface JsonString {
  readonly kind: "string";
  readonly text: string;
  /** В записи есть одиночный суррогат `\uD800`: строку нельзя прочитать как значение (см. `ParsedJson`). */
  readonly malformed: boolean;
}

/** Число в исходной записи: тип (целое/дробь) и точность решает тот, кто читает поле. */
export interface JsonNumber {
  readonly kind: "number";
  readonly text: string;
  /** Число не помещается в f64 (`1e999`): прочитать как значение нельзя (см. `ParsedJson`). */
  readonly overflow: boolean;
}

/** `true`, `false`, `null`. */
export interface JsonLiteral {
  readonly kind: "literal";
  readonly text: "true" | "false" | "null";
}

export type JsonValue = JsonObject | JsonArray | JsonString | JsonNumber | JsonLiteral;

/** Человекочитаемое имя вида значения — для сообщений и отладки. */
export function describeJsonKind(value: JsonValue): string {
  return value.kind === "literal" ? value.text : value.kind;
}

/**
 * Итог разбора синтаксиса.
 *
 * serde_json проверяет синтаксис в два приёма: пропуск значения (`IgnoredAny`) терпит
 * одиночные суррогаты в `\uXXXX` и числа вне f64, а настоящее чтение значения их
 * отвергает как `syntax`. От этого зависит ВИД причины (например, `missing_type` раньше
 * `syntax` у `{"a":"\ud800"}`), поэтому разборщик не падает на таких местах сразу,
 * а помечает узлы и отдаёт флаг `hasDeferredProblem`.
 */
export interface ParsedJson {
  readonly root: JsonValue;
  /** В дереве есть хотя бы один помеченный узел (строка, ключ или число). */
  readonly hasDeferredProblem: boolean;
}
