// Передача видео, чей MediaSource живёт в выделенном воркере (сессия 09b, пакет C): кандидат,
// `begin_handoff` → повтор журнала ИЗ воркера (T0 сразу за объявлением поколения) → итоговый `page_seek` → тень → живой поток с
// ключами «w1/ms-N»; конец передачи глушит воркер; новый MediaSource воркера на том же элементе —
// новое поколение; честные отказы. Хук в воркере настоящий, связь — настоящие MessagePort Node.

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import type { WorkerHookSource } from "../../src/page/worker-capture/index.ts";
import { TRANSPARENT_POLICY } from "../../src/shared/codec-policy.ts";
import type { CandidatesReply, PageEvent } from "../../src/shared/handoff-contract.ts";
import { MAX_FORWARDED_APPEND_BYTES, parseCandidatesReply, parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { handshake, startHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { createFakePage } from "./support/fake-page.ts";
import { addWorkerSupport, closeWorkerChannels, type RunningWorker } from "./support/fake-worker-pair.ts";
import { VIDEO_MP4 } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const WORKER_HOOK: WorkerHookSource = { source: "/* hook */", policy: { current: () => TRANSPARENT_POLICY } };

afterEach(closeWorkerChannels);

async function until(condition: () => boolean, what: string, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timeout waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}

/** Как плеер сайта в воркере: новый MediaSource, его handle — странице. */
function postHandle(running: RunningWorker) {
  const mediaSource = new running.fake.FakeMediaSource();
  const handle = mediaSource.handle;
  running.fake.scope.postMessage({ type: "handle", handle }, [handle]);
  return mediaSource;
}

/** Страница: воркер с MSE (Twitch), handle на `<video>`, видео играет. */
async function workerPlayingPage() {
  const page = createFakePage();
  const pair = addWorkerSupport(page);
  const harness = startHarness({ page, workerHook: WORKER_HOOK });
  const PageWorker = (page.win as unknown as { Worker: new (url: string) => EventTarget }).Worker;
  // Как Twitch (IVS): воркер из blob-адреса страницы — только такие и оборачиваются (решение 192).
  const worker = new PageWorker(pair.siteBlobUrl("/js/player-worker.js"));
  const handles: object[] = [];
  worker.addEventListener("message", (event) => {
    const data = (event as MessageEvent).data as { type?: string; handle?: object };
    if (data.type === "handle" && data.handle !== undefined) handles.push(data.handle);
  });
  await until(() => pair.created[0]?.loadOrder.length === 2, "the worker to start");
  const running = pair.created[0];
  assert.ok(running !== undefined);
  const mediaSource = postHandle(running);
  await until(() => handles.length === 1, "the handle on the page");
  const video = page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  pair.attachHandle(video, handles[0] as object);
  const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes());
  video.testSetState({ paused: false, currentTime: 0.25 });
  const registry = harness.bridge.workerMediaSources;
  await until(() => registry.resolveHandle(handles[0])?.kind === "live", "the worker media source to be live");
  return { ...harness, pair, running, worker, handles, video, mediaSource, sourceBuffer };
}

function kinds(messages: readonly unknown[]): string[] {
  return messages.map((message) => {
    const typed = message as PageEvent;
    return typed.kind === "mse" ? `mse:${typed.event.kind}:${typed.event.media_source}` : typed.kind === "bytes" ? `bytes:${typed.media_source}` : typed.kind;
  });
}

/** Всё, что ушло изолированному миру, обязано пройти его строгую проверку. */
function assertAcceptedByIsolatedWorld(messages: readonly unknown[]): void {
  for (const message of messages) {
    const accepted = (message as { kind?: unknown }).kind === "candidates" ? parseCandidatesReply(message) : parsePageEvent(message);
    assert.ok(accepted !== undefined, `isolated world rejects ${kinds([message]).join()}`);
  }
}

async function beginHandoff(isolated: IsolatedSide): Promise<void> {
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(1);
  const reply = isolated.received[0] as CandidatesReply;
  const candidate = reply.candidates[0];
  assert.ok(candidate !== undefined);
  assert.deepEqual(candidate.media, { kind: "media_source", in_worker: true }, "a worker media source is offered like MSE and marked as living in a worker");
  isolated.received.length = 0;
  isolated.send({ kind: "begin_handoff", request_id: 2, document: candidate.document, element: candidate.element });
}

test("begin_handoff: T0 right after the worker declares the generation, then its replay, page_seek and the shadow, then live worker bytes — keys w1/ms-N", async () => {
  const world = await workerPlayingPage();
  const isolated = handshake(world.page);
  try {
    await beginHandoff(isolated);
    await until(() => world.shadow.started.length === 1, "the shadow");
    assert.deepEqual(kinds(isolated.received), ["handoff_began", "mse:source_buffer_added:w1/ms-1", "page_seek", "bytes:w1/ms-1", "page_seek", "page_playback"]);
    assert.equal(world.shadow.started[0]?.source, "media_source", "the real element keeps its handle under the shadow");
    assert.deepEqual(isolated.received[2], { kind: "page_seek", target_us: 250_000 });
    assert.deepEqual(isolated.received[4], { kind: "page_seek", target_us: 250_000 });
    const replayed = isolated.received[3] as Extract<PageEvent, { kind: "bytes" }>;
    assert.deepEqual([...replayed.bytes], [...VIDEO_MP4.bytes()]);

    const live = VIDEO_MP4.bytes().slice(0, 100);
    world.sourceBuffer.appendBuffer(live);
    await isolated.waitFor(7);
    const liveEvent = isolated.received[6] as Extract<PageEvent, { kind: "bytes" }>;
    assert.equal(liveEvent.media_source, "w1/ms-1");
    assert.deepEqual([...liveEvent.bytes], [...live]);
    // Кусок из воркера уходит изолированному миру тем же буфером (transfer), без второй копии.
    const lastBytes = world.page.postedToPorts.filter(({ message }) => (message as PageEvent).kind === "bytes").at(-1);
    assert.ok(lastBytes !== undefined);
    assert.deepEqual(lastBytes.transfer, [(lastBytes.message as Extract<PageEvent, { kind: "bytes" }>).bytes.buffer]);

    // Живое событие MSE идёт после тени: T0 уже назван, второй раз плеер его не услышит.
    world.sourceBuffer.abort();
    await until(() => isolated.received.length === 8, "the live MSE event");
    assert.deepEqual(kinds(isolated.received.slice(7)), ["mse:source_buffer_aborted:w1/ms-1"]);
    assertAcceptedByIsolatedWorld(isolated.received);
  } finally {
    isolated.close();
  }
});

test("end_handoff closes the worker streams: later worker appends reach nobody", async () => {
  const world = await workerPlayingPage();
  const isolated = handshake(world.page);
  try {
    await beginHandoff(isolated);
    await until(() => world.shadow.started.length === 1, "the shadow");
    isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
    await until(() => isolated.received.some((message) => (message as PageEvent).kind === "handoff_ended"), "handoff_ended");
    await isolated.settle();
    const before = isolated.received.length;
    world.sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 50));
    await isolated.settle();
    await isolated.settle();
    assert.equal(isolated.received.length, before, "nothing streams after the hand-off");
    const commands = world.page.postedToPorts.map(({ message }) => (message as { kind?: string }).kind).filter((kind) => kind === "open_stream" || kind === "close_streams");
    assert.deepEqual(commands, ["open_stream", "close_streams"]);
  } finally {
    isolated.close();
  }
});

test("a new worker media source on the handed element joins as a new generation; one on another element does not", async () => {
  const world = await workerPlayingPage();
  const isolated = handshake(world.page);
  try {
    await beginHandoff(isolated);
    await until(() => world.shadow.started.length === 1, "the shadow");
    // Чужой MediaSource того же воркера (другой элемент) — не наш поток.
    const stray = postHandle(world.running);
    await until(() => world.handles.length === 2, "the stray handle");
    world.pair.attachHandle(world.page.addVideo(), world.handles[1] as object);
    stray.addSourceBuffer(VIDEO_MIME);
    // 09a2: страница снимает источник (`srcObject = null`) и ставит handle нового MediaSource.
    const next = postHandle(world.running);
    await until(() => world.handles.length === 3, "the next handle");
    world.video.srcObject = null;
    world.pair.attachHandle(world.video, world.handles[2] as object);
    next.addSourceBuffer(VIDEO_MIME).appendBuffer(VIDEO_MP4.bytes().slice(0, 64));
    await until(() => kinds(isolated.received).includes("bytes:w1/ms-3"), "the new generation's bytes");
    const streamed = kinds(isolated.received);
    assert.ok(streamed.includes("mse:source_buffer_added:w1/ms-3"));
    assert.ok(streamed.indexOf("mse:source_buffer_added:w1/ms-3") < streamed.indexOf("bytes:w1/ms-3"));
    assert.ok(!streamed.some((kind) => kind.endsWith("w1/ms-2")), "the stray media source is not streamed");
    assertAcceptedByIsolatedWorld(isolated.received);
  } finally {
    isolated.close();
  }
});

test("candidates: an untracked handle is an honest 'not supported yet', a handle without source buffers is nothing yet", async () => {
  const world = await workerPlayingPage();
  const isolated = handshake(world.page);
  try {
    const untracked = world.page.addVideo();
    untracked.srcObject = new world.pair.FakeMediaSourceHandle();
    const pending = postHandle(world.running);
    await until(() => world.handles.length === 2, "the pending handle");
    const waiting = world.page.addVideo();
    world.pair.attachHandle(waiting, world.handles[1] as object);
    assert.equal(pending.sourceBuffers.length, 0);
    isolated.send({ kind: "describe_candidates", request_id: 3 });
    await isolated.waitFor(1);
    const reply = isolated.received[0] as CandidatesReply;
    assert.deepEqual(
      reply.candidates.map((candidate) => candidate.media),
      [{ kind: "media_source", in_worker: true }, { kind: "unsupported", form: "untracked_worker_media_source" }, { kind: "none" }],
    );
    assertAcceptedByIsolatedWorld(isolated.received);
  } finally {
    isolated.close();
  }
});

test("decision 192: a live MSE of a worker created from an http(s) URL is not wrapped — an honest untracked form, not a hand-off", async () => {
  const world = await workerPlayingPage();
  const PageWorker = (world.page.win as unknown as { Worker: new (url: string) => EventTarget }).Worker;
  const httpWorker = new PageWorker("/js/http-worker.js");
  const httpHandles: object[] = [];
  httpWorker.addEventListener("message", (event) => {
    const data = (event as MessageEvent).data as { type?: string; handle?: object };
    if (data.type === "handle" && data.handle !== undefined) httpHandles.push(data.handle);
  });
  const running = world.pair.created[1];
  assert.ok(running !== undefined);
  await until(() => running.loadOrder.length === 1, "the http worker to start");
  assert.deepEqual(running.loadOrder, ["/js/http-worker.js"], "the original script, no hook inside");
  const mediaSource = postHandle(running);
  await until(() => httpHandles.length === 1, "the http worker's handle");
  const video = world.page.addVideo();
  video.rect = { left: 0, top: 0, right: 1280, bottom: 720 };
  world.pair.attachHandle(video, httpHandles[0] as object);
  mediaSource.addSourceBuffer(VIDEO_MIME);
  const isolated = handshake(world.page);
  try {
    isolated.send({ kind: "describe_candidates", request_id: 4 });
    await isolated.waitFor(1);
    const reply = isolated.received[0] as CandidatesReply;
    assert.deepEqual(reply.candidates.at(-1)?.media, { kind: "unsupported", form: "untracked_worker_media_source" });
  } finally {
    isolated.close();
  }
});

test("a worker stream that cannot be opened ends the hand-off honestly; the page keeps its element", async () => {
  const world = await workerPlayingPage();
  world.page.portFailure.error = new Error("port is broken");
  world.page.portFailure.only = (message) => (message as { kind?: unknown }).kind === "open_stream";
  const isolated = handshake(world.page);
  try {
    await beginHandoff(isolated);
    await until(() => kinds(isolated.received).includes("handoff_ended"), "handoff_ended");
    assert.deepEqual(kinds(isolated.received), ["handoff_began", "handoff_ended"]);
    assert.deepEqual(isolated.received[1], { kind: "handoff_ended", cause: "returned" });
    assert.equal(world.shadow.started.length, 0, "no shadow without the replay");
    assert.ok(world.debugLines.some((line) => line.includes("worker stream could not be requested")));
  } finally {
    isolated.close();
  }
});

test("a giant segment in the worker ends the hand-off honestly and is not copied; the site's own append is untouched", async () => {
  const world = await workerPlayingPage();
  const isolated = handshake(world.page);
  try {
    await beginHandoff(isolated);
    await until(() => world.shadow.started.length === 1, "the shadow");
    await isolated.settle();
    isolated.received.length = 0;
    world.sourceBuffer.appendBuffer(new Uint8Array(MAX_FORWARDED_APPEND_BYTES + 1));
    await until(() => isolated.received.some((message) => (message as PageEvent).kind === "handoff_ended"), "the honest end of the hand-off");
    assert.equal(isolated.received.some((message) => (message as PageEvent).kind === "bytes"), false, "no copy reached the page channel");
    assert.deepEqual(isolated.received.at(-1), { kind: "handoff_ended", cause: "page_flood" }, "the background learns it was a flood");
  } finally {
    isolated.close();
  }
});
