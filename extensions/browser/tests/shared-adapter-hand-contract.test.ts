// Договор руки-функции (14b): MAIN-мир принимает по порту только вызов строгой формы, а значение страницы
// дальше уходит только копией в пределах лимитов.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseChannelMessage } from "../src/page/page-command-guard.ts";
import { copyHandValue, MAX_HAND_ARGUMENT_LENGTH, MAX_HAND_NAME_LENGTH, MAX_HAND_VALUE_NODES } from "../src/shared/adapter-hand-contract.ts";

const CALL = { kind: "adapter_hand_call", request_id: 7, site: "bench-series", action: "select_quality", argument: "video-720" } as const;

describe("вызов руки по порту MAIN-мира", () => {
  it("годный вызов разбирается общим разбором порта", () => {
    assert.deepEqual(parseChannelMessage(CALL), CALL);
    assert.deepEqual(parseChannelMessage({ ...CALL, argument: null }), { ...CALL, argument: null });
  });

  it("лишнее поле, номер 0, имя с пробелом или заглавными, длинные имя и аргумент, аргумент не строка — мусор", () => {
    const garbage: unknown[] = [
      { ...CALL, extra: 1 },
      { ...CALL, request_id: 0 },
      { ...CALL, site: "bench series" },
      { ...CALL, action: "selectQuality" },
      { ...CALL, action: "a".repeat(MAX_HAND_NAME_LENGTH + 1) },
      { ...CALL, argument: "x".repeat(MAX_HAND_ARGUMENT_LENGTH + 1) },
      { ...CALL, argument: 720 },
    ];
    for (const candidate of garbage) assert.equal(parseChannelMessage(candidate), null, JSON.stringify(candidate).slice(0, 80));
  });
});

describe("копия значения страницы", () => {
  it("данные копируются, а не передаются ссылкой: правка оригинала копию не трогает", () => {
    const original = { list: [{ id: "a", selected: true }], count: 2 };
    const copy = copyHandValue(original);
    original.list[0]!.id = "changed";
    assert.deepEqual(copy, { list: [{ id: "a", selected: true }], count: 2 });
  });

  it("бесконечность, функция, символ, undefined внутри, слишком много узлов — значения нет", () => {
    const tooMany = Array.from({ length: MAX_HAND_VALUE_NODES }, () => 1);
    for (const candidate of [Number.POSITIVE_INFINITY, () => 1, Symbol("s"), [undefined], tooMany]) {
      assert.equal(copyHandValue(candidate), undefined);
    }
  });

  it("объект без прототипа (Object.create(null)) — данные, берётся", () => {
    const bare = Object.assign(Object.create(null) as Record<string, unknown>, { id: "a" });
    assert.deepEqual(copyHandValue(bare), { id: "a" });
  });
});
