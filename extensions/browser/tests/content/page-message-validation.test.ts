// Строгая форма сообщений MAIN-мира: договорные проходят без изменений, любое отклонение поля — отказ.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  MAX_PAGE_BYTES_EVENT_BYTES,
  MAX_VIDEO_CANDIDATES_PER_DOCUMENT,
  parseCandidatesReply,
  parsePageEvent,
} from "../../src/shared/page-message-validation.ts";

const MP4 = 'video/mp4; codecs="avc1.42c00a"';

/** По одному договорному образцу на каждый вид события. */
const VALID_EVENTS: readonly Record<string, unknown>[] = [
  { kind: "handoff_began", request_id: 4, outcome: "started" },
  { kind: "handoff_began", request_id: 0, outcome: "no_live_media" },
  { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } },
  { kind: "mse", event: { kind: "source_buffer_type_changed", media_source: "ms-12", source_buffer: 7, mime_type: "audio/webm" } },
  { kind: "mse", event: { kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 1, offset_us: -5_000_000 } },
  { kind: "mse", event: { kind: "source_buffer_removed", media_source: "ms-1", source_buffer: 0, start_us: 0, end: { kind: "at", time_us: 10 } } },
  { kind: "mse", event: { kind: "source_buffer_removed", media_source: "ms-1", source_buffer: 0, start_us: 5, end: { kind: "end_of_timeline" } } },
  { kind: "mse", event: { kind: "source_buffer_aborted", media_source: "ms-1", source_buffer: 2 } },
  { kind: "mse", event: { kind: "duration_changed", media_source: "ms-1", duration: { kind: "finite", duration_us: 90_000_000 } } },
  { kind: "mse", event: { kind: "duration_changed", media_source: "ms-1", duration: { kind: "live" } } },
  { kind: "mse", event: { kind: "duration_changed", media_source: "ms-1", duration: { kind: "unknown" } } },
  { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-3" } },
  { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([1, 2, 3]) },
  { kind: "page_seek", target_us: 12_000_000 },
  { kind: "player_seek_applied", seek_id: 0xffff_ffff, target_us: 0 },
  { kind: "page_playback", request: { kind: "play" } },
  { kind: "page_playback", request: { kind: "pause" } },
  { kind: "page_playback", request: { kind: "set_rate", rate: 63 } },
  { kind: "handoff_ended", cause: "returned" },
  { kind: "handoff_ended", cause: "element_released" },
];

/** Испорченные копии: [описание, сообщение]. Каждое обязано быть отвергнуто. */
const INVALID_EVENTS: readonly [string, unknown][] = [
  ["не объект", "page_seek"],
  ["null", null],
  ["массив", [{ kind: "page_seek", target_us: 1 }]],
  ["неизвестный вид", { kind: "page_seek_v2", target_us: 1 }],
  ["лишнее поле", { kind: "page_seek", target_us: 1, extra: true }],
  ["нет поля", { kind: "page_seek" }],
  ["отрицательное время", { kind: "page_seek", target_us: -1 }],
  ["дробное время", { kind: "page_seek", target_us: 1.5 }],
  ["время строкой", { kind: "page_seek", target_us: "1" }],
  ["время больше 2^53", { kind: "page_seek", target_us: 2 ** 53 }],
  ["request_id больше u32", { kind: "handoff_began", request_id: 2 ** 32, outcome: "started" }],
  ["неизвестный исход", { kind: "handoff_began", request_id: 1, outcome: "maybe" }],
  ["seek_id отрицательный", { kind: "player_seek_applied", seek_id: -1, target_us: 0 }],
  ["скорость ниже предела", { kind: "page_playback", request: { kind: "set_rate", rate: 62 } }],
  ["скорость выше предела", { kind: "page_playback", request: { kind: "set_rate", rate: 16_001 } }],
  ["лишнее поле в запросе", { kind: "page_playback", request: { kind: "play", rate: 1000 } }],
  ["неизвестная причина конца", { kind: "handoff_ended", cause: "bored" }],
  ["mse без события", { kind: "mse" }],
  ["mse лишнее поле", { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" }, extra: 1 }],
  ["ключ MediaSource не по форме", { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-0" } }],
  ["ключ MediaSource чужой", { kind: "mse", event: { kind: "end_of_stream", media_source: "__proto__" } }],
  ["индекс SB 8", { kind: "mse", event: { kind: "source_buffer_aborted", media_source: "ms-1", source_buffer: 8 } }],
  ["индекс SB отрицательный", { kind: "mse", event: { kind: "source_buffer_aborted", media_source: "ms-1", source_buffer: -1 } }],
  ["пустой MIME", { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "" } }],
  ["MIME с переводом строки", { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "video/mp4\n" } }],
  ["MIME не ASCII", { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "відео/mp4" } }],
  ["MIME длиннее 256", { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "v".repeat(257) } }],
  ["пустой промежуток удаления", { kind: "mse", event: { kind: "source_buffer_removed", media_source: "ms-1", source_buffer: 0, start_us: 10, end: { kind: "at", time_us: 10 } } }],
  ["конец удаления кривой", { kind: "mse", event: { kind: "source_buffer_removed", media_source: "ms-1", source_buffer: 0, start_us: 0, end: { kind: "forever" } } }],
  ["смещение дробное", { kind: "mse", event: { kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: 0.5 } }],
  ["длительность без числа", { kind: "mse", event: { kind: "duration_changed", media_source: "ms-1", duration: { kind: "finite" } } }],
  ["длительность с лишним полем", { kind: "mse", event: { kind: "duration_changed", media_source: "ms-1", duration: { kind: "live", duration_us: 1 } } }],
  ["байты массивом", { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: [1, 2, 3] }],
  ["байты ArrayBuffer", { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new ArrayBuffer(3) }],
  ["байты другим видом массива", { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint16Array(3) }],
  ["пустые байты", { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(0) }],
  ["байты на SharedArrayBuffer", { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(new SharedArrayBuffer(4)) }],
  ["байты сверх предела", { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(MAX_PAGE_BYTES_EVENT_BYTES + 1) }],
  ["байты без индекса SB", { kind: "bytes", media_source: "ms-1", bytes: new Uint8Array(1) }],
];

describe("форма событий страницы", () => {
  it("договорные события проходят без изменений (байты — тот же объект, без копии)", () => {
    for (const event of VALID_EVENTS) {
      const parsed = parsePageEvent(event);
      assert.deepEqual(parsed, event, JSON.stringify(event));
      if (event["kind"] === "bytes") assert.equal((parsed as { bytes: Uint8Array }).bytes, event["bytes"]);
    }
  });

  it("байты ровно на пределе проходят", () => {
    const event = { kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array(MAX_PAGE_BYTES_EVENT_BYTES) };
    assert.ok(parsePageEvent(event) !== undefined);
  });

  for (const [description, message] of INVALID_EVENTS) {
    it(`отказ: ${description}`, () => {
      assert.equal(parsePageEvent(message), undefined);
    });
  }

  it("подсунутый метод (toJSON) — лишнее поле, отказ", () => {
    assert.equal(parsePageEvent({ kind: "page_seek", target_us: 1, toJSON: () => ({}) }), undefined);
  });
});

describe("форма ответа candidates", () => {
  const candidate = { document: "Qx7_-9aF", element: 3, has_live_media: true, playing: false, visible_area_px: 1280.5 * 720 };

  it("договорный ответ проходит (метка документа ровно 64 символа — тоже)", () => {
    assert.ok(parseCandidatesReply({ kind: "candidates", request_id: 2, candidates: [{ ...candidate, document: "f".repeat(64) }] }) !== undefined);
    const reply = { kind: "candidates", request_id: 2, candidates: [candidate] };
    assert.deepEqual(parseCandidatesReply(reply), reply);
    assert.deepEqual(parseCandidatesReply({ kind: "candidates", request_id: 2, candidates: [] }), { kind: "candidates", request_id: 2, candidates: [] });
  });

  for (const [description, reply] of [
    ["нет метки документа", { kind: "candidates", request_id: 1, candidates: [{ element: 3, has_live_media: true, playing: false, visible_area_px: 1 }] }],
    ["пустая метка документа", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, document: "" }] }],
    ["метка документа длиннее 64", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, document: "a".repeat(65) }] }],
    ["метка документа с чужими символами", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, document: "doc/1" }] }],
    ["метка документа числом", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, document: 7 }] }],
    ["кандидат с лишним полем", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, src: "x" }] }],
    ["площадь отрицательная", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, visible_area_px: -1 }] }],
    ["площадь бесконечная", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, visible_area_px: Infinity }] }],
    ["флаг строкой", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, playing: "yes" }] }],
    ["ключ элемента дробный", { kind: "candidates", request_id: 1, candidates: [{ ...candidate, element: 1.5 }] }],
    ["кандидаты не массив", { kind: "candidates", request_id: 1, candidates: { 0: candidate } }],
    ["кандидатов сверх предела", { kind: "candidates", request_id: 1, candidates: new Array(MAX_VIDEO_CANDIDATES_PER_DOCUMENT + 1).fill(candidate) }],
    ["нет request_id", { kind: "candidates", candidates: [] }],
    ["лишнее поле ответа", { kind: "candidates", request_id: 1, candidates: [], more: 1 }],
  ] as const) {
    it(`отказ: ${description}`, () => {
      assert.equal(parseCandidatesReply(reply), undefined);
    });
  }
});
