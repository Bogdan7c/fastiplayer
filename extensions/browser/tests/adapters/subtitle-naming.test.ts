// Именование документа субтитров по адресу (13b2, решение 276): настоящие формы адресов YouTube и общий читатель.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { nameSubtitleDocumentByAddress, parseAddressParts, queryValueOf } from "../../src/adapters/subtitle-naming/index.ts";

/** Адрес timedtext в форме, в какой его шлёт плеер YouTube (идентификатор и подпись вымышлены). */
const YOUTUBE_TIMEDTEXT = "https://www.youtube.com/api/timedtext?v=abcdefghijk&ei=XXXX&caps=asr&opi=112496729&xoaf=5&hl=en&ip=0.0.0.0&ipbits=0&expire=1760000000&sparams=ip,ipbits,expire,v,ei,caps,opi,xoaf&signature=ABCDEF.123456&key=yt8&lang=en";

describe("адаптер timedtext YouTube", () => {
  it("ручные субтитры: язык из lang, не авто", () => {
    assert.deepEqual(nameSubtitleDocumentByAddress(`${YOUTUBE_TIMEDTEXT}&fmt=json3`), { language: "en", label: null, auto_generated: false });
  });

  it("автосубтитры: kind=asr — auto_generated", () => {
    assert.deepEqual(nameSubtitleDocumentByAddress(`${YOUTUBE_TIMEDTEXT}&kind=asr&fmt=srv3`), { language: "en", label: null, auto_generated: true });
  });

  it("автоперевод: язык берётся из tlang, а не из lang исходной дорожки", () => {
    assert.deepEqual(nameSubtitleDocumentByAddress(`${YOUTUBE_TIMEDTEXT}&tlang=de&fmt=json3`), { language: "de", label: null, auto_generated: false });
    assert.deepEqual(nameSubtitleDocumentByAddress(`${YOUTUBE_TIMEDTEXT}&kind=asr&tlang=pt-BR`), { language: "pt-BR", label: null, auto_generated: true });
  });

  it("хосты youtube-nocookie.com, www. и m. узнаются; чужой хост и чужой путь — нет", () => {
    assert.equal(nameSubtitleDocumentByAddress("https://www.youtube-nocookie.com/api/timedtext?v=abcdefghijk&lang=uk&kind=asr").auto_generated, true);
    assert.equal(nameSubtitleDocumentByAddress("https://youtube.com/api/timedtext?lang=fr&kind=asr").language, "fr");
    assert.equal(nameSubtitleDocumentByAddress("https://m.youtube.com/api/timedtext?lang=es&kind=asr").auto_generated, true);
    // Чужой хост: `kind=asr` значения не имеет, остаётся общий читатель параметра lang.
    assert.deepEqual(nameSubtitleDocumentByAddress("https://evil-youtube.com/api/timedtext?lang=en&kind=asr"), { language: "en", label: null, auto_generated: false });
    // Другой путь того же хоста — общий читатель.
    assert.deepEqual(nameSubtitleDocumentByAddress("https://www.youtube.com/watch?lang=ru&kind=asr"), { language: "ru", label: null, auto_generated: false });
  });

  it("слишком длинный или пустой tlang не затирает lang", () => {
    assert.equal(nameSubtitleDocumentByAddress(`${YOUTUBE_TIMEDTEXT}&tlang=`).language, "en");
    assert.equal(nameSubtitleDocumentByAddress(`${YOUTUBE_TIMEDTEXT}&tlang=${"x".repeat(80)}`).language, "en");
  });
});

describe("общий читатель адреса", () => {
  it("lang и language; регистр имени параметра не важен, значение декодируется", () => {
    assert.deepEqual(nameSubtitleDocumentByAddress("https://cdn.example/subs/movie.vtt?lang=pt-BR"), { language: "pt-BR", label: null, auto_generated: false });
    assert.deepEqual(nameSubtitleDocumentByAddress("https://cdn.example/s?Language=zh%2DHans"), { language: "zh-Hans", label: null, auto_generated: false });
  });

  it("адрес без языка, не http(s) и мусор — «не названа»", () => {
    const unnamed = { language: null, label: null, auto_generated: false };
    assert.deepEqual(nameSubtitleDocumentByAddress("https://cdn.example/subs/en.vtt"), unnamed);
    assert.deepEqual(nameSubtitleDocumentByAddress("blob:https://site.example/1?lang=en"), unnamed);
    assert.deepEqual(nameSubtitleDocumentByAddress("not an address"), unnamed);
    assert.deepEqual(nameSubtitleDocumentByAddress("https://cdn.example/s?lang="), unnamed);
  });
});

describe("разбор частей адреса без глобального URL", () => {
  it("хост без логина и порта, путь, запрос; битый % не роняет разбор", () => {
    const parts = parseAddressParts("HTTPS://User:pw@Cdn.Example:8443/a/b.vtt?x=1&bad=%E0%A4%A&Lang=en#frag");
    assert.ok(parts !== null);
    assert.equal(parts.host, "cdn.example");
    assert.equal(parts.path, "/a/b.vtt");
    assert.equal(queryValueOf(parts, "x"), "1");
    assert.equal(queryValueOf(parts, "bad"), "%E0%A4%A", "битая последовательность остаётся как есть");
    assert.equal(queryValueOf(parts, "lang"), "en");
    assert.equal(queryValueOf(parts, "missing"), null);
  });
});
