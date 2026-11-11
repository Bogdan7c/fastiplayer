// Договор протокола 1.10 (сессия 16a2 - номер поколения в тике часов, решение 311), которого не покрывает общий
// проход по manifest.json (его делает fixtures.test.ts для тех же фикстур):
// - новых сообщений нет (fixtures/message-versions.json), новое - одно необязательное поле `clock.generation`
//   (fixtures/field-versions.json); тип `clock` по-прежнему из 1.0;
// - тик пишется ровно задокументированным JSON с полем и без него (без номера - ключа нет, свойства нет);
// - `null`, строка, дробь, ноль, отрицательное и больше u32 - отказ с точной причиной; строгость формы осталась;
// - кодек необязательного поля `optionalField` сам по себе: нет ключа - нет свойства, `null` - отказ, обязательное
//   поле рядом по-прежнему `missing_field`;
// - тик с номером требует общей версии 1.10 (`playerSessionRequiredVersion`); с плеером 1.9 общая версия 1.9.
// Те же договоры проверяет Rust (crates/browser-bridge-protocol/tests/protocol_1_10.rs).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  CLOCK_GENERATION_INTRODUCED_IN,
  CURRENT_PROTOCOL_VERSION,
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_9,
  PROTOCOL_VERSION_1_10,
  decodePlayerSession,
  encodePlayerSession,
  negotiateProtocolVersion,
  playerSessionIntroducedIn,
  playerSessionRequiredVersion,
  versionIncludes,
  type PlayerSessionMessage,
} from "../../src/protocol/index.ts";
import { ProtocolFailure } from "../../src/protocol/errors.ts";
import { parseStrictJson } from "../../src/protocol/json/parse.ts";
import { plainInteger } from "../../src/protocol/numbers.ts";
import { optionalField, struct } from "../../src/protocol/schema.ts";
import { FIXTURES_ROOT } from "./fixtures-support.ts";

const readTable = (name: string): Record<string, Record<string, string>> =>
  JSON.parse(readFileSync(join(FIXTURES_ROOT, name), "utf8")) as Record<string, Record<string, string>>;

/** Тик часов с номером поколения или без него (тогда свойства нет вовсе). */
function clockTick(generation?: number): PlayerSessionMessage {
  const tick = { type: "clock", seq: 42, position_us: 11_100_000, state: "playing", rate: 1000 } as const;
  return generation === undefined ? tick : { ...tick, generation };
}

/** Тики и их точный JSON на проводе: без номера (форма 1.9 и раньше), с номером, крайние номера. */
const DOCUMENTED: readonly { json: string; value: PlayerSessionMessage }[] = [
  { json: '{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000}', value: clockTick() },
  { json: '{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":2}', value: clockTick(2) },
  { json: '{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":1}', value: clockTick(1) },
  {
    json: '{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":4294967295}',
    value: clockTick(0xffff_ffff),
  },
];

test("в 1.10 нет новых сообщений, новое - ровно поле clock.generation; экстеншен реализует 1.10", () => {
  const messages = readTable("message-versions.json");
  const newIn110 = Object.values(messages).flatMap((side) => Object.entries(side).filter(([, version]) => version === "1.10"));
  assert.deepEqual(newIn110, []);
  // Тип `clock` старый: меняется только форма тика с полем.
  assert.equal(messages["player_session"]?.["clock"], "1.0");
  assert.deepEqual(playerSessionIntroducedIn("clock"), PROTOCOL_VERSION_1_0);
  assert.deepEqual(readTable("field-versions.json"), { page_session: {}, player_session: { "clock.generation": "1.10" } });
  assert.deepEqual(CLOCK_GENERATION_INTRODUCED_IN, PROTOCOL_VERSION_1_10);
  assert.deepEqual(CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_10);
});

test("тик пишется ровно задокументированным JSON с номером и без и читается обратно", () => {
  for (const { json, value } of DOCUMENTED) {
    assert.deepEqual(encodePlayerSession(value), { ok: true, value: json });
    assert.deepEqual(decodePlayerSession(json), { ok: true, value });
  }
  // Без номера в разобранном тике нет даже свойства `generation` (а не `generation: undefined`).
  const withoutNumber = decodePlayerSession(DOCUMENTED[0]?.json ?? "");
  assert.equal(withoutNumber.ok && "generation" in withoutNumber.value, false);
  // С номером получатель видит то же поколение.
  const withNumber = decodePlayerSession('{"type":"clock","seq":7,"position_us":0,"state":"ended","rate":1000,"generation":3}');
  assert.equal(withNumber.ok && withNumber.value.type === "clock" && withNumber.value.generation, 3);
});

test("плохой номер поколения - отказ с точной причиной, строгость формы не ослабла", () => {
  const prefix = '{"type":"clock","seq":1,"position_us":0,"state":"playing","rate":1000';
  const cases: readonly [string, string][] = [
    // Одна форма «номера нет» - отсутствие ключа; `null` - значение не того типа.
    [',"generation":null}', "wrong_value_type"],
    [',"generation":"2"}', "wrong_value_type"],
    [',"generation":1.5}', "wrong_value_type"],
    [',"generation":true}', "wrong_value_type"],
    [',"generation":{"number":2}}', "wrong_value_type"],
    // Поколения считаются с 1.
    [',"generation":0}', "number_out_of_range"],
    [',"generation":-1}', "number_out_of_range"],
    [',"generation":4294967296}', "number_out_of_range"],
    [',"generation":2,"generation":3}', "duplicate_field"],
    [',"generation":2,"source_generation":2}', "unknown_field"],
  ];
  for (const [tail, kind] of cases) {
    const decoded = decodePlayerSession(prefix + tail);
    assert.equal(decoded.ok ? "ok" : decoded.error.kind, kind, prefix + tail);
  }
  // Отправитель не пишет того, что получатель отвергнет: ноль в номере не уходит на провод.
  const written = encodePlayerSession(clockTick(0));
  assert.equal(written.ok ? "ok" : written.error.kind, "number_out_of_range");
});

test("optionalField: нет ключа - нет свойства, есть - разбор по правилам поля, null - отказ, обязательное рядом - missing_field", () => {
  const codec = struct<{ readonly required: number; readonly extra?: number }>({
    required: plainInteger("u32"),
    extra: optionalField(plainInteger("u32")),
  });
  const decode = (text: string): unknown => codec.decode(parseStrictJson(text).root);
  const failureKind = (text: string): string => {
    try {
      decode(text);
      return "ok";
    } catch (failure) {
      assert.ok(failure instanceof ProtocolFailure);
      return failure.kind;
    }
  };
  assert.deepEqual(decode('{"required":1}'), { required: 1 });
  assert.deepEqual(decode('{"required":1,"extra":2}'), { required: 1, extra: 2 });
  // Порядок свойств - порядок схемы, а не входа.
  assert.deepEqual(Object.keys(decode('{"extra":2,"required":1}') as object), ["required", "extra"]);
  assert.equal(failureKind('{"required":1,"extra":null}'), "wrong_value_type");
  assert.equal(failureKind('{"required":1,"extra":-1}'), "number_out_of_range");
  assert.equal(failureKind('{"extra":2}'), "missing_field");
  // Запись: отсутствующее значение не пишется, присутствующее - пишется на своём месте.
  assert.deepEqual(codec.encode({ required: 1 }), { required: 1 });
  assert.deepEqual(codec.encode({ extra: 2, required: 1 }), { required: 1, extra: 2 });
});

test("только тик с номером требует 1.10; с плеером 1.9 общая версия 1.9 и номера в тике нет", () => {
  const [withoutNumber, ...withNumbers] = DOCUMENTED;
  assert.deepEqual(playerSessionRequiredVersion(withoutNumber?.value ?? clockTick()), PROTOCOL_VERSION_1_0);
  for (const { value } of withNumbers) {
    assert.deepEqual(playerSessionRequiredVersion(value), CLOCK_GENERATION_INTRODUCED_IN);
  }
  // Остальные сообщения плеера требуют ровно версию своего типа.
  assert.deepEqual(playerSessionRequiredVersion({ type: "show_page" }), playerSessionIntroducedIn("show_page"));

  for (const [player, extension] of [
    [PROTOCOL_VERSION_1_10, PROTOCOL_VERSION_1_9],
    [PROTOCOL_VERSION_1_9, PROTOCOL_VERSION_1_10],
  ] as const) {
    const agreed = negotiateProtocolVersion(player, extension);
    assert.deepEqual(agreed, { ok: true, version: PROTOCOL_VERSION_1_9 });
    assert.equal(agreed.ok && versionIncludes(agreed.version, CLOCK_GENERATION_INTRODUCED_IN), false);
  }
  const both = negotiateProtocolVersion(PROTOCOL_VERSION_1_10, PROTOCOL_VERSION_1_10);
  assert.equal(both.ok && versionIncludes(both.version, CLOCK_GENERATION_INTRODUCED_IN), true);
  // Двузначная minor сравнивается числом: 1.1 не включает 1.10.
  assert.equal(versionIncludes({ major: 1, minor: 1 }, PROTOCOL_VERSION_1_10), false);
});
