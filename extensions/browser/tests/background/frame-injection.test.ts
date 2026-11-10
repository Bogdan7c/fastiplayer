// Внедрение во фреймы по одному (сессия 15c): исходы фреймов, общий срок, лимит фреймов, итог для
// позднего входа и возврата страницы, выбор живых фреймов из списка браузера.

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { answersOfEachFrame, askEachFrame, liveFrameIds, MAX_FRAMES_PER_TAB, type FrameInjection } from "../../src/background/frame-injection/frame-injection.ts";
import { createFakeFrameInjection, type FakeFrame } from "./frame-injection-fake.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const TAB = 12;
const DEADLINE_MS = 5_000;
const INJECTION: FrameInjection<[string]> = { func: (name: string) => name, args: ["probe"] };

function ask(frames: readonly FakeFrame[], listError?: Error) {
  const clock = createFakeClock();
  const port = createFakeFrameInjection(() => frames, () => listError);
  return { clock, port, replies: askEachFrame(TAB, port, clock.scheduler, INJECTION, DEADLINE_MS) };
}

describe("askEachFrame", () => {
  it("каждый фрейм получает своё внедрение с теми же аргументами; ответы, отказы и пустые ответы различимы", async () => {
    const { port, replies } = ask([
      { frameId: 0, result: "top" },
      { frameId: 4, refuses: true },
      { frameId: 5, rejects: true },
      { frameId: 6, result: undefined },
    ]);
    assert.deepEqual(await replies, {
      kind: "replies",
      replies: [
        { kind: "answered", frameId: 0, result: "top" },
        { kind: "refused", frameId: 4, error: "the frame did not accept the injection" },
        { kind: "refused", frameId: 5, error: "Error: Cannot access contents of the page." },
        { kind: "answered", frameId: 6, result: undefined },
      ],
      framesTruncated: false,
    });
    assert.deepEqual(port.injections(), [0, 4, 5, 6].map((frameId) => ({ tabId: TAB, frameId, args: ["probe"] })));
  });

  it("молчащий фрейм не задерживает ответ до срока: до срока опрос не кончается, по сроку фрейм — silent", async () => {
    const { clock, replies } = ask([{ frameId: 0, result: "top" }, { frameId: 6, silent: true }]);
    let settled = false;
    void replies.then(() => (settled = true));
    await flushPromises();
    clock.advance(DEADLINE_MS - 1);
    await flushPromises();
    assert.equal(settled, false, "срок ещё не вышел");
    clock.advance(1);
    assert.deepEqual(await replies, {
      kind: "replies",
      replies: [{ kind: "answered", frameId: 0, result: "top" }, { kind: "silent", frameId: 6 }],
      framesTruncated: false,
    });
  });

  it("без молчащих срок не ждётся, а таймер снимается (часы не держат ничего)", async () => {
    const { clock, replies } = ask([{ frameId: 0, result: "top" }]);
    assert.equal((await replies).kind, "replies");
    assert.equal(clock.liveTimers(), 0);
  });

  it("список фреймов не получен — frames_unknown, внедрений нет", async () => {
    const { port, replies } = ask([{ frameId: 0, result: "top" }], new Error("No tab with id: 12."));
    assert.deepEqual(await replies, { kind: "frames_unknown", error: "Error: No tab with id: 12." });
    assert.deepEqual(port.injections(), []);
  });

  it("повторы номеров в списке браузера спрашиваются один раз", async () => {
    const { port, replies } = ask([{ frameId: 0, result: "a" }, { frameId: 0, result: "a" }]);
    assert.equal((await replies).kind, "replies");
    assert.equal(port.injections().length, 1);
  });

  it("сверх лимита фреймы не спрашиваются; верхний — всегда", async () => {
    const frames: FakeFrame[] = Array.from({ length: MAX_FRAMES_PER_TAB + 1 }, (_, index) => ({ frameId: MAX_FRAMES_PER_TAB - index, result: index }));
    const { port, replies } = ask(frames);
    const outcome = await replies;
    assert.equal(outcome.kind === "replies" && outcome.framesTruncated, true);
    assert.equal(port.injections().length, MAX_FRAMES_PER_TAB);
    assert.ok(port.injections().some((injection) => injection.frameId === 0), "верхний фрейм последний в списке, но спрошен");
  });
});

describe("answersOfEachFrame (поздний вход, возврат страницы)", () => {
  const answersOf = (frames: readonly FakeFrame[], listError?: Error) => {
    const clock = createFakeClock();
    const port = createFakeFrameInjection(() => frames, () => listError);
    return { clock, answers: answersOfEachFrame(TAB, port, clock.scheduler, INJECTION, DEADLINE_MS) };
  };

  it("ответы ответивших фреймов; отказавшие и молчащие ответа не дают", async () => {
    const { clock, answers } = answersOf([{ frameId: 0, result: [1] }, { frameId: 3, refuses: true }, { frameId: 6, silent: true }, { frameId: 7, result: [2] }]);
    await flushPromises();
    clock.advance(DEADLINE_MS);
    assert.deepEqual(await answers, [{ result: [1] }, { result: [2] }]);
  });

  it("все отказали — отказ промиса: читать на вкладке нечего", async () => {
    await assert.rejects(answersOf([{ frameId: 0, rejects: true }]).answers, /no frame of the tab accepted the injection/);
  });

  it("список фреймов не получен — отказ промиса", async () => {
    await assert.rejects(answersOf([], new Error("No tab")).answers, /the frames of the tab are unknown/);
  });

  it("все молчат — пустой ответ, а не отказ: вкладка жива, следующий круг спросит снова", async () => {
    const { clock, answers } = answersOf([{ frameId: 0, silent: true }]);
    await flushPromises();
    clock.advance(DEADLINE_MS);
    assert.deepEqual(await answers, []);
  });
});

describe("liveFrameIds (список фреймов браузера)", () => {
  it("документы вне показа (кэш «назад-вперёд», предзагрузка, удаление) не спрашиваются; Firefox без поля — все", () => {
    assert.deepEqual(
      liveFrameIds([
        { frameId: 0, documentLifecycle: "active" },
        { frameId: 1, documentLifecycle: "cached" },
        { frameId: 2, documentLifecycle: "prerender" },
        { frameId: 3, documentLifecycle: "pending_deletion" },
        { frameId: 4 },
      ]),
      [0, 4],
    );
  });

  it("`null` от браузера — вкладки нет: ошибка, а не пустая вкладка", () => {
    assert.throws(() => liveFrameIds(null), /no tab with this id/);
  });
});
