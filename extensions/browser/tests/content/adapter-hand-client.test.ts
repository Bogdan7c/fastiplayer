// Клиент руки-функции в изолированном мире (14b): ответ по номеру, срок, лимит вызовов в полёте и пограничник
// ответов — страница может прислать что угодно.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { ADAPTER_HAND_TIMEOUT_MS, createAdapterHandClient, MAX_ADAPTER_HAND_CALLS_IN_FLIGHT } from "../../src/content/adapter-hand-client.ts";
import { ADAPTER_HAND_REPLIES_PER_SECOND } from "../../src/content/page-event-budget.ts";
import { MAX_HAND_STRING_LENGTH, MAX_HAND_VALUE_DEPTH, type AdapterHandCall } from "../../src/shared/adapter-hand-contract.ts";
import { createFakeClock, flushPromises } from "../background/support.ts";

function harness() {
  const clock = createFakeClock();
  const calls: AdapterHandCall[] = [];
  const client = createAdapterHandClient({ sendToPage: (call) => void calls.push(call), timers: clock.scheduler, now: clock.scheduler.now });
  const reply = (requestId: number, outcome: unknown): boolean => client.acceptPageMessage({ kind: "adapter_hand_reply", request_id: requestId, outcome });
  return { clock, calls, client, reply };
}

describe("вызов и ответ", () => {
  it("вызов несёт имя своего сайта и действие; ответ с тем же номером закрывает обещание значением", async () => {
    const { calls, client, reply } = harness();
    const result = client.handFor("bench-series").call("qualities", null);
    assert.deepEqual(calls, [{ kind: "adapter_hand_call", request_id: 1, site: "bench-series", action: "qualities", argument: null }]);
    assert.equal(reply(1, { kind: "done", value: [{ id: "a", label: "A", selected: true }] }), true);
    assert.deepEqual(await result, { kind: "done", value: [{ id: "a", label: "A", selected: true }] });
    assert.deepEqual(client.counters(), { rejectedReplies: 0, unanswered: 0 });
  });

  it("MAIN-мир молчит — через срок no_answer; поздний ответ уже ничей и считается отброшенным", async () => {
    const { clock, client, reply } = harness();
    const result = client.handFor("bench-series").call("next_episode", null);
    clock.advance(ADAPTER_HAND_TIMEOUT_MS - 1);
    await flushPromises();
    clock.advance(1);
    assert.deepEqual(await result, { kind: "no_answer" });
    reply(1, { kind: "done", value: true });
    assert.deepEqual(client.counters(), { rejectedReplies: 1, unanswered: 1 });
  });

  it("порт бросил при отправке — no_answer сразу, а не через срок", async () => {
    const clock = createFakeClock();
    const client = createAdapterHandClient({
      sendToPage: () => {
        throw new Error("port is closed");
      },
      timers: clock.scheduler,
      now: clock.scheduler.now,
    });
    assert.deepEqual(await client.handFor("bench-series").call("qualities", null), { kind: "no_answer" });
    assert.equal(clock.liveTimers(), 0);
  });

  it("вызовов в полёте не больше предела: лишний сразу no_answer и не уходит странице", async () => {
    const { calls, client } = harness();
    const hand = client.handFor("bench-series");
    for (let index = 0; index < MAX_ADAPTER_HAND_CALLS_IN_FLIGHT; index += 1) void hand.call("qualities", null);
    assert.deepEqual(await hand.call("qualities", null), { kind: "no_answer" });
    assert.equal(calls.length, MAX_ADAPTER_HAND_CALLS_IN_FLIGHT);
  });
});

describe("пограничник ответов (страница — враг)", () => {
  it("сообщения не руки не берёт: их разбирает пограничник передачи", () => {
    const { client } = harness();
    assert.equal(client.acceptPageMessage({ kind: "handoff_began", request_id: 1, outcome: "started" }), false);
    assert.equal(client.acceptPageMessage("adapter_hand_reply"), false);
    assert.equal(client.acceptPageMessage(null), false);
  });

  it("ответ не по форме, на незаданный вопрос или с недопустимым значением — отброшен, обещание ждёт своего", async () => {
    const { client, reply } = harness();
    const result = client.handFor("bench-series").call("qualities", null);
    let deep: unknown = "leaf";
    for (let depth = 0; depth <= MAX_HAND_VALUE_DEPTH; depth += 1) deep = [deep];
    const garbage: [number, unknown][] = [
      [99, { kind: "done", value: 1 }],
      [1, { kind: "done", value: 1, extra: true }],
      [1, { kind: "done" }],
      [1, { kind: "maybe" }],
      [1, { kind: "done", value: Number.NaN }],
      [1, { kind: "done", value: "x".repeat(MAX_HAND_STRING_LENGTH + 1) }],
      [1, { kind: "done", value: deep }],
      [1, { kind: "done", value: JSON.parse('{"__proto__": {"polluted": true}}') }],
    ];
    for (const [requestId, outcome] of garbage) assert.equal(reply(requestId, outcome), true);
    assert.equal(client.counters().rejectedReplies, garbage.length);
    assert.equal(({} as Record<string, unknown>)["polluted"], undefined);
    reply(1, { kind: "failed" });
    assert.deepEqual(await result, { kind: "failed" });
  });

  it("поток ответов сверх бюджета отбрасывается до разбора, даже на настоящий вопрос", async () => {
    const { client, reply } = harness();
    for (let index = 0; index < ADAPTER_HAND_REPLIES_PER_SECOND; index += 1) reply(1_000 + index, { kind: "failed" });
    const result = client.handFor("bench-series").call("qualities", null);
    reply(1, { kind: "done", value: true });
    assert.equal(client.counters().rejectedReplies, ADAPTER_HAND_REPLIES_PER_SECOND + 1);
    // Настоящий ответ съеден флудом: вызов кончится по сроку, а не чужим значением.
    let settled = false;
    void result.then(() => (settled = true));
    await flushPromises();
    assert.equal(settled, false);
  });
});
