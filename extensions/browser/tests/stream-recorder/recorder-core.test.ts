// Ядро записи: формат файла, склад с потолком, приём пачки воркера, порядок событий.

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { ByteStore } from "../../src/page/stream-recorder/byte-store.ts";
import { encodeRecordingParts, FPREC_FORMAT_NAME, FPREC_MAGIC, FPREC_VERSION, jsonNumber, type RecordingManifest } from "../../src/page/stream-recorder/format.ts";
import { StreamRecorder } from "../../src/page/stream-recorder/recorder.ts";
import { concatStore } from "./support/recording-scope.ts";

describe("формат файла .fprec", () => {
  it("магия, u32 little-endian длина манифеста, UTF-8 JSON, затем склад подряд", () => {
    const manifest: RecordingManifest = {
      format: FPREC_FORMAT_NAME,
      version: FPREC_VERSION,
      browser: "firefox",
      origin: "https://www.youtube.com",
      started_at_unix_ms: 1_793_700_000_000,
      truncated: false,
      events: [{ t_ms: 12.5, ctx: "main", op: "source_open", ms: "ms-1", note: "тест юникода" }],
    };
    const parts = encodeRecordingParts(manifest, [new Uint8Array([1, 2, 3]), new Uint8Array([4])]);
    const file = concatStore(parts);
    assert.equal(new TextDecoder().decode(file.slice(0, 8)), FPREC_MAGIC);
    const length = new DataView(file.buffer).getUint32(8, true);
    assert.deepEqual(JSON.parse(new TextDecoder().decode(file.slice(12, 12 + length))), manifest);
    assert.deepEqual(file.slice(12 + length), new Uint8Array([1, 2, 3, 4]));
  });

  it("бесконечность и NaN уходят строкой, конечные числа — числом", () => {
    assert.deepEqual([jsonNumber(1.5), jsonNumber(Infinity), jsonNumber(-Infinity), jsonNumber(Number.NaN)], [1.5, "Infinity", "-Infinity", "NaN"]);
  });
});

describe("склад байтов", () => {
  it("принимает до потолка, копирует, после первого отказа не принимает ничего", () => {
    const store = new ByteStore(6);
    const original = new Uint8Array([1, 2, 3, 4]);
    assert.deepEqual(store.append(original), { kind: "stored", offset: 0 });
    original.fill(0);
    assert.deepEqual(store.chunks[0], new Uint8Array([1, 2, 3, 4]), "a copy, not a reference");
    assert.deepEqual(store.append(new Uint8Array(3)), { kind: "dropped" });
    assert.equal(store.truncated, true);
    assert.deepEqual(store.append(new Uint8Array(1)), { kind: "dropped" }, "after truncation even small chunks are not taken: the tail is cut cleanly");
    assert.equal(store.sizeBytes, 4);
  });
});

describe("приём пачки воркера", () => {
  const batchBase = { timeOrigin: 500 };

  it("общий склад полон: append воркера превращается в append_truncated, truncated=true", () => {
    const main = new StreamRecorder({ ctx: "main", now: () => 0, startMs: 0, storeCeilingBytes: 4 });
    main.absorbWorkerBatch(
      1,
      { ...batchBase, events: [{ t_ms: 1, ctx: "worker", op: "append", ms: "ms-1", sb: "sb-1", offset: 0, length: 8, hash: "00000000" }], bytes: new Uint8Array(8), truncated: false },
      0,
    );
    const [event] = main.sortedEvents();
    assert.equal(event?.op, "append_truncated");
    assert.equal("offset" in (event ?? {}), false);
    assert.equal(main.truncated, true);
  });

  it("пачка с признаком truncated от воркера делает truncated всю запись", () => {
    const main = new StreamRecorder({ ctx: "main", now: () => 0, startMs: 0 });
    main.absorbWorkerBatch(1, { ...batchBase, events: [], bytes: null, truncated: true }, 0);
    assert.equal(main.truncated, true);
  });

  it("события главного потока и воркеров упорядочены по t_ms, при равенстве — по порядку записи", () => {
    let now = 0;
    const main = new StreamRecorder({ ctx: "main", now: () => now, startMs: 0 });
    now = 30;
    main.record("source_open", { ms: "ms-1" });
    main.absorbWorkerBatch(
      1,
      { timeOrigin: 0, events: [{ t_ms: 10, ctx: "worker", op: "a" }, { t_ms: 30, ctx: "worker", op: "b" }], bytes: null, truncated: false },
      0,
    );
    now = 5;
    main.record("early", {});
    assert.deepEqual(main.sortedEvents().map((event) => `${event.ctx}:${event.op}@${event.t_ms}`), ["main:early@5", "worker-1:a@10", "main:source_open@30", "worker-1:b@30"]);
  });

  it("потолок числа событий: лишние не пишутся, truncated=true", () => {
    const recorder = new StreamRecorder({ ctx: "main", now: () => 0, startMs: 0, maxEvents: 2 });
    recorder.record("a");
    recorder.record("b");
    recorder.record("c");
    assert.equal(recorder.sortedEvents().length, 2);
    assert.equal(recorder.truncated, true);
  });
});
