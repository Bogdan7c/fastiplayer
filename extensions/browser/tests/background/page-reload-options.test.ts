// Раздел «Page reload» страницы настроек (16c, решение 305): значение из настройки, запись годного ввода, подсказка.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { mountPageReloadWaitControl, PAGE_RELOAD_WAIT_HINT } from "../../src/options/page-reload-wait-control.ts";
import { PAGE_RELOAD_WAIT_SETTINGS_KEY } from "../../src/shared/page-reload-wait-settings.ts";
import { FakeElement, memoryStore, withFakeDocument } from "./auto-handoff-lists-fake-dom.ts";

async function settle(): Promise<void> {
  for (let round = 0; round < 10; round += 1) await new Promise((resolve) => setImmediate(resolve));
}

async function mounted(initial: Record<string, unknown> = {}) {
  const storage = memoryStore(initial);
  const container = new FakeElement("div");
  await withFakeDocument(() => mountPageReloadWaitControl(container as unknown as HTMLElement, storage.store));
  return { container, storage };
}

describe("раздел «Page reload»", () => {
  it("показывает срок из настройки, без настройки — 10", async () => {
    assert.equal((await mounted({ [PAGE_RELOAD_WAIT_SETTINGS_KEY]: { seconds: 25 } })).container.byId("page-reload-wait-seconds").value, "25");
    assert.equal((await mounted()).container.byId("page-reload-wait-seconds").value, "10");
  });

  it("годный ввод пишется, негодный — подсказка и ни одной записи", async () => {
    const { container, storage } = await mounted();
    const input = container.byId("page-reload-wait-seconds");
    input.value = "0";
    input.fire("change");
    await settle();
    assert.deepEqual(storage.values.get(PAGE_RELOAD_WAIT_SETTINGS_KEY), { seconds: 0 });
    const writes = storage.state.writes;
    input.value = "90";
    input.fire("change");
    await settle();
    assert.equal(container.byId("page-reload-wait-status").textContent, PAGE_RELOAD_WAIT_HINT);
    assert.equal(storage.state.writes, writes);
  });

  it("сбой чтения и записи — текстом, не молча", async () => {
    const storage = memoryStore();
    storage.state.failReads = true;
    const container = new FakeElement("div");
    await withFakeDocument(() => mountPageReloadWaitControl(container as unknown as HTMLElement, storage.store));
    assert.match(container.byId("page-reload-wait-status").textContent ?? "", /Could not read the setting: .*read broke/);
    storage.state.failWrites = true;
    const input = container.byId("page-reload-wait-seconds");
    input.value = "5";
    input.fire("change");
    await settle();
    assert.match(container.byId("page-reload-wait-status").textContent ?? "", /Could not save: .*write broke/);
  });
});
