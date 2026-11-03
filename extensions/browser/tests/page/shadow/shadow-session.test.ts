// Тень на фейковом DOM: что видит страница, что делает настоящий элемент, что уходит наружу.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks, recordEvents } from "./fake-media-dom.ts";
import { createCountingGuard, createShadowHarness, emittedWithoutPulse, playingVideo } from "./shadow-harness.ts";
import { startShadow } from "../../../src/page/shadow/index.ts";
import { DETACHED_ELEMENT_GRACE_MS } from "../../../src/page/shadow/detached-element-watch.ts";

const MEDIA_EVENTS = ["play", "playing", "pause", "timeupdate", "seeking", "seeked", "waiting", "ended", "ratechange", "volumechange", "emptied", "canplay", "canplaythrough"];

describe("тень: старт и гашение событий настоящего элемента", () => {
  it("настоящий элемент на паузе и заглушен, страница видит прежние paused/muted и не видит наших действий", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const page = recordEvents(video, MEDIA_EVENTS);
    harness.start(video);
    await flushMicrotasks();

    // Настоящий элемент: пауза и заглушка.
    assert.equal(video.real.paused, true);
    assert.equal(video.real.muted, true);
    // Страница: «играет», звук есть, ни одного доверенного pause/volumechange.
    assert.equal(video.paused, false);
    assert.equal(video.muted, false);
    assert.equal(video.currentTime, 10);
    assert.deepEqual(page.seen, []);
    assert.deepEqual(harness.guardCalls, ["suspend"]);
  });

  it("элемент внутри shadow root: доверенные события гасятся слушателем на теневом корне", async () => {
    const harness = createShadowHarness();
    const { video } = harness.dom.createVideoInShadowRoot();
    video.real.paused = false;
    const page = recordEvents(video, MEDIA_EVENTS);
    const session = harness.start(video);
    await flushMicrotasks();
    assert.deepEqual(page.seen, [], "pause/volumechange от нашей паузы и заглушки не должны дойти до страницы");

    // Синтетические события тени при этом доходят.
    session.apply({ kind: "pause" });
    await flushMicrotasks();
    assert.deepEqual(page.seen, [
      { type: "timeupdate", trusted: false },
      { type: "pause", trusted: false },
    ]);
  });

  it("доверенные события, отражающие поток (durationchange, progress, loadedmetadata), проходят", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const page = recordEvents(video, ["durationchange", "progress", "loadedmetadata", "timeupdate"]);
    harness.start(video);
    await flushMicrotasks();
    for (const type of ["durationchange", "progress", "loadedmetadata", "timeupdate"]) video.dispatchEvent(harness.dom.trusted(type));
    assert.deepEqual(page.types(), ["durationchange", "progress", "loadedmetadata"]);
  });

  it("настоящий элемент, запущенный мимо тени, снова ставится на паузу", async () => {
    // Родной play() снят до наших переопределений (как оригинал из другого фрейма: его прототип
    // переопределения этого документа не трогают).
    let nativePlay: (this: unknown) => Promise<void> = () => Promise.resolve();
    const harness = createShadowHarness({
      beforeDocumentStart: (dom) => {
        nativePlay = dom.classes.FakeHTMLMediaElement.prototype.play;
      },
    });
    const video = playingVideo(harness.dom);
    harness.start(video);
    await flushMicrotasks();
    nativePlay.call(video);
    assert.equal(video.real.paused, false);
    await flushMicrotasks();
    assert.equal(video.real.paused, true);
  });
});

describe("тень: часы плеера → страница", () => {
  it("тик playing → события страницы, позиция идёт между тиками, readyState по часам", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const page = recordEvents(video, MEDIA_EVENTS);
    const session = harness.start(video);
    harness.tick(session, 10, "waiting");
    await flushMicrotasks();
    assert.equal(video.readyState, 2);
    assert.deepEqual(page.types(), ["timeupdate", "waiting"]);
    page.clear();

    harness.tick(session, 10, "playing");
    harness.clock.advance(200);
    assert.equal(video.currentTime, 10.2);
    await flushMicrotasks();
    assert.equal(video.readyState, 4);
    // timeupdate этого же мгновения уже был с waiting — второй не шлётся.
    assert.deepEqual(page.types(), ["canplay", "playing", "canplaythrough"]);
    page.clear();
    harness.clock.advance(50);
    harness.tick(session, 10.25, "playing");
    await flushMicrotasks();
    assert.deepEqual(page.types(), ["timeupdate"]);
  });

  it("ended от часов: страница видит pause и ended, paused/ended = true", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 99);
    const page = recordEvents(video, MEDIA_EVENTS);
    const session = harness.start(video);
    harness.tick(session, 100, "ended");
    await flushMicrotasks();
    assert.equal(video.ended, true);
    assert.equal(video.paused, true);
    assert.deepEqual(page.types().slice(-3), ["timeupdate", "pause", "ended"]);
  });

  it("requestVideoFrameCallback зовётся на тиках игры, счётчики кадров растут, отчёт — VideoPlaybackQuality", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    const frames: { mediaTime: number; presentedFrames: number }[] = [];
    video.requestVideoFrameCallback((_now: number, metadata: { mediaTime: number; presentedFrames: number }) => void frames.push(metadata));
    harness.tick(session, 10, "playing");
    await flushMicrotasks();
    assert.equal(frames.length, 1);
    assert.equal(frames[0]?.mediaTime, 10);
    // Колбэк одноразовый: без новой просьбы второго вызова нет.
    harness.clock.advance(1_000);
    harness.tick(session, 11, "playing");
    await flushMicrotasks();
    assert.equal(frames.length, 1);
    // Настоящий элемент не просили: рамки браузера не трогаются.
    assert.equal(video.frameCallbacks.size, 0);

    const quality = video.getVideoPlaybackQuality();
    assert.ok(quality instanceof harness.dom.classes.FakeVideoPlaybackQuality);
    // 500 настоящих до передачи + 1 с × 30 к/с.
    assert.equal(quality.totalVideoFrames, 530);
    assert.equal(quality.droppedVideoFrames, 3);
    assert.equal(video.webkitDecodedFrameCount, 530);
  });
});

describe("тень: перемотка", () => {
  it("перемотка плеером: currentTime сразу = цели, seeking, наружу player_seek_applied; seeked на тике", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const page = recordEvents(video, MEDIA_EVENTS);
    const session = harness.start(video);
    session.apply({ kind: "seek", seek_id: 7, target_us: 42_000_000 });
    assert.equal(video.currentTime, 42);
    assert.equal(video.seeking, true);
    assert.deepEqual(harness.emitted, [{ kind: "player_seek_applied", seek_id: 7, target_us: 42_000_000 }]);
    await flushMicrotasks();
    assert.equal(page.types()[0], "seeking");
    assert.ok(!page.types().includes("seeked"));
    harness.tick(session, 42, "playing");
    await flushMicrotasks();
    assert.equal(video.seeking, false);
    assert.ok(page.types().includes("seeked"));
    // Настоящий элемент перемотка плеера не двигает.
    assert.equal(video.real.currentTime, 10);
  });

  it("перемотка плеером за конец: позиция = длительность, ended", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    session.apply({ kind: "seek", seek_id: 1, target_us: 900_000_000 });
    assert.deepEqual(harness.emitted, [{ kind: "player_seek_applied", seek_id: 1, target_us: 100_000_000 }]);
    harness.tick(session, 100, "ended");
    assert.equal(video.currentTime, 100);
    assert.equal(video.ended, true);
  });

  it("страница ставит currentTime: page_seek, seeking, позиция сразу; NaN — TypeError и ничего наружу", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const page = recordEvents(video, MEDIA_EVENTS);
    harness.start(video);
    video.currentTime = 30.5;
    assert.equal(video.currentTime, 30.5);
    assert.equal(video.seeking, true);
    assert.deepEqual(harness.emitted, [{ kind: "page_seek", target_us: 30_500_000 }]);
    await flushMicrotasks();
    assert.equal(page.types()[0], "seeking");
    assert.equal(video.real.currentTime, 10, "настоящий элемент страница через тень не двигает");

    assert.throws(() => {
      video.currentTime = Number.NaN;
    }, TypeError);
    assert.equal(harness.emitted.length, 1);
  });

  it("fastSeek страницы — тоже page_seek с ограничением длительностью", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.fastSeek(1_000);
    assert.deepEqual(harness.emitted, [{ kind: "page_seek", target_us: 100_000_000 }]);
  });
});

describe("тень: play/pause/скорость страницы", () => {
  it("play() на паузе: paused=false синхронно, page_playback{play}, обещание исполняется после playing", async () => {
    const harness = createShadowHarness();
    const video = harness.dom.createVideo();
    video.real.currentTime = 5;
    const page = recordEvents(video, MEDIA_EVENTS);
    harness.start(video);
    const order: string[] = [];
    video.addEventListener("playing", () => void order.push("playing"));
    const played = video.play().then(() => void order.push("resolved"));
    assert.equal(video.paused, false);
    assert.deepEqual(harness.emitted, [{ kind: "page_playback", request: { kind: "play" } }]);
    await played;
    assert.deepEqual(order, ["playing", "resolved"]);
    assert.deepEqual(page.types(), ["play", "playing"]);
    // Настоящий элемент при этом не запускался.
    assert.equal(video.real.paused, true);
  });

  it("события тени приходят после вызова страницы, как у браузера: подписка сразу после play() их ловит", async () => {
    const harness = createShadowHarness();
    const video = harness.dom.createVideo();
    harness.start(video);
    void video.play();
    const page = recordEvents(video, ["play", "playing"]);
    await flushMicrotasks();
    assert.deepEqual(page.types(), ["play", "playing"]);
  });

  it("play() и сразу pause() пока ждём данных: обещание отвергнуто AbortError, наружу play и pause", async () => {
    const harness = createShadowHarness();
    const video = harness.dom.createVideo();
    const session = harness.start(video);
    harness.tick(session, 0, "waiting");
    const played = video.play();
    video.pause();
    await assert.rejects(played, (problem: unknown) => problem instanceof DOMException && problem.name === "AbortError");
    assert.equal(video.paused, true);
    assert.deepEqual(emittedWithoutPulse(harness), [
      { kind: "page_playback", request: { kind: "play" } },
      { kind: "page_playback", request: { kind: "pause" } },
    ]);
  });

  it("повторный pause() ничего не шлёт", () => {
    const harness = createShadowHarness();
    const video = harness.dom.createVideo();
    harness.start(video);
    video.pause();
    assert.deepEqual(harness.emitted, []);
  });

  it("playbackRate: проверка браузером, ratechange синтетический, page_playback{set_rate}", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const page = recordEvents(video, MEDIA_EVENTS);
    harness.start(video);
    await flushMicrotasks();
    video.playbackRate = 1.5;
    assert.equal(video.playbackRate, 1.5);
    assert.deepEqual(harness.emitted, [{ kind: "page_playback", request: { kind: "set_rate", rate: 1500 } }]);
    await flushMicrotasks();
    assert.deepEqual(page.seen, [{ type: "ratechange", trusted: false }]);
    assert.throws(() => {
      video.playbackRate = 99;
    }, (problem: unknown) => problem instanceof DOMException && problem.name === "NotSupportedError");
    assert.equal(video.playbackRate, 1.5);
    assert.equal(harness.emitted.length, 1);
  });

  it("muted/volume страницы хранит тень: настоящий элемент остаётся заглушённым", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const page = recordEvents(video, ["volumechange"]);
    harness.start(video);
    await flushMicrotasks();
    video.muted = true;
    video.volume = 0.3;
    assert.equal(video.muted, true);
    assert.equal(video.volume, 0.3);
    video.muted = false;
    assert.equal(video.real.muted, true);
    assert.throws(() => {
      video.volume = 2;
    }, (problem: unknown) => problem instanceof DOMException && problem.name === "IndexSizeError");
    await flushMicrotasks();
    assert.deepEqual(page.seen, [
      { type: "volumechange", trusted: false },
      { type: "volumechange", trusted: false },
      { type: "volumechange", trusted: false },
    ]);
  });
});

describe("тень: элемент не в тени", () => {
  it("другой элемент документа работает через оригиналы без изменений", async () => {
    const harness = createShadowHarness();
    const shadowed = playingVideo(harness.dom);
    const other = harness.dom.createVideo();
    harness.start(shadowed);
    const page = recordEvents(other, MEDIA_EVENTS);
    await other.play();
    other.currentTime = 7;
    await flushMicrotasks();
    assert.equal(other.real.paused, false);
    assert.equal(other.paused, false);
    assert.equal(other.real.currentTime, 7);
    assert.ok(page.seen.every((entry) => entry.trusted));
    assert.ok(page.types().includes("play"));
    assert.deepEqual(harness.emitted, []);
  });

  it("чужая подмена, поставленная ДО нашего document_start (другой экстеншен), работает для элементов вне тени", async () => {
    let foreignCalls = 0;
    const harness = createShadowHarness({
      beforeDocumentStart: (dom) => {
        const prototype = dom.classes.FakeHTMLMediaElement.prototype;
        const nativePlay = prototype.play;
        prototype.play = function (this: unknown) {
          foreignCalls += 1;
          return nativePlay.call(this as never);
        };
      },
    });
    const shadowed = playingVideo(harness.dom);
    const other = harness.dom.createVideo();
    harness.start(shadowed);
    await other.play();
    assert.equal(foreignCalls, 1, "чужой элемент: наше переопределение → чужая подмена → браузер");
    assert.equal(other.real.paused, false);
    await shadowed.play();
    assert.equal(foreignCalls, 1, "переданный элемент обслуживает тень, чужая подмена не зовётся");
    assert.equal(shadowed.real.paused, true, "настоящий элемент под тенью не запущен");
  });

  it("чужая подмена, поставленная после нашей установки, оборачивает тень", async () => {
    const harness = createShadowHarness();
    const shadowed = harness.dom.createVideo();
    const other = harness.dom.createVideo();
    // «Блокировщик» подменил pause() после нашего document_start (до или после передачи — всё равно).
    const prototype = harness.dom.classes.FakeHTMLMediaElement.prototype;
    const ours = prototype.pause;
    let foreignCalls = 0;
    prototype.pause = function (this: unknown) {
      foreignCalls += 1;
      ours.call(this as never);
    };
    harness.start(shadowed);
    void shadowed.play();
    shadowed.pause();
    assert.equal(foreignCalls, 1);
    assert.equal(shadowed.paused, true);
    assert.deepEqual(emittedWithoutPulse(harness).at(-1), { kind: "page_playback", request: { kind: "pause" } });
    // Чужой элемент: подмена → наше прозрачное переопределение → браузер.
    await other.play();
    other.pause();
    assert.equal(foreignCalls, 2);
    assert.equal(other.real.paused, true);
  });
});

describe("тень: подтягивание настоящего элемента", () => {
  const currentTimeSets = (video: { realCalls: readonly { kind: string; property?: string }[] }) =>
    video.realCalls.filter((call) => call.kind === "set" && call.property === "currentTime").length;

  it("раз в 15 с при расхождении больше 20 с — одна перестановка на позицию тени", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 0);
    const session = harness.start(video);
    // Плеер играет 40 с, тикая раз в 250 мс.
    for (let elapsedMs = 0; elapsedMs <= 40_000; elapsedMs += 250) {
      harness.tick(session, elapsedMs / 1000, "playing");
      harness.clock.advance(250);
    }
    // 15 с: разница 15 с — рано; 30 с: разница 30 с — переставили; 45 с ещё не наступило.
    assert.equal(currentTimeSets(video), 1);
    assert.ok(Math.abs(video.real.currentTime - 30) < 0.5, `настоящий элемент на ${video.real.currentTime}`);
    assert.equal(video.real.paused, true);
  });

  it("при расхождении меньше 20 с перестановок нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 0);
    const session = harness.start(video);
    for (let elapsedMs = 0; elapsedMs <= 19_000; elapsedMs += 250) {
      harness.tick(session, elapsedMs / 1000, "playing");
      harness.clock.advance(250);
    }
    assert.equal(currentTimeSets(video), 0);
  });

  it("готовность настоящего элемента после нашей перестановки странице не показывается", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 0);
    const page = recordEvents(video, ["canplay", "canplaythrough"]);
    const session = harness.start(video);
    harness.clock.advance(16_000);
    harness.tick(session, 30, "playing");
    assert.equal(currentTimeSets(video), 1);
    await flushMicrotasks();
    page.clear();
    video.dispatchEvent(harness.dom.trusted("canplay"));
    video.dispatchEvent(harness.dom.trusted("canplaythrough"));
    assert.deepEqual(page.seen, []);
    // А готовность не от нас (новые данные потока) снова проходит.
    video.dispatchEvent(harness.dom.trusted("canplay"));
    assert.deepEqual(page.types(), ["canplay"]);
  });
});

describe("тень: снятие", () => {
  it("end{at, playing}: позиция, громкость/muted страницы, игра настоящего элемента, handoff_ended{returned}", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    video.real.volume = 0.6;
    const session = harness.start(video);
    video.muted = true;
    harness.tick(session, 50, "playing");
    session.end({ kind: "at", position_us: 55_000_000 }, { kind: "exactly", resume: "playing" });
    assert.equal(video.real.currentTime, 55);
    assert.equal(video.real.muted, true, "muted страницы (true), а не наш");
    assert.equal(video.real.volume, 0.6);
    assert.equal(video.real.paused, false);
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(harness.guardCalls, ["suspend", "restore"]);
    // Элемент снова настоящий: геттеры идут в оригинал.
    video.real.currentTime = 77;
    assert.equal(video.currentTime, 77);
  });

  it("end: muted страницы false — настоящий элемент снова со звуком (находка Ashdi)", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    assert.equal(video.real.muted, true);
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(video.real.muted, false);
  });

  it("end{shadow_position, as_shadow}: позиция тени, играет как тень", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 20, "playing");
    harness.clock.advance(500);
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(video.real.currentTime, 20.5);
    assert.equal(video.real.paused, false);
  });

  it("страница видела «играет», итог — пауза: синтетический pause и настоящий элемент стоит", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    await flushMicrotasks();
    const page = recordEvents(video, MEDIA_EVENTS);
    session.end({ kind: "at", position_us: 12_000_000 }, { kind: "exactly", resume: "paused" });
    assert.equal(video.paused, true, "после снятия — настоящее значение");
    await flushMicrotasks();
    assert.ok(page.seen.some((entry) => entry.type === "pause" && !entry.trusted), JSON.stringify(page.seen));
    // Перестановка настоящего элемента видна странице как настоящая (тень уже снята).
    assert.ok(page.seen.some((entry) => entry.type === "seeked" && entry.trusted));
  });

  it("на той же позиции настоящий элемент не переставляется", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    session.end({ kind: "at", position_us: 10_000_000 }, { kind: "exactly", resume: "paused" });
    assert.ok(!video.realCalls.some((call) => call.kind === "set" && call.property === "currentTime"));
  });

  it("после end команды и повторный end — ничего", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    const emittedCount = harness.emitted.length;
    session.apply({ kind: "seek", seek_id: 3, target_us: 1 });
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(harness.emitted.length, emittedCount);
  });

  it("ожидающие rVFC уходят настоящему элементу", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.requestVideoFrameCallback(() => {});
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(video.frameCallbacks.size, 1);
  });
});

describe("тень: смена источника страницей", () => {
  it("emptied мимо наших переопределений (setAttribute) с обычным файлом → element_released сразу", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    await flushMicrotasks();
    // Атрибут ставится в обход сеттера `src`: тень узнаёт только по доверенному `emptied`.
    video.setAttribute("src", "https://cdn.example/movie.mp4");
    await flushMicrotasks();
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "element_released" });
    assert.equal(video.real.muted, false);
  });

  it("новый MediaSource на том же элементе: тень остаётся, позиция 0, пауза, play() страницы сразу после src= работает", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 30, "playing");
    harness.mediaSourceUrls.add("blob:https://site.example/ms-2"); // страница создала адрес для нового MediaSource
    video.src = "blob:https://site.example/ms-2";
    assert.equal(video.paused, true);
    assert.equal(video.currentTime, 0);
    const played = video.play();
    assert.equal(video.paused, false);
    // Запоздавший emptied браузера не должен сбросить play() страницы.
    await flushMicrotasks();
    assert.equal(video.paused, false);
    assert.ok(!harness.emitted.some((event) => event.kind === "handoff_ended"));
    // Старые тики прошлого поколения не возвращают позицию 30 с.
    harness.tick(session, 30.25, "playing");
    assert.equal(video.currentTime, 0);
    video.real.readyState = 4;
    harness.tick(session, 0, "playing");
    assert.equal(video.currentTime, 0);
    await played;
  });

  it("blob-адрес обычного файла (не MediaSource по реестру MAIN-мира) → element_released", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.src = "blob:https://site.example/plain-file";
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(harness.guardCalls, ["suspend", "restore"]);
  });

  it("srcObject = MediaSource — тоже новое поколение; srcObject = не MSE — element_released", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.srcObject = new harness.dom.classes.FakeMediaSource();
    assert.ok(!harness.emitted.some((event) => event.kind === "handoff_ended"));
    video.srcObject = { notAMediaSource: true };
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "element_released" });
  });
});

describe("тень: элемент ушёл из документа и сбой запуска", () => {
  it("элемент убран из документа: после допуска — element_released; перенос в пределах допуска — нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    // Плеер переставил себя: вынут и вставлен обратно через несколько задач — в пределах допуска.
    video.remove();
    harness.tick(session, 10.25, "playing");
    harness.clock.advance(DETACHED_ELEMENT_GRACE_MS / 2);
    harness.dom.body.appendChild(video);
    harness.tick(session, 10.5, "playing");
    harness.clock.advance(DETACHED_ELEMENT_GRACE_MS);
    harness.tick(session, 11.5, "playing");
    assert.ok(!harness.emitted.some((event) => event.kind === "handoff_ended"), "a short move does not release");
    // Страница закрыла плеер: элемента нет дольше допуска.
    video.remove();
    harness.tick(session, 11.75, "playing");
    harness.clock.advance(DETACHED_ELEMENT_GRACE_MS);
    harness.tick(session, 12.75, "playing");
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "element_released" });
    assert.equal(video.real.muted, false, "page muted state restored");
    assert.deepEqual(harness.guardCalls, ["suspend", "restore"]);
  });

  it("шлагбаум не встал: startShadow бросает, регистрация откатана — элемент снова целиком страницы", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const brokenWindow = {} as unknown as Window; // addEventListener на нём бросит
    assert.throws(() =>
      startShadow(video as unknown as HTMLMediaElement, () => {}, {
        win: brokenWindow,
        originals: harness.originals,
        mediaSession: createCountingGuard().guard,
        now: () => harness.clock.nowMs,
        isMediaSourceObjectUrl: () => true,
        scheduleTimeout: harness.timers.schedule,
      }, "media_source"),
    );
    // Переопределения прототипа больше не обслуживают элемент мёртвым контроллером.
    video.real.currentTime = 42;
    assert.equal(video.currentTime, 42);
    // И повторный запуск возможен (раньше: «element is already shadowed»).
    harness.start(video);
    assert.deepEqual(harness.guardCalls, ["suspend"]);
  });
});
