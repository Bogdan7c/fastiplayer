// Сторож следующего видео (сессия 09c2) с НАСТОЯЩЕЙ тенью на фейковом DOM: тень встаёт раньше первого
// кадра, страница получает обещание `play()` в сохранности, а отказ фона возвращает элемент без потерь.
// Сценарии повторяют живые формы: `play()` у загруженного фильма (Ashdi, Tortuga), `play()` рекламы до
// метаданных, смена `src` в цепочке рекламы тем же элементом (Tortuga).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { DomReaders } from "../../src/page/dom-readers.ts";
import type { ElementSource } from "../../src/page/element-source.ts";
import type { ElementRegistry } from "../../src/page/element-registry.ts";
import type { BinaryIntrinsics } from "../../src/page/binary-view.ts";
import type { ForwardByteMeter } from "../../src/page/forward-byte-meter.ts";
import { HandoffController } from "../../src/page/handoff-controller.ts";
import { NextVideoGuard } from "../../src/page/next-video-guard.ts";
import type { PagePort } from "../../src/page/page-channel.ts";
import type { WorkerMediaSources } from "../../src/page/worker-capture/index.ts";
import { heldShadowStarterFor, playWatchSeamFor } from "../../src/page/shadow-assembly.ts";
import type { VideoCandidates } from "../../src/page/video-candidates.ts";
import type { PageEvent, VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { flushMicrotasks, recordEvents, type FakeVideo } from "./shadow/fake-media-dom.ts";
import { createCountingGuard, createShadowHarness, type ShadowHarness } from "./shadow/shadow-harness.ts";

const FILE_URL = "https://cdn.example/ad.mp4";
const LEASE_MS = 3_000;

/** Страница и настоящая тень вокруг сторожа. */
function setupGuard() {
  const harness: ShadowHarness = createShadowHarness();
  const sent: PageEvent[] = [];
  const videos: FakeVideo[] = [];
  const protectedVideos = new Set<object>();
  const sources = new Map<object, ElementSource>();
  const mediaSession = createCountingGuard();
  const environment = {
    win: harness.dom.window as unknown as Window,
    originals: harness.originals,
    mediaSession: mediaSession.guard,
    now: () => harness.clock.nowMs,
    scheduleTimeout: harness.timers.schedule,
  };
  const keyOf = (element: object): number => videos.indexOf(element as FakeVideo) + 1;
  const dom = {
    isVideoElement: (candidate: unknown) => videos.includes(candidate as FakeVideo),
    isPaused: (element: object) => (element as FakeVideo).real.paused,
    isEnded: () => false,
    readyStateOf: (element: object) => (element as FakeVideo).real.readyState,
    isLooping: (element: object) => (element as unknown as { loop?: boolean }).loop === true,
    currentSrcOf: (element: object) => String((element as FakeVideo).real.src),
  } as unknown as DomReaders;
  const describeVideos = (): VideoCandidate[] =>
    videos.map((video) => ({
      ...DEFAULT_CANDIDATE_FACTS,
      document: "doc-a",
      element: keyOf(video),
      media: { kind: "file" },
      playing: !video.real.paused,
      visible_area_px: 400_000,
    }));
  const guard = new NextVideoGuard({
    dom,
    registry: {
      findVideoElements: () => videos,
      keyOf,
      elementByKey: (key: number) => videos[key - 1] ?? null,
      isMediaSourceObjectUrl: () => true,
    } as unknown as ElementRegistry,
    candidates: {
      sourceOf: (element: Element) => sources.get(element) ?? { kind: "file", url: FILE_URL },
      describe: describeVideos,
    } as unknown as VideoCandidates,
    startHeldShadow: heldShadowStarterFor(environment),
    seam: playWatchSeamFor(environment),
    isProtected: (element) => protectedVideos.has(element),
    send: (event) => void sent.push(event),
    now: () => harness.clock.nowMs,
    reportOddity: (line) => assert.fail(`oddity: ${line}`),
    documentEvents: harness.dom.window,
  });
  const addVideo = (configure: (video: FakeVideo) => void = () => {}): FakeVideo => {
    const video = harness.dom.createVideo() as unknown as FakeVideo;
    configure(video);
    videos.push(video);
    return video;
  };
  return { harness, guard, sent, addVideo, sources, protectedVideos, keyOf, mediaSessionCalls: mediaSession.calls };
}

/** Страница зовёт `play()` как обычно: через прототип с нашими переопределениями. */
function pagePlay(video: FakeVideo): Promise<void> {
  return (video as unknown as { play(): Promise<void> }).play();
}

const browserPlays = (video: FakeVideo): number => video.realCalls.filter((call) => call.kind === "play").length;

describe("сторож спит", () => {
  it("не взведён — `play()` прозрачен, тени нет, фону ни слова", async () => {
    const { guard, sent, addVideo } = setupGuard();
    void guard;
    const video = addVideo();
    await pagePlay(video);
    assert.equal(browserPlays(video), 1);
    assert.equal(video.real.paused, false);
    assert.deepEqual(sent, []);
  });

  it("срок взвода вышел — сторож снова прозрачен, команда «снять» не нужна", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const video = addVideo();
    guard.arm(LEASE_MS);
    harness.clock.advance(LEASE_MS);
    await pagePlay(video);
    assert.equal(browserPlays(video), 1);
    assert.deepEqual(sent, []);
  });
});

describe("тень раньше первого кадра", () => {
  it("загруженный фильм: настоящий `play()` не звался, браузеру нечего декодировать и озвучивать", async () => {
    const { guard, sent, addVideo, keyOf } = setupGuard();
    const film = addVideo();
    guard.arm(LEASE_MS);
    const promise = pagePlay(film);
    assert.equal(browserPlays(film), 0, "браузер фильм не запускал");
    assert.equal(film.real.paused, true);
    assert.equal(film.real.muted, true, "настоящий элемент заглушён");
    assert.equal((film as unknown as { paused: boolean }).paused, false, "страница видит, что видео играет");
    const held = sent.at(-1);
    assert.ok(held?.kind === "next_video_held");
    assert.equal(held.element, keyOf(film));
    assert.equal(held.candidates.find((candidate) => candidate.element === keyOf(film))?.playing, true, "фон видит его играющим");
    await promise;
  });

  it("у ролика ещё нет метаданных: `play()` проходит, тень встаёт на `loadedmetadata`, а обещание не отвергается", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const ad = addVideo((video) => {
      video.real.readyState = 0;
      video.real.duration = Number.NaN;
    });
    guard.arm(LEASE_MS);
    let outcome = "pending";
    pagePlay(ad).then(
      () => (outcome = "resolved"),
      (error: { name: string }) => (outcome = `rejected:${error.name}`),
    );
    assert.equal(browserPlays(ad), 1, "браузер должен скачать начало файла");
    assert.deepEqual(sent, [], "судить о ролике рано: длительности нет");
    await flushMicrotasks();
    ad.real.readyState = 1;
    ad.real.duration = 24;
    ad.dispatchEvent(harness.dom.trusted("loadedmetadata"));
    await flushMicrotasks();
    assert.equal(sent.length, 1);
    assert.equal(ad.real.paused, true, "тень поставила элемент на паузу до первого кадра");
    assert.equal((ad as unknown as { duration: number }).duration, 24, "снимок файла с настоящей длительностью");
    assert.equal(outcome, "pending", "обещание страницы досталось тени: AbortError плеер сайта принял бы за сбой");
  });

  it("запуск мимо `play()` (autoplay, поздний взвод): обход видео при взводе ставит тень", () => {
    const { guard, sent, addVideo } = setupGuard();
    const running = addVideo((video) => {
      video.real.paused = false;
    });
    guard.arm(LEASE_MS);
    assert.equal(running.real.paused, true);
    assert.equal(sent.length, 1);
  });

  it("баннер с `loop` и защищённое видео сторож не трогает", async () => {
    const { guard, sent, addVideo, protectedVideos } = setupGuard();
    const banner = addVideo((video) => void Object.assign(video, { loop: true }));
    const drm = addVideo();
    protectedVideos.add(drm);
    guard.arm(LEASE_MS);
    await pagePlay(banner);
    await pagePlay(drm);
    assert.equal(browserPlays(banner) + browserPlays(drm), 2);
    assert.deepEqual(sent, []);
  });

  it("источника передавать нечего: `play()` идёт браузеру", async () => {
    const { guard, sent, addVideo, sources } = setupGuard();
    const hls = addVideo();
    sources.set(hls, { kind: "unsupported", form: "native_hls" });
    guard.arm(LEASE_MS);
    await pagePlay(hls);
    assert.equal(browserPlays(hls), 1);
    assert.deepEqual(sent, []);
  });
});

describe("запись HLS под сторожем (10a2)", () => {
  it("плейлист записи держится тенью «файл», как файл: настоящий `play()` не звался, фон получил next_video_held", async () => {
    const { guard, sent, addVideo, sources, keyOf } = setupGuard();
    const playlist = addVideo();
    sources.set(playlist, { kind: "hls", url: "https://cdn.example/vod/master.m3u8" });
    guard.arm(LEASE_MS);
    const promise = pagePlay(playlist);
    assert.equal(browserPlays(playlist), 0, "браузер запись не запускал");
    const held = sent.at(-1);
    assert.ok(held?.kind === "next_video_held");
    assert.equal(held.element, keyOf(playlist));
    await promise;
  });
});

describe("решение фона", () => {
  it("отказ: файл с элемента не снимался, он заиграл сам и обещание исполнилось", async () => {
    const { guard, addVideo, keyOf } = setupGuard();
    const banner = addVideo((video) => {
      video.real.src = FILE_URL;
    });
    guard.arm(LEASE_MS);
    const promise = pagePlay(banner);
    assert.equal(banner.real.srcObject, null, "до решения фона файл на месте");
    guard.release(keyOf(banner));
    await flushMicrotasks();
    assert.equal(banner.real.paused, false);
    assert.equal(banner.real.muted, false);
    assert.equal(browserPlays(banner), 1);
    await promise;
  });

  it("отпущенное видео при следующем взводе не задерживается снова, пока не сменит источник", () => {
    const { guard, sent, addVideo, keyOf } = setupGuard();
    const mirror = addVideo((video) => {
      video.real.paused = false;
      video.real.src = FILE_URL;
    });
    guard.arm(LEASE_MS);
    guard.release(keyOf(mirror));
    guard.arm(LEASE_MS);
    assert.equal(sent.length, 1, "скрытый зеркальный плеер не ходит по кругу");
    mirror.real.src = "https://cdn.example/next.mp4";
    guard.arm(LEASE_MS);
    assert.equal(sent.length, 2);
  });

  it("взяли: файл снимается только сейчас, события тени уходят новому хозяину", async () => {
    const { guard, addVideo } = setupGuard();
    const film = addVideo((video) => {
      video.real.src = FILE_URL;
    });
    guard.arm(LEASE_MS);
    void pagePlay(film);
    const held = guard.take(film);
    assert.ok(held !== null);
    const events: PageEvent[] = [];
    held.connect((event) => void events.push(event));
    held.session.commitHold();
    assert.ok(film.real.srcObject !== null, "файл снят после решения");
    (film as unknown as { pause(): void }).pause();
    await flushMicrotasks();
    assert.ok(events.some((event) => event.kind === "page_playback"), "пауза страницы дошла до нового хозяина");
    assert.equal(guard.take(film), null, "второй раз забрать нельзя");
  });

  it("взятая тень не верит `ended` прошлого поколения: страница не видит конец нового ролика", () => {
    const { guard, harness, addVideo } = setupGuard();
    const film = addVideo();
    guard.arm(LEASE_MS);
    void pagePlay(film);
    const held = guard.take(film);
    assert.ok(held !== null);
    held.connect(() => {});
    held.session.commitHold();
    const pageEvents = recordEvents(film, ["ended"]);
    harness.tick(held.session, 24, "ended");
    assert.equal((film as unknown as { ended: boolean }).ended, false);
    assert.deepEqual(pageEvents.types(), []);
  });

  it("страница сменила источник задержанного видео: запись снята, а чужой ключ не ломает отказ", async () => {
    const { guard, addVideo, keyOf } = setupGuard();
    const video = addVideo((element) => {
      element.real.src = FILE_URL;
    });
    guard.arm(LEASE_MS);
    void pagePlay(video);
    (video as unknown as { src: string }).src = "https://cdn.example/other.mp4";
    await flushMicrotasks();
    assert.equal(guard.take(video), null);
    guard.release(keyOf(video));
    guard.release(9_999);
  });
});

describe("цепочка рекламы тем же элементом (Tortuga)", () => {
  it("`setAttribute('src')` и `play()` подряд: тень старого ролика уходит, `play()` не достаётся ему", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const ad = addVideo((video) => {
      video.setAttribute("src", FILE_URL);
      video.real.src = FILE_URL;
      video.real.readyState = 4;
    });
    guard.arm(LEASE_MS);
    void pagePlay(ad);
    const held = guard.take(ad);
    assert.ok(held !== null);
    const events: PageEvent[] = [];
    held.connect((event) => void events.push(event));
    held.session.commitHold();
    await flushMicrotasks();
    harness.clock.advance(1);
    guard.arm(LEASE_MS);
    sent.length = 0;

    ad.setAttribute("src", "https://cdn.example/ad-2.mp4");
    ad.real.src = "https://cdn.example/ad-2.mp4";
    ad.real.readyState = 0;
    void pagePlay(ad);
    assert.ok(events.some((event) => event.kind === "handoff_ended" && event.cause === "element_released"), "тень снялась сразу, а не по `emptied`");
    assert.ok(!events.some((event) => event.kind === "page_seek"), "закончившийся ролик не перематывается на начало у плеера");
    assert.equal(ad.real.srcObject, null, "источник страницы работает");
  });
});

describe("mediaSession страницы", () => {
  it("задержанное видео его не трогает: рядом могла стоять живая тень рекламы; взятое — приостанавливает", () => {
    const { guard, addVideo, keyOf, mediaSessionCalls } = setupGuard();
    const banner = addVideo();
    guard.arm(LEASE_MS);
    void pagePlay(banner);
    guard.release(keyOf(banner));
    assert.deepEqual(mediaSessionCalls, [], "отказ не вернул странице то, что приостановила реклама");
    const film = addVideo();
    void pagePlay(film);
    const held = guard.take(film);
    assert.ok(held !== null);
    held.connect(() => {});
    held.session.commitHold();
    assert.deepEqual(mediaSessionCalls, ["suspend"]);
  });
});

describe("сбой старта передачи задержанного видео (ревью 09c2)", () => {
  it("открыть поток не вышло: тень снята, элемент не остаётся молчать на паузе", () => {
    const { guard, addVideo, keyOf } = setupGuard();
    const film = addVideo((video) => {
      video.real.src = FILE_URL;
    });
    guard.arm(LEASE_MS);
    void pagePlay(film);
    const controller = new HandoffController({
      registry: { elementByKey: () => film, keyOf, isMediaSourceObjectUrl: () => true } as unknown as ElementRegistry,
      candidates: { documentTag: "doc-a", sourceOf: () => ({ kind: "file", url: FILE_URL }) } as unknown as VideoCandidates,
      dom: {
        isVideoElement: () => true,
        referrerPolicyOf: () => "",
        metaReferrerContents: () => [],
        documentOrigin: () => "https://site.example",
      } as unknown as DomReaders,
      binary: {} as BinaryIntrinsics,
      meter: {} as ForwardByteMeter,
      startShadow: () => assert.fail("задержанное видео не должно получить вторую тень"),
      workerMediaSources: { closeAllStreams: () => {} } as unknown as WorkerMediaSources,
      reportOddity: () => {},
      protection: { isProtected: () => false },
      nextVideo: guard,
    });
    const port: PagePort = {
      send: (event: PageEvent) => {
        if (event.kind === "file_source") throw new Error("port refused the message");
      },
    } as unknown as PagePort;
    controller.connect(port);
    controller.handle({ kind: "begin_handoff", request_id: 1, document: "doc-a", element: keyOf(film) });
    assert.equal(controller.isHandingOff, false);
    assert.equal(film.real.muted, false, "заглушка снята");
    assert.equal(film.real.paused, false, "страница видела игру, и элемент играет сам");
  });
});
