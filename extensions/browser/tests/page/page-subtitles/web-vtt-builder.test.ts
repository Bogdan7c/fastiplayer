// Сборка WebVTT из реплик (13b2): формат времени, безопасный текст, настройки, и главное — результат
// опознаётся как WebVTT тем же `sniffSubtitleDocument`, которым пользуются склад и фон.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { buildWebVttFromCues, formatCueTime, NO_CUE_SETTINGS, sanitizeCueText, type CueSnapshot } from "../../../src/page/page-subtitles/web-vtt-builder.ts";
import { sniffSubtitleDocument } from "../../../src/shared/subtitle-document-sniff.ts";

function cue(overrides: Partial<CueSnapshot> & Pick<CueSnapshot, "startSeconds" | "endSeconds" | "text">): CueSnapshot {
  return { id: "", settings: NO_CUE_SETTINGS, ...overrides };
}

describe("время реплики", () => {
  it("чч:мм:сс.ммм с округлением до миллисекунды", () => {
    assert.equal(formatCueTime(0), "00:00:00.000");
    assert.equal(formatCueTime(61.5), "00:01:01.500");
    assert.equal(formatCueTime(3725.0004), "01:02:05.000");
    assert.equal(formatCueTime(3725.0006), "01:02:05.001");
    assert.equal(formatCueTime(360_000), "100:00:00.000", "больше 99 часов — три цифры часов, как допускает WebVTT");
  });

  it("не число, минус и бесконечность — нет времени", () => {
    for (const bad of [Number.NaN, -1, Number.POSITIVE_INFINITY]) assert.equal(formatCueTime(bad), null, String(bad));
  });
});

describe("текст реплики", () => {
  it("пустые строки выброшены, переводы строк приведены к \\n, `-->` обезврежен", () => {
    assert.equal(sanitizeCueText("one\r\n\r\ntwo\r  \nthree"), "one\ntwo\nthree");
    assert.equal(sanitizeCueText("a --> b"), "a --&gt; b");
    assert.equal(sanitizeCueText("  \n \n"), "");
  });
});

describe("документ WebVTT из реплик", () => {
  it("собранный документ опознаётся как web_vtt и содержит реплики по порядку", () => {
    const built = buildWebVttFromCues([
      cue({ startSeconds: 1, endSeconds: 2.5, text: "Hello" }),
      cue({ id: "second", startSeconds: 3, endSeconds: 4, text: "line one\n\nline two" }),
    ]);
    assert.equal(built.cueCount, 2);
    assert.equal(built.text, "WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nHello\n\nsecond\n00:00:03.000 --> 00:00:04.000\nline one\nline two\n\n");
    assert.deepEqual(sniffSubtitleDocument(new TextEncoder().encode(built.text)), { kind: "document", format: "web_vtt" });
  });

  it("непригодные реплики пропускаются и не считаются: пустой текст, обратное время, NaN", () => {
    const built = buildWebVttFromCues([
      cue({ startSeconds: 1, endSeconds: 2, text: "keep" }),
      cue({ startSeconds: 2, endSeconds: 3, text: "   \n  " }),
      cue({ startSeconds: 5, endSeconds: 4, text: "backwards" }),
      cue({ startSeconds: Number.NaN, endSeconds: 4, text: "nan" }),
      cue({ startSeconds: 6, endSeconds: 6, text: "zero length" }),
    ]);
    assert.equal(built.cueCount, 1);
    assert.equal(built.text, "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nkeep\n\n");
  });

  it("без годных реплик — только заголовок и cueCount 0", () => {
    const built = buildWebVttFromCues([]);
    assert.deepEqual({ cueCount: built.cueCount, text: built.text }, { cueCount: 0, text: "WEBVTT\n\n" });
  });

  it("безопасные настройки реплики пишутся, по умолчанию и мусор — нет", () => {
    const built = buildWebVttFromCues([
      cue({
        startSeconds: 1,
        endSeconds: 2,
        text: "styled",
        settings: { align: "start", line: 10, lineIsPercent: true, position: 20, size: 80, vertical: "rl" },
      }),
      cue({
        startSeconds: 3,
        endSeconds: 4,
        text: "hostile",
        settings: { align: "start\n\nNOTE evil", line: Number.NaN, lineIsPercent: false, position: null, size: null, vertical: "diagonal" },
      }),
      cue({ startSeconds: 5, endSeconds: 6, text: "plain", settings: { align: null, line: -2, lineIsPercent: false, position: null, size: null, vertical: null } }),
    ]);
    assert.equal(
      built.text,
      "WEBVTT\n\n00:00:01.000 --> 00:00:02.000 align:start line:10% position:20% size:80% vertical:rl\nstyled\n\n00:00:03.000 --> 00:00:04.000\nhostile\n\n00:00:05.000 --> 00:00:06.000 line:-2\nplain\n\n",
    );
  });

  it("небезопасный идентификатор не попадает в документ", () => {
    const built = buildWebVttFromCues([
      cue({ id: "a --> b", startSeconds: 1, endSeconds: 2, text: "x" }),
      cue({ id: "line\nbreak", startSeconds: 3, endSeconds: 4, text: "y" }),
      cue({ id: "z".repeat(101), startSeconds: 5, endSeconds: 6, text: "w" }),
      cue({ id: "ok-7", startSeconds: 7, endSeconds: 8, text: "v" }),
    ]);
    assert.equal(built.text, "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nx\n\n00:00:03.000 --> 00:00:04.000\ny\n\n00:00:05.000 --> 00:00:06.000\nw\n\nok-7\n00:00:07.000 --> 00:00:08.000\nv\n\n");
  });
});
