// Пределы MAIN-мира (решение 200): страница не заставит нас копировать память без предела и копить
// записи журнала вечно. Сайт при этом работает как без нас: мы только наблюдаем.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createContentRelay } from "../../src/content/relay.ts";
import { captureBinaryIntrinsics } from "../../src/page/binary-view.ts";
import { createForwardByteMeter, MAX_FORWARDED_BYTES_PER_SECOND } from "../../src/page/forward-byte-meter.ts";
import { DocumentMemoryBudget, DOCUMENT_MEMORY_BUDGET_BYTES } from "../../src/page/journal/memory-budget.ts";
import { sendStreamItem, type StreamMessage } from "../../src/page/stream-sender.ts";
import { MAX_FORWARDED_APPEND_BYTES, MAX_JOURNAL_ENTRIES, MAX_PAGE_BYTES_EVENT_BYTES, parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { JOURNAL_REPLAY_EVENT_CREDIT } from "../../src/content/page-event-budget.ts";
import { MAX_SESSION_QUEUE_BYTES } from "../../src/background/handoff/constants.ts";
import { handshake, startHarness } from "./support/bridge-harness.ts";
import { createFakePage } from "./support/fake-page.ts";
import { VIDEO_MP4 } from "./support/media-samples.ts";
import { describeVia } from "./support/survey-helpers.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const MIB = 1024 * 1024;

describe("счётчик копий", () => {
  it("кусок больше потолка отвергается сразу, счёт не тратит", () => {
    const meter = createForwardByteMeter(() => 0, MAX_FORWARDED_APPEND_BYTES * 2);
    assert.equal(meter.admit(MAX_FORWARDED_APPEND_BYTES + 1), "append_too_large");
    assert.equal(meter.admit(MAX_FORWARDED_APPEND_BYTES), "admitted");
    assert.equal(meter.admit(MAX_FORWARDED_APPEND_BYTES), "admitted");
  });

  it("сверх лимита в секунду — отказ, через секунду снова можно", () => {
    const clock = { nowMs: 0 };
    const meter = createForwardByteMeter(() => clock.nowMs, 10 * MIB);
    assert.equal(meter.admit(10 * MIB), "admitted");
    assert.equal(meter.admit(1), "rate_exceeded");
    clock.nowMs = 1_000;
    assert.equal(meter.admit(10 * MIB), "admitted");
  });

  it("боевые числа: пик живых сайтов, повтор журнала и самый большой кусок в секунду помещаются", () => {
    assert.ok(MAX_FORWARDED_BYTES_PER_SECOND >= DOCUMENT_MEMORY_BUDGET_BYTES + MAX_FORWARDED_APPEND_BYTES + 2 * MIB);
    assert.ok(MAX_FORWARDED_APPEND_BYTES <= MAX_SESSION_QUEUE_BYTES, "кусок обязан помещаться в очередь сеанса");
  });
});

describe("отправка куска", () => {
  const binary = captureBinaryIntrinsics(globalThis as never);

  function sendTo(meter: ReturnType<typeof createForwardByteMeter>, view: Uint8Array): StreamMessage[] {
    const sent: StreamMessage[] = [];
    sendStreamItem({ binary, meter }, "ms-1", { kind: "bytes", sourceBuffer: 0, view }, (message) => sent.push(message));
    return sent;
  }

  it("цикл страницы без отдыха потока упирается в счётчик раньше, чем копии съедят память", () => {
    const meter = createForwardByteMeter(() => 0, 3 * MIB);
    const view = new Uint8Array(MIB);
    let copiedBytes = 0;
    assert.throws(() => {
      for (let round = 0; round < 1_000; round += 1) copiedBytes += sendTo(meter, view).length * MIB;
    }, /rate_exceeded/);
    assert.equal(copiedBytes, 3 * MIB, "копий ровно на лимит, ни одной сверх");
  });

  it("отказ случается ДО копии: ни одного сообщения", () => {
    const sent: StreamMessage[] = [];
    const meter = createForwardByteMeter(() => 0);
    const huge = new Uint8Array(MAX_FORWARDED_APPEND_BYTES + 1);
    assert.throws(() => sendStreamItem({ binary, meter }, "ms-1", { kind: "bytes", sourceBuffer: 0, view: huge }, (message) => sent.push(message)), /append_too_large/);
    assert.deepEqual(sent, []);
  });

  it("большой законный кусок режется на части в пределах договора", () => {
    const meter = createForwardByteMeter(() => 0);
    const sent = sendTo(meter, new Uint8Array(MAX_PAGE_BYTES_EVENT_BYTES + 1));
    assert.deepEqual(sent.map((message) => (message.kind === "bytes" ? message.bytes.length : -1)), [MAX_PAGE_BYTES_EVENT_BYTES, 1]);
  });

  it("событие MSE счётчик не трогает", () => {
    const sent: StreamMessage[] = [];
    const meter = createForwardByteMeter(() => 0, 0);
    sendStreamItem({ binary, meter }, "ms-1", { kind: "mse", event: { kind: "end_of_stream", media_source: "ms-1" } }, (message) => sent.push(message));
    assert.equal(sent.length, 1);
  });
});

describe("гигантский сегмент в живой передаче", () => {
  it("передача честно кончается, кусок не копируется, а `appendBuffer` страницы работает как без нас", async () => {
    const harness = startHarness();
    const { page } = harness;
    const video = page.addVideo();
    video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
    const mediaSource = page.attachMediaSource(video);
    const buffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    buffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
    video.testSetState({ paused: false, currentTime: 0 });
    const isolated = handshake(page);
    try {
      const [candidate] = await describeVia(isolated);
      assert.ok(candidate !== undefined);
      isolated.send({ kind: "begin_handoff", request_id: 2, document: candidate.document, element: candidate.element });
      await isolated.waitFor(3);
      await isolated.settle();
      isolated.received.length = 0;

      const appendedBefore = buffer.appended.length;
      const giantSegment = new Uint8Array(MAX_FORWARDED_APPEND_BYTES + 1);
      assert.doesNotThrow(() => buffer.appendBuffer(giantSegment), "исключения из хука на страницу не выходят");
      assert.equal(buffer.appended.length, appendedBefore + 1, "браузер получил кусок как обычно");
      await isolated.waitFor(1);
      await isolated.settle();
      assert.equal(isolated.received.some((message) => message.kind === "bytes"), false, "копии нет");
      assert.deepEqual(isolated.received.at(-1), { kind: "handoff_ended", cause: "page_flood" }, "причина конца — флуд, а не сбой на стороне страницы");
      assert.equal(parsePageEvent(isolated.received.at(-1)) !== undefined, true, "причина проходит проверку формы");
    } finally {
      isolated.close();
    }
  });
});

describe("подмена прототипа типизированных массивов", () => {
  const binary = captureBinaryIntrinsics(globalThis as never);
  const prototype = Object.getPrototypeOf(Uint8Array.prototype) as object;
  const lengthDescriptor = Object.getOwnPropertyDescriptor(prototype, "length");
  const subarrayDescriptor = Object.getOwnPropertyDescriptor(prototype, "subarray");

  function underForgedPrototype(forged: { length: number; subarray?: () => Uint8Array }, action: () => void): void {
    Object.defineProperty(prototype, "length", { configurable: true, get: () => forged.length });
    if (forged.subarray !== undefined) Object.defineProperty(prototype, "subarray", { configurable: true, value: forged.subarray });
    try {
      action();
    } finally {
      Object.defineProperty(prototype, "length", lengthDescriptor as PropertyDescriptor);
      Object.defineProperty(prototype, "subarray", subarrayDescriptor as PropertyDescriptor);
    }
  }

  for (const [name, forgedLength] of [["NaN", Number.NaN], ["0", 0], ["огромная", 2 ** 40]] as const) {
    it(`подменённая длина (${name}) не меняет ни счёт, ни байты: уходит настоящий кусок`, () => {
      const meter = createForwardByteMeter(() => 0, 1_024);
      const view = new Uint8Array(1_024).fill(7);
      const sent: Uint8Array[] = [];
      underForgedPrototype({ length: forgedLength }, () => {
        sendStreamItem({ binary, meter }, "ms-1", { kind: "bytes", sourceBuffer: 0, view }, (message) => {
          if (message.kind === "bytes") sent.push(message.bytes);
        });
      });
      assert.deepEqual(sent.map((bytes) => bytes.byteLength), [1_024], "кусок дошёл целиком");
      assert.equal(meter.admit(1), "rate_exceeded", "счёт потратил настоящую длину: ведро пусто");
    });
  }

  it("подменённый `subarray` не подсовывает счётчику меньшую длину, чем уходит байтов", () => {
    const meter = createForwardByteMeter(() => 0, 1_024);
    const sent: Uint8Array[] = [];
    const smuggled = new Uint8Array(64 * MIB);
    underForgedPrototype({ length: 1, subarray: () => smuggled }, () => {
      sendStreamItem({ binary, meter }, "ms-1", { kind: "bytes", sourceBuffer: 0, view: new Uint8Array(16) }, (message) => {
        if (message.kind === "bytes") sent.push(message.bytes);
      });
    });
    assert.deepEqual(sent.map((bytes) => bytes.byteLength), [16]);
  });
});

describe("часы счётчика байтов", () => {
  it("по умолчанию счётчик идёт по монотонным часам: подмена `Date.now` его не кормит", () => {
    const realDateNow = Date.now;
    const meter = createForwardByteMeter(undefined, MIB);
    assert.equal(meter.admit(MIB), "admitted");
    // Страница двигает часы на час вперёд: на `Date.now` ведро наполнилось бы сразу.
    Date.now = () => realDateNow() + 3_600_000;
    try {
      assert.equal(meter.admit(MIB), "rate_exceeded");
    } finally {
      Date.now = realDateNow;
    }
  });

  it("подмена `performance.now` после старта счётчик не трогает", () => {
    const realNow = Object.getOwnPropertyDescriptor(Performance.prototype, "now") as PropertyDescriptor;
    const meter = createForwardByteMeter(undefined, MIB);
    assert.equal(meter.admit(MIB), "admitted");
    Object.defineProperty(Performance.prototype, "now", { configurable: true, value: () => 1e15 });
    try {
      assert.equal(meter.admit(MIB), "rate_exceeded");
    } finally {
      Object.defineProperty(Performance.prototype, "now", realNow);
    }
  });
});

describe("потолок записей журнала", () => {
  function setupJournal() {
    const budget = new DocumentMemoryBudget(1024);
    const journal = budget.createJournal("ms-1", { isGone: () => false });
    journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: VIDEO_MIME });
    return journal;
  }

  it("события без байтов не копятся вечно: повтор не длиннее потолка, а свёрнутое живёт в снимке", () => {
    const journal = setupJournal();
    for (let index = 1; index <= MAX_JOURNAL_ENTRIES * 2; index += 1) {
      journal.recordEvent({ kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: index });
    }
    const items = journal.replay();
    assert.ok(items.length <= JOURNAL_REPLAY_EVENT_CREDIT, `повтор ${items.length}`);
    assert.ok(items.length >= MAX_JOURNAL_ENTRIES);
    assert.deepEqual(items[0], { kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: VIDEO_MIME } });
    const last = items.at(-1);
    assert.ok(last?.kind === "mse" && last.event.kind === "timestamp_offset_changed");
    assert.equal(last.event.offset_us, MAX_JOURNAL_ENTRIES * 2, "свежие записи целы");
  });

  it("худший повтор журнала целиком проходит настоящего пограничника, бюджет не срабатывает", () => {
    const journal = setupJournal();
    for (let index = 1; index <= MAX_JOURNAL_ENTRIES * 2; index += 1) {
      journal.recordEvent({ kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: index });
    }
    const crossed: { kind: string }[] = [];
    let listener: (message: unknown) => void = () => {};
    const relay = createContentRelay({
      sendToPage: () => {},
      reportHeldVideo: () => Promise.resolve("release"),
      sendViewerOfferChoice: () => Promise.reject(new Error("the viewer offer is not used here")),
      timers: { setTimeout: () => undefined, clearTimeout: () => {} },
      debugOnce: () => {},
      now: () => 0,
      ownDocument: () => ({ hostname: "site.example", origin: null }),
    });
    relay.acceptBackgroundPort({
      postMessage: (message) => void crossed.push(message as { kind: string }),
      disconnect: () => {},
      onMessage: { addListener: (added) => void (listener = added) },
      onDisconnect: { addListener: () => {} },
    });
    listener({ kind: "begin_handoff", request_id: 1, document: "doc", element: 1 });
    relay.handlePageMessage({ kind: "handoff_began", request_id: 1, outcome: "started" });
    const replayed = journal.replay();
    for (const item of replayed) {
      assert.ok(item.kind === "mse");
      relay.handlePageMessage({ kind: "mse", event: item.event });
    }
    assert.equal(crossed.filter((message) => message.kind === "mse").length, replayed.length);
    assert.equal(crossed.some((message) => message.kind === "page_flood"), false);
  });

  it("записи байтов считаются тоже: виды на один буфер не обходят потолок памяти", () => {
    const budget = new DocumentMemoryBudget(1024 * MIB);
    const journal = budget.createJournal("ms-1", { isGone: () => false });
    journal.recordEvent({ kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: VIDEO_MIME });
    const init = VIDEO_MP4.bytes().slice(0, 733);
    journal.recordBytes(0, init);
    const fragment = VIDEO_MP4.bytes().slice(733, 5714);
    for (let index = 0; index < MAX_JOURNAL_ENTRIES + 500; index += 1) journal.recordBytes(0, fragment);
    assert.ok(journal.replay().length <= JOURNAL_REPLAY_EVENT_CREDIT);
  });
});

describe("Referer файла в MAIN-мире", () => {
  const FILE_URL = "https://cdn.example/movie.mp4";

  async function fileSourceOf(
    prepare: (video: ReturnType<ReturnType<typeof createFakePage>["addVideo"]>) => void,
    origin: string | undefined,
    prepareDocument: (page: ReturnType<typeof createFakePage>) => void = () => {},
  ) {
    const page = createFakePage(origin === undefined ? {} : { origin });
    prepareDocument(page);
    const harness = startHarness({ page });
    const video = harness.page.addVideo();
    video.src = FILE_URL;
    prepare(video);
    const isolated = handshake(harness.page);
    try {
      const [candidate] = await describeVia(isolated);
      assert.ok(candidate !== undefined);
      isolated.send({ kind: "begin_handoff", request_id: 2, document: candidate.document, element: candidate.element });
      await isolated.waitFor(3);
      return isolated.received.find((message) => message.kind === "file_source");
    } finally {
      isolated.close();
    }
  }

  it("сайт-источник документа уходит с file_source и проходит проверку формы", async () => {
    const event = await fileSourceOf(() => {}, "https://site.example");
    assert.deepEqual(event, { kind: "file_source", url: FILE_URL, referrer_origin: "https://site.example", referrer_policy: "strict-origin-when-cross-origin" });
    assert.deepEqual(parsePageEvent(event), event);
  });

  it("referrerpolicy=no-referrer (в любом регистре) и непрозрачное происхождение — без Referer", async () => {
    const noReferrer = await fileSourceOf((video) => video.setAttribute("referrerpolicy", "No-Referrer"), "https://site.example");
    assert.deepEqual(noReferrer, { kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    assert.deepEqual(await fileSourceOf(() => {}, "null"), { kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
  });

  it("другая политика элемента уходит фону как есть, сайт-источник остаётся", async () => {
    const event = await fileSourceOf((video) => video.setAttribute("referrerpolicy", "origin"), "https://site.example");
    assert.deepEqual(event, { kind: "file_source", url: FILE_URL, referrer_origin: "https://site.example", referrer_policy: "origin" });
    assert.deepEqual(parsePageEvent(event), event);
  });

  it("<meta name=referrer> документа читается, последний годный побеждает", async () => {
    const event = await fileSourceOf(() => {}, "https://site.example", (page) => {
      page.addMeta("referrer", "origin");
      page.addMeta("Referrer", "same-origin");
      page.addMeta("referrer", "непонятно");
      page.addMeta("description", "no-referrer");
    });
    assert.deepEqual(event, { kind: "file_source", url: FILE_URL, referrer_origin: "https://site.example", referrer_policy: "same-origin" });
  });

  it("<meta name=referrer content=no-referrer> снимает Referer целиком", async () => {
    const event = await fileSourceOf(() => {}, "https://site.example", (page) => page.addMeta("referrer", "no-referrer"));
    assert.deepEqual(event, { kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
  });

  it("атрибут элемента важнее <meta>", async () => {
    const event = await fileSourceOf((video) => video.setAttribute("referrerpolicy", "origin"), "https://site.example", (page) => page.addMeta("referrer", "no-referrer"));
    assert.deepEqual(event, { kind: "file_source", url: FILE_URL, referrer_origin: "https://site.example", referrer_policy: "origin" });
  });
});
