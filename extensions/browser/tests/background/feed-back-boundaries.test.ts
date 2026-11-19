// Границы прокрутки назад (16b3b, решение 372) по отдельности: нумерация документа, переводчик событий
// страницы и выбор, как вернуть прежнее видео при переезде. Сквозной сценарий — `feed-back.test.ts`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createDocumentTranslator, type StreamPageEvent } from "../../src/background/handoff/page-event-translator.ts";
import { leavePreviousVideoCommand, LEAVE_PREVIOUS_FEED_REEL, LEAVE_PREVIOUS_VIDEO } from "../../src/background/handoff/session-page-commands.ts";
import { createStreamNumbering } from "../../src/background/handoff/stream-numbering.ts";
import { createVideoRelocation, type RelocationVideo } from "../../src/background/handoff/video-relocation.ts";
import { createFakeClock } from "./support.ts";

const MP4 = 'video/mp4; codecs="avc1.42c00a"';

const added = (mediaSource: string): StreamPageEvent => ({ kind: "mse", event: { kind: "source_buffer_added", media_source: mediaSource, source_buffer: 0, mime_type: MP4 } });
const bytes = (mediaSource: string): StreamPageEvent => ({ kind: "bytes", media_source: mediaSource, source_buffer: 0, bytes: new Uint8Array([7]) });

/** Сообщения и заголовки рамок результата перевода — коротко, для сравнения. */
function wireOf(result: ReturnType<ReturnType<typeof createDocumentTranslator>["translate"]>): string[] {
  assert.ok(result.ok, `перевод отвергнут: ${result.ok ? "" : result.problem}`);
  return result.items.map((item) => {
    if (item.kind === "message") return `${item.message.type}${"generation" in item.message ? ` ${String(item.message.generation)}` : ""}`;
    return item.header.kind === "source_buffer_bytes" ? `frame ${item.header.generation}#${item.header.sequence}` : item.header.kind;
  });
}

describe("нумерация документа: следующее видео", () => {
  it("после beginNextVideo прежний ключ открывает новое поколение; счёт не идёт назад", () => {
    const numbering = createStreamNumbering();
    const document = numbering.openDocument();
    document.generationOf("ms-1");
    document.generationOf("ms-2");
    document.beginNextVideo();
    assert.deepEqual(document.generationOf("ms-1"), { kind: "started", generation: 3 });
    assert.deepEqual(document.generationOf("ms-1"), { kind: "existing", generation: 3 }, "внутри нового видео ключ снова привязан");
    assert.equal(numbering.latestGeneration(), 3);
  });

  it("beginNextVideo одного документа не трогает ключи другого документа сеанса", () => {
    const numbering = createStreamNumbering();
    const first = numbering.openDocument();
    const second = numbering.openDocument();
    first.generationOf("ms-1");
    second.generationOf("ms-1");
    first.beginNextVideo();
    assert.deepEqual(second.generationOf("ms-1"), { kind: "existing", generation: 2 });
  });

  it("beginNextVideo без единого ключа — пустое действие: следующий ключ получает следующий номер", () => {
    const numbering = createStreamNumbering();
    const document = numbering.openDocument();
    document.beginNextVideo();
    assert.deepEqual(document.generationOf("ms-1"), { kind: "started", generation: 1 });
  });
});

describe("переводчик документа: следующее видео", () => {
  it("вернувшийся MediaSource после beginNextVideo — generation_started, своё объявление и рамки с нуля", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    translator.translate(added("ms-1"));
    translator.translate(bytes("ms-1"));
    translator.beginNextVideo();
    assert.deepEqual(wireOf(translator.translate(added("ms-1"))), ["generation_started 2", "source_buffer_added 2"]);
    assert.deepEqual(wireOf(translator.translate(bytes("ms-1"))), ["frame 2#0"]);
  });

  it("без beginNextVideo повтор объявления — по-прежнему отказ `source_buffer_announced_twice`", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    translator.translate(added("ms-1"));
    assert.deepEqual(translator.translate(added("ms-1")), { ok: false, problem: "source_buffer_announced_twice" });
  });

  it("после beginNextVideo байты прежнего ключа без нового объявления — отказ, а не тихий приём в старое поколение", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    translator.translate(added("ms-1"));
    translator.beginNextVideo();
    assert.deepEqual(translator.translate(bytes("ms-1")), { ok: false, problem: "source_buffer_not_announced" });
  });
});

/** Переезд на видео `video`, затем шаги `steps` — и что он ответит о снятии прежнего. */
function leaveAfter(video: RelocationVideo, steps: (relocation: ReturnType<typeof createVideoRelocation>) => void): string {
  const clock = createFakeClock();
  const relocation = createVideoRelocation({ scheduler: clock.scheduler, armFrames: () => undefined, onWaitExpired: () => undefined });
  relocation.videoStarted(video);
  steps(relocation);
  const leave = relocation.howToLeavePreviousVideo();
  relocation.stop();
  return leave;
}

const REEL: RelocationVideo = { role: "film", mayBeAd: false };

describe("как вернуть прежнее видео при переезде", () => {
  it("ролик ленты ушёл прокруткой посреди просмотра — на своей секунде", () => {
    assert.equal(leaveAfter(REEL, (relocation) => relocation.ourVideoLeftScreen()), "at_its_second");
  });

  it("удалённый ролик ленты, после которого ждём следующий (F2), — тоже на своей секунде", () => {
    assert.equal(leaveAfter(REEL, (relocation) => relocation.feedVideoLost()), "at_its_second");
  });

  it("реклама, даже ушедшая с экрана, — на начало (правило 09c2)", () => {
    assert.equal(leaveAfter({ role: "ad", mayBeAd: false }, (relocation) => relocation.ourVideoLeftScreen()), "at_start");
  });

  it("ролик «может быть рекламой» — на начало", () => {
    assert.equal(leaveAfter({ role: "film", mayBeAd: true }, (relocation) => relocation.ourVideoLeftScreen()), "at_start");
  });

  it("ролик доиграл до конца, потом ушёл с экрана — на начало: второго `ended` страница не получит", () => {
    const leave = leaveAfter(REEL, (relocation) => {
      relocation.noteClockState({ state: "ended", relation: "this_video" });
      relocation.ourVideoLeftScreen();
    });
    assert.equal(leave, "at_start");
  });

  it("лента не листалась (фильм доиграл, страница поставила следующий) — на начало", () => {
    assert.equal(leaveAfter(REEL, (relocation) => relocation.noteClockState({ state: "ended", relation: "this_video" })), "at_start");
  });

  it("страница снова запустила наш ролик — признак ленты снят, на начало", () => {
    const leave = leaveAfter(REEL, (relocation) => {
      relocation.ourVideoLeftScreen();
      relocation.ourVideoResumed();
    });
    assert.equal(leave, "at_start");
  });

  it("решение переезда отображается в команду странице один к одному", () => {
    assert.equal(leavePreviousVideoCommand("at_its_second"), LEAVE_PREVIOUS_FEED_REEL);
    assert.equal(leavePreviousVideoCommand("at_start"), LEAVE_PREVIOUS_VIDEO);
    assert.deepEqual(LEAVE_PREVIOUS_FEED_REEL, { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "exactly", resume: "paused" } });
  });
});
