// Свойство «encode(decode(x)) == x» на сгенерированных сообщениях (воспроизводимый генератор).
// Для каждой случайно собранной валидной структуры: запись проходит, разбор записанного
// даёт ту же структуру, повторная запись даёт те же байты.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  decodeExtensionControl,
  decodePageSession,
  decodePlayerControl,
  decodePlayerSession,
  encodeExtensionControl,
  encodePageSession,
  encodePlayerControl,
  encodePlayerSession,
  type DecodeResult,
  type EncodeResult,
  type ExtensionControlMessage,
  type PageSessionMessage,
  type PlayerControlMessage,
  type PlayerSessionMessage,
} from "../../src/protocol/index.ts";

/** Небольшой детерминированный генератор (LCG): тест воспроизводим на любой машине. */
class Random {
  private state: number;
  constructor(seed: number) {
    this.state = seed;
  }
  next(): number {
    this.state = (Math.imul(this.state, 1664525) + 1013904223) >>> 0;
    return this.state;
  }
  int(maxExclusive: number): number {
    return Math.floor((this.next() / 2 ** 32) * maxExclusive);
  }
  pick<T>(items: readonly T[]): T {
    return items[this.int(items.length)] as T;
  }
  /** Целое в 0..2^53-1 с перекосом к границам. */
  safeInt(): number {
    return this.pick([0, 1, this.int(1000), this.next(), Number.MAX_SAFE_INTEGER, this.next() * 2 ** 20 + this.int(2 ** 20)]);
  }
  text(alphabet: readonly string[], minLength: number, maxLength: number): string {
    const length = minLength + this.int(maxLength - minLength + 1);
    return Array.from({ length }, () => this.pick(alphabet)).join("");
  }
}

const DISPLAY = [..."abcXYZ 019-—!?'\"\\/", "я", "Ї", "é", "日", "😀", "🎬", "ש", "ب", "‍", "‎"];
const BASE64URL = [..."abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_"];
const DOMAIN = [..."abcdefghijklmnopqrstuvwxyz0123456789-."];
const MIME = [..."video/mp4;codecs=\"avc1.64002a, \""];

function generation(random: Random): number {
  return 1 + random.int(2 ** 32 - 1);
}

function duration(random: Random): { kind: "unknown" } | { kind: "finite"; duration_us: number } | { kind: "live" } {
  return random.pick([
    { kind: "unknown" } as const,
    { kind: "finite", duration_us: random.safeInt() } as const,
    { kind: "live" } as const,
  ]);
}

function extensionControl(random: Random): ExtensionControlMessage {
  if (random.int(2) === 0) {
    const capabilities = Array.from(new Set(Array.from({ length: random.int(8) }, () => random.text([..."abc_.-12"], 1, 12))));
    return {
      type: "hello",
      protocol: { major: random.int(65536), minor: random.int(65536) },
      software_version: random.text([..."0123456789.-+abc"], 1, 32),
      capabilities,
    };
  }
  return {
    type: "open_session",
    request_id: random.next(),
    session_key: random.text(BASE64URL, 1, 64),
    page: { title: random.text(DISPLAY, 0, 40), domain: random.text(DOMAIN, 1, 60) },
    intent: random.pick(["window", "picture_in_picture", "in_page"] as const),
    hints: {
      duration: duration(random),
      position: random.int(2) === 0 ? { kind: "unknown" } : { kind: "at", position_us: random.safeInt() },
    },
  };
}

function playerControl(random: Random): PlayerControlMessage {
  const pick = random.int(3);
  if (pick === 0) {
    return {
      type: "hello_accepted",
      agreed_protocol: { major: random.int(65536), minor: random.int(65536) },
      player: {
        protocol: { major: 1, minor: random.int(5) },
        software_version: "0.2.0",
        capabilities: ["cheap_codec.h264", "feature.future"].slice(0, random.int(3)),
      },
    };
  }
  if (pick === 1) {
    return {
      type: "session_granted",
      request_id: random.next(),
      session_key: random.text(BASE64URL, 1, 64),
      port: 1 + random.int(65535),
      token: random.text(BASE64URL, 32, 128),
      outcome: random.pick(["started", "reused_existing", "replaced_previous"] as const),
    };
  }
  return {
    type: "refused",
    in_reply_to: random.pick([
      { kind: "hello" } as const,
      { kind: "open_session", request_id: random.next() } as const,
      { kind: "unparsed_message" } as const,
    ]),
    reason: random.pick([
      { kind: "player_unavailable" } as const,
      { kind: "incompatible_version", update: "player" } as const,
      { kind: "malformed_message", error: "duplicate_field" } as const,
      { kind: "hello_required" } as const,
      { kind: "protected_content" } as const,
    ]),
  };
}

function pageSession(random: Random): PageSessionMessage {
  const base = { generation: generation(random), source_buffer: random.int(8) };
  const options = Array.from({ length: random.int(6) }, (_, index) => ({
    id: `id-${index}`,
    label: random.text(DISPLAY.filter((symbol) => symbol !== "‍"), 1, 20),
    selection: "available" as const,
  }));
  return random.pick<PageSessionMessage>([
    { type: "attach", session_key: random.text(BASE64URL, 1, 64), token: random.text(BASE64URL, 32, 128) },
    { type: "generation_started", generation: base.generation },
    { type: "file_source_added", generation: base.generation },
    { type: "source_buffer_added", ...base, mime_type: random.text(MIME, 1, 80) },
    { type: "source_buffer_type_changed", ...base, mime_type: random.text(MIME, 1, 80) },
    { type: "timestamp_offset_changed", ...base, offset_us: random.int(2) === 0 ? random.safeInt() : 0 - random.safeInt() },
    { type: "source_buffer_removed", ...base, start_us: 10, end: { kind: "at", time_us: 11 + random.int(1000) } },
    { type: "source_buffer_removed", ...base, start_us: random.safeInt(), end: { kind: "end_of_timeline" } },
    { type: "source_buffer_aborted", ...base },
    { type: "seek_started", target_us: random.safeInt(), cause: { kind: "player_seek", seek_id: random.next() } },
    { type: "seek_started", target_us: random.safeInt(), cause: { kind: "page_seek" } },
    { type: "duration_changed", generation: base.generation, duration: duration(random) },
    { type: "end_of_stream", generation: base.generation },
    { type: "page_playback", request: { kind: "set_rate", rate: 63 + random.int(16000 - 63 + 1) } },
    { type: "page_playback", request: { kind: "play" } },
    { type: "variant_menu_changed", menu: { kind: random.pick(["audio", "quality", "subtitles"] as const), options } },
    {
      type: "range_fetch_finished",
      request_id: random.next(),
      outcome: { kind: "completed", resource_size: random.int(2) === 0 ? { kind: "unknown" } : { kind: "known", size_bytes: random.safeInt() } },
    },
    { type: "range_fetch_finished", request_id: random.next(), outcome: { kind: "failed", failure: "http_error" } },
    { type: "tab_gone", reason: random.pick(["closed", "navigated_away", "browser_closing"] as const) },
  ]);
}

function playerSession(random: Random): PlayerSessionMessage {
  return random.pick<PlayerSessionMessage>([
    { type: "attached" },
    { type: "clock", seq: random.next(), position_us: random.safeInt(), state: random.pick(["playing", "paused", "waiting", "ended"] as const), rate: 63 + random.int(15938) },
    { type: "buffer_state", generation: generation(random), ahead_us: random.safeInt(), intake: random.pick(["accepting", "full"] as const) },
    { type: "play" },
    { type: "pause" },
    { type: "seek", seek_id: random.next(), target_us: random.safeInt() },
    { type: "set_rate", rate: 63 + random.int(15938) },
    { type: "return_to_page", position_us: random.safeInt(), resume: random.pick(["playing", "paused"] as const) },
    { type: "select_variant", kind: random.pick(["audio", "quality", "subtitles"] as const), variant_id: random.text(BASE64URL, 1, 64) },
    { type: "next_episode" },
    { type: "fetch_range", request_id: random.next(), offset: random.safeInt(), length: 1 + random.int(16 * 1024 * 1024) },
    { type: "stream_rejected", generation: generation(random), reason: random.pick(["protected_content", "unsupported_codec", "unparsable_media", "file_unavailable", "playback_failed"] as const) },
  ]);
}

function checkRoundTrip<Message>(
  label: string,
  message: Message,
  encode: (message: Message) => EncodeResult<string>,
  decode: (text: string) => DecodeResult<Message>,
): void {
  const written = encode(message);
  assert.ok(written.ok, `${label}: запись ${written.ok ? "" : `${written.error.kind} ${written.error.detail}`}: ${JSON.stringify(message)}`);
  const decoded = decode(written.value);
  assert.ok(decoded.ok, `${label}: разбор ${decoded.ok ? "" : `${decoded.error.kind} ${decoded.error.detail}`}: ${written.value}`);
  assert.deepEqual(decoded.value, message, `${label}: структура после разбора`);
  const rewritten = encode(decoded.value);
  assert.ok(rewritten.ok);
  assert.equal(rewritten.value, written.value, `${label}: байты при повторной записи`);
}

const ROUNDS = 400;

test("extension_control: encode(decode(x)) == x на сгенерированных сообщениях", () => {
  const random = new Random(1);
  for (let round = 0; round < ROUNDS; round += 1) {
    checkRoundTrip("extension_control", extensionControl(random), encodeExtensionControl, decodeExtensionControl);
  }
});

test("player_control: encode(decode(x)) == x на сгенерированных сообщениях", () => {
  const random = new Random(2);
  for (let round = 0; round < ROUNDS; round += 1) {
    checkRoundTrip("player_control", playerControl(random), encodePlayerControl, decodePlayerControl);
  }
});

test("page_session: encode(decode(x)) == x на сгенерированных сообщениях", () => {
  const random = new Random(3);
  const seenTypes = new Set<string>();
  for (let round = 0; round < ROUNDS; round += 1) {
    const message = pageSession(random);
    seenTypes.add(message.type);
    checkRoundTrip("page_session", message, encodePageSession, decodePageSession);
  }
  assert.equal(seenTypes.size, 15, "генератор покрыл все типы сообщений страницы");
});

test("player_session: encode(decode(x)) == x на сгенерированных сообщениях", () => {
  const random = new Random(4);
  const seenTypes = new Set<string>();
  for (let round = 0; round < ROUNDS; round += 1) {
    const message = playerSession(random);
    seenTypes.add(message.type);
    checkRoundTrip("player_session", message, encodePlayerSession, decodePlayerSession);
  }
  assert.equal(seenTypes.size, 12, "генератор покрыл все типы сообщений плеера");
});
