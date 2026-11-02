// Целые числа протокола. Зеркало поведения serde_json при чтении u16/u32/u64/i64.
//
// Правила (закреплены общими фикстурами):
// - не число, дробь (`1.5`, `1e3`, даже `1.0`) или `-0` -> `wrong_value_type`;
// - целое вне диапазона своего типа (отрицательное для беззнакового, 2^40 для u32) -> `number_out_of_range`;
// - целое, которое не влезает даже в u64/i64, serde читает как дробное -> `wrong_value_type`;
// - целое в диапазоне типа, но вне правил поля (например > 2^53 − 1 у времени) -> `number_out_of_range`.
//
// Текст числа читается как BigInt: большие значения не теряют точность.

import { ProtocolFailure } from "./errors.ts";
import type { JsonValue } from "./json/value.ts";
import type { Codec } from "./schema.ts";

/** Машинные целые типы Rust, которыми записаны поля протокола. */
export type IntegerPrimitive = "u16" | "u32" | "u64" | "i64";

const PRIMITIVE_RANGES: Readonly<Record<IntegerPrimitive, { min: bigint; max: bigint }>> = {
  u16: { min: 0n, max: 0xffffn },
  u32: { min: 0n, max: 0xffff_ffffn },
  u64: { min: 0n, max: 0xffff_ffff_ffff_ffffn },
  i64: { min: -(2n ** 63n), max: 2n ** 63n - 1n },
};

/** Границы, в которые serde_json кладёт целое как целое (иначе читает как дробное). */
const SERDE_INTEGER_MIN = PRIMITIVE_RANGES.i64.min;
const SERDE_INTEGER_MAX = PRIMITIVE_RANGES.u64.max;

const INTEGER_TEXT = /^-?\d+$/;

/** Читает целое по правилам машинного типа; `null` у значения — не число и не целое. */
function readPrimitiveInteger(json: JsonValue, primitive: IntegerPrimitive): bigint {
  if (json.kind !== "number" || !INTEGER_TEXT.test(json.text) || json.text === "-0") {
    throw new ProtocolFailure("wrong_value_type");
  }
  const value = BigInt(json.text);
  if (value < SERDE_INTEGER_MIN || value > SERDE_INTEGER_MAX) {
    throw new ProtocolFailure("wrong_value_type");
  }
  const range = PRIMITIVE_RANGES[primitive];
  if (value < range.min || value > range.max) {
    throw new ProtocolFailure("number_out_of_range");
  }
  return value;
}

/** Правило поля поверх машинного типа. */
export interface IntegerRule {
  readonly primitive: IntegerPrimitive;
  /** Наименьшее допустимое значение (по умолчанию — наименьшее у типа). */
  readonly min?: number;
  /** Наибольшее допустимое значение (по умолчанию — наибольшее у типа). */
  readonly max?: number;
}

/**
 * Целое поле, помещающееся в `number` (≤ 2^53 − 1). Значения выше допустимого
 * — `number_out_of_range`.
 */
export function safeInteger(rule: IntegerRule): Codec<number> {
  const lowest = BigInt(rule.min ?? Number.MIN_SAFE_INTEGER);
  const highest = BigInt(rule.max ?? Number.MAX_SAFE_INTEGER);
  return {
    decode(json) {
      const value = readPrimitiveInteger(json, rule.primitive);
      if (value < lowest || value > highest) {
        throw new ProtocolFailure("number_out_of_range");
      }
      return Number(value);
    },
    encode: (value) => value,
  };
}

/** Целое без дополнительных правил: весь диапазон u16/u32, для u64 — до 2^53 − 1. */
export function plainInteger(primitive: "u16" | "u32"): Codec<number> {
  return safeInteger({ primitive, min: 0 });
}
