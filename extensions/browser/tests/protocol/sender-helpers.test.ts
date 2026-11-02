// Помощники отправителя: чистка заголовка и нарезка appendBuffer.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  MAX_DATA_FRAME_PAYLOAD_BYTES,
  MAX_PAGE_TITLE_BYTES,
  decodeDataFrame,
  decodeExtensionControl,
  encodeDataFrame,
  sanitizePageTitle,
  splitAppendPayload,
  utf8ByteLength,
} from "../../src/protocol/index.ts";

/** Проверка «как у получателя»: заголовок проходит строгий разбор open_session. */
function titleIsAccepted(title: string): boolean {
  const message = {
    type: "open_session",
    request_id: 1,
    session_key: "k",
    page: { title, domain: "example.com" },
    intent: "window",
    hints: { duration: { kind: "unknown" }, position: { kind: "unknown" } },
  };
  return decodeExtensionControl(JSON.stringify(message)).ok;
}

test("sanitizePageTitle: обычный текст, эмодзи и RTL остаются как есть", () => {
  for (const title of ["", "Simple title", "Шрек 2 — серія 3 🎬", "بث مباشر — שידור חי", "a‎b‍c"]) {
    assert.equal(sanitizePageTitle(title), title);
    assert.ok(titleIsAccepted(sanitizePageTitle(title)));
  }
});

test("sanitizePageTitle: разрывы строки -> один пробел, прочие управляющие и переворот направления убираются", () => {
  // NUL и DEL убираются, TAB/LF/CR/NEL становятся пробелом-разделителем слов.
  assert.equal(sanitizePageTitle("a\u0000b\tc\nd\re\u007ff\u0085g"), "ab c d ef g");
  assert.equal(sanitizePageTitle("x y z"), "x y z");
  // Подряд идущие разрывы (и пробел перед ними) дают ровно один пробел: слова не склеиваются и не разъезжаются.
  assert.equal(sanitizePageTitle("Серія 3 \r\n\n\tОзвучка"), "Серія 3 Озвучка");
  assert.equal(sanitizePageTitle("exe‮4pm.txt"), "exe4pm.txt");
  assert.equal(sanitizePageTitle("‪‫‬‭‮⁦⁧⁨⁩"), "");
  assert.equal(sanitizePageTitle("a\ud800b\udc00c"), "abc", "одиночные суррогаты убираются");
});

test("sanitizePageTitle: обрезка до 1024 байт ровно по границе символа", () => {
  for (const [character, bytes] of [["a", 1], ["я", 2], ["€", 3], ["😀", 4]] as const) {
    const long = character.repeat(2000);
    const cut = sanitizePageTitle(long);
    assert.ok(utf8ByteLength(cut) <= MAX_PAGE_TITLE_BYTES);
    assert.equal(cut, character.repeat(Math.floor(MAX_PAGE_TITLE_BYTES / bytes)), character);
    assert.ok(titleIsAccepted(cut));
  }
  // Ровно по границе: 1024 байта входа не меняются, 1025-й байт отрезается.
  const exact = "я".repeat(512);
  assert.equal(sanitizePageTitle(exact), exact);
  assert.equal(sanitizePageTitle(exact + "a"), exact);
  // 3-байтовый символ на границе не влезает целиком - не попадает вовсе.
  assert.equal(sanitizePageTitle("a".repeat(1022) + "€"), "a".repeat(1022));
  // Запрещённые символы не занимают места в лимите.
  assert.equal(sanitizePageTitle("\u0000".repeat(5000) + "ok"), "ok");
  // Серия разрывов схлопывается в один пробел и занимает 1 байт лимита.
  assert.equal(sanitizePageTitle("\n".repeat(5000) + "ok"), " ok");
});

test("sanitizePageTitle: результат на смеси мусора всегда проходит строгий разбор", () => {
  const junk = ["\u0000", "‮", " ", "😀", "я", "a", "\ud800", "\u0085", " "];
  let seed = 12345;
  const nextIndex = (): number => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed % junk.length;
  };
  for (let round = 0; round < 200; round += 1) {
    const raw = Array.from({ length: 600 }, () => junk[nextIndex()]).join("");
    assert.ok(titleIsAccepted(sanitizePageTitle(raw)), `раунд ${round}`);
  }
});

test("splitAppendPayload: куски не больше 1 МиБ, склейка равна входу, без копий", () => {
  const total = 2 * MAX_DATA_FRAME_PAYLOAD_BYTES + 123;
  const input = new Uint8Array(total);
  for (let index = 0; index < total; index += 4099) input[index] = (index / 4099) % 251;
  const chunks = splitAppendPayload(input);
  assert.deepEqual(chunks.map((chunk) => chunk.length), [MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES, 123]);
  for (const chunk of chunks) {
    assert.equal(chunk.buffer, input.buffer, "ссылка на тот же буфер, не копия");
  }
  assert.ok(Buffer.concat(chunks).equals(input));
  // Каждый кусок годится в рамку.
  for (const [index, chunk] of chunks.entries()) {
    const frame = encodeDataFrame({ kind: "source_buffer_bytes", generation: 1, source_buffer: 0, sequence: BigInt(index) }, chunk);
    assert.ok(frame.ok);
    assert.equal(decodeDataFrame(frame.value).ok, true);
  }
});

test("splitAppendPayload: границы размера", () => {
  assert.deepEqual(splitAppendPayload(new Uint8Array(0)), [], "пустой вход - нет рамок");
  assert.equal(splitAppendPayload(new Uint8Array(1)).length, 1);
  assert.deepEqual(splitAppendPayload(new Uint8Array(MAX_DATA_FRAME_PAYLOAD_BYTES)).map((c) => c.length), [MAX_DATA_FRAME_PAYLOAD_BYTES]);
  assert.deepEqual(splitAppendPayload(new Uint8Array(MAX_DATA_FRAME_PAYLOAD_BYTES + 1)).map((c) => c.length), [MAX_DATA_FRAME_PAYLOAD_BYTES, 1]);
  assert.deepEqual(splitAppendPayload(new Uint8Array(10), 4).map((c) => c.length), [4, 4, 2]);
  assert.throws(() => splitAppendPayload(new Uint8Array(10), 0), RangeError);
  assert.throws(() => splitAppendPayload(new Uint8Array(10), MAX_DATA_FRAME_PAYLOAD_BYTES + 1), RangeError);
});

test("splitAppendPayload: принимает ArrayBuffer и представления, уважая смещение", () => {
  const buffer = new ArrayBuffer(10);
  new Uint8Array(buffer).set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  const fromBuffer = splitAppendPayload(buffer, 4);
  assert.deepEqual(fromBuffer.map((chunk) => [...chunk]), [[0, 1, 2, 3], [4, 5, 6, 7], [8, 9]]);
  const view = new DataView(buffer, 2, 5);
  assert.deepEqual(splitAppendPayload(view, 3).map((chunk) => [...chunk]), [[2, 3, 4], [5, 6]]);
  const shifted = new Uint8Array(buffer, 3, 4);
  assert.deepEqual(splitAppendPayload(shifted, 3).map((chunk) => [...chunk]), [[3, 4, 5], [6]]);
});
