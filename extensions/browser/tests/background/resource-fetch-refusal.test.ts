// Протокол 1.1 в экстеншене сессии 10a1: ресурсы HLS по номеркам фон ещё не качает (это 10a2),
// поэтому на каждый `fetch_resource` плеера честно отвечает `resource_fetch_finished{failed: refused}`:
// сразу, без сети и без страницы, не обрывая сеанс.

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { RangeFetch } from "../../src/background/handoff/range-fetcher.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import { decodePageSession } from "../../src/protocol/index.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { setup, startHandoff, TAB } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const REFUSED_7 = '{"type":"resource_fetch_finished","request_id":7,"outcome":{"kind":"failed","failure":{"kind":"refused"}}}';
const PLAYING_FILE: VideoCandidate = { document: "doc-7f3a", element: 4, media: { kind: "file" }, playing: true, visible_area_px: 200_000, ...DEFAULT_CANDIDATE_FACTS, file_host: "cdn.example" };

/** Сеть, которая только записывает обращения: в этих тестах к ней ходить нельзя. */
function forbiddenNetwork() {
  const calls: string[] = [];
  const fetch: RangeFetch = (url) => {
    calls.push(url);
    return Promise.reject(new Error("network must not be used"));
  };
  return { fetch, calls };
}

async function settle(): Promise<void> {
  for (let round = 0; round < 20; round += 1) await flushPromises();
}

describe("fetch_resource в 10a1: честный отказ", () => {
  it("переводчик отвечает отказом с тем же номером запроса, и ответ проходит строгий разбор", () => {
    const action = translatePlayerMessage(
      { type: "fetch_resource", request_id: 7, resource: 3, byte_range: { kind: "whole" }, max_bytes: 1024 },
      1,
    );
    assert.deepEqual(action, {
      kind: "reply",
      message: { type: "resource_fetch_finished", request_id: 7, outcome: { kind: "failed", failure: { kind: "refused" } } },
    });
    assert.ok(action.kind === "reply");
    assert.equal(decodePageSession(JSON.stringify(action.message)).ok, true);
  });

  it("MSE-сеанс: плееру уходит отказ байт в байт, сеть не тронута, сеанс жив", async () => {
    const network = forbiddenNetwork();
    const world = setup(undefined, network.fetch);
    await startHandoff(world);
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_resource", request_id: 7, resource: 0, byte_range: { kind: "whole" }, max_bytes: 4096 });
    await settle();
    assert.equal(socket.sent.at(-1), REFUSED_7);
    assert.equal(network.calls.length, 0);
    assert.equal(socket.closedWithCode, undefined);
    assert.equal(world.handoffs.activeTabIds().includes(TAB), true);
  });

  it("поколение-файл: отказ не трогает загрузчик файла (номерки не адреса файла)", async () => {
    const network = forbiddenNetwork();
    const world = setup([{ frameId: 0, result: [PLAYING_FILE] }], network.fetch);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: "https://cdn.example/movie.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    socket.receive({
      type: "fetch_resource",
      request_id: 7,
      resource: 1,
      byte_range: { kind: "slice", offset: 0, length: 4 },
      max_bytes: 4,
    });
    await settle();
    assert.equal(socket.sent.at(-1), REFUSED_7);
    assert.equal(network.calls.length, 0, "файл по номерку не качается");
    assert.equal(socket.closedWithCode, undefined);
  });
});
