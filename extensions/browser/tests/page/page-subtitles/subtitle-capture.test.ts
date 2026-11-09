// Ловля документов субтитров хуками `fetch` и XHR (13b2) до передачи: что попадает на склад, а что не читается вовсе.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { decode, encode, pageFetches, pageXhrs, settleReads, startSubtitlePage, VTT_DOCUMENT } from "../support/subtitle-setup.ts";

const VTT_URL = "https://cdn.example/subs/en.vtt";

describe("хук fetch: что читается", () => {
  it("документ субтитров попадает на склад, а страница получает тот же промис и своё нетронутое тело", async () => {
    const subtitlePage = startSubtitlePage();
    const reply = { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) };
    const pageBytes = await pageFetches(subtitlePage, reply);
    assert.equal(decode(pageBytes ?? new Uint8Array()), VTT_DOCUMENT, "тело, прочитанное страницей, целое");
    const stored = subtitlePage.harness.bridge.subtitles.store.snapshot();
    assert.equal(stored.length, 1);
    assert.equal(decode(stored[0]?.bytes ?? new Uint8Array()), VTT_DOCUMENT, "на складе те же байты");
    assert.equal(stored[0]?.formatHint, "web_vtt");
    assert.equal(subtitlePage.env.ledger.clones, 1, "ровно один clone");
    assert.equal(subtitlePage.harness.debugLines.length, 0);
  });

  it("страница получает ИМЕННО промис оригинального fetch, а отказ сети остаётся отказом", async () => {
    const subtitlePage = startSubtitlePage();
    const pageFetch = (subtitlePage.harness.page.win as unknown as { fetch: (url: string) => Promise<unknown> }).fetch;
    subtitlePage.env.serve({ url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
    const returned = pageFetch(VTT_URL);
    assert.equal(returned, subtitlePage.env.lastFetch.promise, "тот же объект промиса");
    await returned;
    const failed = pageFetch("https://cdn.example/unserved.vtt");
    await assert.rejects(failed, TypeError);
    await settleReads();
  });

  it("имя и язык дорожки берутся из адреса (YouTube timedtext, автосубтитры)", async () => {
    const subtitlePage = startSubtitlePage();
    const url = "https://www.youtube.com/api/timedtext?v=abcdefghijk&lang=en&kind=asr&fmt=json3";
    await pageFetches(subtitlePage, { url, contentType: "application/json; charset=UTF-8", body: encode('{"wireMagic":"pb3","events":[{"tStartMs":0}]}') });
    const [stored] = subtitlePage.harness.bridge.subtitles.store.snapshot();
    assert.deepEqual(stored?.naming, { language: "en", label: null, auto_generated: true });
    assert.equal(stored?.formatHint, "json3");
  });

  it("одинаковое содержимое по двум адресам — одна дорожка", async () => {
    const subtitlePage = startSubtitlePage();
    await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
    await pageFetches(subtitlePage, { url: "https://cdn.example/subs/en-copy.vtt", contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 1);
  });
});

describe("хук fetch: что не читается и не попадает на склад", () => {
  it("двоичные ответы и чужие типы не читаются вовсе: clone не вызывается", async () => {
    const subtitlePage = startSubtitlePage();
    const unreadable = [
      { url: "https://cdn.example/video/segment1.m4s", contentType: "video/mp4", body: new Uint8Array([0, 0, 0, 24]) },
      { url: "https://cdn.example/audio.mp3", contentType: "audio/mpeg", body: new Uint8Array([0xff, 0xfb]) },
      { url: "https://cdn.example/poster.png", contentType: "image/png", body: new Uint8Array([0x89, 0x50]) },
      { url: "https://cdn.example/live/master.m3u8", contentType: "application/vnd.apple.mpegurl", body: encode("#EXTM3U\n") },
      { url: "https://cdn.example/api/me", contentType: "application/json", body: encode('{"events":[]}') },
      { url: "https://cdn.example/page.html", contentType: "text/html", body: encode("<html></html>") },
      { url: "https://cdn.example/subs/missing.vtt", contentType: "text/vtt", status: 404, body: encode("not found") },
      { url: "https://cdn.example/subs/opaque.vtt", contentType: "text/vtt", type: "opaque", body: encode(VTT_DOCUMENT) },
      { url: "https://cdn.example/subs/huge.vtt", contentType: "text/vtt", contentLength: String(9 * 1024 * 1024), body: encode(VTT_DOCUMENT) },
    ];
    for (const reply of unreadable) await pageFetches(subtitlePage, reply);
    assert.equal(subtitlePage.env.ledger.clones, 0, "ни один ответ не клонировался");
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 0);
  });

  it("тело длиннее 8 МиБ при неизвестной длине: чтение прерывается cancel(), документа нет", async () => {
    const subtitlePage = startSubtitlePage();
    const chunkBytes = 1024 * 1024;
    await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", endlessChunkBytes: chunkBytes });
    assert.equal(subtitlePage.env.ledger.clones, 1);
    assert.equal(subtitlePage.env.ledger.cancelledStreams, 1, "поток отменён");
    assert.ok(subtitlePage.env.ledger.chunksPulled <= 12, `читали ${subtitlePage.env.ledger.chunksPulled} кусков по 1 МиБ — должны остановиться у потолка 8 МиБ`);
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 0);
  });

  it("содержимое решает: кусочек HLS, чужой JSON под адресом fmt=json3 и мусор не попадают на склад", async () => {
    const subtitlePage = startSubtitlePage();
    const hlsSegment = "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:00:01.000 --> 00:00:02.000\nHi\n";
    await pageFetches(subtitlePage, { url: "https://cdn.example/hls/sub0.vtt", contentType: "text/vtt", body: encode(hlsSegment) });
    await pageFetches(subtitlePage, { url: "https://api.example/x?fmt=json3", contentType: "application/json", body: encode('{"user":"x"}') });
    await pageFetches(subtitlePage, { url: "https://cdn.example/subs/garbage.vtt", contentType: "text/vtt", body: new Uint8Array([1, 2, 3, 4]) });
    assert.equal(subtitlePage.env.ledger.clones, 3, "все три прошли фильтр по заголовкам и были прочитаны");
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 0);
  });

  it("сбой clone() не виден странице: она получает ответ, в консоль — одна отладочная запись", async () => {
    const subtitlePage = startSubtitlePage();
    subtitlePage.env.lastFetch.cloneFailure = new Error("body already used");
    const pageBytes = await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
    assert.equal(decode(pageBytes ?? new Uint8Array()), VTT_DOCUMENT, "страница прочитала тело как обычно");
    assert.deepEqual(subtitlePage.harness.debugLines, ["Fastiplayer Bridge: observing a fetch response failed"]);
  });
});

describe("хук XHR", () => {
  const xhrReply = (overrides: Record<string, unknown> = {}) => ({ url: VTT_URL, contentType: "text/vtt", ...overrides });

  it("responseType text и пустой: responseText → UTF-8 байты на складе", async () => {
    const subtitlePage = startSubtitlePage();
    await pageXhrs(subtitlePage, xhrReply(), "", { text: "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nПривет\n" });
    const [stored] = subtitlePage.harness.bridge.subtitles.store.snapshot();
    assert.equal(decode(stored?.bytes ?? new Uint8Array()), "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nПривет\n", "кириллица прошла без искажений");
    await pageXhrs(subtitlePage, xhrReply({ url: "https://cdn.example/subs/de.vtt" }), "text", { text: VTT_DOCUMENT });
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 2);
  });

  it("responseType arraybuffer: копия байт", async () => {
    const subtitlePage = startSubtitlePage();
    const buffer = encode(VTT_DOCUMENT).buffer as ArrayBuffer;
    await pageXhrs(subtitlePage, xhrReply(), "arraybuffer", { buffer });
    const [stored] = subtitlePage.harness.bridge.subtitles.store.snapshot();
    assert.equal(decode(stored?.bytes ?? new Uint8Array()), VTT_DOCUMENT);
    assert.notEqual(stored?.bytes.buffer, buffer, "склад держит свою копию, а не буфер страницы");
  });

  it("responseType json: объект превращается в документ только при субтитровом адресе", async () => {
    const subtitlePage = startSubtitlePage();
    const json3 = { wireMagic: "pb3", events: [{ tStartMs: 0, dDurationMs: 1000, segs: [{ utf8: "Hi" }] }] };
    await pageXhrs(subtitlePage, xhrReply({ url: "https://www.youtube.com/api/timedtext?v=abcdefghijk&lang=en&fmt=json3", contentType: "application/json" }), "json", { json: json3 });
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 1);
    assert.equal(subtitlePage.harness.bridge.subtitles.store.snapshot()[0]?.formatHint, "json3");
    await pageXhrs(subtitlePage, xhrReply({ url: "https://api.example/v1/state", contentType: "application/json" }), "json", { json: { events: [1] } });
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 1, "обычный JSON сайта не читается");
  });

  it("blob, двоичные типы и неуспешные ответы не читаются", async () => {
    const subtitlePage = startSubtitlePage();
    await pageXhrs(subtitlePage, xhrReply(), "blob", { text: VTT_DOCUMENT });
    await pageXhrs(subtitlePage, xhrReply({ url: "https://cdn.example/v.mp4", contentType: "video/mp4" }), "", { text: VTT_DOCUMENT });
    await pageXhrs(subtitlePage, xhrReply({ status: 500 }), "", { text: VTT_DOCUMENT });
    await pageXhrs(subtitlePage, xhrReply({ url: "https://cdn.example/subs/long.vtt", contentLength: String(9 * 1024 * 1024) }), "", { text: VTT_DOCUMENT });
    assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 0);
  });

  it("пустой responseURL: берётся адрес последнего open() (запрос переиспользован)", async () => {
    const subtitlePage = startSubtitlePage();
    const xhr = new subtitlePage.env.FakeXhr();
    xhr.open("GET", "https://cdn.example/other.bin");
    xhr.open("GET", "https://cdn.example/subs/movie.vtt?lang=uk");
    xhr.responseType = "text";
    xhr.testRespond(xhrReply({ url: "" }), { text: VTT_DOCUMENT });
    await settleReads();
    const [stored] = subtitlePage.harness.bridge.subtitles.store.snapshot();
    assert.deepEqual(stored?.naming, { language: "uk", label: null, auto_generated: false });
  });
});
