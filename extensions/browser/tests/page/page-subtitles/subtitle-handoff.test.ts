// Сквозные тесты субтитров страницы в MAIN-мире (13b2): фальшивая страница, настоящая сборка моста и
// канал участка 1. Что скачано ДО передачи, приходит сразу за журналом MSE; пойманное во время передачи —
// сразу; после `handoff_ended` — ничего.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parsePageEvent } from "../../../src/shared/page-message-validation.ts";
import { sniffSubtitleDocument } from "../../../src/shared/subtitle-document-sniff.ts";
import { handshake, type IsolatedSide } from "../support/bridge-harness.ts";
import { addPlayingMseVideo, describeAndBegin, playerClock } from "../support/handoff-steps.ts";
import {
  decode,
  documentsOf,
  encode,
  onlyDocument,
  pageFetches,
  SITE_ORIGIN,
  startSubtitlePage,
  subtitleEventsOf,
  trackSourcesOf,
  VTT_DOCUMENT,
  type SubtitlePage,
} from "../support/subtitle-setup.ts";

const VTT_URL = "https://cdn.example/subs/en.vtt";

async function withHandoffPage(body: (subtitlePage: SubtitlePage, isolated: IsolatedSide, video: ReturnType<typeof addPlayingMseVideo>["video"]) => Promise<void>): Promise<void> {
  const subtitlePage = startSubtitlePage();
  const { video } = addPlayingMseVideo(subtitlePage.harness);
  const isolated = handshake(subtitlePage.harness.page);
  try {
    await body(subtitlePage, isolated, video);
  } finally {
    isolated.close();
  }
}

/** Всё, что MAIN-мир отправил, обязано пройти пограничник изолированного мира. */
function assertAllAcceptedByBorder(subtitlePage: SubtitlePage): void {
  for (const { clonedBeforeSend } of subtitlePage.harness.page.postedToPorts) {
    const message = clonedBeforeSend as { kind?: unknown };
    if (message.kind !== "subtitle_document" && message.kind !== "subtitle_track_source") continue;
    assert.ok(parsePageEvent(message) !== undefined, `border rejects ${String(message.kind)}`);
  }
}

describe("отдача склада при начале передачи", () => {
  it("документ, скачанный ДО передачи, приходит после handoff_began и после журнала MSE, тем же содержимым", async () => {
    await withHandoffPage(async (subtitlePage, isolated) => {
      await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
      assert.equal(subtitleEventsOf(isolated).length, 0, "пока передачи нет — тишина");
      await describeAndBegin(isolated);
      await isolated.settle();
      const kinds = isolated.received.map((message) => (message as { kind: string }).kind);
      const beganAt = kinds.indexOf("handoff_began");
      const lastJournalAt = kinds.lastIndexOf("bytes");
      const documentAt = kinds.indexOf("subtitle_document");
      assert.ok(beganAt >= 0 && lastJournalAt > beganAt, "журнал MSE (байты) пришёл после handoff_began");
      assert.ok(documentAt > lastJournalAt, "документ субтитров — сразу за журналом, не раньше");
      const sent = onlyDocument(isolated);
      assert.equal(decode(sent.bytes), VTT_DOCUMENT, "ровно то же содержимое");
      assert.deepEqual(sent.naming, { language: null, label: null, auto_generated: false });
      assert.equal(sent.origin, "page_download");
      assert.equal(sent.format_hint, "web_vtt");
      assert.equal(sent.display, "showing", "единственный скачанный = showing");
      assertAllAcceptedByBorder(subtitlePage);
    });
  });

  it("байты уходят точной копией с transfer, а склад остаётся целым для следующей передачи", async () => {
    await withHandoffPage(async (subtitlePage, isolated) => {
      await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
      await describeAndBegin(isolated);
      await isolated.settle();
      const posted = subtitlePage.harness.page.postedToPorts.find((entry) => (entry.message as { kind?: string }).kind === "subtitle_document");
      assert.ok(posted !== undefined);
      assert.equal(posted.transfer.length, 1, "буфер передаётся, а не копируется структурным клоном");
      const sentBytes = (posted.clonedBeforeSend as { bytes: Uint8Array }).bytes;
      assert.equal(sentBytes.byteOffset, 0);
      assert.equal(sentBytes.byteLength, sentBytes.buffer.byteLength, "точная копия, без хвоста буфера");
      assert.equal(decode(sentBytes), VTT_DOCUMENT);
      assert.equal((posted.message as { bytes: Uint8Array }).bytes.byteLength, 0, "отправленный буфер передан (отсоединён), а не скопирован клоном");
      const stored = subtitlePage.harness.bridge.subtitles.store.snapshot()[0];
      assert.equal(decode(stored?.bytes ?? new Uint8Array()), VTT_DOCUMENT, "склад не отдал свой буфер: его байты целы");
      // Следующая передача получает тот же документ ещё раз.
      isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      await isolated.settle();
      isolated.received.length = 0;
      await describeAndBegin(isolated);
      await isolated.settle();
      assert.equal(decode(onlyDocument(isolated).bytes), VTT_DOCUMENT);
    });
  });

  it("несколько документов: самый свежий — showing, остальные — not_showing; повторная загрузка старого снова делает его showing под прежним ключом", async () => {
    await withHandoffPage(async (subtitlePage, isolated) => {
      const english = VTT_DOCUMENT;
      const german = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHallo\n";
      await pageFetches(subtitlePage, { url: "https://cdn.example/subs/en.vtt?lang=en", contentType: "text/vtt", body: encode(english) });
      await pageFetches(subtitlePage, { url: "https://cdn.example/subs/de.vtt?lang=de", contentType: "text/vtt", body: encode(german) });
      await describeAndBegin(isolated);
      await isolated.settle();
      const first = documentsOf(isolated);
      assert.deepEqual(first.map((entry) => [entry.naming.language, entry.display]), [["en", "not_showing"], ["de", "showing"]]);
      // Зритель переключился на английский: плеер сайта скачал его снова — тот же ключ, теперь showing.
      await pageFetches(subtitlePage, { url: "https://cdn.example/subs/en.vtt?lang=en", contentType: "text/vtt", body: encode(english) });
      const all = documentsOf(isolated);
      assert.equal(all.length, 3);
      assert.equal(all[2]?.key, first[0]?.key, "тот же ключ");
      assert.equal(all[2]?.display, "showing");
      assert.equal(decode(all[2]?.bytes ?? new Uint8Array()), english);
    });
  });
});

describe("видео-файл (`<video src>`)", () => {
  it("склад уходит сразу за file_source, и <track src> объявляется", async () => {
    const subtitlePage = startSubtitlePage();
    const video = subtitlePage.harness.page.addVideo();
    video.src = "https://cdn.example/media/movie.mp4";
    video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
    video.testSetState({ paused: false, currentTime: 1 });
    subtitlePage.env.addTrackElement(video, { src: "https://cdn.example/subs/en.vtt", srclang: "en", mode: "showing" });
    const isolated = handshake(subtitlePage.harness.page);
    try {
      await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
      await describeAndBegin(isolated, "file");
      await isolated.settle();
      const kinds = isolated.received.map((message) => (message as { kind: string }).kind);
      assert.ok(kinds.indexOf("subtitle_document") > kinds.indexOf("file_source"), "после источника файла");
      assert.equal(decode(onlyDocument(isolated).bytes), VTT_DOCUMENT);
      assert.equal(trackSourcesOf(isolated).length, 1);
      assertAllAcceptedByBorder(subtitlePage);
    } finally {
      isolated.close();
    }
  });
});

describe("пойманное во время и после передачи", () => {
  it("документ, скачанный во время передачи, уходит сразу и становится showing", async () => {
    await withHandoffPage(async (subtitlePage, isolated) => {
      await describeAndBegin(isolated);
      await isolated.settle();
      assert.equal(documentsOf(isolated).length, 0, "на складе было пусто");
      await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
      await isolated.settle();
      const sent = onlyDocument(isolated);
      assert.equal(sent.display, "showing");
      assert.equal(decode(sent.bytes), VTT_DOCUMENT);
    });
  });

  it("после handoff_ended новые документы остаются на складе и никуда не уходят", async () => {
    await withHandoffPage(async (subtitlePage, isolated) => {
      await describeAndBegin(isolated);
      isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      await isolated.settle();
      assert.ok(isolated.received.some((message) => (message as { kind: string }).kind === "handoff_ended"));
      await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
      await isolated.settle();
      assert.equal(subtitleEventsOf(isolated).length, 0, "после конца передачи — ничего");
      assert.equal(subtitlePage.harness.bridge.subtitles.store.documentCount, 1, "документ на складе: пригодится следующей передаче");
    });
  });

  it("слушатели списка дорожек стоят только пока идёт передача", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      assert.equal(subtitlePage.env.trackListListeners(video), 0, "до передачи мы не слушаем ничего");
      await describeAndBegin(isolated);
      assert.equal(subtitlePage.env.trackListListeners(video), 2, "addtrack и change");
      isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      await isolated.settle();
      assert.equal(subtitlePage.env.trackListListeners(video), 0, "передача кончилась — слушатели сняты");
    });
  });

  it("передача не состоялась (элемента нет): субтитры не уходят", async () => {
    const subtitlePage = startSubtitlePage();
    const isolated = handshake(subtitlePage.harness.page);
    try {
      await pageFetches(subtitlePage, { url: VTT_URL, contentType: "text/vtt", body: encode(VTT_DOCUMENT) });
      isolated.send({ kind: "begin_handoff", request_id: 2, document: "0".repeat(32), element: 1 });
      await isolated.settle();
      assert.equal(subtitleEventsOf(isolated).length, 0);
    } finally {
      isolated.close();
    }
  });
});

describe("<track> переданного видео", () => {
  it("<track src=http(s)> — subtitle_track_source с именем, режимом и фактами Referer; адрес в документ не попадает", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      subtitlePage.env.addTrackElement(video, { src: "https://cdn.example/subs/de.vtt", srclang: "de", label: "Deutsch", mode: "showing" });
      subtitlePage.env.addTrackElement(video, { src: "https://cdn.example/subs/fr.vtt?lang=fr", srclang: "", label: "", mode: "disabled" });
      await describeAndBegin(isolated);
      await isolated.settle();
      const sources = trackSourcesOf(isolated);
      assert.equal(sources.length, 2);
      assert.deepEqual(sources[0]?.naming, { language: "de", label: "Deutsch", auto_generated: false });
      assert.equal(sources[0]?.display, "showing");
      assert.equal(sources[0]?.url, "https://cdn.example/subs/de.vtt");
      assert.equal(sources[0]?.referrer_origin, SITE_ORIGIN, "происхождение документа для Referer");
      assert.deepEqual(sources[1]?.naming, { language: "fr", label: null, auto_generated: false }, "язык из адреса, когда srclang пуст");
      assert.equal(sources[1]?.display, "not_showing");
      assert.notEqual(sources[0]?.key, sources[1]?.key);
      assertAllAcceptedByBorder(subtitlePage);
    });
  });

  it("<track>, добавленный во время передачи (addtrack), объявляется сразу и один раз", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      await describeAndBegin(isolated);
      await isolated.settle();
      subtitlePage.env.addTrackElement(video, { src: "https://cdn.example/subs/es.vtt", srclang: "es" });
      await isolated.settle();
      assert.equal(trackSourcesOf(isolated).length, 1);
      subtitlePage.env.changeTracks(video);
      playerClock(isolated);
      await isolated.settle();
      assert.equal(trackSourcesOf(isolated).length, 1, "повторные осмотры его не повторяют");
    });
  });

  it("главы, метаданные и прочие виды дорожек не субтитры: ничего не уходит", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      subtitlePage.env.addTrackElement(video, { src: "https://cdn.example/chapters.vtt", kind: "chapters", mode: "hidden" });
      const metadata = subtitlePage.env.addTextTrack(video, { kind: "metadata", mode: "hidden" });
      subtitlePage.env.addCue(metadata, { startTime: 0, endTime: 1, text: "id3" });
      await describeAndBegin(isolated);
      await isolated.settle();
      assert.equal(subtitleEventsOf(isolated).length, 0);
    });
  });
});

describe("TextTrack без файла (hls.js, dash.js, blob:)", () => {
  it("реплики превращаются в WebVTT: документ text_track_cues, опознаётся как web_vtt, режим → display", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      const track = subtitlePage.env.addTextTrack(video, { kind: "captions", label: "English CC", language: "en", mode: "showing" });
      subtitlePage.env.addCue(track, { startTime: 1, endTime: 2.5, text: "Hello", id: "c1" });
      subtitlePage.env.addCue(track, { startTime: 3, endTime: 4, text: "two\n\nlines", align: "start", line: 80, snapToLines: false });
      await describeAndBegin(isolated);
      await isolated.settle();
      const sent = onlyDocument(isolated);
      assert.equal(sent.origin, "text_track_cues");
      assert.equal(sent.display, "showing");
      assert.deepEqual(sent.naming, { language: "en", label: "English CC", auto_generated: false });
      assert.equal(sent.format_hint, "web_vtt");
      assert.equal(decode(sent.bytes), "WEBVTT\n\nc1\n00:00:01.000 --> 00:00:02.500\nHello\n\n00:00:03.000 --> 00:00:04.000 align:start line:80%\ntwo\nlines\n\n");
      assert.deepEqual(sniffSubtitleDocument(sent.bytes), { kind: "document", format: "web_vtt" });
      assertAllAcceptedByBorder(subtitlePage);
    });
  });

  it("растущая дорожка: пересылка целиком под тем же ключом, только если реплик стало больше и не чаще раза в 5 с", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      const track = subtitlePage.env.addTextTrack(video, { language: "en", mode: "hidden" });
      subtitlePage.env.addCue(track, { startTime: 1, endTime: 2, text: "one" });
      await describeAndBegin(isolated);
      await isolated.settle();
      const firstKey = onlyDocument(isolated).key;

      subtitlePage.env.addCue(track, { startTime: 3, endTime: 4, text: "two" });
      subtitlePage.harness.page.clock.advance(4_000);
      playerClock(isolated);
      await isolated.settle();
      assert.equal(documentsOf(isolated).length, 1, "прошло 4 с — рано");

      subtitlePage.harness.page.clock.advance(1_001);
      playerClock(isolated);
      await isolated.settle();
      let documents = documentsOf(isolated);
      assert.equal(documents.length, 2, "прошло больше 5 с — реплика ушла");
      assert.equal(documents[1]?.key, firstKey, "замена того же ключа");
      assert.ok(decode(documents[1]?.bytes ?? new Uint8Array()).includes("two"));
      assert.ok(decode(documents[1]?.bytes ?? new Uint8Array()).includes("one"), "документ целиком, а не добавка");

      subtitlePage.harness.page.clock.advance(10_000);
      playerClock(isolated);
      await isolated.settle();
      documents = documentsOf(isolated);
      assert.equal(documents.length, 2, "реплик не прибавилось — не пересылаем");
    });
  });

  it("выключенная дорожка не читается; включили (change) — уходит сразу", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      const track = subtitlePage.env.addTextTrack(video, { language: "en", mode: "disabled" });
      subtitlePage.env.addCue(track, { startTime: 1, endTime: 2, text: "hidden until enabled" });
      await describeAndBegin(isolated);
      await isolated.settle();
      assert.equal(documentsOf(isolated).length, 0, "disabled: реплик браузер не отдаёт");
      track.mode = "showing";
      subtitlePage.env.changeTracks(video);
      await isolated.settle();
      const sent = onlyDocument(isolated);
      assert.equal(sent.display, "showing");
      assert.ok(decode(sent.bytes).includes("hidden until enabled"));
    });
  });

  it("<track src=blob:> без http(s) адреса берётся репликами", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      const element = subtitlePage.env.addTrackElement(video, { src: "blob:https://site.example/42", srclang: "en", mode: "hidden" });
      subtitlePage.env.addCue(element.textTrack, { startTime: 0, endTime: 1, text: "from blob" });
      await describeAndBegin(isolated);
      await isolated.settle();
      assert.equal(trackSourcesOf(isolated).length, 0, "адрес blob: фону не отдаём — он его не скачает");
      assert.ok(decode(onlyDocument(isolated).bytes).includes("from blob"));
    });
  });

  it("после handoff_ended рост реплик не пересылается", async () => {
    await withHandoffPage(async (subtitlePage, isolated, video) => {
      const track = subtitlePage.env.addTextTrack(video, { language: "en", mode: "hidden" });
      subtitlePage.env.addCue(track, { startTime: 1, endTime: 2, text: "one" });
      await describeAndBegin(isolated);
      isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      await isolated.settle();
      subtitlePage.env.addCue(track, { startTime: 3, endTime: 4, text: "two" });
      subtitlePage.harness.page.clock.advance(60_000);
      playerClock(isolated);
      subtitlePage.env.changeTracks(video);
      await isolated.settle();
      assert.equal(documentsOf(isolated).length, 1, "только то, что ушло до конца");
    });
  });
});
