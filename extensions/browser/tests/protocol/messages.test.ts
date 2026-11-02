// Строгость разбора и записи на уровне сообщений: пустые варианты, вложенные объекты,
// правила текстов и чисел, проверки между полями, порядок полей при записи.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  MAX_CAPABILITY_TOKENS,
  MAX_PAGE_TITLE_BYTES,
  decodeExtensionControl,
  decodePageSession,
  decodePlayerControl,
  decodePlayerSession,
  encodeExtensionControl,
  encodePageSession,
  encodePlayerControl,
  encodePlayerSession,
  type DecodeResult,
  type ExtensionOpenSession,
  type PageSessionMessage,
} from "../../src/protocol/index.ts";

function kindOf(result: DecodeResult<unknown>): string {
  return result.ok ? "ok" : result.error.kind;
}

const OPEN_SESSION: ExtensionOpenSession = {
  type: "open_session",
  request_id: 7,
  session_key: "tab-1-video-1",
  page: { title: "Фильм", domain: "example.com" },
  intent: "window",
  hints: { duration: { kind: "finite", duration_us: 90_000_000 }, position: { kind: "unknown" } },
};

test("лишнее поле у каждого «пустого» варианта - unknown_field", () => {
  const unitMessages = ["attached", "play", "pause", "next_episode"];
  for (const type of unitMessages) {
    assert.equal(kindOf(decodePlayerSession(`{"type":"${type}","x":1}`)), "unknown_field", type);
    assert.equal(kindOf(decodePlayerSession(`{"type":"${type}"}`)), "ok", type);
  }
  const nestedUnits = [
    '{"type":"seek_started","target_us":0,"cause":{"kind":"page_seek","x":1}}',
    '{"type":"page_playback","request":{"kind":"play","x":1}}',
    '{"type":"range_fetch_finished","request_id":1,"outcome":{"kind":"completed","resource_size":{"kind":"unknown","x":1}}}',
    '{"type":"source_buffer_removed","generation":1,"source_buffer":0,"start_us":0,"end":{"kind":"end_of_timeline","x":1}}',
    '{"type":"duration_changed","generation":1,"duration":{"kind":"live","x":1}}',
  ];
  for (const text of nestedUnits) {
    assert.equal(kindOf(decodePageSession(text)), "unknown_field", text);
  }
  assert.equal(kindOf(decodePlayerControl('{"type":"refused","in_reply_to":{"kind":"hello","x":1},"reason":{"kind":"hello_required"}}')), "unknown_field");
});

test("вложенный объект вместо числа, массив вместо вложенного объекта - wrong_value_type", () => {
  assert.equal(kindOf(decodePageSession('{"type":"seek_started","target_us":{},"cause":{"kind":"page_seek"}}')), "wrong_value_type");
  assert.equal(kindOf(decodePageSession('{"type":"seek_started","target_us":1,"cause":["page_seek"]}')), "wrong_value_type");
  assert.equal(kindOf(decodeExtensionControl('{"type":"hello","protocol":[1,0],"software_version":"0.1.0","capabilities":[]}')), "wrong_value_type");
  assert.equal(kindOf(decodeExtensionControl('{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":{}}')), "wrong_value_type");
});

test("метка вложенного перечисления: нет - missing_field, не строка - wrong_value_type, незнакомая - unknown_value, повтор - duplicate_field", () => {
  const base = (cause: string): string => `{"type":"seek_started","target_us":0,"cause":${cause}}`;
  assert.equal(kindOf(decodePageSession(base("{}"))), "missing_field");
  assert.equal(kindOf(decodePageSession(base('{"kind":true}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(base('{"kind":null}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(base('{"kind":-1}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(base('{"kind":1.5}'))), "wrong_value_type");
  // Целое вместо метки (бывший «индекс варианта» serde) — не тот тип, в пределах и вне предела одинаково.
  assert.equal(kindOf(decodePageSession(base('{"kind":1}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(base('{"kind":7}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(base('{"kind":"teleport"}'))), "unknown_value");
  assert.equal(kindOf(decodePageSession(base('{"kind":"page_seek","kind":"page_seek"}'))), "duplicate_field");
});

test("перечисление-строка: любая не строка (в том числе объект с одним ключом) - wrong_value_type, незнакомая строка - unknown_value", () => {
  const reason = (value: string): string => `{"type":"tab_gone","reason":${value}}`;
  assert.equal(kindOf(decodePageSession(reason('{"nope":null}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(reason('{"closed":null}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(reason('"nope"'))), "unknown_value");
  assert.equal(kindOf(decodePageSession(reason('{}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(reason('{"a":1,"b":2}'))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(reason("5"))), "wrong_value_type");
  assert.equal(kindOf(decodePageSession(reason("[]"))), "wrong_value_type");
  // Форма `{"closed":null}` у Rust принимается; в протоколе ее не пишет никто, мы ее отвергаем.
  assert.equal(kindOf(decodePageSession(reason('{"closed":null}'))), "wrong_value_type");
});

test("метка проверяется раньше полей варианта, как в serde: незнакомая метка важнее неизвестного поля перед ней", () => {
  const text = '{"type":"seek_started","target_us":0,"cause":{"bogus":1,"kind":"teleport"}}';
  assert.equal(kindOf(decodePageSession(text)), "unknown_value");
  const fieldFirst = '{"type":"seek_started","target_us":0,"cause":{"bogus":1,"kind":"page_seek"}}';
  assert.equal(kindOf(decodePageSession(fieldFirst)), "unknown_field");
});

test("первая ошибка по порядку полей во входе прерывает разбор", () => {
  // target_us раньше cause: плохое число выигрывает у плохого вложенного объекта.
  assert.equal(kindOf(decodePageSession('{"type":"seek_started","target_us":-1,"cause":{"kind":"teleport"}}')), "number_out_of_range");
  assert.equal(kindOf(decodePageSession('{"type":"seek_started","cause":{"kind":"teleport"},"target_us":-1}')), "unknown_value");
});

test("числа: тип, диапазон и форма записи", () => {
  const seek = (target: string): string => `{"type":"seek","seek_id":1,"target_us":${target}}`;
  assert.equal(kindOf(decodePlayerSession(seek("1.0"))), "wrong_value_type");
  assert.equal(kindOf(decodePlayerSession(seek("1e3"))), "wrong_value_type");
  assert.equal(kindOf(decodePlayerSession(seek("-0"))), "wrong_value_type");
  assert.equal(kindOf(decodePlayerSession(seek('"5"'))), "wrong_value_type");
  assert.equal(kindOf(decodePlayerSession(seek("null"))), "wrong_value_type");
  assert.equal(kindOf(decodePlayerSession(seek("-1"))), "number_out_of_range");
  assert.equal(kindOf(decodePlayerSession(seek("0"))), "ok");
  assert.equal(kindOf(decodePlayerSession(`{"type":"seek","seek_id":4294967296,"target_us":0}`)), "number_out_of_range");
  assert.equal(kindOf(decodePlayerSession(`{"type":"seek","seek_id":4294967295,"target_us":0}`)), "ok");
});

test("скорость: границы 63 и 16000 тысячных", () => {
  const rate = (permille: number): string => `{"type":"set_rate","rate":${permille}}`;
  assert.equal(kindOf(decodePlayerSession(rate(62))), "number_out_of_range");
  assert.equal(kindOf(decodePlayerSession(rate(63))), "ok");
  assert.equal(kindOf(decodePlayerSession(rate(16_000))), "ok");
  assert.equal(kindOf(decodePlayerSession(rate(16_001))), "number_out_of_range");
});

test("fetch_range: длина 1..16 МиБ", () => {
  const range = (length: number): string => `{"type":"fetch_range","request_id":1,"offset":0,"length":${length}}`;
  assert.equal(kindOf(decodePlayerSession(range(0))), "number_out_of_range");
  assert.equal(kindOf(decodePlayerSession(range(1))), "ok");
  assert.equal(kindOf(decodePlayerSession(range(16 * 1024 * 1024))), "ok");
  assert.equal(kindOf(decodePlayerSession(range(16 * 1024 * 1024 + 1))), "number_out_of_range");
});

test("timestamp_offset: отрицательное допустимо, за пределами ±(2^53-1) - нет", () => {
  const offset = (value: string): string => `{"type":"timestamp_offset_changed","generation":1,"source_buffer":0,"offset_us":${value}}`;
  assert.equal(kindOf(decodePageSession(offset("-9007199254740991"))), "ok");
  assert.equal(kindOf(decodePageSession(offset("-9007199254740992"))), "number_out_of_range");
  assert.equal(kindOf(decodePageSession(offset("9007199254740992"))), "number_out_of_range");
  assert.equal(kindOf(decodePageSession(offset("-9223372036854775809"))), "wrong_value_type");
});

test("заголовок: длина считается в БАЙТАХ UTF-8, запрещённые символы отвергаются", () => {
  const withTitle = (title: string): string => JSON.stringify({ ...OPEN_SESSION, page: { title, domain: "example.com" } });
  assert.equal(kindOf(decodeExtensionControl(withTitle("я".repeat(MAX_PAGE_TITLE_BYTES / 2)))), "ok");
  assert.equal(kindOf(decodeExtensionControl(withTitle("я".repeat(MAX_PAGE_TITLE_BYTES / 2) + "a"))), "text_too_long");
  assert.equal(kindOf(decodeExtensionControl(withTitle("😀".repeat(MAX_PAGE_TITLE_BYTES / 4)))), "ok");
  assert.equal(kindOf(decodeExtensionControl(withTitle("😀".repeat(MAX_PAGE_TITLE_BYTES / 4) + "a"))), "text_too_long");
  assert.equal(kindOf(decodeExtensionControl(withTitle(""))), "ok");
  for (const bad of ["a\u0000b", "a\tb", "a\nb", "a\u007fb", "a\u0085b", "a b", "a b", "a‮b", "a‪b", "a⁦b", "a⁩b"]) {
    assert.equal(kindOf(decodeExtensionControl(withTitle(bad))), "text_forbidden_character", JSON.stringify(bad));
  }
  // Метки направления и соединитель эмодзи разрешены.
  assert.equal(kindOf(decodeExtensionControl(withTitle("a‎b‏c‍d"))), "ok");
});

test("домен, ключ сеанса и MIME: допустимые наборы символов", () => {
  const withDomain = (domain: string): string => JSON.stringify({ ...OPEN_SESSION, page: { title: "t", domain } });
  assert.equal(kindOf(decodeExtensionControl(withDomain("[::1]"))), "ok");
  assert.equal(kindOf(decodeExtensionControl(withDomain("xn--e1afmkfd.xn--p1ai"))), "ok");
  assert.equal(kindOf(decodeExtensionControl(withDomain("Example.com"))), "text_forbidden_character");
  assert.equal(kindOf(decodeExtensionControl(withDomain("a".repeat(254)))), "text_too_long");
  assert.equal(kindOf(decodeExtensionControl(withDomain(""))), "text_empty");
  const withKey = (key: string): string => JSON.stringify({ ...OPEN_SESSION, session_key: key });
  assert.equal(kindOf(decodeExtensionControl(withKey("a".repeat(64)))), "ok");
  assert.equal(kindOf(decodeExtensionControl(withKey("a".repeat(65)))), "text_too_long");
  assert.equal(kindOf(decodeExtensionControl(withKey("a b"))), "text_forbidden_character");
  const withMime = (mime: string): string =>
    JSON.stringify({ type: "source_buffer_added", generation: 1, source_buffer: 0, mime_type: mime });
  assert.equal(kindOf(decodePageSession(withMime('video/mp4; codecs="avc1.64002a,mp4a.40.2"'))), "ok");
  assert.equal(kindOf(decodePageSession(withMime("video/mp4;\tcodecs"))), "text_forbidden_character");
  assert.equal(kindOf(decodePageSession(withMime("видео"))), "text_forbidden_character");
});

test("токен сеанса: 32..128 символов base64url", () => {
  const granted = (token: string): string =>
    JSON.stringify({ type: "session_granted", request_id: 1, session_key: "k", port: 4000, token, outcome: "started" });
  assert.equal(kindOf(decodePlayerControl(granted("a".repeat(31)))), "text_too_short");
  assert.equal(kindOf(decodePlayerControl(granted("a".repeat(32)))), "ok");
  assert.equal(kindOf(decodePlayerControl(granted("a".repeat(128)))), "ok");
  assert.equal(kindOf(decodePlayerControl(granted("a".repeat(129)))), "text_too_long");
  assert.equal(kindOf(decodePlayerControl(granted("a".repeat(31) + "+"))), "text_forbidden_character");
  const withPort = (port: number): string =>
    JSON.stringify({ type: "session_granted", request_id: 1, session_key: "k", port, token: "a".repeat(32), outcome: "started" });
  assert.equal(kindOf(decodePlayerControl(withPort(0))), "number_out_of_range");
  assert.equal(kindOf(decodePlayerControl(withPort(65535))), "ok");
  assert.equal(kindOf(decodePlayerControl(withPort(65536))), "number_out_of_range");
});

test("hello: до 64 меток без повторов; проверка идёт и у hello_accepted", () => {
  const helloWith = (capabilities: readonly string[]): string =>
    JSON.stringify({ type: "hello", protocol: { major: 1, minor: 0 }, software_version: "0.1.0", capabilities });
  const tokens = (count: number): string[] => Array.from({ length: count }, (_, index) => `feature.f${index}`);
  assert.equal(kindOf(decodeExtensionControl(helloWith(tokens(MAX_CAPABILITY_TOKENS)))), "ok");
  assert.equal(kindOf(decodeExtensionControl(helloWith(tokens(MAX_CAPABILITY_TOKENS + 1)))), "too_many_items");
  assert.equal(kindOf(decodeExtensionControl(helloWith(["a", "b", "a"]))), "duplicate_id");
  assert.equal(kindOf(decodeExtensionControl(helloWith(["Bad Token"]))), "text_forbidden_character");
  const accepted = (capabilities: readonly string[]): string =>
    JSON.stringify({
      type: "hello_accepted",
      agreed_protocol: { major: 1, minor: 0 },
      player: { protocol: { major: 1, minor: 0 }, software_version: "0.2.0", capabilities },
    });
  assert.equal(kindOf(decodePlayerControl(accepted(["x", "x"]))), "duplicate_id");
  assert.equal(kindOf(decodePlayerControl(accepted(["cheap_codec.h264"]))), "ok");
});

test("меню вариантов: до 256, уникальные ID, выбран не больше одного", () => {
  const menu = (options: readonly { id: string; selection: string }[]): string =>
    JSON.stringify({ type: "variant_menu_changed", menu: { kind: "audio", options: options.map((option) => ({ ...option, label: "L" })) } });
  const options = (count: number): { id: string; selection: string }[] =>
    Array.from({ length: count }, (_, index) => ({ id: `v${index}`, selection: "available" }));
  assert.equal(kindOf(decodePageSession(menu(options(256)))), "ok");
  assert.equal(kindOf(decodePageSession(menu(options(257)))), "too_many_items");
  assert.equal(kindOf(decodePageSession(menu([{ id: "a", selection: "available" }, { id: "a", selection: "available" }]))), "duplicate_id");
  assert.equal(kindOf(decodePageSession(menu([{ id: "a", selection: "selected" }, { id: "b", selection: "selected" }]))), "conflicting_selection");
  assert.equal(kindOf(decodePageSession(menu([]))), "ok");
});

test("удаление из буфера: конец не позже начала - empty_range", () => {
  const removed = (start: number, end: number): string =>
    `{"type":"source_buffer_removed","generation":1,"source_buffer":0,"start_us":${start},"end":{"kind":"at","time_us":${end}}}`;
  assert.equal(kindOf(decodePageSession(removed(5, 5))), "empty_range");
  assert.equal(kindOf(decodePageSession(removed(5, 4))), "empty_range");
  assert.equal(kindOf(decodePageSession(removed(5, 6))), "ok");
});

test("причины отказа посредника: все виды разбираются, неизвестная причина - unknown_value", () => {
  const reasons = [
    "player_unavailable",
    "player_not_responding",
    "player_shutting_down",
    "protected_content",
    "unsupported_codec",
    "hello_required",
  ];
  for (const kind of reasons) {
    const text = `{"type":"refused","in_reply_to":{"kind":"open_session","request_id":3},"reason":{"kind":"${kind}"}}`;
    assert.equal(kindOf(decodePlayerControl(text)), "ok", kind);
  }
  assert.equal(kindOf(decodePlayerControl('{"type":"refused","in_reply_to":{"kind":"hello"},"reason":{"kind":"coffee_break"}}')), "unknown_value");
  assert.equal(
    kindOf(decodePlayerControl('{"type":"refused","in_reply_to":{"kind":"unparsed_message"},"reason":{"kind":"malformed_message","error":"made_up"}}')),
    "unknown_value",
  );
});

// ---------------------------------------------------------------- запись

test("запись ставит поля в порядке схемы независимо от порядка в объекте вызывающего", () => {
  const shuffled = {
    hints: OPEN_SESSION.hints,
    intent: OPEN_SESSION.intent,
    page: { domain: "example.com", title: "Фильм" },
    session_key: OPEN_SESSION.session_key,
    request_id: OPEN_SESSION.request_id,
    type: "open_session",
  } as ExtensionOpenSession;
  const written = encodeExtensionControl(shuffled);
  assert.ok(written.ok);
  assert.equal(
    written.value,
    '{"type":"open_session","request_id":7,"session_key":"tab-1-video-1","page":{"title":"Фильм","domain":"example.com"},"intent":"window","hints":{"duration":{"kind":"finite","duration_us":90000000},"position":{"kind":"unknown"}}}',
  );
});

test("запись отбрасывает чужие поля и не пишет ничего сверх схемы", () => {
  const withExtra = { type: "play", surprise: 1 } as unknown as Parameters<typeof encodePlayerSession>[0];
  const written = encodePlayerSession(withExtra);
  assert.ok(written.ok);
  assert.equal(written.value, '{"type":"play"}');
});

test("запись отвергает то, что получатель отверг бы, с той же причиной", () => {
  const badTitle = { ...OPEN_SESSION, page: { title: "a‮b", domain: "example.com" } };
  const rejected = encodeExtensionControl(badTitle);
  assert.equal(rejected.ok, false);
  if (!rejected.ok) {
    assert.equal(rejected.error.kind, "text_forbidden_character");
    assert.equal(rejected.error.detail, "page.title");
  }
  const outOfRange = encodePlayerSession({ type: "set_rate", rate: 10 });
  assert.equal(outOfRange.ok === false && outOfRange.error.kind, "number_out_of_range");
  const duplicated = encodePlayerControl({
    type: "hello_accepted",
    agreed_protocol: { major: 1, minor: 0 },
    player: { protocol: { major: 1, minor: 0 }, software_version: "1", capabilities: ["a", "a"] },
  });
  assert.equal(duplicated.ok === false && duplicated.error.kind, "duplicate_id");
  const missing = encodePageSession({ type: "generation_started" } as unknown as PageSessionMessage);
  assert.equal(missing.ok === false && missing.error.kind, "missing_field");
  const bigint = encodePageSession({ type: "generation_started", generation: 1n } as unknown as PageSessionMessage);
  assert.equal(bigint.ok === false && bigint.error.kind, "wrong_value_type");
  const lone = encodeExtensionControl({ ...OPEN_SESSION, page: { title: "a\ud800b", domain: "example.com" } });
  assert.equal(lone.ok === false && lone.error.kind, "text_forbidden_character");
});

test("ошибка не содержит содержимого сообщения, только путь по схеме", () => {
  const secretTitle = "секретный‮заголовок";
  const result = decodeExtensionControl(JSON.stringify({ ...OPEN_SESSION, page: { title: secretTitle, domain: "example.com" } }));
  assert.equal(result.ok, false);
  if (!result.ok) {
    assert.equal(result.error.detail, "page.title");
    assert.ok(!JSON.stringify(result.error).includes("секретный"));
  }
});
