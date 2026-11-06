// Поздний скрипт (сессия 10d): Firefox сам вставляет скрипт в открытую вкладку, и начало потока упущено.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { classifyMediaUrl } from "../../src/page/element-source.ts";
import { scriptArrivalOf, type ScriptArrival } from "../../src/page/script-arrival.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { createFakePage } from "./support/fake-page.ts";
import { handshake, startHarness, type IsolatedSide } from "./support/bridge-harness.ts";

const BLOB = "blob:https://films.example/1234";
const notMediaSource = (): boolean => false;
const knownMediaSource = (): boolean => true;

describe("когда встал скрипт", () => {
  it("документ ещё грузится — скрипт вовремя", () => {
    assert.equal(scriptArrivalOf("loading", "https://films.example/"), "at_document_start");
  });

  it("документ уже загружен — браузер вставил скрипт позже", () => {
    assert.equal(scriptArrivalOf("complete", "https://films.example/"), "after_page_started");
    assert.equal(scriptArrivalOf("interactive", "https://films.example/"), "after_page_started");
  });

  it("about:blank не считается опоздавшим: у него complete бывает и на старте", () => {
    assert.equal(scriptArrivalOf("complete", "about:blank"), "at_document_start");
    assert.equal(scriptArrivalOf("complete", "about:srcdoc"), "at_document_start");
  });
});

describe("незнакомый blob-адрес", () => {
  it("при позднем скрипте: поток начался без нас, это лечит перезагрузка", () => {
    assert.deepEqual(classifyMediaUrl(BLOB, null, notMediaSource, "after_page_started"), { kind: "unsupported", form: "started_before_script" });
  });

  it("при скрипте вовремя: настоящий Blob, а не упущенный поток", () => {
    assert.deepEqual(classifyMediaUrl(BLOB, null, notMediaSource, "at_document_start"), { kind: "unsupported", form: "blob_file" });
  });

  it("свой MediaSource остаётся своим и при позднем скрипте: созданный после хука поток передаётся", () => {
    assert.deepEqual(classifyMediaUrl(BLOB, null, knownMediaSource, "after_page_started"), { kind: "none" });
  });

  it("обычный файл при позднем скрипте передаётся без перезагрузки", () => {
    const url = "https://cdn.example/movie.mp4";
    assert.deepEqual(classifyMediaUrl(url, null, notMediaSource, "after_page_started"), { kind: "file", url });
  });
});

/** Спросить MAIN-мир про видео так, как спрашивает изолированный мир. */
async function describeCandidates(isolated: IsolatedSide): Promise<readonly VideoCandidate[]> {
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(1);
  const reply = isolated.received[0];
  assert.ok(reply !== undefined && reply.kind === "candidates");
  return reply.candidates;
}

describe("описание видео при позднем скрипте (настоящий MAIN-мир на фейковой странице)", () => {
  async function describeWith(scriptArrival: ScriptArrival): Promise<readonly VideoCandidate[]> {
    const page = createFakePage();
    const harness = startHarness({ page, scriptArrival });
    page.addVideo().src = BLOB;
    const isolated = handshake(page);
    try {
      return await describeCandidates(isolated);
    } finally {
      assert.equal(harness.bridge.hooked, true);
      isolated.close();
    }
  }

  it("элемент с незнакомым blob-адресом описан как упущенный поток", async () => {
    const [candidate] = await describeWith("after_page_started");
    assert.deepEqual(candidate?.media, { kind: "unsupported", form: "started_before_script" });
  });

  it("при скрипте вовремя тот же элемент — обычный Blob", async () => {
    const [candidate] = await describeWith("at_document_start");
    assert.deepEqual(candidate?.media, { kind: "unsupported", form: "blob_file" });
  });
});
