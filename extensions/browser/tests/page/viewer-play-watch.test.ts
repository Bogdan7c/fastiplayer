// Режим 2 «автозапуск» (сессия 12a): сторож задерживает видео, которое запустил САМ человек со звуком.
// Настоящая тень на фейковом DOM; ввод человека и часы — под управлением теста.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { judgeViewerPlay, PLAY_CALL_MARK_WINDOW_MS, type ViewerPlayFacts } from "../../src/page/auto-handoff/viewer-play-watch.ts";
import { MAX_PLAY_CREDIT_LEASE_MS } from "../../src/shared/auto-handoff-contract.ts";
import type { BinaryIntrinsics } from "../../src/page/binary-view.ts";
import type { DomReaders } from "../../src/page/dom-readers.ts";
import type { ElementRegistry } from "../../src/page/element-registry.ts";
import type { ForwardByteMeter } from "../../src/page/forward-byte-meter.ts";
import { HandoffController } from "../../src/page/handoff-controller.ts";
import type { PagePort } from "../../src/page/page-channel.ts";
import type { VideoCandidates } from "../../src/page/video-candidates.ts";
import type { WorkerMediaSources } from "../../src/page/worker-capture/index.ts";
import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { flushMicrotasks, type FakeVideo } from "./shadow/fake-media-dom.ts";
import { browserPlays, FILE_URL, LEASE_MS, pagePlay, setupGuard } from "./support/next-video-guard-setup.ts";

/** Охрана включена, у человека свежий клик. */
function watchedPage() {
  const page = setupGuard();
  page.guard.setViewerPlayWatch("on");
  page.activation.transient = true;
  return page;
}

function heldEvent(sent: ReturnType<typeof setupGuard>["sent"]) {
  const event = sent.at(-1);
  assert.ok(event?.kind === "next_video_held", "фону ушло донесение о задержанном видео");
  return event;
}

describe("человек нажал play на видео со звуком", () => {
  it("метаданные есть: тень встала в самом вызове, настоящий `play()` не звался, донесение с `viewer_play`", async () => {
    const { sent, addVideo, keyOf } = watchedPage();
    const film = addVideo();
    const promise = pagePlay(film);
    assert.equal(browserPlays(film), 0, "браузер фильм не запускал: нечего декодировать и озвучивать");
    assert.equal(film.real.paused, true);
    assert.equal((film as unknown as { paused: boolean }).paused, false, "страница видит, что видео играет");
    assert.equal(sent.length, 1);
    const held = heldEvent(sent);
    assert.equal(held.trigger, "viewer_play");
    assert.equal(held.element, keyOf(film));
    await promise;
  });

  it("нет жеста: `play()` идёт браузеру, фону ни слова", async () => {
    const { guard, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("on");
    activation.transient = false;
    const film = addVideo();
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.equal(film.real.paused, false);
    assert.deepEqual(sent, []);
  });

  it("видео без звука (muted): не наше, `play()` идёт браузеру", async () => {
    const { sent, addVideo } = watchedPage();
    const silent = addVideo((video) => {
      video.real.muted = true;
    });
    await pagePlay(silent);
    assert.equal(browserPlays(silent), 1);
    assert.deepEqual(sent, []);
  });

  it("фильтр звука снят (`on_including_muted`, решение 260): беззвучное видео с жестом задерживается", async () => {
    const { guard, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("on_including_muted");
    activation.transient = true;
    const silent = addVideo((video) => {
      video.real.muted = true;
    });
    const promise = pagePlay(silent);
    assert.equal(browserPlays(silent), 0, "браузеру play() не ушёл: видео под тенью");
    assert.equal(heldEvent(sent).trigger, "viewer_play");
    await promise;
  });

  it("фильтр звука снят, но жеста нет: беззвучное автовоспроизведение ленты не задерживается", async () => {
    const { guard, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("on_including_muted");
    activation.transient = false;
    const silent = addVideo((video) => {
      video.real.muted = true;
    });
    await pagePlay(silent);
    assert.equal(browserPlays(silent), 1);
    assert.deepEqual(sent, []);
  });

  it("охрана выключена: `play()` строго прозрачен даже при свежем жесте", async () => {
    const { guard, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("off");
    activation.transient = true;
    const film = addVideo();
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
  });

  it("охрана выключена с рождения: доверенный `play` тоже ничего не задерживает", async () => {
    const { activation, harness, sent, addVideo } = setupGuard();
    activation.transient = true;
    const film = addVideo((video) => {
      video.real.paused = false;
    });
    film.dispatchEvent(harness.dom.trusted("play"));
    await flushMicrotasks();
    assert.equal(film.real.paused, false, "тени нет");
    assert.deepEqual(sent, []);
  });

  it("защищённое (DRM) видео и видео с `loop` сторож не трогает", async () => {
    const { sent, addVideo, protectedVideos } = watchedPage();
    const drm = addVideo();
    protectedVideos.add(drm);
    const banner = addVideo((video) => void Object.assign(video, { loop: true }));
    await pagePlay(drm);
    await pagePlay(banner);
    assert.equal(browserPlays(drm) + browserPlays(banner), 2);
    assert.deepEqual(sent, []);
  });

  it("отказ фона запоминается: отпущенное видео не задерживается снова, пока не сменит источник", async () => {
    const { guard, sent, addVideo, keyOf } = watchedPage();
    const film = addVideo();
    void pagePlay(film);
    guard.release(keyOf(film));
    await flushMicrotasks();
    assert.equal(film.real.paused, false, "заиграло само");
    void pagePlay(film);
    await flushMicrotasks();
    assert.equal(sent.length, 1, "второй раз фону не доносим");
  });
});

describe("кредит клика", () => {
  it("без активации, но с кредитом от фона: видео задержано", () => {
    const { guard, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("on");
    activation.transient = false;
    guard.grantViewerPlayCredit(30_000);
    const film = addVideo();
    void pagePlay(film);
    assert.equal(heldEvent(sent).trigger, "viewer_play");
    assert.equal(browserPlays(film), 0);
  });

  it("кредит кончился по сроку: `play()` снова идёт браузеру", async () => {
    const { guard, harness, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("on");
    activation.transient = false;
    guard.grantViewerPlayCredit(1_000);
    harness.clock.advance(1_000);
    const film = addVideo();
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
  });

  it("кредит без охраны ничего не значит", async () => {
    const { guard, activation, sent, addVideo } = setupGuard();
    activation.transient = false;
    guard.grantViewerPlayCredit(30_000);
    const film = addVideo();
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
  });
});

describe("защита от зацикливания после конца передачи", () => {
  /** Видео уже прошло передачу, которая кончилась «сейчас». */
  function handedBackPage() {
    const page = watchedPage();
    const film = page.addVideo();
    page.harness.clock.advance(100);
    page.guard.noteHandoffEnded(film);
    return { ...page, film };
  }

  it("старая активация (клик «Return») не засчитывается: `play()` идёт браузеру", async () => {
    const { activation, harness, sent, film } = handedBackPage();
    activation.lastInputAtMs = harness.clock.nowMs - 10; // клик Return стоял раньше конца передачи
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
  });

  it("кредит клика не отменяет метку возврата", async () => {
    const { guard, activation, harness, sent, film } = handedBackPage();
    activation.transient = false;
    activation.lastInputAtMs = harness.clock.nowMs - 10;
    guard.grantViewerPlayCredit(30_000);
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
  });

  it("новый доверенный ввод после конца передачи: видео задержано снова", () => {
    const { activation, harness, sent, film } = handedBackPage();
    harness.clock.advance(50);
    activation.lastInputAtMs = harness.clock.nowMs;
    void pagePlay(film);
    assert.equal(heldEvent(sent).trigger, "viewer_play");
    assert.equal(browserPlays(film), 0);
  });

  it("другое видео той же страницы возвратом не затронуто", () => {
    const { addVideo, sent } = handedBackPage();
    const other = addVideo();
    void pagePlay(other);
    assert.equal(sent.length, 1);
  });
});

describe("`play()` до метаданных", () => {
  /** Ролик без метаданных, `play()` от человека с жестом. */
  function pendingFilm(page: ReturnType<typeof watchedPage>): FakeVideo {
    const film = page.addVideo((video) => {
      video.real.readyState = 0;
      video.real.duration = Number.NaN;
    });
    void pagePlay(film);
    return film;
  }
  function metadataArrives(page: ReturnType<typeof watchedPage>, film: FakeVideo): void {
    film.real.readyState = 1;
    film.real.duration = 5_400;
    film.dispatchEvent(page.harness.dom.trusted("loadedmetadata"));
  }

  it("настоящий `play()` проходит, тень встаёт на `loadedmetadata` даже после истечения активации", async () => {
    const page = watchedPage();
    const film = pendingFilm(page);
    assert.equal(browserPlays(film), 1, "браузер качает начало файла");
    assert.deepEqual(page.sent, [], "судить рано: длительности нет");
    page.activation.transient = false; // метаданные шли дольше 5 с
    page.harness.clock.advance(8_000);
    metadataArrives(page, film);
    await flushMicrotasks();
    assert.equal(film.real.paused, true, "тень поставила элемент на паузу");
    assert.equal(heldEvent(page.sent).trigger, "viewer_play");
  });

  it("метаданные позже окна метки: `play()` давно забыт, тень не встаёт", async () => {
    const page = watchedPage();
    const film = pendingFilm(page);
    page.activation.transient = false;
    page.harness.clock.advance(PLAY_CALL_MARK_WINDOW_MS + 1);
    metadataArrives(page, film);
    await flushMicrotasks();
    assert.deepEqual(page.sent, []);
    assert.equal(film.real.paused, false);
  });

  it("`loadedmetadata` без засчитанного `play()` (сайт грузит ролик сам): ничего", async () => {
    const page = watchedPage();
    const film = page.addVideo((video) => {
      video.real.readyState = 0;
      video.real.paused = false;
    });
    metadataArrives(page, film);
    await flushMicrotasks();
    assert.deepEqual(page.sent, []);
  });
});

describe("другие запуски мимо `play()`", () => {
  it("`seeked` сам по себе передачу не начинает", async () => {
    const page = watchedPage();
    const film = page.addVideo((video) => {
      video.real.paused = false;
    });
    film.dispatchEvent(page.harness.dom.trusted("seeked"));
    await flushMicrotasks();
    assert.deepEqual(page.sent, []);
    assert.equal(film.real.paused, false);
  });

  it("доверенное событие `play` (родные кнопки, `autoplay`) со свежим жестом: тень встаёт", async () => {
    const page = watchedPage();
    const film = page.addVideo((video) => {
      video.real.paused = false;
    });
    film.dispatchEvent(page.harness.dom.trusted("play"));
    await flushMicrotasks();
    assert.equal(film.real.paused, true);
    assert.equal(heldEvent(page.sent).trigger, "viewer_play");
  });

  it("доверенное событие `play` без жеста (автозапуск сайта): ничего", async () => {
    const page = watchedPage();
    page.activation.transient = false;
    const film = page.addVideo((video) => {
      video.real.paused = false;
    });
    film.dispatchEvent(page.harness.dom.trusted("play"));
    await flushMicrotasks();
    assert.deepEqual(page.sent, []);
    assert.equal(film.real.paused, false);
  });
});

describe("память отказов своя у каждой причины задержки", () => {
  it("отказ переезда (хвост взвода сеанса после «Return») не отнимает видео у нового нажатия человека", async () => {
    const { guard, sent, addVideo, keyOf, harness } = watchedPage();
    const film = addVideo();
    // Хвост взвода сеанса: вернувшееся видео задержано как «следующее», фон сеанса уже нет — отпустил.
    guard.arm(LEASE_MS);
    void pagePlay(film);
    assert.equal(heldEvent(sent).trigger, "next_video");
    guard.release(keyOf(film));
    await flushMicrotasks();
    assert.equal(film.real.paused, false, "отпущенное заиграло само");
    // Взвод кончился; человек снова сам жмёт play с тем же источником.
    harness.clock.nowMs += LEASE_MS + 1;
    const browserPlaysBefore = browserPlays(film);
    void pagePlay(film);
    await flushMicrotasks();
    assert.equal(sent.length, 2, "новое нажатие человека доходит до фона");
    assert.equal(heldEvent(sent).trigger, "viewer_play");
    assert.equal(browserPlays(film), browserPlaysBefore, "браузеру этот play() не ушёл: видео под тенью");
  });

  it("отказ режима 2 по-прежнему помнится для режима 2 (тот же источник второй раз не задерживается)", async () => {
    const { guard, sent, addVideo, keyOf } = watchedPage();
    const clip = addVideo();
    void pagePlay(clip);
    assert.equal(heldEvent(sent).trigger, "viewer_play");
    guard.release(keyOf(clip));
    await flushMicrotasks();
    void pagePlay(clip);
    await flushMicrotasks();
    assert.equal(sent.length, 1);
  });
});

describe("взвод фона важнее охраны", () => {
  it("пока взвод жив, донесение несёт `next_video`, а не `viewer_play`", () => {
    const { guard, sent, addVideo } = watchedPage();
    const film = addVideo();
    guard.arm(LEASE_MS);
    void pagePlay(film);
    assert.equal(heldEvent(sent).trigger, "next_video");
  });
});

describe("судья вердиктов (чистая функция)", () => {
  const base: ViewerPlayFacts = {
    watch: "on",
    transientActivation: true,
    creditActive: false,
    handedBackAtMs: null,
    lastTrustedInputAtMs: 500,
    audibility: "audible",
  };

  it("каждое условие даёт свой названный отказ", () => {
    assert.equal(judgeViewerPlay(base), "viewer_started");
    assert.equal(judgeViewerPlay({ ...base, watch: "off" }), "watch_off");
    assert.equal(judgeViewerPlay({ ...base, watch: "off", audibility: "silent" }), "watch_off");
    assert.equal(judgeViewerPlay({ ...base, audibility: "silent" }), "muted");
    assert.equal(judgeViewerPlay({ ...base, watch: "on_including_muted", audibility: "silent" }), "viewer_started", "фильтр звука снят");
    assert.equal(judgeViewerPlay({ ...base, watch: "on_including_muted", audibility: "silent", transientActivation: false }), "no_viewer_gesture");
    assert.equal(judgeViewerPlay({ ...base, transientActivation: false }), "no_viewer_gesture");
    assert.equal(judgeViewerPlay({ ...base, transientActivation: false, creditActive: true }), "viewer_started");
    assert.equal(judgeViewerPlay({ ...base, handedBackAtMs: 500 }), "handed_back_without_new_input", "ввод ровно в момент конца не новый");
    assert.equal(judgeViewerPlay({ ...base, handedBackAtMs: 499 }), "viewer_started");
    assert.equal(judgeViewerPlay({ ...base, transientActivation: false, creditActive: true, handedBackAtMs: 600 }), "handed_back_without_new_input");
  });

  it("предел кредита из договора не превышается", async () => {
    const { guard, harness, activation, sent, addVideo } = setupGuard();
    guard.setViewerPlayWatch("on");
    activation.transient = false;
    guard.grantViewerPlayCredit(MAX_PLAY_CREDIT_LEASE_MS * 10);
    harness.clock.advance(MAX_PLAY_CREDIT_LEASE_MS);
    const film = addVideo();
    await pagePlay(film);
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
  });
});

describe("конец передачи через контроллер", () => {
  it("передача кончилась (старт не вышел): тот же элемент не задерживается на старом жесте, а после нового ввода — задерживается", async () => {
    const page = watchedPage();
    const { guard, harness, activation, sent, addVideo, keyOf } = page;
    const film = addVideo((video) => {
      video.real.src = FILE_URL;
    });
    activation.lastInputAtMs = harness.clock.nowMs;
    void pagePlay(film);
    assert.equal(heldEvent(sent).trigger, "viewer_play");
    const controller = new HandoffController({
      registry: { elementByKey: () => film, keyOf, isMediaSourceObjectUrl: () => true } as unknown as ElementRegistry,
      candidates: { documentTag: "doc-a", sourceOf: () => ({ kind: "file", url: FILE_URL }) } as unknown as VideoCandidates,
      dom: { isVideoElement: () => true, referrerPolicyOf: () => "", metaReferrerContents: () => [], documentOrigin: () => "https://site.example" } as unknown as DomReaders,
      binary: {} as BinaryIntrinsics,
      meter: {} as ForwardByteMeter,
      startShadow: () => assert.fail("задержанное видео не должно получить вторую тень"),
      workerMediaSources: { closeAllStreams: () => {} } as unknown as WorkerMediaSources,
      reportOddity: () => {},
      protection: { isProtected: () => false },
      nextVideo: guard,
    });
    const port = {
      send: (event: PageEvent) => {
        if (event.kind === "file_source") throw new Error("port refused the message");
      },
    } as unknown as PagePort;
    controller.connect(port);
    harness.clock.advance(100);
    controller.handle({ kind: "begin_handoff", request_id: 1, document: "doc-a", element: keyOf(film) });
    assert.equal(controller.isHandingOff, false, "передача кончилась");
    await flushMicrotasks();

    sent.length = 0;
    activation.transient = true; // активация браузера ещё жива, но ввод был ДО конца передачи
    await pagePlay(film);
    assert.deepEqual(sent, [], "возвращённое видео не задерживается снова на старом жесте");

    harness.clock.advance(50);
    activation.lastInputAtMs = harness.clock.nowMs;
    film.real.paused = true;
    // Отпущенное видео помнит отпечаток источника: меняем источник, как делает плеер сайта на новом ролике.
    film.real.src = "https://cdn.example/next.mp4";
    void pagePlay(film);
    assert.equal(heldEvent(sent).trigger, "viewer_play", "после нового ввода — снова наше");
  });
});
