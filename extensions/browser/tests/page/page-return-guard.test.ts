// Взвод возврата страницы (сессия 11b2): сторож с НАСТОЯЩЕЙ тенью на фейковом DOM. Вкладку открыли заново;
// тень должна встать на то самое видео раньше первого кадра, страница — сама перемотаться на секунду плеера,
// а запрет автозапуска (вкладка без жеста зрителя) не должен ничего сломать.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageReturnTarget } from "../../src/shared/handoff-contract.ts";
import { flushMicrotasks, type FakeVideo } from "./shadow/fake-media-dom.ts";
import { browserPlays, LEASE_MS, pagePlay, setupGuard } from "./support/next-video-guard-setup.ts";

const FILM_DURATION_S = 1_800;
const PLAYER_SECOND = 600;
const FILM: PageReturnTarget = { kind: "recorded", duration_s: FILM_DURATION_S, start_s: PLAYER_SECOND };

/** Свежая страница: видео ещё без метаданных, как сразу после загрузки. */
function freshVideo(video: FakeVideo): void {
  video.real.readyState = 0;
  video.real.duration = Number.NaN;
  video.real.currentTime = 0;
}

/** Метаданные пришли (MSE: страница добавила init-сегмент; файл: браузер прочитал заголовок). */
async function metadataArrives(video: FakeVideo, durationS: number, dispatch: (type: string) => void): Promise<void> {
  video.real.readyState = 1;
  video.real.duration = durationS;
  dispatch("loadedmetadata");
  await flushMicrotasks();
}

/** Что страница видит у элемента (переопределения прототипа: при тени — модель тени). */
function pageView(video: FakeVideo): { readonly currentTime: number; readonly paused: boolean } {
  return video as unknown as { currentTime: number; paused: boolean };
}

const seeksOf = (video: FakeVideo): number[] =>
  video.realCalls.filter((call) => call.kind === "set" && call.property === "currentTime").map((call) => Number((call as { value: unknown }).value));

describe("взвод возврата: тот же фильм", () => {
  it("`play()` до метаданных не идёт в браузер; страница сама перематывается на секунду плеера, тень встаёт на `seeked`", async () => {
    const { guard, harness, sent, addVideo, keyOf } = setupGuard();
    const film = addVideo(freshVideo);
    guard.arm(LEASE_MS, FILM);
    let outcome = "pending";
    pagePlay(film).then(
      () => (outcome = "resolved"),
      (error: { name: string }) => (outcome = `rejected:${error.name}`),
    );
    assert.equal(browserPlays(film), 0, "запрет автозапуска не сработает: браузеру play() не передан");
    await metadataArrives(film, FILM_DURATION_S, (type) => film.dispatchEvent(harness.dom.trusted(type)));
    assert.deepEqual(seeksOf(film), [PLAYER_SECOND], "страница перемотана на секунду плеера настоящим currentTime");
    // `seeking`/`seeked` фейк шлёт микрозадачами: после них тень уже стоит.
    await flushMicrotasks();
    const held = sent.at(-1);
    assert.ok(held?.kind === "next_video_held", "фон получил донесение о задержанном видео");
    assert.equal(held.element, keyOf(film));
    assert.equal(browserPlays(film), 0, "браузер так и не запускал фильм: декодировать нечего");
    assert.equal(film.real.paused, true);
    assert.equal(film.real.muted, true, "настоящий элемент заглушён тенью");
    assert.equal(pageView(film).currentTime, PLAYER_SECOND, "страница видит секунду плеера");
    assert.equal(pageView(film).paused, false, "страница просила играть — тень показывает игру");
    assert.equal(outcome, "pending", "обещание страницы досталось тени, а не отвергнуто");
  });

  it("страница уже на секунде плеера (свой «продолжить с места»): тень сразу, без лишней перемотки", () => {
    const { guard, sent, addVideo } = setupGuard();
    const film = addVideo((video) => {
      video.real.duration = FILM_DURATION_S + 1.5;
      video.real.currentTime = PLAYER_SECOND - 0.4;
    });
    guard.arm(LEASE_MS, FILM);
    void pagePlay(film);
    assert.deepEqual(seeksOf(film), []);
    assert.equal(sent.at(-1)?.kind, "next_video_held");
    assert.equal(browserPlays(film), 0);
  });

  it("сайт сам сбрасывает позицию после нашей перемотки: две попытки, потом тень там, где страница", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const film = addVideo((video) => void (video.real.duration = FILM_DURATION_S));
    guard.arm(LEASE_MS, FILM);
    // Сайт перехватывает каждую перемотку и возвращает видео в начало.
    const resetBySite = (): void => {
      film.real.currentTime = 0;
    };
    void pagePlay(film);
    resetBySite();
    film.dispatchEvent(harness.dom.trusted("seeked"));
    await flushMicrotasks();
    resetBySite();
    film.dispatchEvent(harness.dom.trusted("seeked"));
    await flushMicrotasks();
    assert.deepEqual(seeksOf(film), [PLAYER_SECOND, PLAYER_SECOND], "ровно две попытки, без бесконечной борьбы с сайтом");
    assert.equal(sent.filter((event) => event.kind === "next_video_held").length, 1, "тень встала один раз");
  });
});

describe("взвод возврата: не то видео", () => {
  it("ролик другой длины (реклама перед фильмом) уходит браузеру: отложенный play() исполняется, тени нет", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const ad = addVideo(freshVideo);
    guard.arm(LEASE_MS, FILM);
    let outcome = "pending";
    pagePlay(ad).then(
      () => (outcome = "resolved"),
      () => (outcome = "rejected"),
    );
    assert.equal(browserPlays(ad), 0);
    await metadataArrives(ad, 15, (type) => ad.dispatchEvent(harness.dom.trusted(type)));
    assert.equal(browserPlays(ad), 1, "реклама играет в браузере сама (вкладка без звука)");
    assert.deepEqual(seeksOf(ad), [], "чужой ролик не перематываем");
    assert.deepEqual(sent, []);
    assert.equal(outcome, "resolved", "обещание страницы следует за настоящим");
  });

  it("эфир ждёт видео без конца шкалы: запись не берётся, эфир — сразу и без перемотки", () => {
    const { guard, sent, addVideo, keyOf } = setupGuard();
    const recorded = addVideo((video) => void (video.real.duration = 600));
    const live = addVideo((video) => void (video.real.duration = Number.POSITIVE_INFINITY));
    guard.arm(LEASE_MS, { kind: "live" });
    void pagePlay(recorded);
    void pagePlay(live);
    assert.equal(browserPlays(recorded), 1);
    const held = sent.filter((event) => event.kind === "next_video_held");
    assert.equal(held.length, 1);
    assert.ok(held[0]?.kind === "next_video_held");
    assert.equal(held[0].element, keyOf(live));
    assert.deepEqual(seeksOf(live), []);
  });
});

describe("взвод возврата: границы", () => {
  it("срок взвода вышел, а play() отложен: следующее событие отдаёт его браузеру, страница не зависает", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const film = addVideo(freshVideo);
    guard.arm(LEASE_MS, FILM);
    let outcome = "pending";
    pagePlay(film).then(
      () => (outcome = "resolved"),
      () => (outcome = "rejected"),
    );
    harness.clock.advance(LEASE_MS);
    await metadataArrives(film, FILM_DURATION_S, (type) => film.dispatchEvent(harness.dom.trusted(type)));
    assert.equal(browserPlays(film), 1);
    assert.deepEqual(sent, []);
    assert.equal(outcome, "resolved");
  });

  it("видео стоит и страница его не запускала: сторож его не трогает (сайт ждёт клика — фон честно откажет)", async () => {
    const { guard, harness, sent, addVideo } = setupGuard();
    const film = addVideo(freshVideo);
    guard.arm(LEASE_MS, FILM);
    await metadataArrives(film, FILM_DURATION_S, (type) => film.dispatchEvent(harness.dom.trusted(type)));
    guard.arm(LEASE_MS, FILM);
    assert.deepEqual(seeksOf(film), []);
    assert.deepEqual(sent, []);
  });

  it("обычный взвод переезда после взвода возврата прежний: play() до метаданных идёт в браузер", () => {
    const { guard, addVideo } = setupGuard();
    const ad = addVideo(freshVideo);
    guard.arm(LEASE_MS, FILM);
    guard.arm(LEASE_MS);
    void pagePlay(ad).catch(() => {});
    assert.equal(browserPlays(ad), 1);
  });

  it("защищённое видео взвод возврата не трогает: play() прозрачен", () => {
    const { guard, sent, addVideo, protectedVideos } = setupGuard();
    const drm = addVideo((video) => void (video.real.duration = FILM_DURATION_S));
    protectedVideos.add(drm);
    guard.arm(LEASE_MS, FILM);
    void pagePlay(drm);
    assert.equal(browserPlays(drm), 1);
    assert.deepEqual(sent, []);
  });
});
