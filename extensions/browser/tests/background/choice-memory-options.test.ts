// Раздел «Remembered choices» страницы настроек (14b2): число сериалов от фона, «Forget» через фон, объём —
// настройка с проверкой ввода. Фейковый DOM и хранилище в памяти; фон — функция, отвечающая как настоящий.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { CAPACITY_HINT, mountChoiceMemoryControl } from "../../src/options/choice-memory-control.ts";
import type { ChoiceMemoryRequest } from "../../src/shared/choice-memory-contract.ts";
import { CHOICE_MEMORY_SETTINGS_KEY } from "../../src/shared/choice-memory-settings.ts";
import { FakeElement, memoryStore, withFakeDocument } from "./auto-handoff-lists-fake-dom.ts";

async function settle(): Promise<void> {
  for (let round = 0; round < 10; round += 1) await new Promise((resolve) => setImmediate(resolve));
}

/** Фон с памятью из `series` сериалов: считает и стирает. */
function fakeBackground(series: number, options: { readonly forgetFails?: boolean } = {}) {
  const requests: ChoiceMemoryRequest["kind"][] = [];
  let remembered = series;
  const askBackground = async (request: ChoiceMemoryRequest): Promise<unknown> => {
    requests.push(request.kind);
    if (request.kind === "count_remembered_choices") return { kind: "count", series: remembered };
    if (options.forgetFails === true) return { kind: "failed", reason: "storage is full" };
    remembered = 0;
    return { kind: "forgotten" };
  };
  return { askBackground, requests };
}

async function mounted(series: number, initial: Record<string, unknown> = {}, options: { readonly forgetFails?: boolean } = {}) {
  const storage = memoryStore(initial);
  const background = fakeBackground(series, options);
  const container = new FakeElement("div");
  await withFakeDocument(() => mountChoiceMemoryControl(container as unknown as HTMLElement, { store: storage.store, askBackground: background.askBackground }));
  return { container, storage, background };
}

describe("раздел «Remembered choices»", () => {
  it("показывает объём из настройки и число сериалов от фона", async () => {
    const { container } = await mounted(3, { [CHOICE_MEMORY_SETTINGS_KEY]: { capacity: 120 } });
    assert.equal(container.byId("choice-memory-capacity").value, "120");
    assert.equal(container.byId("choice-memory-count").textContent, "3 series remembered.");
  });

  it("«Forget remembered choices» просит фон, показывает итог и новое число", async () => {
    const { container, background } = await mounted(4);
    container.byId("choice-memory-forget").fire("click");
    await settle();
    assert.deepEqual(background.requests, ["count_remembered_choices", "forget_remembered_choices", "count_remembered_choices"]);
    assert.equal(container.byId("choice-memory-status").textContent, "Remembered choices are forgotten.");
    assert.equal(container.byId("choice-memory-count").textContent, "0 series remembered.");
  });

  it("фон отказал — причина текстом, число прежнее", async () => {
    const { container } = await mounted(2, {}, { forgetFails: true });
    container.byId("choice-memory-forget").fire("click");
    await settle();
    assert.equal(container.byId("choice-memory-status").textContent, "Could not forget: storage is full");
    assert.equal(container.byId("choice-memory-count").textContent, "2 series remembered.");
  });

  it("объём: годное число пишется, негодное — подсказка и ни одной записи", async () => {
    const { container, storage } = await mounted(0);
    const input = container.byId("choice-memory-capacity");
    input.value = "3";
    input.fire("change");
    await settle();
    assert.equal(container.byId("choice-memory-status").textContent, CAPACITY_HINT);
    assert.equal(storage.state.writes, 0);
    input.value = "800";
    input.fire("change");
    await settle();
    assert.deepEqual(storage.values.get(CHOICE_MEMORY_SETTINGS_KEY), { capacity: 800 });
  });
});
