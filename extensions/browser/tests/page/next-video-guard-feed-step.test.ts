// Сторож следующего видео и шаг ленты (16b2, находка живой проверки Instagram, решение 361).
//
// Живой замер: лента зовёт `play()` следующего ролика той же задачей, что и `pause()` нашего. Хвост прежнего
// взвода задерживает следующий ролик раньше, чем фон узнал, что наш ушёл с экрана, фон отвечает «не сейчас» —
// и без шага ленты сторож запоминал этот отказ навсегда: ролик играл в браузере со звуком, без заглушки.
// Здесь НАСТОЯЩАЯ тень на фейковом DOM; проверяется то, что видит фон (донесения) и браузер (игра элемента).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks } from "./shadow/fake-media-dom.ts";
import { browserPlays, FILE_URL, LEASE_MS, pagePlay, setupGuard } from "./support/next-video-guard-setup.ts";
import { WATCH_ON } from "../support/auto-handoff-watch-fixtures.ts";

/** Страница ленты: сторож взведён хвостом прежнего взвода, следующий ролик загружен и ждёт `play()`. */
function feedPage() {
  const page = setupGuard();
  const nextReel = page.addVideo((video) => {
    video.real.src = FILE_URL;
  });
  page.guard.arm(LEASE_MS);
  return { ...page, nextReel };
}

function heldReports(sent: ReturnType<typeof setupGuard>["sent"]): number {
  return sent.filter((event) => event.kind === "next_video_held").length;
}

describe("отказ переезда относится к шагу ленты", () => {
  it("ролик задержан, лента листнулась, фон отказал по-старому — ролик сразу задержан снова и фон спрошен заново", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    // Одна задача страницы: play() следующего ролика (хвост взвода ставит тень)…
    void pagePlay(nextReel);
    assert.equal(heldReports(sent), 1);
    // …и pause() нашего: тень доносит уход с экрана, сторож узнаёт о нём раньше фона.
    guard.noteHandedOffVideoLeftScreen();
    // Ответ фона на ПЕРВОЕ донесение («переезд закрыт») приходит уже после.
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), 2, "фон спрошен заново — теперь он знает, что лента листнулась");
    assert.equal(nextReel.real.paused, true, "ролик снова под тенью, в браузере не играет");
  });

  it("сначала отказ, потом лента листнулась — ближайший взвод задерживает тот же ролик", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    void pagePlay(nextReel);
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(nextReel.real.paused, false, "отпущенный ролик играет сам");
    guard.arm(LEASE_MS);
    assert.equal(heldReports(sent), 1, "пока лента не листнулась, отказ в силе");
    guard.noteHandedOffVideoLeftScreen();
    guard.arm(LEASE_MS);
    assert.equal(heldReports(sent), 2);
    assert.equal(nextReel.real.paused, true);
  });

  it("повторный отказ на том же шаге ленты в силе: по кругу «задержан — отпущен» сторож не ходит", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    void pagePlay(nextReel);
    guard.noteHandedOffVideoLeftScreen();
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), 2);
    // Фон отказал и на второе донесение (ролик ему не подошёл): шаг тот же — больше не спрашиваем.
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    guard.arm(LEASE_MS);
    guard.arm(LEASE_MS);
    assert.equal(heldReports(sent), 2);
    assert.equal(nextReel.real.paused, false, "ролик остался играть в браузере");
  });

  it("отказ без нового шага ленты не переспрашивается сразу: настоящий play() один, донесение одно", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    const promise = pagePlay(nextReel);
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), 1);
    assert.equal(browserPlays(nextReel), 1);
    await promise;
  });

  it("сам шаг ленты ничего не задерживает и не отпускает", () => {
    const { guard, sent, nextReel } = feedPage();
    void pagePlay(nextReel);
    const reportsBefore = sent.length;
    guard.noteHandedOffVideoLeftScreen();
    assert.equal(sent.length, reportsBefore, "фону ни слова");
    assert.equal(nextReel.real.paused, true, "задержанный ролик по-прежнему под тенью");
    assert.equal(browserPlays(nextReel), 0, "браузер его не запускал");
  });

  it("вне взвода шаг ленты не задерживает играющее видео", () => {
    const page = setupGuard();
    const playing = page.addVideo((video) => {
      video.real.paused = false;
      video.real.src = FILE_URL;
    });
    page.guard.noteHandedOffVideoLeftScreen();
    assert.deepEqual(page.sent, []);
    assert.equal(playing.real.paused, false);
  });

  it("отказ режима 2 (нажатие play человеком) шагом ленты не снимается", async () => {
    const page = setupGuard();
    page.guard.setViewerPlayWatch(WATCH_ON);
    page.activation.clickOnVideo = true;
    const film = page.addVideo();
    void pagePlay(film);
    assert.equal(heldReports(page.sent), 1);
    page.guard.release(page.keyOf(film));
    await flushMicrotasks();
    page.guard.noteHandedOffVideoLeftScreen();
    // Человек жмёт play на том же видео ещё раз: фон уже отказал, вопрос не повторяется.
    page.activation.clickOnVideo = true;
    void pagePlay(film);
    await flushMicrotasks();
    assert.equal(heldReports(page.sent), 1);
  });
});

describe("обратный порядок: уход донесён раньше задержания, а отказ фона обогнал его в пути (16b3, F6)", () => {
  it("отказ на том же шаге, пока новой передачи не было, — фон спрошен ещё раз, ролик под тенью", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    // pause() нашего ролика пришёл первым: тень донесла уход, шаг ленты уже новый.
    guard.noteHandedOffVideoLeftScreen();
    void pagePlay(nextReel);
    assert.equal(heldReports(sent), 1);
    // Донесение сторожа дошло до фона раньше `video_left_screen`: «переезд закрыт».
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), 2, "фон спрошен заново — теперь он знает об уходе");
    assert.equal(nextReel.real.paused, true, "ролик снова под тенью, в браузере не звучит");
  });

  it("второй отказ на том же шаге — окончательный: ролик играет в браузере, по кругу сторож не ходит", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    guard.noteHandedOffVideoLeftScreen();
    void pagePlay(nextReel);
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    guard.arm(LEASE_MS);
    assert.equal(heldReports(sent), 2);
    assert.equal(nextReel.real.paused, false);
  });

  it("после ухода уже началась новая передача — фон ответил со знанием дела, отказ не переспрашивается", async () => {
    const { guard, sent, nextReel, keyOf, addVideo } = feedPage();
    const adopted = addVideo((video) => {
      video.real.src = FILE_URL;
    });
    guard.noteHandedOffVideoLeftScreen();
    void pagePlay(adopted);
    assert.ok(guard.take(adopted) !== null, "фон принял ролик: тень встала на него");
    void pagePlay(nextReel);
    const reportsBefore = heldReports(sent);
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), reportsBefore, "повторного вопроса нет");
    assert.equal(nextReel.real.paused, false, "отпущенный ролик играет сам");
  });

  it("до первого ухода с экрана (шаг 0) отказ не переспрашивается", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    void pagePlay(nextReel);
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), 1);
  });
});

describe("переспрос не снимает тень (стенд 16b3: пул TikTok, превью при уходе)", () => {
  it("обещание play() страницы не отвергается, браузер ролик не запускал, после окончательного отказа ролик играет", async () => {
    const { guard, sent, nextReel, keyOf } = feedPage();
    guard.noteHandedOffVideoLeftScreen();
    const promise = pagePlay(nextReel);
    let rejection: unknown;
    promise.catch((error: unknown) => {
      rejection = error;
    });
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    assert.equal(heldReports(sent), 2);
    assert.equal(browserPlays(nextReel), 0, "переспрос: тень не снималась, настоящий play() не звался");
    assert.equal(nextReel.real.paused, true);
    guard.release(keyOf(nextReel));
    await flushMicrotasks();
    await promise;
    assert.equal(rejection, undefined, "страница не получила AbortError, которого не вызывала");
    assert.equal(nextReel.real.paused, false, "окончательный отказ: ролик играет в браузере");
  });
});

