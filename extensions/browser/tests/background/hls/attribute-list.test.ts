// Разбор списка атрибутов: кавычки с запятыми, перечисления, порча.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { formatAttributeList, parseAttributeList } from "../../../src/background/handoff/hls/attribute-list.ts";

describe("список атрибутов", () => {
  it("разбирает строки в кавычках с запятыми и значения без кавычек", () => {
    const parsed = parseAttributeList('BANDWIDTH=1280000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1280x720,URI="a.m3u8"');
    assert.deepEqual(parsed, {
      ok: true,
      attributes: [
        { name: "BANDWIDTH", value: "1280000", quoted: false },
        { name: "CODECS", value: "avc1.4d401f,mp4a.40.2", quoted: true },
        { name: "RESOLUTION", value: "1280x720", quoted: false },
        { name: "URI", value: "a.m3u8", quoted: true },
      ],
    });
  });

  it("пустая строка - пустой список; пустое значение в кавычках допустимо", () => {
    assert.deepEqual(parseAttributeList(""), { ok: true, attributes: [] });
    assert.deepEqual(parseAttributeList('URI=""'), { ok: true, attributes: [{ name: "URI", value: "", quoted: true }] });
  });

  const malformed: readonly [string, string][] = [
    ["нет знака равенства", "URI"],
    ["нет имени", '="x"'],
    ["не закрыта кавычка", 'URI="abc'],
    ["висячая запятая", 'A=1,'],
    ["пустое значение без кавычек", "A=,B=2"],
    ["мусор после кавычек", 'A="x"y'],
    ["пробел в значении без кавычек", "A=1 2"],
    ["кавычка внутри значения без кавычек", 'A=1"2'],
    ["двойная запятая", "A=1,,B=2"],
  ];
  for (const [name, text] of malformed) {
    it(`отказывает: ${name}`, () => {
      assert.deepEqual(parseAttributeList(text), { ok: false });
    });
  }

  it("обратная сборка сохраняет порядок и кавычки", () => {
    const source = 'METHOD=AES-128,URI="k.bin",IV=0x01';
    const parsed = parseAttributeList(source);
    assert.ok(parsed.ok);
    assert.equal(formatAttributeList(parsed.attributes), source);
  });
});
