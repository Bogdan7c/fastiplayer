// Нумерация поколений по документу и перевод событий страницы в провод сеанса.
// Ключевой тест: записанные потоки протокола (fixtures/streams), пересказанные событиями страницы
// с локальными ключами, проходят переводчик и сокет и дают на проводе ТЕ ЖЕ байты, что в фикстуре.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";
import { createDocumentTranslator, type StreamPageEvent } from "../../src/background/handoff/page-event-translator.ts";
import { openSessionSocket } from "../../src/background/handoff/session-socket.ts";
import { createStreamNumbering } from "../../src/background/handoff/stream-numbering.ts";
import {
  decodeDataFrame,
  decodePageSession,
  encodeDataFrame,
  MAX_DATA_FRAME_PAYLOAD_BYTES,
  type PageSessionMessage,
} from "../../src/protocol/index.ts";
import { FIXTURES_ROOT, readFixtureBytes } from "../protocol/fixtures-support.ts";
import { createFakeClock, recordSockets, TEST_PORT, TEST_TOKEN } from "./support.ts";

const MP4 = 'video/mp4; codecs="avc1.42c00a"';

function added(mediaSource: string, sourceBuffer: number): StreamPageEvent {
  return { kind: "mse", event: { kind: "source_buffer_added", media_source: mediaSource, source_buffer: sourceBuffer, mime_type: MP4 } };
}

function bytes(mediaSource: string, sourceBuffer: number, payload: Uint8Array): StreamPageEvent {
  return { kind: "bytes", media_source: mediaSource, source_buffer: sourceBuffer, bytes: payload };
}

describe("нумерация поколений", () => {
  it("каждый новый MediaSource документа — следующее поколение; повтор ключа — то же поколение", () => {
    const numbering = createStreamNumbering();
    const document = numbering.openDocument();
    assert.deepEqual(document.generationOf("ms-1"), { kind: "started", generation: 1 });
    assert.deepEqual(document.generationOf("ms-2"), { kind: "started", generation: 2 });
    assert.deepEqual(document.generationOf("ms-1"), { kind: "existing", generation: 1 });
    assert.equal(numbering.latestGeneration(), 2);
  });

  it("перезагруженный документ снова говорит «ms-1», но получает НОВЫЙ номер (находка 03b)", () => {
    const numbering = createStreamNumbering();
    const first = numbering.openDocument();
    first.generationOf("ms-1");
    first.generationOf("ms-2");
    const reloaded = numbering.openDocument();
    assert.deepEqual(reloaded.generationOf("ms-1"), { kind: "started", generation: 3 });
    assert.deepEqual(first.generationOf("ms-1"), { kind: "existing", generation: 1 }, "старый документ свои ключи помнит");
  });

  it("номера рамок свои у каждой пары (поколение, SourceBuffer), с 0 подряд", () => {
    const numbering = createStreamNumbering();
    assert.equal(numbering.nextFrameSequence(1, 0), 0n);
    assert.equal(numbering.nextFrameSequence(1, 0), 1n);
    assert.equal(numbering.nextFrameSequence(1, 1), 0n);
    assert.equal(numbering.nextFrameSequence(2, 0), 0n);
    assert.equal(numbering.nextFrameSequence(1, 0), 2n);
  });
});

describe("переводчик событий страницы", () => {
  it("первое упоминание MediaSource открывает поколение сообщением generation_started", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    const result = translator.translate(added("ms-1", 0));
    assert.ok(result.ok);
    assert.deepEqual(
      result.items.map((item) => (item.kind === "message" ? item.message : item.kind)),
      [
        { type: "generation_started", generation: 1 },
        { type: "source_buffer_added", generation: 1, source_buffer: 0, mime_type: MP4 },
      ],
    );
  });

  it("два документа одного сеанса: номера не повторяются, порядковые номера рамок свои", () => {
    const numbering = createStreamNumbering();
    const first = createDocumentTranslator(numbering);
    const second = createDocumentTranslator(numbering);
    first.translate(added("ms-1", 0));
    const frameOfFirst = first.translate(bytes("ms-1", 0, new Uint8Array([1])));
    second.translate(added("ms-1", 0));
    const frameOfSecond = second.translate(bytes("ms-1", 0, new Uint8Array([2])));
    assert.ok(frameOfFirst.ok && frameOfSecond.ok);
    const headerOf = (item: (typeof frameOfFirst.items)[number]) => (item.kind === "frame" ? item.header : undefined);
    assert.deepEqual(headerOf(frameOfFirst.items[0]!), { kind: "source_buffer_bytes", generation: 1, source_buffer: 0, sequence: 0n });
    assert.deepEqual(headerOf(frameOfSecond.items[0]!), { kind: "source_buffer_bytes", generation: 2, source_buffer: 0, sequence: 0n });
  });

  it("кусок больше 1 МиБ режется на рамки ≤ 1 МиБ с номерами подряд, байты не теряются", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    translator.translate(added("ms-1", 0));
    const payload = new Uint8Array(MAX_DATA_FRAME_PAYLOAD_BYTES * 2 + 5).map((_, index) => index % 251);
    const result = translator.translate(bytes("ms-1", 0, payload));
    assert.ok(result.ok);
    const frames = result.items.filter((item) => item.kind === "frame");
    assert.deepEqual(frames.map((frame) => (frame.header.kind === "source_buffer_bytes" ? frame.header.sequence : undefined)), [0n, 1n, 2n]);
    assert.deepEqual(frames.map((frame) => frame.payload.length), [MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES, 5]);
    const joined = new Uint8Array(payload.length);
    let offset = 0;
    for (const frame of frames) {
      joined.set(frame.payload, offset);
      offset += frame.payload.length;
    }
    assert.deepEqual(joined, payload);
  });

  it("байты и события необъявленного SourceBuffer — проблема потока, а не битый провод", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    assert.deepEqual(translator.translate(bytes("ms-1", 0, new Uint8Array([1]))), { ok: false, problem: "source_buffer_not_announced" });
    translator.translate(added("ms-2", 0));
    assert.deepEqual(translator.translate(bytes("ms-2", 1, new Uint8Array([1]))), { ok: false, problem: "source_buffer_not_announced" });
    assert.deepEqual(
      translator.translate({ kind: "mse", event: { kind: "source_buffer_aborted", media_source: "ms-2", source_buffer: 3 } }),
      { ok: false, problem: "source_buffer_not_announced" },
    );
    assert.deepEqual(translator.translate(added("ms-2", 0)), { ok: false, problem: "source_buffer_announced_twice" });
  });

  it("перемотки и play/pause страницы переводятся в seek_started и page_playback", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    const messages = [
      translator.translate({ kind: "page_seek", target_us: 7_000_000 }),
      translator.translate({ kind: "player_seek_applied", seek_id: 9, target_us: 8_000_000 }),
      translator.translate({ kind: "page_playback", request: { kind: "set_rate", rate: 1500 } }),
    ].flatMap((result) => (result.ok ? result.items : [])).map((item) => (item.kind === "message" ? item.message : undefined));
    assert.deepEqual(messages, [
      { type: "seek_started", target_us: 7_000_000, cause: { kind: "page_seek" } },
      { type: "seek_started", target_us: 8_000_000, cause: { kind: "player_seek", seek_id: 9 } },
      { type: "page_playback", request: { kind: "set_rate", rate: 1500 } },
    ]);
  });
});

// ------------------------------------------------------------------ записанные потоки

type StreamStep = { text: string } | { binary: string } | { await_clock_us: number };

/** Пересказ сообщения протокола событием страницы (обратный перевод; поколение N = «ms-N»). */
function pageEventFor(message: PageSessionMessage): StreamPageEvent | undefined {
  const key = (generation: number): string => `ms-${generation}`;
  switch (message.type) {
    case "generation_started":
      return undefined; // переводчик выводит его сам из первого упоминания MediaSource
    case "source_buffer_added":
    case "source_buffer_type_changed":
      return { kind: "mse", event: { kind: message.type, media_source: key(message.generation), source_buffer: message.source_buffer, mime_type: message.mime_type } };
    case "timestamp_offset_changed":
      return { kind: "mse", event: { kind: message.type, media_source: key(message.generation), source_buffer: message.source_buffer, offset_us: message.offset_us } };
    case "source_buffer_removed":
      return { kind: "mse", event: { kind: message.type, media_source: key(message.generation), source_buffer: message.source_buffer, start_us: message.start_us, end: message.end } };
    case "source_buffer_aborted":
      return { kind: "mse", event: { kind: message.type, media_source: key(message.generation), source_buffer: message.source_buffer } };
    case "duration_changed":
      return { kind: "mse", event: { kind: message.type, media_source: key(message.generation), duration: message.duration } };
    case "end_of_stream":
      return { kind: "mse", event: { kind: message.type, media_source: key(message.generation) } };
    case "seek_started":
      return message.cause.kind === "page_seek"
        ? { kind: "page_seek", target_us: message.target_us }
        : { kind: "player_seek_applied", seek_id: message.cause.seek_id, target_us: message.target_us };
    case "page_playback":
      return { kind: "page_playback", request: message.request };
    default:
      throw new Error(`в MSE-потоке нет ${message.type}`);
  }
}

for (const streamName of ["two-source-buffers-mid-box-cuts", "quality-switch-new-media-source", "muxed-av-single-source-buffer"]) {
  it(`поток «${streamName}»: события страницы → переводчик → сокет дают байты фикстуры`, () => {
    const directory = join("streams", streamName);
    const recorded = JSON.parse(readFileSync(join(FIXTURES_ROOT, directory, "stream.json"), "utf8")) as { steps: StreamStep[] };
    const clock = createFakeClock();
    const sockets = recordSockets();
    const session = openSessionSocket({
      port: TEST_PORT,
      sessionKey: "fixture",
      token: TEST_TOKEN,
      createWebSocket: sockets.create,
      scheduler: clock.scheduler,
      callbacks: { onAttached: () => {}, onPlayerMessage: () => {}, onEnded: () => assert.fail("сеанс не должен кончаться") },
    });
    sockets.last().open();
    sockets.last().receive({ type: "attached" });
    const translator = createDocumentTranslator(createStreamNumbering());
    const expectedWire: (string | Uint8Array)[] = [];
    for (const step of recorded.steps) {
      if ("await_clock_us" in step) continue;
      const fixtureBytes = readFixtureBytes(join(directory, "text" in step ? step.text : step.binary));
      let event: StreamPageEvent | undefined;
      if ("text" in step) {
        const decoded = decodePageSession(fixtureBytes);
        assert.ok(decoded.ok);
        expectedWire.push(new TextDecoder().decode(fixtureBytes));
        event = pageEventFor(decoded.value);
      } else {
        const frame = decodeDataFrame(fixtureBytes);
        assert.ok(frame.ok && frame.value.header.kind === "source_buffer_bytes");
        expectedWire.push(fixtureBytes);
        // Страница отдала ровно груз рамки (каждый кусок фикстуры ≤ 1 МиБ — одна рамка).
        event = bytes(`ms-${frame.value.header.generation}`, frame.value.header.source_buffer, frame.value.payload.slice());
      }
      if (event === undefined) continue;
      const translated = translator.translate(event);
      assert.ok(translated.ok, `${JSON.stringify(step)}: ${translated.ok ? "" : translated.problem}`);
      for (const item of translated.items) session.enqueue(item);
    }
    const wire = sockets.last().sent.slice(1); // без attach
    assert.equal(wire.length, expectedWire.length, "столько же сообщений и рамок, сколько в фикстуре");
    wire.forEach((payload, index) => assert.deepEqual(payload, expectedWire[index], `элемент ${index} провода отличается от фикстуры`));
  });
}

it("рамка переводчика байт в байт равна encodeDataFrame с тем же заголовком", () => {
  const translator = createDocumentTranslator(createStreamNumbering());
  translator.translate(added("ms-1", 2));
  const payload = new Uint8Array([9, 8, 7, 6]);
  const result = translator.translate(bytes("ms-1", 2, payload));
  assert.ok(result.ok);
  const frame = result.items[0]!;
  assert.equal(frame.kind, "frame");
  const expected = encodeDataFrame({ kind: "source_buffer_bytes", generation: 1, source_buffer: 2, sequence: 0n }, payload);
  assert.ok(expected.ok);
  if (frame.kind !== "frame") return;
  const actual = encodeDataFrame(frame.header, frame.payload);
  assert.ok(actual.ok);
  assert.deepEqual(actual.value, expected.value);
});
