// Создание пункта меню: Chrome сообщает о создании колбэком, Firefox колбэк не зовёт (живой прогон 10c: без этого
// в Firefox создавался только первый пункт из трёх). Итог — промис, который обязан закончиться.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createMenuItem, type MenuCreator } from "../../src/background/entries/menu-create.ts";

/** Промис должен закончиться за разумный срок: «зависание» превращается в провал теста, а не в вечное ожидание. */
async function settlesWithin(promise: Promise<void>, ms: number): Promise<"resolved" | "rejected" | "hung"> {
  const hang = new Promise<"hung">((resolve) => setTimeout(() => resolve("hung"), ms));
  return Promise.race([promise.then(() => "resolved" as const, () => "rejected" as const), hang]);
}

const NO_ERROR = (): undefined => undefined;

describe("createMenuItem", () => {
  it("firefox: the callback never comes, yet the item counts as created and the next item can follow", async () => {
    const created: string[] = [];
    // Фейк Firefox: пункт создаёт, колбэк не зовёт.
    const firefoxLike: MenuCreator = {
      create(properties) {
        created.push(String(properties["id"]));
        return properties["id"];
      },
    };
    const first = createMenuItem(firefoxLike, { id: "a" }, { lastError: NO_ERROR, callbackIsReliable: false });
    assert.equal(await settlesWithin(first, 200), "resolved");
    const second = createMenuItem(firefoxLike, { id: "b" }, { lastError: NO_ERROR, callbackIsReliable: false });
    assert.equal(await settlesWithin(second, 200), "resolved");
    assert.deepEqual(created, ["a", "b"]);
  });

  it("firefox: a validation error thrown at once is a rejection, not a silent success", async () => {
    const throwing: MenuCreator = {
      create() {
        throw new Error("Invalid value for contexts");
      },
    };
    const result = createMenuItem(throwing, { id: "a" }, { lastError: NO_ERROR, callbackIsReliable: false });
    await assert.rejects(result, /Invalid value for contexts/);
  });

  it("chrome: waits for the callback and resolves when there is no lastError", async () => {
    let finish: (() => void) | undefined;
    const chromeLike: MenuCreator = {
      create(_properties, callback) {
        finish = callback;
      },
    };
    const pending = createMenuItem(chromeLike, { id: "a" }, { lastError: NO_ERROR, callbackIsReliable: true });
    assert.equal(await settlesWithin(pending, 50), "hung", "must not resolve before the callback");
    finish?.();
    await pending;
  });

  it("chrome: lastError inside the callback rejects with its message", async () => {
    const chromeLike: MenuCreator = {
      create(_properties, callback) {
        callback?.();
      },
    };
    const result = createMenuItem(chromeLike, { id: "a" }, { lastError: () => ({ message: "Cannot create item with duplicate id a" }), callbackIsReliable: true });
    await assert.rejects(result, /duplicate id a/);
  });
});
