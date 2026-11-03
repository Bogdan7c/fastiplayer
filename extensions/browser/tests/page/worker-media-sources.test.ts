// Реестр MediaSource воркеров (src/page/worker-capture/worker-media-sources.ts) на уровне потоков:
// срок повтора, запоздавшие и чужие донесения, элемент чужого MediaSource, закрытие потоков.
// Сторону хука воркера играет тест: он сам отвечает по личному порту.

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import { captureBinaryIntrinsics } from "../../src/page/binary-view.ts";
import { captureChannelOriginals } from "../../src/page/page-channel.ts";
import type { StreamMessage } from "../../src/page/stream-sender.ts";
import { WORKER_STREAM_REPLAY_TIMEOUT_MS, WorkerMediaSources, type WorkerMediaSourceRef } from "../../src/page/worker-capture/worker-media-sources.ts";
import type { WorkerCommand, WorkerReport } from "../../src/shared/worker-channel-contract.ts";

const openPorts: MessagePort[] = [];
afterEach(() => {
  for (const port of openPorts.splice(0)) port.close();
});

class FakeHandle {}

async function settle(): Promise<void> {
  for (let round = 0; round < 5; round += 1) await new Promise((resolve) => setTimeout(resolve, 2));
}

function startRegistry(callbacks: { readonly onSiteScriptLoaded?: () => void } = {}) {
  const timers: { callback: () => void; delayMs: number; cancelled: boolean }[] = [];
  const oddities: string[] = [];
  const registry = new WorkerMediaSources({
    isMediaSourceHandle: (candidate) => candidate instanceof FakeHandle,
    binary: captureBinaryIntrinsics({ Uint8Array, ArrayBuffer, DataView }),
    scheduleTimeout: (callback, delayMs) => {
      const timer = { callback, delayMs, cancelled: false };
      timers.push(timer);
      return () => {
        timer.cancelled = true;
      };
    },
    reportOddity: (what) => oddities.push(what),
  });
  const started: string[] = [];
  registry.setListener({ onWorkerMediaSourceStarted: (mediaSource) => started.push(mediaSource.key) });
  const originals = captureChannelOriginals({ EventTarget, Event, MessageEvent, MessagePort });
  const channel = registry.createChannel({ originals, onHookReady: () => {}, onSiteScriptLoaded: callbacks.onSiteScriptLoaded ?? (() => {}), reportOddity: (what) => oddities.push(what) });
  const pair = new MessageChannel();
  openPorts.push(pair.port1, pair.port2);
  channel.connect(pair.port1);
  const commands: WorkerCommand[] = [];
  pair.port2.addEventListener("message", (event) => commands.push((event as MessageEvent).data as WorkerCommand));
  pair.port2.start();
  const worker = {
    report(report: WorkerReport, transfer: Transferable[] = []): void {
      pair.port2.postMessage(report, transfer);
    },
  };
  return { registry, channel, worker, commands, timers, oddities, started };
}

/** Живой MediaSource «ms-1» воркера 1, handle пришёл во 2-м сообщении. */
async function liveMediaSource(world: ReturnType<typeof startRegistry>): Promise<WorkerMediaSourceRef> {
  const handle = new FakeHandle();
  world.channel.noteWorkerMessage();
  world.registry.noteHandleArrived(handle, world.channel, world.channel.noteWorkerMessage());
  world.worker.report({ kind: "handle_sent", message: 2, media_source: "ms-1" });
  world.worker.report({ kind: "media_source_started", media_source: "ms-1" });
  await settle();
  const resolution = world.registry.resolveHandle(handle);
  assert.ok(resolution?.kind === "live");
  return resolution.mediaSource;
}

function recordingHandlers() {
  const events: string[] = [];
  const delivered: StreamMessage[] = [];
  return {
    events,
    delivered,
    handlers: {
      deliver: (message: StreamMessage) => {
        delivered.push(message);
        events.push(`deliver:${message.kind}`);
      },
      replayed: () => events.push("replayed"),
      unavailable: () => events.push("unavailable"),
    },
  };
}

test("started media sources reach the listener with the contract key", async () => {
  const world = startRegistry();
  await liveMediaSource(world);
  assert.deepEqual(world.started, ["w1/ms-1"]);
});

test("stream: open_stream with a fresh id, namespaced items, replayed cancels the replay deadline", async () => {
  const world = startRegistry();
  const mediaSource = await liveMediaSource(world);
  const recorded = recordingHandlers();
  world.registry.openStream(mediaSource, recorded.handlers);
  await settle();
  assert.deepEqual(world.commands, [{ kind: "open_stream", stream: 1, media_source: "ms-1" }]);
  assert.equal(world.timers[0]?.delayMs, WORKER_STREAM_REPLAY_TIMEOUT_MS);
  world.worker.report({ kind: "stream_item", stream: 1, item: { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "video/mp4" } } });
  const bytes = new Uint8Array([1, 2, 3]);
  world.worker.report({ kind: "stream_item", stream: 1, item: { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes } }, [bytes.buffer]);
  world.worker.report({ kind: "stream_replayed", stream: 1 });
  await settle();
  assert.deepEqual(recorded.events, ["deliver:mse", "deliver:bytes", "replayed"]);
  assert.deepEqual(recorded.delivered[0], { kind: "mse", event: { kind: "source_buffer_added", media_source: "w1/ms-1", source_buffer: 0, mime_type: "video/mp4" } });
  const delivered = recorded.delivered[1];
  assert.ok(delivered?.kind === "bytes");
  assert.equal(delivered.media_source, "w1/ms-1");
  assert.deepEqual([...delivered.bytes], [1, 2, 3]);
  assert.equal(world.timers[0]?.cancelled, true, "the replay deadline is gone");
});

test("no replay in time: unavailable once; a late replayed or item of that stream is ignored", async () => {
  const world = startRegistry();
  const mediaSource = await liveMediaSource(world);
  const recorded = recordingHandlers();
  world.registry.openStream(mediaSource, recorded.handlers);
  world.timers[0]?.callback();
  world.worker.report({ kind: "stream_replayed", stream: 1 });
  world.worker.report({ kind: "stream_item", stream: 1, item: { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" } } });
  await settle();
  assert.deepEqual(recorded.events, ["unavailable"]);
  assert.ok(world.oddities.includes("worker did not replay its journal in time"));
});

test("an item of another media source under this stream id ends the stream; foreign stream ids are ignored", async () => {
  const world = startRegistry();
  const mediaSource = await liveMediaSource(world);
  const recorded = recordingHandlers();
  world.registry.openStream(mediaSource, recorded.handlers);
  world.worker.report({ kind: "stream_item", stream: 99, item: { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" } } });
  world.worker.report({ kind: "stream_item", stream: 1, item: { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-2" } } });
  world.worker.report({ kind: "stream_item", stream: 1, item: { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" } } });
  await settle();
  assert.deepEqual(recorded.events, ["unavailable"]);
  assert.equal(world.timers[0]?.cancelled, true);
});

test("closeAllStreams tells the worker once and drops later items; no unavailable for our own decision", async () => {
  const world = startRegistry();
  const mediaSource = await liveMediaSource(world);
  const recorded = recordingHandlers();
  world.registry.openStream(mediaSource, recorded.handlers);
  world.registry.openStream(mediaSource, recordingHandlers().handlers);
  world.registry.closeAllStreams();
  world.worker.report({ kind: "stream_replayed", stream: 1 });
  await settle();
  assert.deepEqual(world.commands.map((command) => command.kind), ["open_stream", "open_stream", "close_streams"]);
  assert.deepEqual(recorded.events, []);
  assert.ok(world.timers.every((timer) => timer.cancelled));
});

test("garbage reports change nothing and leave a trace", async () => {
  const world = startRegistry();
  const handle = new FakeHandle();
  world.registry.noteHandleArrived(handle, world.channel, world.channel.noteWorkerMessage());
  for (const garbage of [
    { kind: "handle_sent", message: 1, media_source: "w1/ms-1" },
    { kind: "handle_sent", message: 0, media_source: "ms-1" },
    { kind: "media_source_started", media_source: "ms-01" },
    { kind: "stream_item", stream: 1, item: { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(0) } },
  ]) {
    world.worker.report(garbage as unknown as WorkerReport);
  }
  await settle();
  assert.deepEqual(world.registry.resolveHandle(handle), { kind: "not_live" });
  assert.deepEqual(world.started, []);
  assert.equal(world.oddities.filter((what) => what === "worker report is malformed").length, 4);
});

test("site_script_loaded (decision 192): only the exact shape reaches the wrapper; extra fields are malformed", async () => {
  let loadedReports = 0;
  const world = startRegistry({ onSiteScriptLoaded: () => (loadedReports += 1) });
  world.worker.report({ kind: "site_script_loaded", url: "blob:https://site.example/1" } as unknown as WorkerReport);
  await settle();
  assert.equal(loadedReports, 0, "a report with extra fields is not trusted");
  assert.deepEqual(world.oddities, ["worker report is malformed"]);
  world.worker.report({ kind: "site_script_loaded" });
  await settle();
  assert.equal(loadedReports, 1);
  assert.deepEqual(world.started, [], "the report does not touch media source accounting");
});
