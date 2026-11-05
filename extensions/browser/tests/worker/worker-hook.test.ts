// Хук внутри выделенного воркера (src/worker): рукопожатие личного канала (скрыто от кода сайта),
// ответы про кодеки по запечённой политике, донесения о MediaSource и handle, поток журнала и
// живых кусков по команде MAIN-мира. Глобал воркера — фейк, порты — настоящие MessagePort Node.

import assert from "node:assert/strict";
import { test } from "node:test";

import { refuse, TRANSPARENT_POLICY, type CodecPolicy } from "../../src/shared/codec-policy.ts";
import { MAX_FORWARDED_APPEND_BYTES } from "../../src/shared/page-message-validation.ts";
import { parseWorkerReport, WORKER_CHANNEL_HANDSHAKE, WORKER_CHANNEL_MARKER_FIELD, type WorkerReport } from "../../src/shared/worker-channel-contract.ts";
import { installWorkerHook } from "../../src/worker/worker-hook.ts";
import { VIDEO_MP4 } from "../page/support/media-samples.ts";
import { createFakeWorkerScope, type FakeWorkerScope } from "./support/fake-worker-scope.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AV1_MIME = 'video/mp4; codecs="av01.0.05M.08"';

/** MAIN-сторона канала в тесте: шлёт команды, копит донесения (проверенные строгим разбором). */
class MainSide {
  readonly reports: WorkerReport[] = [];
  readonly rejected: unknown[] = [];
  readonly port: MessagePort;
  constructor(port: MessagePort) {
    this.port = port;
    port.addEventListener("message", (event) => {
      const data = (event as MessageEvent).data;
      const report = parseWorkerReport(data, (value): value is Uint8Array => value instanceof Uint8Array && value.byteLength > 0);
      if (report === undefined) this.rejected.push(data);
      else this.reports.push(report);
    });
    port.start();
  }
  send(command: unknown): void {
    this.port.postMessage(command);
  }
  async waitFor(predicate: (reports: readonly WorkerReport[]) => boolean, timeoutMs = 2000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (!predicate(this.reports)) {
      if (Date.now() > deadline) throw new Error(`timeout; reports: ${JSON.stringify(this.reports.map((report) => report.kind))}`);
      await new Promise((resolve) => setTimeout(resolve, 2));
    }
  }
  async settle(): Promise<void> {
    for (let round = 0; round < 5; round += 1) await new Promise((resolve) => setTimeout(resolve, 2));
  }
  kinds(): string[] {
    return this.reports.map((report) => (report.kind === "stream_item" ? `item:${report.item.kind === "mse" ? report.item.event.kind : "bytes"}` : report.kind));
  }
  close(): void {
    this.port.close();
  }
}

function installHook(policy: CodecPolicy = TRANSPARENT_POLICY) {
  const fake = createFakeWorkerScope();
  const oddities: string[] = [];
  const installation = installWorkerHook(fake.scope, { codecPolicy: policy }, (what) => oddities.push(what));
  return { fake, oddities, installation };
}

/** Рукопожатие, как его делает обёртка MAIN-мира: первым сообщением, метка + один порт. */
function connect(fake: FakeWorkerScope): MainSide {
  const channel = new MessageChannel();
  fake.deliverToWorker(WORKER_CHANNEL_HANDSHAKE, [channel.port2]);
  return new MainSide(channel.port1);
}

/** Как плеер сайта: новый MediaSource, его handle уходит странице, страница его подключила, SourceBuffer. */
function startMediaSource(fake: FakeWorkerScope) {
  const mediaSource = new fake.FakeMediaSource();
  const handle = mediaSource.handle;
  fake.scope.postMessage({ type: "handle", handle }, [handle]);
  mediaSource.testOpen();
  const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
  return { mediaSource, handle, sourceBuffer };
}

test("a giant append reports stream_flooded, not stream_unavailable, and the stream is closed", async () => {
  const { fake } = installHook();
  const main = connect(fake);
  try {
    const { sourceBuffer } = startMediaSource(fake);
    main.send({ kind: "open_stream", stream: 1, media_source: "ms-1" });
    await main.waitFor((reports) => reports.some((report) => report.kind === "stream_replayed"));
    sourceBuffer.appendBuffer(new Uint8Array(MAX_FORWARDED_APPEND_BYTES + 1));
    await main.waitFor((reports) => reports.at(-1)?.kind === "stream_flooded");
    assert.deepEqual(main.reports.at(-1), { kind: "stream_flooded", stream: 1 });
    assert.equal(parseWorkerReport({ kind: "stream_flooded", stream: 1 }, (value): value is Uint8Array => value instanceof Uint8Array)?.kind, "stream_flooded");
    const before = main.reports.length;
    sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 32));
    await main.settle();
    assert.equal(main.reports.length, before, "no live items after the flood");
  } finally {
    main.close();
  }
});

test("handshake: the first message with the marker and one port is taken and hidden from the site", async () => {
  const { fake } = installHook();
  const siteSaw: unknown[] = [];
  // Код сайта ставит слушатели ПОСЛЕ хука — и в фазе захвата тоже.
  fake.scope.addEventListener("message", (event) => siteSaw.push((event as MessageEvent).data), true);
  fake.scope.addEventListener("message", (event) => siteSaw.push((event as MessageEvent).data));
  const main = connect(fake);
  try {
    await main.waitFor((reports) => reports.length >= 1);
    assert.deepEqual(main.reports, [{ kind: "hook_ready" }]);
    assert.deepEqual(siteSaw, [], "the site never sees our handshake");
    // Подделка метки позже первой — обычное сообщение сайта.
    fake.deliverToWorker({ [WORKER_CHANNEL_MARKER_FIELD]: 1 }, [new MessageChannel().port2]);
    fake.deliverToWorker({ type: "init" });
    assert.deepEqual(siteSaw.length, 4, "both later messages reach both site listeners");
    await main.settle();
    assert.equal(main.reports.length, 1, "a forged handshake opens no second channel");
  } finally {
    main.close();
  }
});

test("handshake: a first message that is not ours goes to the site and the hook stays without a channel", async () => {
  const { fake, oddities } = installHook();
  const siteSaw: unknown[] = [];
  fake.scope.addEventListener("message", (event) => siteSaw.push((event as MessageEvent).data));
  fake.deliverToWorker({ type: "init" });
  // Метка не первой — уже не рукопожатие (мы смотрим только первое сообщение).
  const channel = new MessageChannel();
  const main = new MainSide(channel.port1);
  fake.deliverToWorker(WORKER_CHANNEL_HANDSHAKE, [channel.port2]);
  try {
    await main.settle();
    assert.equal(siteSaw.length, 2);
    assert.deepEqual(main.reports, [], "no hook_ready: the channel was never taken");
    assert.deepEqual(oddities, ["worker channel handshake is missing"]);
  } finally {
    main.close();
  }
});

test("handshake: two ports or extra fields are not a handshake", async () => {
  for (const [data, portCount] of [
    [{ [WORKER_CHANNEL_MARKER_FIELD]: 1, extra: true }, 1],
    [{ [WORKER_CHANNEL_MARKER_FIELD]: 2 }, 1],
    [WORKER_CHANNEL_HANDSHAKE, 2],
  ] as const) {
    const { fake } = installHook();
    const siteSaw: unknown[] = [];
    fake.scope.addEventListener("message", (event) => siteSaw.push((event as MessageEvent).data));
    const channels = Array.from({ length: portCount }, () => new MessageChannel());
    fake.deliverToWorker(data, channels.map((channel) => channel.port2));
    assert.equal(siteSaw.length, 1, `${JSON.stringify(data)} with ${portCount} port(s) reaches the site`);
    for (const channel of channels) channel.port1.close();
  }
});

test("reports made before the port are kept and arrive right after hook_ready, handle first", async () => {
  const { fake } = installHook();
  // Сайт отдал handle и даже начал MediaSource до того, как воркер раздал сообщения.
  fake.scope.postMessage({ type: "hello" });
  startMediaSource(fake);
  const main = connect(fake);
  try {
    await main.waitFor((reports) => reports.length >= 3);
    assert.deepEqual(main.reports, [
      { kind: "hook_ready" },
      { kind: "handle_sent", message: 2, media_source: "ms-1" },
      { kind: "media_source_started", media_source: "ms-1" },
    ]);
  } finally {
    main.close();
  }
});

test("handle_sent: the n-th successful postMessage, honest null for foreign, several or too many transfers", async () => {
  const { fake } = installHook();
  const main = connect(fake);
  try {
    await main.waitFor((reports) => reports.length >= 1);
    const first = new fake.FakeMediaSource();
    const second = new fake.FakeMediaSource();
    fake.scope.postMessage({ ping: 1 }); // 1
    fake.scope.postMessage({ handle: first.handle }, { transfer: [first.handle] }); // 2: форма с options
    fake.scope.postMessage({ both: [first.handle, second.handle] }, [first.handle, second.handle]); // 3: два сразу
    fake.scope.postMessage({ handle: new fake.FakeMediaSourceHandle() }, [new fake.FakeMediaSourceHandle()]); // 4: не через геттер
    const many = Array.from({ length: 65 }, () => new ArrayBuffer(1));
    fake.scope.postMessage({ second: second.handle }, [...many, second.handle]); // 5: хвост за пределом
    first.testOpen();
    first.addSourceBuffer(VIDEO_MIME);
    await main.waitFor((reports) => reports.some((report) => report.kind === "media_source_started"));
    assert.deepEqual(main.reports.slice(1), [
      { kind: "handle_sent", message: 3, media_source: null },
      { kind: "handle_sent", message: 4, media_source: null },
      { kind: "handle_sent", message: 5, media_source: null },
      { kind: "handle_sent", message: 2, media_source: "ms-1" },
      { kind: "media_source_started", media_source: "ms-1" },
    ]);
  } finally {
    main.close();
  }
});

test("open_stream: journal replay, then stream_replayed, then live items — copies with transfer, worker keys", async () => {
  const { fake } = installHook();
  const main = connect(fake);
  try {
    const { sourceBuffer } = startMediaSource(fake);
    const file = VIDEO_MP4.bytes();
    // Кусок — вид посреди большой загрузки страницы: уйти должна ровно его копия.
    const download = new Uint8Array(file.length + 100);
    download.set(file, 50);
    sourceBuffer.appendBuffer(new Uint8Array(download.buffer, 50, file.length));
    await main.waitFor((reports) => reports.some((report) => report.kind === "media_source_started"));
    assert.ok(!main.reports.some((report) => report.kind === "stream_item"), "nothing streams before the command");

    main.send({ kind: "open_stream", stream: 7, media_source: "ms-1" });
    await main.waitFor((reports) => reports.some((report) => report.kind === "stream_replayed"));
    sourceBuffer.appendBuffer(file.slice(0, 64)); // живой кусок — после конца повтора
    await main.waitFor((reports) => reports.filter((report) => report.kind === "stream_item" && report.item.kind === "bytes").length >= 2);
    const streamed = main.reports.filter((report) => report.kind === "stream_item" || report.kind === "stream_replayed");
    assert.deepEqual(main.kinds().slice(main.kinds().indexOf("item:source_buffer_added")), [
      "item:source_buffer_added",
      "item:bytes",
      "stream_replayed",
      "item:bytes",
    ]);
    for (const report of streamed) assert.equal(report.stream, 7);
    const bytes = streamed.flatMap((report) => (report.kind === "stream_item" && report.item.kind === "bytes" ? [report.item] : []));
    assert.equal(bytes[0]?.media_source, "ms-1");
    assert.deepEqual([...(bytes[0]?.bytes ?? [])], [...file], "the replayed chunk is exactly the page's view");
    assert.equal(bytes[0]?.bytes.buffer.byteLength, file.length, "a dense copy, not the whole download");
    assert.deepEqual([...(bytes[1]?.bytes ?? [])], [...file.slice(0, 64)]);
  } finally {
    main.close();
  }
});

test("close_streams stops live items; open_stream of an unknown or closed media source is unavailable", async () => {
  const { fake } = installHook();
  const main = connect(fake);
  try {
    const { mediaSource, sourceBuffer } = startMediaSource(fake);
    main.send({ kind: "open_stream", stream: 1, media_source: "ms-1" });
    await main.waitFor((reports) => reports.some((report) => report.kind === "stream_replayed"));
    main.send({ kind: "close_streams" });
    main.send({ kind: "open_stream", stream: 2, media_source: "ms-9" });
    await main.waitFor((reports) => reports.some((report) => report.kind === "stream_unavailable"));
    const before = main.reports.length;
    sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 32));
    await main.settle();
    assert.equal(main.reports.length, before, "no live items after close_streams");
    assert.deepEqual(main.reports.at(-1), { kind: "stream_unavailable", stream: 2 });

    mediaSource.testClose();
    await main.waitFor((reports) => reports.some((report) => report.kind === "media_source_closed"));
    main.send({ kind: "open_stream", stream: 3, media_source: "ms-1" });
    await main.waitFor((reports) => reports.at(-1)?.kind === "stream_unavailable");
    assert.deepEqual(main.reports.at(-1), { kind: "stream_unavailable", stream: 3 });
  } finally {
    main.close();
  }
});

test("a malformed command changes nothing and leaves a trace", async () => {
  const { fake, oddities } = installHook();
  const main = connect(fake);
  try {
    startMediaSource(fake);
    await main.waitFor((reports) => reports.some((report) => report.kind === "media_source_started"));
    const before = main.reports.length;
    main.send({ kind: "open_stream", stream: 1, media_source: "w1/ms-1" }); // ключ договора, не учёта воркера
    main.send({ kind: "open_stream", stream: 0, media_source: "ms-1" });
    await main.settle();
    assert.equal(main.reports.length, before);
    assert.deepEqual(oddities, ["worker channel command is malformed", "worker channel command is malformed"]);
  } finally {
    main.close();
  }
});

test("codec answers inside the worker follow the embedded policy; HLS is refused always", () => {
  const strict = installHook(refuse(["av1"])).fake.scope.MediaSource;
  assert.equal(strict.isTypeSupported(AV1_MIME), false);
  assert.equal(strict.isTypeSupported(VIDEO_MIME), true);
  assert.equal(strict.isTypeSupported("application/vnd.apple.mpegurl"), false);
  const open = installHook(TRANSPARENT_POLICY).fake.scope.MediaSource;
  assert.equal(open.isTypeSupported(AV1_MIME), true);
  assert.equal(open.isTypeSupported("application/x-mpegURL"), false);
});

test("the worker hook never throws into the site and keeps the site's postMessage working", () => {
  const { fake } = installHook();
  const message = { value: 1 };
  fake.scope.postMessage(message, []);
  assert.deepEqual(fake.outbox.at(-1), { message, transfer: [] });
  // Мусор во втором аргументе — дело оригинала; хук не бросает своё.
  fake.scope.postMessage(message, { transfer: "not a list" });
  assert.equal(fake.outbox.length, 2);
});
