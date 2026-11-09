// Записанные потоки (fixtures/streams/*): живые последовательности сообщений и рамок.
//
// Что проверяется на стороне TypeScript:
// 1. каждое текстовое сообщение разбирается и пишется обратно байт в байт;
// 2. каждая двоичная рамка разбирается и собирается обратно байт в байт;
// 3. порядковые номера рамок каждого SourceBuffer идут 0, 1, 2... без пропусков;
// 4. склейка груза рамок по каждому SourceBuffer равна склейке указанных диапазонов
//    медиафайла (то есть через TS-разбор рамок байты доезжают без потерь и искажений);
// 5. число «устаревших» рамок и сообщений совпадает с ожиданием;
// 6. тела ответов рамками вида 2 (ресурсы HLS, протокол 1.1): смещения одного запроса
//    идут подряд с 0 (смещение - внутри ТЕЛА, а не в файле на сервере), склейка равна
//    диапазонам исходного файла, а `completed` называет ровно длину тела.
//
// Сторож поколений (StreamGate) - дело приёмника (плеера), в TypeScript его нет.
// Для пункта 4-5 тест применяет его единственное простое правило сам: всё, что
// относится к поколению СТАРШЕ уже начатого, - устаревшее и в склейку не входит.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import {
  decodeDataFrame,
  decodePageSession,
  encodeDataFrame,
  encodePageSession,
  type PageSessionMessage,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT, concatBytes, loadManifest, readFixtureBytes } from "./fixtures-support.ts";

type StreamStep = { text: string } | { binary: string } | { await_clock_us: number };

interface RecordedStream {
  readonly steps: readonly StreamStep[];
  readonly expect: {
    readonly admitted: readonly {
      readonly generation: number;
      readonly source_buffer: number;
      readonly media: string;
      readonly ranges: readonly (readonly [number, number])[];
    }[];
    readonly dropped_frames: number;
    readonly dropped_messages: number;
    readonly resource_bodies: readonly {
      readonly request_id: number;
      /** Путь от корня фикстур. */
      readonly source: string;
      readonly ranges: readonly (readonly [number, number])[];
    }[];
  };
}

/** Поколение, к которому относится сообщение; `undefined` - сообщение вне поколений. */
function generationOf(message: PageSessionMessage): number | undefined {
  return "generation" in message ? message.generation : undefined;
}

interface ReplayOutcome {
  readonly bytesBySourceBuffer: Map<string, Uint8Array[]>;
  readonly sequencesBySourceBuffer: Map<string, bigint[]>;
  droppedFrames: number;
  droppedMessages: number;
  messageCount: number;
  frameCount: number;
  awaitedClocks: number[];
  /** Тела ответов рамками вида 2 по номеру запроса. */
  readonly resourceBodies: Map<number, Uint8Array[]>;
  /** Запросы, чей итог `completed` пришёл (длина уже сверена с телом). */
  readonly completedRequests: number[];
}

/** Длина уже полученного тела запроса. */
function receivedLength(outcome: ReplayOutcome, requestId: number): number {
  return (outcome.resourceBodies.get(requestId) ?? []).reduce((sum, part) => sum + part.length, 0);
}

function replay(directory: string, steps: readonly StreamStep[]): ReplayOutcome {
  const outcome: ReplayOutcome = {
    bytesBySourceBuffer: new Map(),
    sequencesBySourceBuffer: new Map(),
    droppedFrames: 0,
    droppedMessages: 0,
    messageCount: 0,
    frameCount: 0,
    awaitedClocks: [],
    resourceBodies: new Map(),
    completedRequests: [],
  };
  let newestGeneration = 0;
  for (const step of steps) {
    if ("text" in step) {
      const bytes = readFixtureBytes(join("streams", directory, step.text));
      const decoded = decodePageSession(bytes);
      assert.ok(decoded.ok, `${step.text}: ${decoded.ok ? "" : decoded.error.kind}`);
      const written = encodePageSession(decoded.value);
      assert.ok(written.ok);
      assert.deepEqual(new TextEncoder().encode(written.value), bytes, `${step.text}: обратно не байт в байт`);
      outcome.messageCount += 1;
      const generation = generationOf(decoded.value);
      if (generation !== undefined && generation < newestGeneration) {
        outcome.droppedMessages += 1;
      } else if (decoded.value.type === "generation_started") {
        newestGeneration = decoded.value.generation;
      }
      const finished = decoded.value;
      if (finished.type === "resource_fetch_finished" && finished.outcome.kind === "completed") {
        assert.equal(finished.outcome.size_bytes, receivedLength(outcome, finished.request_id), `${step.text}: размер не равен телу`);
        outcome.completedRequests.push(finished.request_id);
      }
    } else if ("binary" in step) {
      const bytes = readFixtureBytes(join("streams", directory, step.binary));
      const decoded = decodeDataFrame(bytes);
      assert.ok(decoded.ok, `${step.binary}: ${decoded.ok ? "" : decoded.error.kind}`);
      const { header, payload } = decoded.value;
      const rewritten = encodeDataFrame(header, payload);
      assert.ok(rewritten.ok);
      assert.deepEqual(rewritten.value, bytes, `${step.binary}: рамка обратно не байт в байт`);
      outcome.frameCount += 1;
      if (header.kind === "range_bytes") {
        // Тело ответа: смещение продолжает уже полученное тело этого запроса.
        assert.equal(header.offset, receivedLength(outcome, header.request_id), `${step.binary}: смещение не продолжает тело`);
        outcome.resourceBodies.set(header.request_id, [...(outcome.resourceBodies.get(header.request_id) ?? []), payload]);
        continue;
      }
      // Субтитры страницы (1.5) в записанных потоках не встречаются (их случаи - page-subtitle-gate.json).
      assert.notEqual(header.kind, "page_subtitle_bytes", `${step.binary}: субтитры в записанном потоке`);
      if (header.kind === "page_subtitle_bytes") continue;
      if (header.generation < newestGeneration) {
        outcome.droppedFrames += 1;
        continue;
      }
      const key = `${header.generation}/${header.source_buffer}`;
      outcome.bytesBySourceBuffer.set(key, [...(outcome.bytesBySourceBuffer.get(key) ?? []), payload]);
      outcome.sequencesBySourceBuffer.set(key, [...(outcome.sequencesBySourceBuffer.get(key) ?? []), header.sequence]);
    } else {
      // Темп записи: страница ждала часы плеера. Не провод, часы не идут назад.
      const previous = outcome.awaitedClocks.at(-1);
      assert.ok(previous === undefined || previous < step.await_clock_us, "паузы записи должны ждать всё более поздние часы");
      outcome.awaitedClocks.push(step.await_clock_us);
    }
  }
  return outcome;
}

for (const streamName of loadManifest().streams) {
  test(`поток ${streamName}: рамки и сообщения разбираются, груз склеивается в исходные байты медиа`, () => {
    const recorded = JSON.parse(readFileSync(join(FIXTURES_ROOT, "streams", streamName, "stream.json"), "utf8")) as RecordedStream;
    const outcome = replay(streamName, recorded.steps);

    assert.ok(outcome.messageCount > 0, "в потоке есть сообщения");
    const expectedKeys = recorded.expect.admitted.map((entry) => `${entry.generation}/${entry.source_buffer}`).sort();
    assert.deepEqual([...outcome.bytesBySourceBuffer.keys()].sort(), expectedKeys, "набор SourceBuffer");

    for (const expected of recorded.expect.admitted) {
      const key = `${expected.generation}/${expected.source_buffer}`;
      const media = readFixtureBytes(join("media", expected.media));
      const expectedBytes = concatBytes(expected.ranges.map(([start, end]) => media.subarray(start, end)));
      const actualBytes = concatBytes(outcome.bytesBySourceBuffer.get(key) ?? []);
      assert.equal(actualBytes.length, expectedBytes.length, `${key}: длина склейки`);
      assert.ok(Buffer.compare(actualBytes, expectedBytes) === 0, `${key}: байты не совпали с медиафайлом`);

      // Номера рамок идут подряд с нуля: пропуск означал бы потерянный кусок.
      const sequences = outcome.sequencesBySourceBuffer.get(key) ?? [];
      assert.deepEqual(sequences, sequences.map((_, index) => BigInt(index)), `${key}: порядковые номера`);
    }
    assert.equal(outcome.droppedFrames, recorded.expect.dropped_frames, "устаревшие рамки");
    assert.equal(outcome.droppedMessages, recorded.expect.dropped_messages, "устаревшие сообщения");

    const expectedRequests = recorded.expect.resource_bodies.map((body) => body.request_id).sort();
    assert.deepEqual([...outcome.resourceBodies.keys()].sort(), expectedRequests, "запросы с телом");
    assert.deepEqual([...outcome.completedRequests].sort(), expectedRequests, "итоги completed");
    for (const body of recorded.expect.resource_bodies) {
      const source = readFixtureBytes(body.source);
      const expectedBytes = concatBytes(body.ranges.map(([start, end]) => source.subarray(start, end)));
      const actualBytes = concatBytes(outcome.resourceBodies.get(body.request_id) ?? []);
      assert.ok(Buffer.compare(actualBytes, expectedBytes) === 0, `запрос ${body.request_id}: тело не совпало с ${body.source}`);
    }
  });
}

test("поток смены качества несёт паузы записи с растущими часами", () => {
  const recorded = JSON.parse(
    readFileSync(join(FIXTURES_ROOT, "streams", "quality-switch-new-media-source", "stream.json"), "utf8"),
  ) as RecordedStream;
  const outcome = replay("quality-switch-new-media-source", recorded.steps);
  assert.deepEqual(outcome.awaitedClocks, [1_800_000, 3_200_000, 4_400_000]);
});
