// Рука-функция через настоящий мост MAIN-мира (14b): вызов изолированного мира по личному порту доходит до
// функции плеера страницы, ответ приходит с тем же номером. Вызов чужого белого списка до страницы не доходит.
import assert from "node:assert/strict";
import { test } from "node:test";
import { handshake, startHarness } from "./support/bridge-harness.ts";

type Reply = { kind: string; request_id: number; outcome: unknown };

test("вызов по порту → функция плеера страницы → ответ с тем же номером и копией значения", async () => {
  const harness = startHarness();
  const calls: string[] = [];
  Object.assign(harness.page.win, {
    fastiplayerBenchSeries: {
      selectQuality: (id: string) => {
        calls.push(id);
        return true;
      },
    },
  });
  const isolated = handshake(harness.page);
  isolated.send({ kind: "adapter_hand_call", request_id: 41, site: "bench-series", action: "select_quality", argument: "video-720" });
  isolated.send({ kind: "adapter_hand_call", request_id: 42, site: "youtube", action: "select_quality", argument: "video-720" });
  try {
    await isolated.settle();
    const replies = (isolated.received as unknown[]).filter((message): message is Reply => (message as { kind?: string }).kind === "adapter_hand_reply");
    assert.deepEqual(replies, [
      { kind: "adapter_hand_reply", request_id: 41, outcome: { kind: "done", value: true } },
      { kind: "adapter_hand_reply", request_id: 42, outcome: { kind: "no_such_action" } },
    ]);
    assert.deepEqual(calls, ["video-720"], "чужой белый список до страницы не дошёл");
  } finally {
    isolated.close();
  }
});

test("функция страницы бросила — ответ failed, исключение в страницу не ушло", async () => {
  const harness = startHarness();
  Object.assign(harness.page.win, { fastiplayerBenchSeries: { nextEpisode: () => { throw new Error("site bug"); } } });
  const isolated = handshake(harness.page);
  isolated.send({ kind: "adapter_hand_call", request_id: 1, site: "bench-series", action: "next_episode", argument: null });
  try {
    await isolated.settle();
    assert.deepEqual(
      isolated.received.filter((message) => (message as { kind?: string }).kind === "adapter_hand_reply"),
      [{ kind: "adapter_hand_reply", request_id: 1, outcome: { kind: "failed" } }],
    );
  } finally {
    isolated.close();
  }
});
