// Запись MSE: каждая операция страницы становится событием формата, байты склада = байты `append`,
// сбой append виден, потолок даёт truncated, наблюдение никогда не ломает страницу.

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { fnv1a32Hex } from "../../src/page/stream-recorder/format.ts";
import { HandoffRecording } from "../../src/page/stream-recorder/handoff-recording.ts";
import { concatStore, startMseRecordingRig, VIDEO_MIME, withoutTime } from "./support/recording-scope.ts";

describe("операции MSE → события формата", () => {
  it("весь жизненный путь MediaSource и SourceBuffer пишется в порядке вызовов, со значениями формата", () => {
    const rig = startMseRecordingRig();
    const { recorder } = rig;
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    rig.scope.URL.createObjectURL(mediaSource);
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    sourceBuffer.mode = "sequence";
    sourceBuffer.appendWindowStart = 1.5;
    sourceBuffer.appendWindowEnd = Infinity;
    sourceBuffer.timestampOffset = 2.5;
    mediaSource.duration = Infinity;
    sourceBuffer.appendBuffer(new Uint8Array([1, 2, 3, 4]));
    sourceBuffer.bufferedRanges = [[0, 4.5]];
    sourceBuffer.dispatchEvent(new Event("updateend"));
    sourceBuffer.remove(0, Infinity);
    sourceBuffer.abort();
    sourceBuffer.changeType('video/webm; codecs="vp9"');
    mediaSource.setLiveSeekableRange(1, 9);
    mediaSource.clearLiveSeekableRange();
    mediaSource.endOfStream("network");
    mediaSource.testEnded();
    mediaSource.removeSourceBuffer(sourceBuffer);
    mediaSource.testClose();

    assert.deepEqual(withoutTime(recorder.sortedEvents()), [
      { ctx: "main", op: "media_source_created", ms: "ms-1" },
      { ctx: "main", op: "source_open", ms: "ms-1" },
      { ctx: "main", op: "media_source_object_url", ms: "ms-1" },
      { ctx: "main", op: "add_source_buffer", ms: "ms-1", sb: "sb-1", mime: VIDEO_MIME },
      { ctx: "main", op: "mode", ms: "ms-1", sb: "sb-1", value: "sequence" },
      { ctx: "main", op: "append_window_start", ms: "ms-1", sb: "sb-1", value: 1.5 },
      { ctx: "main", op: "append_window_end", ms: "ms-1", sb: "sb-1", value: "Infinity" },
      { ctx: "main", op: "timestamp_offset", ms: "ms-1", sb: "sb-1", value: 2.5 },
      { ctx: "main", op: "duration", ms: "ms-1", value: "Infinity" },
      { ctx: "main", op: "append", ms: "ms-1", sb: "sb-1", offset: 0, length: 4, hash: fnv1a32Hex(new Uint8Array([1, 2, 3, 4])) },
      { ctx: "main", op: "update_end", ms: "ms-1", sb: "sb-1" },
      { ctx: "main", op: "buffered", ms: "ms-1", sb: "sb-1", ranges: [[0, 4.5]] },
      { ctx: "main", op: "remove", ms: "ms-1", sb: "sb-1", start: 0, end: "Infinity" },
      { ctx: "main", op: "abort", ms: "ms-1", sb: "sb-1" },
      { ctx: "main", op: "change_type", ms: "ms-1", sb: "sb-1", mime: 'video/webm; codecs="vp9"' },
      { ctx: "main", op: "live_seekable_range", ms: "ms-1", start: 1, end: 9 },
      { ctx: "main", op: "live_seekable_range_cleared", ms: "ms-1" },
      { ctx: "main", op: "end_of_stream", ms: "ms-1", error: "network" },
      { ctx: "main", op: "source_ended", ms: "ms-1" },
      { ctx: "main", op: "remove_source_buffer", ms: "ms-1", sb: "sb-1" },
      { ctx: "main", op: "source_close", ms: "ms-1" },
    ]);
    assert.deepEqual(rig.oddities, []);
  });

  it("время t_ms считается от старта записи; идентификаторы сквозные: второй MediaSource — ms-2, его буфер — sb-2", () => {
    const rig = startMseRecordingRig();
    rig.clock.nowMs = 112.5;
    const first = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    first.testOpen();
    first.addSourceBuffer(VIDEO_MIME);
    rig.clock.nowMs = 200;
    const second = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    second.testOpen();
    second.addSourceBuffer(VIDEO_MIME);
    const events = rig.recorder.sortedEvents();
    assert.equal(events[0]?.t_ms, 12.5);
    const secondCreated = events.find((event) => event.op === "media_source_created" && event["ms"] === "ms-2");
    assert.equal(secondCreated?.t_ms, 100);
    assert.ok(events.some((event) => event.op === "add_source_buffer" && event["ms"] === "ms-2" && event["sb"] === "sb-2"));
  });

  it("endOfStream без ошибки пишется без поля error", () => {
    const rig = startMseRecordingRig();
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    mediaSource.addSourceBuffer(VIDEO_MIME);
    mediaSource.endOfStream();
    const end = rig.recorder.sortedEvents().find((event) => event.op === "end_of_stream");
    assert.deepEqual(end && withoutTime([end])[0], { ctx: "main", op: "end_of_stream", ms: "ms-1" });
  });
});

describe("байты склада", () => {
  it("склад = ровно то, что принял браузер: виды со смещением, DataView, копия в момент вызова, хэш FNV-1a", () => {
    const rig = startMseRecordingRig();
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    // Страница держит большую загрузку и отдаёт куски-виды на неё.
    const download = new Uint8Array(1000);
    for (let index = 0; index < download.length; index += 1) download[index] = (index * 7 + 3) % 251;
    sourceBuffer.appendBuffer(new Uint8Array(download.buffer, 100, 300));
    sourceBuffer.appendBuffer(new DataView(download.buffer, 400, 50));
    sourceBuffer.appendBuffer(download.slice(450, 460).buffer);
    // Страница переиспользует буфер: затирает его после вызовов.
    download.fill(0xee);

    const store = concatStore(rig.recorder.storeChunks);
    const appends = rig.recorder.sortedEvents().filter((event) => event.op === "append");
    assert.equal(appends.length, 3);
    appends.forEach((event, order) => {
      const stored = store.slice(event["offset"] as number, (event["offset"] as number) + (event["length"] as number));
      const accepted = sourceBuffer.appended[order]!;
      assert.deepEqual(stored, accepted, `append #${order}: the store holds the bytes the browser took`);
      assert.equal(event["hash"], fnv1a32Hex(accepted));
    });
    assert.deepEqual(appends.map((event) => [event["offset"], event["length"]]), [[0, 300], [300, 50], [350, 10]]);
    assert.equal(store.length, 360);
    assert.notEqual(store[0], 0xee, "the copy was taken at call time");
  });

  it("FNV-1a 32: эталонные значения алгоритма", () => {
    assert.equal(fnv1a32Hex(new Uint8Array([])), "811c9dc5");
    assert.equal(fnv1a32Hex(new TextEncoder().encode("a")), "e40c292c");
    assert.equal(fnv1a32Hex(new TextEncoder().encode("foobar")), "bf9cf968");
  });

  it("сбой appendBuffer: страница получает ту же ошибку, пишется append_error, байтов на складе нет", () => {
    const rig = startMseRecordingRig();
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    const quota = Object.assign(new Error("buffer is full"), { name: "QuotaExceededError" });
    sourceBuffer.failNextAppend = quota;
    assert.throws(() => sourceBuffer.appendBuffer(new Uint8Array([9, 9])), (error: unknown) => error === quota);
    const failure = rig.recorder.sortedEvents().find((event) => event.op === "append_error");
    assert.deepEqual(failure && withoutTime([failure])[0], { ctx: "main", op: "append_error", ms: "ms-1", sb: "sb-1", error: "QuotaExceededError: buffer is full" });
    assert.equal(rig.recorder.storeChunks.length, 0);
    assert.equal(rig.recorder.sortedEvents().filter((event) => event.op === "append").length, 0);
  });

  it("потолок склада: следующий append не пишется байтами (append_truncated), truncated=true, события без байтов идут дальше", () => {
    const rig = startMseRecordingRig({ storeCeilingBytes: 10 });
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    sourceBuffer.appendBuffer(new Uint8Array(8).fill(1));
    assert.equal(rig.recorder.truncated, false);
    sourceBuffer.appendBuffer(new Uint8Array(8).fill(2));
    sourceBuffer.appendBuffer(new Uint8Array(1).fill(3));
    sourceBuffer.timestampOffset = 4;
    assert.equal(rig.recorder.truncated, true);
    const ops = rig.recorder.sortedEvents().map((event) => event.op);
    assert.deepEqual(ops.slice(-4), ["append", "append_truncated", "append_truncated", "timestamp_offset"]);
    assert.equal(concatStore(rig.recorder.storeChunks).length, 8, "the dropped tail is not in the store");
  });
});

describe("наблюдение не меняет поведение страницы", () => {
  it("обёртки похожи на оригиналы (имя, длина), результаты и исключения те же; сбой записи не доходит до страницы", () => {
    const rig = startMseRecordingRig();
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    assert.equal(sourceBuffer.appendBuffer.name, "appendBuffer");
    assert.equal(sourceBuffer.appendBuffer.length, 1);
    assert.ok(mediaSource instanceof rig.FakeMediaSource, "прокси конструктора не ломает instanceof");
    assert.equal(rig.scope.MediaSource.isTypeSupported(VIDEO_MIME), true, "статика конструктора на месте");

    // Запись ломается — страница не должна этого заметить, а мост по-прежнему всё учитывает.
    rig.recorder.record = () => {
      throw new Error("recorder is broken");
    };
    sourceBuffer.appendBuffer(new Uint8Array([5, 6, 7]));
    assert.deepEqual(sourceBuffer.appended, [new Uint8Array([5, 6, 7])]);
    assert.ok(rig.oddities.length >= 1, "the failure left a trace");
    const record = rig.tracker.recordOf(mediaSource);
    assert.ok(record !== null && record.journal.replay().some((item) => item.kind === "bytes"), "the bridge hook still saw the bytes");
  });
});

describe("снимок buffered", () => {
  it("раз в секунду, только если изменился; после updateend — сразу", () => {
    const rig = startMseRecordingRig();
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    const tick = rig.intervals[0];
    assert.ok(tick !== undefined, "the periodic snapshot is scheduled");
    tick();
    assert.equal(rig.recorder.sortedEvents().filter((event) => event.op === "buffered").length, 1, "the first look at the empty ranges is recorded");
    tick();
    assert.equal(rig.recorder.sortedEvents().filter((event) => event.op === "buffered").length, 1, "no change — no event");
    sourceBuffer.bufferedRanges = [[0, 2], [3, 4]];
    tick();
    const snapshots = rig.recorder.sortedEvents().filter((event) => event.op === "buffered");
    assert.deepEqual(snapshots.at(-1)?.["ranges"], [[0, 2], [3, 4]]);
  });
});

describe("что ушло в плеер (sent)", () => {
  it("кусок моста получает идентификаторы записи, хэш и признак повтора; неизвестный буфер не пишется", () => {
    const rig = startMseRecordingRig();
    const mediaSource = new rig.scope.MediaSource() as InstanceType<typeof rig.FakeMediaSource>;
    mediaSource.testOpen();
    mediaSource.addSourceBuffer(VIDEO_MIME);
    const tap = new HandoffRecording(rig.recorder, (value) => JSON.parse(JSON.stringify(value)));
    const replayed = new Uint8Array([1, 2, 3]);
    const live = new Uint8Array([4, 5]);
    tap.chunkSent({ mediaSource: "ms-1", sourceBuffer: 0, bytes: replayed, origin: "replay" });
    tap.chunkSent({ mediaSource: "ms-1", sourceBuffer: 0, bytes: live, origin: "live" });
    tap.chunkSent({ mediaSource: "ms-9", sourceBuffer: 0, bytes: live, origin: "live" });
    tap.replayFinished();
    const events = rig.recorder.sortedEvents().filter((event) => event.op === "sent" || event.op === "handoff_replayed");
    assert.deepEqual(withoutTime(events), [
      { ctx: "main", op: "sent", ms: "ms-1", sb: "sb-1", length: 3, hash: fnv1a32Hex(replayed), replay: true },
      { ctx: "main", op: "sent", ms: "ms-1", sb: "sb-1", length: 2, hash: fnv1a32Hex(live), replay: false },
      { ctx: "main", op: "handoff_replayed", items: 1, bytes: 3 },
    ]);
  });
});
