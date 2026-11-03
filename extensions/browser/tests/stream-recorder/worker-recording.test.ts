// Запись внутри воркера: настоящий хук воркера с расширением записи, личный порт на MessageChannel.
// Воркер копит события и байты, по просьбе сдаёт пачку; главный поток принимает её с пересчётом
// времени и смещений склада.

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { captureBinaryIntrinsics } from "../../src/page/binary-view.ts";
import { fnv1a32Hex } from "../../src/page/stream-recorder/format.ts";
import { StreamRecorder } from "../../src/page/stream-recorder/recorder.ts";
import { BATCH_REPORT_KIND, FLUSH_COMMAND_KIND, parseBatchReport, type ParsedBatch } from "../../src/page/stream-recorder/worker-wire.ts";
import { WORKER_CHANNEL_HANDSHAKE } from "../../src/shared/worker-channel-contract.ts";
import { installWorkerHook } from "../../src/worker/worker-hook.ts";
import { createWorkerRecordingExtension } from "../../src/worker/worker-recording.ts";
import { VIDEO_MP4 } from "../page/support/media-samples.ts";
import { createFakeWorkerScope } from "../worker/support/fake-worker-scope.ts";
import { concatStore, withoutTime } from "./support/recording-scope.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';

/** Воркер с хуком и записью + главная сторона порта, которая копит всё, что не разобрал договор канала. */
function startRecordedWorker() {
  const fake = createFakeWorkerScope();
  const intervals: (() => void)[] = [];
  // Часы воркера под управлением теста; timeOrigin воркера на 1000 мс позже главного потока.
  const clock = { nowMs: 50 };
  Object.assign(fake.scope, {
    performance: { now: () => clock.nowMs, timeOrigin: 1_000 },
    JSON,
    setInterval: (callback: () => void) => intervals.push(callback),
  });
  const oddities: string[] = [];
  installWorkerHook(fake.scope, { codecPolicy: { kind: "transparent" } }, (what) => oddities.push(what), createWorkerRecordingExtension);

  const channel = new MessageChannel();
  const unparsed: unknown[] = [];
  channel.port1.addEventListener("message", (event) => unparsed.push((event as MessageEvent).data));
  channel.port1.start();
  fake.deliverToWorker(WORKER_CHANNEL_HANDSHAKE, [channel.port2]);

  const binary = captureBinaryIntrinsics(globalThis);
  async function nextBatches(count: number): Promise<ParsedBatch[]> {
    const deadline = Date.now() + 2000;
    const batches = () => unparsed.flatMap((data) => parseBatchReport(data, binary, JSON.parse) ?? []);
    while (batches().length < count) {
      if (Date.now() > deadline) throw new Error(`timeout waiting for ${count} batches`);
      await new Promise((resolve) => setTimeout(resolve, 2));
    }
    return batches();
  }
  return { fake, port: channel.port1, intervals, clock, oddities, nextBatches, close: () => channel.port1.close() };
}

describe("запись в воркере", () => {
  it("просьба сдать пачку: события и байты приходят главному потоку, жетон возвращается; главный пересчитывает время и смещения", async () => {
    const worker = startRecordedWorker();
    try {
      const mediaSource = new (worker.fake.scope.MediaSource as typeof worker.fake.FakeMediaSource)();
      worker.clock.nowMs = 60;
      mediaSource.testOpen();
      const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
      const bytes = VIDEO_MP4.bytes().slice(0, 733);
      worker.clock.nowMs = 75;
      sourceBuffer.appendBuffer(bytes);

      worker.port.postMessage({ kind: FLUSH_COMMAND_KIND, token: 7 });
      const [flushed] = await worker.nextBatches(1);
      assert.ok(flushed !== undefined);
      assert.equal(flushed.token, 7, "the answer carries the token of the request");
      assert.deepEqual(flushed.batch.events.map((event) => event.op), ["media_source_created", "source_open", "add_source_buffer", "append"]);
      assert.deepEqual(flushed.batch.bytes, bytes);

      // Главный поток: его timeOrigin = 0, запись началась на его часах в 10 мс; склад уже содержит 5 байт.
      const main = new StreamRecorder({ ctx: "main", now: () => 0, startMs: 10 });
      main.recordAppend({ ms: "ms-1", sb: "sb-1" }, new Uint8Array([1, 2, 3, 4, 5]));
      main.absorbWorkerBatch(2, flushed.batch, 0);
      const absorbed = main.sortedEvents().filter((event) => event.ctx === "worker-2");
      assert.deepEqual(
        absorbed.map((event) => [event.op, event.t_ms]),
        // t_ms = время воркера + его timeOrigin (1000) - timeOrigin главного (0) - старт записи (10).
        [["media_source_created", 1040], ["source_open", 1050], ["add_source_buffer", 1050], ["append", 1065]],
      );
      const append = absorbed.find((event) => event.op === "append")!;
      assert.equal(append["offset"], 5, "the worker's offset is rebased behind the bytes already in the main store");
      const store = concatStore(main.storeChunks);
      assert.deepEqual(store.slice(5, 5 + (append["length"] as number)), bytes);
      assert.equal(append["hash"], fnv1a32Hex(bytes));
    } finally {
      worker.close();
    }
  });

  it("плановая пачка по таймеру уходит сама, пустая — нет; sent при повторе журнала пишется в контексте воркера", async () => {
    const worker = startRecordedWorker();
    try {
      const tick = worker.intervals.at(-1);
      assert.ok(tick !== undefined, "a periodic flush is scheduled");
      tick();
      await new Promise((resolve) => setTimeout(resolve, 10));
      assert.deepEqual(await worker.nextBatches(0), [], "nothing recorded yet: no empty batch is sent");

      const mediaSource = new (worker.fake.scope.MediaSource as typeof worker.fake.FakeMediaSource)();
      const handle = mediaSource.handle;
      worker.fake.scope.postMessage({ type: "handle", handle }, [handle]);
      mediaSource.testOpen();
      const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
      const bytes = VIDEO_MP4.bytes().slice(0, 500);
      sourceBuffer.appendBuffer(bytes);
      // MAIN-мир открывает поток MediaSource воркера: журнал уходит как повтор.
      worker.port.postMessage({ kind: "open_stream", stream: 1, media_source: "ms-1" });
      await new Promise((resolve) => setTimeout(resolve, 10));
      tick();
      const [batch] = await worker.nextBatches(1);
      assert.ok(batch !== undefined);
      assert.equal(batch.token, null, "a scheduled batch answers no request");
      const sent = batch.batch.events.filter((event) => event.op === "sent");
      assert.deepEqual(withoutTime(sent), [{ ctx: "worker", op: "sent", ms: "ms-1", sb: "sb-1", length: 500, hash: fnv1a32Hex(bytes), replay: true }]);
      assert.ok(batch.batch.events.some((event) => event.op === "media_source_handle"), "the handle read of the site is recorded once");
    } finally {
      worker.close();
    }
  });

  it("мусор вместо пачки не принимается разбором: форма проверяется строго", () => {
    const binary = captureBinaryIntrinsics(globalThis);
    assert.equal(parseBatchReport({ kind: "other" }, binary, JSON.parse), undefined);
    assert.equal(parseBatchReport({ kind: BATCH_REPORT_KIND, token: "x", time_origin: 0, events_json: "[]", bytes: null, truncated: false }, binary, JSON.parse), undefined);
    assert.equal(parseBatchReport({ kind: BATCH_REPORT_KIND, token: null, time_origin: 0, events_json: "{}", bytes: null, truncated: false }, binary, JSON.parse), undefined);
    assert.equal(parseBatchReport({ kind: BATCH_REPORT_KIND, token: null, time_origin: 0, events_json: "[]", bytes: [1, 2], truncated: false }, binary, JSON.parse), undefined);
    assert.notEqual(parseBatchReport({ kind: BATCH_REPORT_KIND, token: null, time_origin: 0, events_json: "[]", bytes: null, truncated: false }, binary, JSON.parse), undefined);
  });
});
