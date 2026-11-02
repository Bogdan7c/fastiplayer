import test from "node:test";
import assert from "node:assert/strict";
import { frameLib, readFixture, decodeDataFrame } from "./support.mjs";

test("buildFrame is byte-identical to the protocol fixture source-buffer-first-frame.bin", () => {
  const fixture = new Uint8Array(readFixture("data/valid/source-buffer-first-frame.bin"));
  const payload = fixture.subarray(24);
  assert.equal(payload.length, 12);
  assert.deepEqual(frameLib.buildFrame(1, 0, 0, payload), fixture);
});

test("buildFrame is byte-identical to the fixture with maximal fields (u32 generation, u64 sequence)", () => {
  const fixture = new Uint8Array(readFixture("data/valid/source-buffer-max-fields.bin"));
  const built = frameLib.buildFrame(0xffffffff, 7, 18446744073709551615n, fixture.subarray(24));
  assert.deepEqual(built, fixture);
});

test("a frame built by the stand is accepted by the strict protocol decoder", () => {
  const frame = frameLib.buildFrame(3, 1, 42, Uint8Array.from([9, 8, 7]));
  const decoded = decodeDataFrame(frame);
  assert.equal(decoded.ok, true);
  assert.equal(decoded.value.header.kind, "source_buffer_bytes");
  assert.equal(decoded.value.header.generation, 3);
  assert.equal(decoded.value.header.source_buffer, 1);
  assert.equal(decoded.value.header.sequence, 42n);
  assert.deepEqual(decoded.value.payload, Uint8Array.from([9, 8, 7]));
});

test("sequencer splits a big append into valid 1 MiB frames with consecutive sequences per source buffer", () => {
  const sequencer = frameLib.createFrameSequencer();
  const big = new Uint8Array(2 * 1024 * 1024 + 5).fill(7);
  const frames = sequencer.framesFor(1, 0, big);
  assert.equal(frames.length, 3);
  const sequences = frames.map((frame) => {
    const decoded = decodeDataFrame(frame);
    assert.equal(decoded.ok, true, "every chunk must be a valid frame");
    return decoded.value.header.sequence;
  });
  assert.deepEqual(sequences, [0n, 1n, 2n]);
  assert.equal(frames.reduce((sum, frame) => sum + frame.length - 24, 0), big.length, "no payload bytes lost");
  // другой SourceBuffer и другое поколение считают с нуля независимо
  assert.equal(decodeDataFrame(sequencer.framesFor(1, 1, Uint8Array.of(1))[0]).value.header.sequence, 0n);
  assert.equal(decodeDataFrame(sequencer.framesFor(2, 0, Uint8Array.of(1))[0]).value.header.sequence, 0n);
  assert.equal(decodeDataFrame(sequencer.framesFor(1, 0, Uint8Array.of(1))[0]).value.header.sequence, 3n);
});
