// Строгая форма сообщений MAIN-мира: договорные проходят без изменений, любое отклонение поля — отказ.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import {
  MAX_PAGE_BYTES_EVENT_BYTES,
  MAX_VIDEO_CANDIDATES_PER_DOCUMENT,
  parseCandidatesReply,
  parsePageEvent,
  MAX_NEXT_VIDEO_LEASE_MS,
  parseArmNextVideoRequest,
} from "../../src/shared/page-message-validation.ts";

const MP4 = 'video/mp4; codecs="avc1.42c00a"';

/** По одному договорному образцу на каждый вид события. */
const VALID_EVENTS: readonly Record<string, unknown>[] = [
  { kind: "handoff_began", request_id: 4, outcome: "started" },
  { kind: "handoff_began", request_id: 0, outcome: "no_media" },
  { kind: "handoff_began", request_id: 0, outcome: "protected" },
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
  // 09b: MediaSource выделенного воркера — «w<номер воркера>/ms-N».
  { kind: "mse", event: { kind: "end_of_stream", media_source: "w1/ms-1" } },
  { kind: "bytes", media_source: "w12/ms-3", source_buffer: 1, bytes: new Uint8Array([4]) },
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
  ["ключ воркера с нулём", { kind: "mse", event: { kind: "end_of_stream", media_source: "w0/ms-1" } }],
  ["ключ воркера с ведущим нулём", { kind: "mse", event: { kind: "end_of_stream", media_source: "w01/ms-1" } }],
  ["ключ воркера двух уровней", { kind: "mse", event: { kind: "end_of_stream", media_source: "w1/w2/ms-1" } }],
  ["ключ воркера без номера", { kind: "mse", event: { kind: "end_of_stream", media_source: "w/ms-1" } }],
  ["ключ воркера без MediaSource", { kind: "mse", event: { kind: "end_of_stream", media_source: "w1/" } }],
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
  const candidate = { ...DEFAULT_CANDIDATE_FACTS, document: "Qx7_-9aF", element: 3, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 1280.5 * 720 };

  it("договорный ответ проходит (метка документа ровно 64 символа — тоже)", () => {
    assert.ok(parseCandidatesReply({ kind: "candidates", request_id: 2, candidates: [{ ...candidate, document: "f".repeat(64) }] }) !== undefined);
    const reply = { kind: "candidates", request_id: 2, candidates: [candidate] };
    assert.deepEqual(parseCandidatesReply(reply), reply);
    assert.deepEqual(parseCandidatesReply({ kind: "candidates", request_id: 2, candidates: [] }), { kind: "candidates", request_id: 2, candidates: [] });
  });

  for (const [description, reply] of [
    ["нет метки документа", { kind: "candidates", request_id: 1, candidates: [{ ...DEFAULT_CANDIDATE_FACTS, element: 3, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 1 }] }],
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

  // --- факты 09c: каждое поле строго, мусор страницы отвергает весь ответ ---
  const replyWith = (overrides: Record<string, unknown>) => ({ kind: "candidates", request_id: 1, candidates: [{ ...candidate, ...overrides }] });

  it("факты 09c доходят до фона без изменений (все поля, файл с хостом, MIME)", () => {
    const rich = {
      media: { kind: "file" },
      duration_s: 5400.5,
      muted: true,
      loop: true,
      video_width_px: 1920,
      video_height_px: 1080,
      drm: true,
      frame_host: "player.site.example",
      file_host: "cdn.example",
      source_mime_types: ['video/mp4; codecs="avc1.42E01E"', 'audio/mp4; codecs="mp4a.40.2"'],
    };
    assert.deepEqual(parseCandidatesReply(replyWith(rich))?.candidates[0], { ...candidate, ...rich });
  });

  it("допустимые крайние значения: длительность null, пустой хост документа, 8 MIME, кадр 0x0", () => {
    const edge = { duration_s: null, frame_host: "", video_width_px: 0, video_height_px: 0, source_mime_types: new Array<string>(8).fill("video/mp4") };
    assert.ok(parseCandidatesReply(replyWith(edge)) !== undefined);
  });

  for (const [description, overrides] of [
    ["in_worker не булево", { media: { kind: "media_source", in_worker: 1 } }],
    ["нет in_worker", { media: { kind: "media_source" } }],
    ["длительность строкой", { duration_s: "60" }],
    ["длительность отрицательная", { duration_s: -1 }],
    ["длительность NaN", { duration_s: Number.NaN }],
    ["длительность бесконечная", { duration_s: Infinity }],
    ["muted числом", { muted: 1 }],
    ["loop строкой", { loop: "yes" }],
    ["drm числом", { drm: 1 }],
    ["ширина кадра дробная", { video_width_px: 1.5 }],
    ["ширина кадра отрицательная", { video_width_px: -1 }],
    ["высота кадра сверх предела", { video_height_px: 1_000_000 }],
    ["высота кадра строкой", { video_height_px: "720" }],
    ["хост документа не строка", { frame_host: 7 }],
    ["хост документа с пробелом", { frame_host: "a b.example" }],
    ["хост документа с управляющим символом", { frame_host: "a\u0000.example" }],
    ["хост документа не ASCII", { frame_host: "пример.рф" }],
    ["хост документа длиннее 253", { frame_host: "a".repeat(254) }],
    ["хост файла у не-файла", { file_host: "cdn.example" }],
    ["хост файла пустой строкой", { media: { kind: "file" }, file_host: "" }],
    ["хост файла длиннее 253", { media: { kind: "file" }, file_host: "a".repeat(254) }],
    ["хост файла числом", { media: { kind: "file" }, file_host: 5 }],
    ["MIME не массив", { source_mime_types: "video/mp4" }],
    ["девять MIME", { source_mime_types: new Array<string>(9).fill("video/mp4") }],
    ["MIME числом", { source_mime_types: [7] }],
    ["MIME пустой", { source_mime_types: [""] }],
    ["MIME длиннее 256", { source_mime_types: ["a".repeat(257)] }],
    ["MIME не печатный ASCII", { source_mime_types: ["video/mp4\n"] }],
  ] as const) {
    it(`отказ (факты 09c): ${description}`, () => {
      assert.equal(parseCandidatesReply(replyWith(overrides)), undefined);
    });
  }

  for (const field of ["duration_s", "muted", "loop", "video_width_px", "video_height_px", "drm", "frame_host", "file_host", "source_mime_types"]) {
    it(`отказ (факты 09c): нет поля ${field}`, () => {
      const without = Object.fromEntries(Object.entries(candidate).filter(([key]) => key !== field));
      assert.equal(parseCandidatesReply({ kind: "candidates", request_id: 1, candidates: [without] }), undefined);
    });
  }
});

describe("referrer_origin в file_source (решение 202)", () => {
  const FILE = "https://cdn.example/a.mp4";

  it("принимает null и ровно происхождение http(s)", () => {
    for (const origin of [null, "https://site.example", "http://site.example:8080", "http://[::1]:3000"]) {
      const event = { kind: "file_source", url: FILE, referrer_origin: origin, referrer_policy: "origin" };
      assert.deepEqual(parsePageEvent(event), event);
    }
  });

  it("путь, запрос, учётные данные, чужие схемы и мусор — отказ", () => {
    const rejected: unknown[] = [
      "https://site.example/",
      "https://site.example/page?secret=1",
      "https://user:pw@site.example",
      "ftp://site.example",
      "chrome-extension://abc",
      "null",
      "",
      `https://${"a".repeat(400)}.example`,
      7,
      undefined,
    ];
    for (const origin of rejected) assert.equal(parsePageEvent({ kind: "file_source", url: FILE, referrer_origin: origin, referrer_policy: "origin" }), undefined, String(origin));
    assert.equal(parsePageEvent({ kind: "file_source", url: FILE }), undefined, "поле обязательно");
  });

  it("политика Referer: только значения стандарта, поле обязательно", () => {
    const event = { kind: "file_source", url: FILE, referrer_origin: "https://site.example", referrer_policy: "same-origin" };
    assert.deepEqual(parsePageEvent(event), event);
    for (const referrer_policy of ["never", "UNSAFE-URL", "", 7, null]) {
      assert.equal(parsePageEvent({ ...event, referrer_policy }), undefined, String(referrer_policy));
    }
    assert.equal(parsePageEvent({ kind: "file_source", url: FILE, referrer_origin: null }), undefined, "поле обязательно");
  });

  it("причина конца page_flood — часть договора, незнакомая причина — мусор", () => {
    assert.deepEqual(parsePageEvent({ kind: "handoff_ended", cause: "page_flood" }), { kind: "handoff_ended", cause: "page_flood" });
    assert.equal(parsePageEvent({ kind: "handoff_ended", cause: "flood" }), undefined);
  });
});

describe("сообщения сторожа следующего видео (сессия 09c2)", () => {
  const candidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-a", element: 4, media: { kind: "none" }, playing: true, visible_area_px: 1 };

  it("`next_video_held`: строгая форма, кандидаты проверяются как при опросе", () => {
    assert.ok(parsePageEvent({ kind: "next_video_held", element: 4, candidates: [candidate] }) !== undefined);
    assert.equal(parsePageEvent({ kind: "next_video_held", element: 4, candidates: [candidate], extra: 1 }), undefined);
    assert.equal(parsePageEvent({ kind: "next_video_held", element: -1, candidates: [candidate] }), undefined);
    assert.equal(parsePageEvent({ kind: "next_video_held", element: 4, candidates: [{ forged: true }] }), undefined);
  });

  it("взвод: только целые миллисекунды в пределах договора", () => {
    assert.deepEqual(parseArmNextVideoRequest({ kind: "arm_next_video", lease_ms: 3_000 }), { kind: "arm_next_video", lease_ms: 3_000 });
    assert.equal(parseArmNextVideoRequest({ kind: "arm_next_video", lease_ms: 0 }), undefined);
    assert.equal(parseArmNextVideoRequest({ kind: "arm_next_video", lease_ms: MAX_NEXT_VIDEO_LEASE_MS + 1 }), undefined);
    assert.equal(parseArmNextVideoRequest({ kind: "arm_next_video", lease_ms: 1.5 }), undefined);
    assert.equal(parseArmNextVideoRequest({ kind: "other", lease_ms: 1 }), undefined);
  });
});
