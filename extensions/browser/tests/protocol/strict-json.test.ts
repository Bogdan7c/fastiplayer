// Строгий разбор: то, что `JSON.parse` делает неправильно для протокола, и границы синтаксиса.

import assert from "node:assert/strict";
import { test } from "node:test";

import { decodePageSession, decodePlayerSession } from "../../src/protocol/index.ts";
import { parseStrictJson } from "../../src/protocol/json/parse.ts";

function kindOf(result: { readonly ok: boolean; readonly error?: { readonly kind: string } }): string {
  return result.ok ? "ok" : (result.error?.kind ?? "?");
}

const GENERATION_STARTED = '{"type":"generation_started","generation":1}';

test("дерево хранит порядок ключей, повторы и исходный текст чисел", () => {
  const tree = parseStrictJson('{"b":1,"a":2,"b":12345678901234567890}').root;
  assert.equal(tree.kind, "object");
  if (tree.kind !== "object") return;
  assert.deepEqual(
    tree.entries.map(([key]) => key),
    ["b", "a", "b"],
  );
  const last = tree.entries[2]?.[1];
  assert.deepEqual(last, { kind: "number", text: "12345678901234567890", overflow: false });
});

test("синтаксис: всё, что не является ровно одним JSON-значением, - syntax", () => {
  const broken = [
    "",
    "   ",
    "{",
    '{"type":"play"',
    '{"type":"play"} {}',
    '{"type":"play"}x',
    "{'type':'play'}",
    '{"type":"play",}',
    '{"type":"play" "a":1}',
    '{"a":01}',
    '{"a":1.}',
    '{"a":.5}',
    '{"a":+1}',
    '{"a":1e}',
    '{"a":-}',
    '{"a":NaN}',
    '{"a":Infinity}',
    '{"a":"\\x"}',
    '{"a":"tab\there"}', // неэкранированная табуляция в строке
    '{"a":"\\u12"}',
    '{"a":/*c*/1}',
    '{"a":tru}',
    "﻿{}", // BOM не пропускается молча
  ];
  for (const text of broken) {
    assert.throws(() => parseStrictJson(text), { kind: "syntax" }, JSON.stringify(text));
  }
});

test("синтаксис: допустимые формы разбираются", () => {
  const good = ['{"a":-0}', '{"a":1E+2}', '{"a":1.5e-3}', ' \t\r\n{"a":[]}\n', '{"a":"\\ud83c\\udfac\\u0041\\n\\/"}', '{"a":null,"b":true,"c":false}'];
  for (const text of good) {
    assert.doesNotThrow(() => parseStrictJson(text), JSON.stringify(text));
  }
  const emoji = parseStrictJson('"\\ud83c\\udfac"');
  assert.deepEqual(emoji.root, { kind: "string", text: "🎬", malformed: false });
});

test("одиночные суррогаты и числа вне f64 не рвут разбор синтаксиса, но помечаются", () => {
  for (const text of ['{"a":"\\ud800"}', '{"a":"\\udc00"}', '{"a":"\\ud800\\u0041"}', '{"a":1e999}', '{"\\ud800":1}']) {
    const parsed = parseStrictJson(text);
    assert.equal(parsed.hasDeferredProblem, true, text);
  }
  assert.equal(parseStrictJson('{"a":"x","b":1e5}').hasDeferredProblem, false);
});

test("такой мусор отвергается как syntax только там, где значение читается по-настоящему (как у serde_json)", () => {
  // Значение поля читается: syntax.
  assert.equal(kindOf(decodePageSession('{"type":"generation_started","generation":1,"x":"\\ud800"}')), "syntax");
  assert.equal(kindOf(decodePageSession('{"type":"generation_started","generation":1e999}')), "syntax");
  // Зонд `type` пропускает чужие значения, поэтому его причины раньше: нет type, повтор type, неизвестный type.
  assert.equal(kindOf(decodePageSession('{"generation":1,"x":"\\ud800"}')), "missing_type");
  assert.equal(kindOf(decodePageSession('{"type":"nope","x":1e999}')), "unknown_type");
  assert.equal(kindOf(decodePageSession('{"type":"a","type":"b","x":"\\ud800"}')), "duplicate_field");
  // Но сам ключ и значение type зонд читает по-настоящему.
  assert.equal(kindOf(decodePageSession('{"\\ud800":1,"type":"end_of_stream"}')), "syntax");
  assert.equal(kindOf(decodePageSession('{"type":"\\ud800"}')), "syntax");
});

test("вложенность глубже 127 уровней - syntax, 127 - принимается", () => {
  assert.doesNotThrow(() => parseStrictJson("[".repeat(127) + "]".repeat(127)));
  assert.throws(() => parseStrictJson("[".repeat(128) + "]".repeat(128)), { kind: "syntax" });
});

test("повтор поля - duplicate_field, хотя JSON.parse молча взял бы последнее", () => {
  const duplicated = '{"type":"generation_started","generation":1,"generation":2}';
  assert.equal(JSON.parse(duplicated).generation, 2, "предпосылка: JSON.parse теряет повтор");
  assert.equal(kindOf(decodePageSession(duplicated)), "duplicate_field");
  assert.equal(kindOf(decodePageSession('{"type":"generation_started","type":"generation_started","generation":1}')), "duplicate_field");
});

test("целые больше 2^53 не округляются молча: это number_out_of_range, а не принятое значение", () => {
  // 9007199254740993 JSON.parse превратил бы в ...992. Строгий разбор читает текст и отвергает число.
  const unsafe = '{"type":"seek","seek_id":1,"target_us":9007199254740993}';
  assert.equal(kindOf(decodePlayerSession(unsafe)), "number_out_of_range");
  assert.equal(kindOf(decodePlayerSession('{"type":"seek","seek_id":1,"target_us":9007199254740991}')), "ok");
  // Целое, не помещающееся даже в u64, serde читает как дробное.
  assert.equal(kindOf(decodePlayerSession('{"type":"seek","seek_id":1,"target_us":18446744073709551616}')), "wrong_value_type");
});

test("корень не объект - not_an_object для любого другого значения JSON", () => {
  for (const text of ["[]", '["play"]', '"play"', "1", "null", "true", '[{"type":"play"}]']) {
    assert.equal(kindOf(decodePlayerSession(text)), "not_an_object", text);
  }
});

test("BOM и байты не UTF-8: not_utf8 для байтов, syntax для BOM", () => {
  const withBom = new Uint8Array([0xef, 0xbb, 0xbf, ...new TextEncoder().encode(GENERATION_STARTED)]);
  assert.equal(kindOf(decodePageSession(withBom)), "syntax");
  assert.equal(kindOf(decodePageSession(new Uint8Array([0x7b, 0xff, 0x7d]))), "not_utf8");
  // Оборванная многобайтовая последовательность
  assert.equal(kindOf(decodePageSession(new Uint8Array([0x7b, 0xd0, 0x7d]))), "not_utf8");
});

test("порядок причин как в Rust: syntax раньше not_an_object, type раньше полей", () => {
  assert.equal(kindOf(decodePlayerSession("[1,")), "syntax");
  assert.equal(kindOf(decodePlayerSession('{"seek_id":1}')), "missing_type");
  assert.equal(kindOf(decodePlayerSession('{"type":5}')), "wrong_value_type");
  assert.equal(kindOf(decodePlayerSession('{"type":"nope","extra":1}')), "unknown_type");
  // Лишнее поле раньше недостающего, недостающее - в самом конце.
  assert.equal(kindOf(decodePlayerSession('{"type":"seek","bogus":1}')), "unknown_field");
  assert.equal(kindOf(decodePlayerSession('{"type":"seek","seek_id":1}')), "missing_field");
});

