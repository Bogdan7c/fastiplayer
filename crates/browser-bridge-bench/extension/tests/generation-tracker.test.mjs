import test from "node:test";
import assert from "node:assert/strict";
import { trackerLib, expectValidPageMessage } from "./support.mjs";

function newTracker() {
  const items = [];
  const tracker = trackerLib.createGenerationTracker({ emit: (item) => items.push(item) });
  const messages = () => items.filter((item) => item.kind === "message").map((item) => item.message);
  return { tracker, items, messages };
}

test("first addSourceBuffer opens generation 1; indexes follow addition order", () => {
  const { tracker, messages } = newTracker();
  const mediaSource = {};
  tracker.noteSourceBufferAdded(mediaSource, {}, 'video/mp4; codecs="avc1.64001e"');
  tracker.noteSourceBufferAdded(mediaSource, {}, 'audio/mp4; codecs="mp4a.40.2"');
  assert.deepEqual(messages(), [
    { type: "generation_started", generation: 1 },
    { type: "source_buffer_added", generation: 1, source_buffer: 0, mime_type: 'video/mp4; codecs="avc1.64001e"' },
    { type: "source_buffer_added", generation: 1, source_buffer: 1, mime_type: 'audio/mp4; codecs="mp4a.40.2"' },
  ]);
});

test("a new MediaSource is a new generation and source buffer indexes restart at 0", () => {
  const { tracker, messages } = newTracker();
  const first = {}, second = {};
  const oldBuffer = {}, newBuffer = {};
  tracker.noteSourceBufferAdded(first, oldBuffer, "video/mp4");
  tracker.noteSourceBufferAdded(second, newBuffer, "video/webm");
  tracker.noteEndOfStream(first);
  assert.deepEqual(messages().slice(-3), [
    { type: "generation_started", generation: 2 },
    { type: "source_buffer_added", generation: 2, source_buffer: 0, mime_type: "video/webm" },
    { type: "end_of_stream", generation: 1 }, // событие старого поколения не путается с новым
  ]);
});

test("append carries generation and index of ITS buffer; empty and unknown buffers are skipped", () => {
  const { tracker, items } = newTracker();
  const mediaSource = {}, video = {}, audio = {};
  tracker.noteSourceBufferAdded(mediaSource, video, "video/mp4");
  tracker.noteSourceBufferAdded(mediaSource, audio, "audio/mp4");
  tracker.noteAppend(audio, Uint8Array.of(1, 2, 3));
  tracker.noteAppend(video, new Uint8Array(0));
  tracker.noteAppend({}, Uint8Array.of(9));
  const appends = items.filter((item) => item.kind === "append");
  assert.equal(appends.length, 1);
  assert.deepEqual([appends[0].generation, appends[0].source_buffer, [...appends[0].bytes]], [1, 1, [1, 2, 3]]);
});

test("ninth source buffer and invalid mime are ignored with a log line, not silently", () => {
  const { tracker, items, messages } = newTracker();
  const mediaSource = {};
  for (let index = 0; index < 8; index += 1) tracker.noteSourceBufferAdded(mediaSource, {}, "video/mp4");
  const ninth = {};
  tracker.noteSourceBufferAdded(mediaSource, ninth, "video/mp4");
  tracker.noteAppend(ninth, Uint8Array.of(1));
  assert.equal(messages().filter((message) => message.type === "source_buffer_added").length, 8);
  assert.equal(items.filter((item) => item.kind === "append").length, 0);
  assert.equal(items.filter((item) => item.kind === "log").length, 1);
  tracker.noteSourceBufferAdded({}, {}, "видео/mp4");
  assert.equal(items.filter((item) => item.kind === "log").length, 2);
});

test("changeType, timestampOffset, remove, abort map to protocol units (microseconds)", () => {
  const { tracker, messages } = newTracker();
  const mediaSource = {}, buffer = {};
  tracker.noteSourceBufferAdded(mediaSource, buffer, "video/mp4");
  tracker.noteChangeType(buffer, "video/webm");
  tracker.noteTimestampOffset(buffer, -0.021333);
  tracker.noteRemove(buffer, 0, 30);
  tracker.noteRemove(buffer, 60, Infinity);
  tracker.noteAbort(buffer);
  assert.deepEqual(messages().slice(2), [
    { type: "source_buffer_type_changed", generation: 1, source_buffer: 0, mime_type: "video/webm" },
    { type: "timestamp_offset_changed", generation: 1, source_buffer: 0, offset_us: -21333 },
    { type: "source_buffer_removed", generation: 1, source_buffer: 0, start_us: 0, end: { kind: "at", time_us: 30000000 } },
    { type: "source_buffer_removed", generation: 1, source_buffer: 0, start_us: 60000000, end: { kind: "end_of_timeline" } },
    { type: "source_buffer_aborted", generation: 1, source_buffer: 0 },
  ]);
});

test("duration: finite, live (Infinity), unknown (NaN); a duration set before the first buffer is replayed after generation_started", () => {
  const { tracker, messages } = newTracker();
  const mediaSource = {}, buffer = {};
  tracker.noteDuration(mediaSource, 30);
  assert.equal(messages().length, 0, "no generation yet, nothing emitted");
  tracker.noteSourceBufferAdded(mediaSource, buffer, "video/mp4");
  tracker.noteDuration(mediaSource, Infinity);
  tracker.noteDuration(mediaSource, NaN);
  assert.deepEqual(messages().map((message) => message.type === "duration_changed" ? message.duration : message.type), [
    "generation_started",
    { kind: "finite", duration_us: 30000000 },
    "source_buffer_added",
    { kind: "live" },
    { kind: "unknown" },
  ]);
});

test("endOfStream with an error argument (network/decode) is not reported as end of stream", () => {
  const { tracker, messages } = newTracker();
  const mediaSource = {};
  tracker.noteSourceBufferAdded(mediaSource, {}, "video/mp4");
  tracker.noteEndOfStream(mediaSource, "decode");
  assert.equal(messages().some((message) => message.type === "end_of_stream"), false);
  tracker.noteEndOfStream(mediaSource, undefined);
  assert.equal(messages().at(-1).type, "end_of_stream");
});

test("every message the tracker can emit is accepted by the strict protocol decoder", () => {
  const { tracker, messages } = newTracker();
  const mediaSource = {}, buffer = {};
  tracker.noteDuration(mediaSource, 12.5);
  tracker.noteSourceBufferAdded(mediaSource, buffer, 'video/mp4; codecs="avc1.64001e"');
  tracker.noteChangeType(buffer, 'video/mp4; codecs="av01.0.05M.08"');
  tracker.noteTimestampOffset(buffer, 1.5);
  tracker.noteRemove(buffer, 1, 2);
  tracker.noteRemove(buffer, 1, Infinity);
  tracker.noteAbort(buffer);
  tracker.noteDuration(mediaSource, Infinity);
  tracker.noteDuration(mediaSource, NaN);
  tracker.noteEndOfStream(mediaSource);
  assert.ok(messages().length >= 10);
  for (const message of messages()) expectValidPageMessage(JSON.stringify(message));
});
