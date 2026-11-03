// Тень файла `<video src>` (сессия 09b, решения 186–187) на фейковом DOM: источник настоящего
// элемента снят без видимой странице смены DOM, страница видит снимок файла и «весь файл» в
// `buffered`, любая её запись источника отдаёт видео ей, возврат — на T без видимой перезагрузки.
// Плюс источник из дочерних `<source>` у тени MSE (ограничение 09a2 снято).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FILE_RETURN_TIMEOUT_MS } from "../../../src/page/shadow/file-return.ts";
import type { PageEvent } from "../../../src/shared/handoff-contract.ts";
import { flushMicrotasks, recordEvents, type FakeVideo } from "./fake-media-dom.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo, type ShadowHarness } from "./shadow-harness.ts";

const FILE_URL = "https://cdn.example/media/movie.mp4";
/** События, которые страница НЕ должна видеть от снятия и возврата источника. */
const LOAD_EVENTS = ["emptied", "loadstart", "abort", "durationchange", "loadedmetadata", "loadeddata", "seeking", "seeked", "canplay"];

/** Играющий файл на 10-й секунде длительностью 100 с. */
function playingFile(harness: ShadowHarness): FakeVideo {
  const video = playingVideo(harness.dom, 10);
  video.real.src = FILE_URL;
  return video;
}

const released: PageEvent = { kind: "handoff_ended", cause: "element_released" };
const returned: PageEvent = { kind: "handoff_ended", cause: "returned" };

/** Чтение свойства так, как его читает страница (через прототипы с переопределениями). */
function pageRead(video: FakeVideo, name: string): unknown {
  return (video as unknown as Record<string, unknown>)[name];
}

describe("тень файла: старт", () => {
  it("источник снят пустым MediaStream, атрибут src страницы на месте, события снятия страница не видит", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    const pageEvents = recordEvents(video, LOAD_EVENTS);
    harness.startFile(video);
    await flushMicrotasks();
    assert.ok(video.real.srcObject instanceof harness.dom.classes.FakeMediaStream, "браузер больше не качает файл");
    assert.equal(video.getAttribute("src"), FILE_URL, "DOM страницы не изменился");
    assert.equal(video.real.paused, true);
    assert.deepEqual(pageEvents.types(), []);
    assert.deepEqual(emittedWithoutPulse(harness), [], "своя запись источника не принята за «страница забрала видео»");
  });

  it("страница видит снимок файла: srcObject, currentSrc, duration, размер кадра, networkState, readyState", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    harness.startFile(video);
    await flushMicrotasks();
    // Настоящий элемент уже с пустым потоком: у него своё (0, NaN, …) — страница этого не видит.
    video.real.duration = Number.NaN;
    video.real.videoWidth = 0;
    assert.equal(pageRead(video, "srcObject"), null);
    assert.equal(pageRead(video, "currentSrc"), FILE_URL);
    assert.equal(pageRead(video, "duration"), 100);
    assert.equal(pageRead(video, "videoWidth"), 1280);
    assert.equal(pageRead(video, "videoHeight"), 720);
    assert.equal(pageRead(video, "networkState"), 1);
    assert.equal(video.real.readyState, 0);
    assert.equal(pageRead(video, "readyState"), 4, "готовность — модели тени, не пустого потока");
    assert.equal(pageRead(video, "paused"), false);
  });

  it("buffered и seekable — весь файл; настоящий seekable [0, длительность] отдаётся как есть (проходит проверку бренда)", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    harness.startFile(video);
    const buffered = pageRead(video, "buffered") as { length: number; start(i: number): number; end(i: number): number };
    assert.ok(buffered instanceof harness.dom.classes.FakeTimeRanges);
    assert.equal(buffered.length, 1);
    assert.equal(buffered.start(0), 0);
    assert.equal(buffered.end(0), 100);
    assert.equal(pageRead(video, "seekable"), buffered, "настоящий объект браузера");
  });

  it("seekable не весь файл — свой TimeRanges [0, длительность]: instanceof проходит, ошибки как у браузера", () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    video.real.seekableRanges = [[0, 40]];
    harness.startFile(video);
    const buffered = pageRead(video, "buffered") as { length: number; start(i: unknown): number; end(i?: unknown): number };
    assert.equal(Object.getPrototypeOf(buffered), harness.dom.classes.FakeTimeRanges.prototype, "instanceof TimeRanges проходит");
    assert.equal(buffered.length, 1);
    assert.equal(buffered.end(0), 100);
    assert.equal(buffered.start("0"), 0, "индекс — WebIDL unsigned long");
    assert.throws(() => buffered.end(1), (error: unknown) => error instanceof DOMException && error.name === "IndexSizeError");
    assert.throws(() => buffered.end(), TypeError);
  });

  it("длительность неизвестна (метаданных не было) — buffered пуст", () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    video.real.duration = Number.NaN;
    harness.startFile(video);
    const buffered = pageRead(video, "buffered") as { length: number };
    assert.equal(buffered.length, 0);
  });

  it("тень MSE эти свойства не трогает: duration и buffered — настоящие", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.real.duration = 55;
    assert.equal(pageRead(video, "duration"), 55);
    assert.equal((pageRead(video, "buffered") as { length: number }).length, 0, "настоящий пустой buffered фейка");
    assert.equal(video.real.srcObject, null, "у MSE источник не снимается");
  });

  it("нет MediaStream в браузере — запасной путь: источник остаётся, страница всё равно видит снимок", () => {
    const harness = createShadowHarness({ beforeDocumentStart: (dom) => void Reflect.deleteProperty(dom.window, "MediaStream") });
    const video = playingFile(harness);
    harness.startFile(video);
    assert.equal(video.real.srcObject, null);
    assert.equal(video.real.src, FILE_URL);
    assert.equal(pageRead(video, "networkState"), 1);
  });
});

describe("тень файла: страница пишет источник — видео её", () => {
  it("src = другой файл → element_released, наш поток снят, источник страницы работает", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    harness.startFile(video);
    await flushMicrotasks();
    (video as unknown as { src: string }).src = "https://cdn.example/next.mp4";
    assert.deepEqual(emittedWithoutPulse(harness), [released]);
    assert.equal(video.real.srcObject, null);
    assert.equal(video.real.src, "https://cdn.example/next.mp4");
  });

  it("srcObject = свой поток страницы → element_released, её поток не трогаем", () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    harness.startFile(video);
    const pageStream = new harness.dom.classes.FakeMediaStream();
    (video as unknown as { srcObject: unknown }).srcObject = pageStream;
    assert.deepEqual(emittedWithoutPulse(harness), [released]);
    assert.equal(video.real.srcObject, pageStream);
  });

  it("load() → element_released ровно одним «алгоритмом загрузки» (снятие нашего потока)", () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    harness.startFile(video);
    video.load();
    assert.deepEqual(emittedWithoutPulse(harness), [released]);
    assert.equal(video.real.srcObject, null);
    assert.ok(!video.realCalls.some((call) => call.kind === "load"), "второй загрузки нет");
  });

  it("setAttribute(src) мимо переопределений → чужое emptied → element_released, наш поток снят", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    harness.startFile(video);
    await flushMicrotasks();
    video.setAttribute("src", "https://cdn.example/other.mp4");
    await flushMicrotasks();
    assert.deepEqual(emittedWithoutPulse(harness), [released]);
    assert.equal(video.real.srcObject, null);
  });
});

describe("тень файла: возврат на T", () => {
  /** Браузер загрузил метаданные возвращённого файла. */
  function browserLoadsMetadata(video: FakeVideo): void {
    video.real.readyState = 1;
    video.fireTrusted("loadedmetadata");
  }

  it("на паузе: источник возвращён, перезагрузку страница не видит, после seeked — элемент её на T на паузе", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    const session = harness.startFile(video);
    await flushMicrotasks();
    const pageEvents = recordEvents(video, LOAD_EVENTS);
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    assert.deepEqual(emittedWithoutPulse(harness), [returned], "плеер узнаёт о конце сразу");
    assert.equal(video.real.srcObject, null, "файл возвращён элементу");
    await flushMicrotasks();
    assert.equal(pageRead(video, "duration"), 100, "пока элемент грузится, страница видит тень");
    assert.equal(pageRead(video, "readyState"), 4);
    browserLoadsMetadata(video);
    await flushMicrotasks();
    await flushMicrotasks();
    assert.deepEqual(video.realCalls.filter((call) => call.kind === "set" && call.property === "currentTime"), [{ kind: "set", property: "currentTime", value: 30 }]);
    assert.equal(pageRead(video, "currentTime"), 30, "тень снята — страница читает настоящий элемент");
    assert.equal(pageRead(video, "paused"), true);
    assert.equal(video.real.muted, false, "заглушка снята");
    assert.deepEqual(pageEvents.types().filter((type) => LOAD_EVENTS.includes(type)), [], "перезагрузки страница не видела");
    assert.equal(harness.timers.pending(), 0, "страховочный таймер снят");
  });

  it("с игрой: play() настоящего элемента — после того, как он встал на T", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    const session = harness.startFile(video);
    await flushMicrotasks();
    session.end({ kind: "at", position_us: 42_000_000 }, { kind: "exactly", resume: "playing" });
    video.realCalls.length = 0;
    browserLoadsMetadata(video);
    await flushMicrotasks();
    await flushMicrotasks();
    const order = video.realCalls.map((call) => (call.kind === "set" ? `${call.property}=${String(call.value)}` : call.kind));
    assert.ok(order.indexOf("currentTime=42") >= 0 && order.indexOf("play") > order.indexOf("currentTime=42"), order.join(","));
    assert.equal(pageRead(video, "paused"), false);
  });

  it("элемент не загрузился за срок — тень снимается всё равно (страница не остаётся под тенью без плеера)", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    const session = harness.startFile(video);
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    assert.equal(harness.timers.pending(), 1, `страховка ${FILE_RETURN_TIMEOUT_MS} мс стоит`);
    assert.equal(pageRead(video, "readyState"), 4, "до срока — тень");
    harness.timers.fireAll();
    assert.equal(pageRead(video, "readyState"), 0, "после срока — настоящий элемент");
  });

  it("страница сама сменила источник во время возврата — возврат брошен, тень снята, её источник на месте", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    const session = harness.startFile(video);
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "playing" });
    (video as unknown as { src: string }).src = "https://cdn.example/next.mp4";
    assert.equal(video.real.src, "https://cdn.example/next.mp4");
    assert.equal(harness.timers.pending(), 0);
    browserLoadsMetadata(video);
    await flushMicrotasks();
    assert.ok(!video.realCalls.some((call) => call.kind === "set" && call.property === "currentTime"), "на чужом источнике позицию не ставим");
    assert.ok(!video.realCalls.some((call) => call.kind === "play"));
  });

  it("запасной путь (источник не снимался) — возврат сразу, как у MSE", () => {
    const harness = createShadowHarness({ beforeDocumentStart: (dom) => void Reflect.deleteProperty(dom.window, "MediaStream") });
    const video = playingFile(harness);
    const session = harness.startFile(video);
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    assert.equal(video.real.currentTime, 30);
    assert.equal(harness.timers.pending(), 0);
    assert.deepEqual(emittedWithoutPulse(harness), [returned]);
  });
});

describe("тень файла: края возврата (находки 3–6 ревью 09b)", () => {
  /** Файл на 10-й секунде, страница поставила громкость 0.4 и звук включён. */
  function fileWithPageVolume(harness: ShadowHarness): FakeVideo {
    const video = playingFile(harness);
    video.real.volume = 0.4;
    video.real.muted = false;
    return video;
  }

  it("3: страница сменила источник посреди возврата — muted и громкость страницы возвращены ровно один раз", async () => {
    const harness = createShadowHarness();
    const video = fileWithPageVolume(harness);
    const session = harness.startFile(video);
    await flushMicrotasks();
    assert.equal(video.real.muted, true, "под тенью настоящий элемент заглушен");
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    assert.equal(video.real.muted, true, "пока файл перезагружается, звука нет");
    video.realCalls.length = 0;
    (video as unknown as { src: string }).src = "https://cdn.example/next.mp4";
    assert.equal(video.real.muted, false, "muted страницы вернулся");
    assert.equal(video.real.volume, 0.4, "громкость страницы не тронута");
    assert.deepEqual(
      video.realCalls.filter((call) => call.kind === "set" && call.property === "muted"),
      [{ kind: "set", property: "muted", value: false }],
      "ровно одна запись muted",
    );
  });

  it("3: страница сама заглушила видео под тенью — после ухода тени элемент остаётся заглушенным её выбором", async () => {
    const harness = createShadowHarness();
    const video = fileWithPageVolume(harness);
    const session = harness.startFile(video);
    (video as unknown as { muted: boolean }).muted = true;
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    (video as unknown as { src: string }).src = "https://cdn.example/next.mp4";
    assert.equal(video.real.muted, true);
  });

  it("4: preload=none — браузер отложил загрузку: T становится позицией начала, тень снимается без ожидания метаданных", async () => {
    const harness = createShadowHarness();
    const video = fileWithPageVolume(harness);
    const session = harness.startFile(video);
    await flushMicrotasks();
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    await flushMicrotasks();
    video.browserDefersFetch();
    await flushMicrotasks();
    await flushMicrotasks();
    assert.equal(video.real.currentTime, 30, "позиция начала воспроизведения — T");
    assert.equal(video.real.readyState, 0, "метаданных так и нет");
    assert.equal(pageRead(video, "readyState"), 0, "тень снята: страница видит настоящий элемент");
    assert.equal(pageRead(video, "currentTime"), 30);
    assert.equal(video.real.muted, false);
    assert.equal(harness.timers.pending(), 0, "страховочный таймер снят — ждать 10 с не пришлось");
  });

  it("4: suspend ДО loadstart возвращённой загрузки (запоздавший) не принимается за отложенную загрузку", async () => {
    const harness = createShadowHarness();
    const video = playingFile(harness);
    const session = harness.startFile(video);
    await flushMicrotasks();
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "paused" });
    await flushMicrotasks();
    video.real.networkState = 1;
    video.fireTrusted("suspend");
    await flushMicrotasks();
    assert.equal(pageRead(video, "readyState"), 4, "тень ещё стоит: ждём настоящую загрузку");
    assert.equal(harness.timers.pending(), 1);
  });

  it("5: смена источника мимо переопределений посреди возврата (setAttribute) — без T и без play() на новом видео", async () => {
    const harness = createShadowHarness();
    const video = fileWithPageVolume(harness);
    const session = harness.startFile(video);
    await flushMicrotasks();
    session.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "playing" });
    await flushMicrotasks(); // наше `emptied` от возврата источника пришло и посчитано своим
    video.realCalls.length = 0;
    video.setAttribute("src", "https://cdn.example/other.mp4");
    await flushMicrotasks();
    assert.equal(harness.timers.pending(), 0, "возврат брошен");
    assert.equal(video.real.muted, false, "muted страницы вернулся");
    // Браузер грузит НОВОЕ видео страницы: тень уже не вмешивается.
    video.real.readyState = 1;
    video.fireTrusted("loadedmetadata");
    await flushMicrotasks();
    assert.ok(!video.realCalls.some((call) => call.kind === "set" && call.property === "currentTime"), "T старого файла на новое видео не ставится");
    assert.ok(!video.realCalls.some((call) => call.kind === "play"), "новое видео не запускается за страницу");
  });

  it("6: новая передача того же элемента посреди возврата — возврат закончен, новая тень встала со снимком файла", async () => {
    const harness = createShadowHarness();
    const video = fileWithPageVolume(harness);
    const first = harness.startFile(video);
    await flushMicrotasks();
    first.end({ kind: "at", position_us: 30_000_000 }, { kind: "exactly", resume: "playing" });
    assert.deepEqual(emittedWithoutPulse(harness), [returned]);
    // Наше `emptied` от «вернуть источник» ещё в пути, метаданных нет.
    video.real.duration = Number.NaN;
    video.real.videoWidth = 0;
    harness.startFile(video);
    assert.equal(pageRead(video, "currentTime"), 30, "прежний возврат поставил элемент на T — новая тень начала с T");
    assert.ok(!video.realCalls.some((call) => call.kind === "play"), "прежний возврат не запускал игру");
    assert.ok(video.real.srcObject instanceof harness.dom.classes.FakeMediaStream, "новая тень снова сняла файл");
    assert.equal(video.real.muted, true, "новая тень заглушила элемент");
    assert.equal(pageRead(video, "duration"), 100, "снимок файла — от прежней тени, не от пустого элемента");
    assert.equal(pageRead(video, "videoWidth"), 1280);
    await flushMicrotasks();
    await flushMicrotasks();
    assert.deepEqual(emittedWithoutPulse(harness), [returned], "запоздавшие свои emptied не приняты за «страница забрала видео»");
    assert.equal(harness.timers.pending(), 0, "страховка прежнего возврата снята");
  });
});

describe("тень MSE: источник из дочерних <source> (ограничение 09a2 снято)", () => {
  it("removeAttribute(src) + <source> с файлом + load() — страница забрала видео сразу, без допуска", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.removeAttribute("src");
    video.appendSource("https://cdn.example/fallback.mp4", "video/mp4");
    video.load();
    assert.deepEqual(emittedWithoutPulse(harness), [released]);
  });

  it("<source> с blob-адресом MediaSource — это новое поколение, тень остаётся", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    const nextMediaSource = "blob:https://site.example/ms-2";
    harness.mediaSourceUrls.add(nextMediaSource);
    video.removeAttribute("src");
    video.appendSource(nextMediaSource);
    video.load();
    assert.deepEqual(emittedWithoutPulse(harness), []);
  });
});
