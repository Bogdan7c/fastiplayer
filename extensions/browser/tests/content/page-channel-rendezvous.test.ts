// Встреча участка 1 (договор src/shared/page-channel-handshake.ts): НАСТОЯЩИЙ изолированный мир
// (`openPageChannel`) и НАСТОЯЩИЙ MAIN-мир (`startPageBridge` на фейковой странице) в одном окне.
//
// Главный случай — гонка Firefox (стенд после 09b): изолированный скрипт исполнился, задача доставки
// `message` успела выполниться, и только потом встал MAIN-скрипт. Прежний слепой `postMessage` терял
// порт, фон слышал тишину на «какие у тебя видео» и отвечал «no_video». Итог каждого теста — то, что
// видит фон: пришёл ли ответ MAIN-мира по каналу, и что досталось обработчикам страницы.
import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";
import { openPageChannel, type PageChannel, type PageChannelPort } from "../../src/content/page-channel.ts";
import { captureChannelOriginals, listenForPageChannel, type RendezvousOriginals } from "../../src/page/page-channel.ts";
import {
  HANDSHAKE_MARKER_FIELD,
  PAGE_CHANNEL_OFFER_EVENT,
  PAGE_CHANNEL_READY_EVENT,
} from "../../src/shared/page-channel-handshake.ts";
import { startHarness } from "../page/support/bridge-harness.ts";
import { createFakePage, type FakePage } from "../page/support/fake-page.ts";

/** Все каналы теста: открытые порты держат процесс Node, после каждого теста их закрываем. */
const openChannels: MessageChannel[] = [];
afterEach(() => {
  for (const messageChannel of openChannels.splice(0)) {
    messageChannel.port1.close();
    messageChannel.port2.close();
  }
});

/** Пауза в одну задачу цикла событий: за неё браузер успевает доставить `postMessage`. */
const oneTask = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

/** Изолированный мир в фейковой странице: канал, всё, что пришло от MAIN-мира, и счёт рукопожатий. */
interface IsolatedWorld {
  readonly channel: PageChannel;
  readonly fromMain: unknown[];
  /** Сколько раз изолированный мир отправил рукопожатие (`window.postMessage` с портом). */
  handshakesPosted: number;
  /** Ждать ответов MAIN-мира не дольше `timeoutMs`. */
  waitForReplies(count: number, timeoutMs?: number): Promise<void>;
}

/**
 * Запустить настоящий `openPageChannel` против окна фейковой страницы. `window.postMessage` здесь
 * асинхронный, как в браузере: событие `message` приходит отдельной задачей, а не внутри вызова.
 */
function startIsolatedWorld(page: FakePage): IsolatedWorld {
  const fromMain: unknown[] = [];
  const world: IsolatedWorld = {
    channel: undefined as unknown as PageChannel,
    fromMain,
    handshakesPosted: 0,
    async waitForReplies(count, timeoutMs = 1000) {
      const deadline = Date.now() + timeoutMs;
      while (fromMain.length < count) {
        if (Date.now() > deadline) throw new Error(`timeout: MAIN answered ${fromMain.length} of ${count}`);
        await new Promise((resolve) => setTimeout(resolve, 2));
      }
    },
  };
  const channel = openPageChannel(
    {
      createChannel: () => {
        const messageChannel = new MessageChannel();
        openChannels.push(messageChannel);
        return { port1: messageChannel.port1 as unknown as PageChannelPort, port2: messageChannel.port2 as unknown as Transferable };
      },
      postToOwnWindow: (message, transfer) => {
        world.handshakesPosted += 1;
        setTimeout(() => {
          page.win.dispatchEvent(new page.WindowMessageEvent("message", { data: message, ports: transfer as MessagePort[] }, page.win));
        }, 0);
      },
      rendezvous: page.win,
      createCancelableEvent: (type) => new Event(type, { cancelable: true }),
    },
    (message) => fromMain.push(message),
  );
  (world as { channel: PageChannel }).channel = channel;
  return world;
}

/** Обработчик `message` «страницы»: что из наших рукопожатий досталось бы её коду. */
function watchPageMessages(page: FakePage): unknown[] {
  const seen: unknown[] = [];
  page.win.addEventListener("message", (event) => seen.push((event as MessageEvent).data));
  return seen;
}

/** Ответ MAIN-мира на «какие у тебя видео» — то, чего фон ждёт перед выбором видео. */
const CANDIDATES_REPLY = { kind: "candidates", request_id: 1, candidates: [] };

describe("встреча участка 1: порт доходит при любом порядке и паузе между мирами", () => {
  it("гонка Firefox: изолированный мир встал, задача доставки прошла, MAIN-мир встал позже → канал работает", async () => {
    const page = createFakePage();
    const isolated = startIsolatedWorld(page);
    // Пауза между скриптами: прежний слепой postMessage доставлялся бы здесь — в пустоту.
    await oneTask();
    await oneTask();
    startHarness({ page });
    // Обработчик страницы — после наших скриптов, как в браузере (скрипты страницы идут позже).
    const pageSaw = watchPageMessages(page);
    isolated.channel.send({ kind: "describe_candidates", request_id: 1 });
    await isolated.waitForReplies(1);
    assert.deepEqual(isolated.fromMain, [CANDIDATES_REPLY]);
    assert.equal(isolated.handshakesPosted, 1, "рукопожатие одно");
    assert.equal(pageSaw.length, 0, "порт не достался обработчикам страницы");
  });

  it("MAIN-мир встал РАНЬШЕ изолированного: OFFER отменён, порт ушёл сразу → канал работает", async () => {
    const page = createFakePage();
    startHarness({ page });
    const pageSaw = watchPageMessages(page);
    const isolated = startIsolatedWorld(page);
    assert.equal(isolated.handshakesPosted, 1, "OFFER принят — рукопожатие отправлено на старте");
    isolated.channel.send({ kind: "describe_candidates", request_id: 1 });
    await isolated.waitForReplies(1);
    assert.deepEqual(isolated.fromMain, [CANDIDATES_REPLY]);
    assert.equal(pageSaw.length, 0);
  });

  it("оба мира в одном синхронном отрезке (как в Chromium) → канал работает", async () => {
    const page = createFakePage();
    const isolated = startIsolatedWorld(page);
    startHarness({ page });
    isolated.channel.send({ kind: "describe_candidates", request_id: 1 });
    await isolated.waitForReplies(1);
    assert.deepEqual(isolated.fromMain, [CANDIDATES_REPLY]);
  });

  it("вопрос фона, заданный ДО встречи, не теряется: порт копит его и отдаёт MAIN-миру после", async () => {
    const page = createFakePage();
    const isolated = startIsolatedWorld(page);
    isolated.channel.send({ kind: "describe_candidates", request_id: 1 });
    await oneTask();
    startHarness({ page });
    await isolated.waitForReplies(1);
    assert.deepEqual(isolated.fromMain, [CANDIDATES_REPLY]);
  });
});

describe("встреча участка 1: нет MAIN-мира, подделки, следы", () => {
  it("MAIN-мира нет: порт не уходит в окно вовсе (странице его не подсунуть)", async () => {
    const page = createFakePage();
    const pageSaw = watchPageMessages(page);
    const isolated = startIsolatedWorld(page);
    await oneTask();
    assert.equal(isolated.handshakesPosted, 0);
    assert.equal(pageSaw.length, 0);
  });

  it("повторные READY (подделка страницы после встречи) не дают второго рукопожатия", async () => {
    const page = createFakePage();
    const isolated = startIsolatedWorld(page);
    startHarness({ page });
    const pageSaw = watchPageMessages(page);
    page.win.dispatchEvent(new Event(PAGE_CHANNEL_READY_EVENT));
    page.win.dispatchEvent(new Event(PAGE_CHANNEL_READY_EVENT));
    await oneTask();
    assert.equal(isolated.handshakesPosted, 1);
    assert.ok(
      pageSaw.every((message) => !(typeof message === "object" && message !== null && HANDSHAKE_MARKER_FIELD in message)),
      "второго порта страница не получила",
    );
  });

  it("после встречи MAIN-мир OFFER не отменяет: страница не узнаёт о нас по отмене", async () => {
    const page = createFakePage();
    const isolated = startIsolatedWorld(page);
    startHarness({ page });
    isolated.channel.send({ kind: "describe_candidates", request_id: 1 });
    await isolated.waitForReplies(1);
    const probe = new Event(PAGE_CHANNEL_OFFER_EVENT, { cancelable: true });
    assert.equal(page.win.dispatchEvent(probe), true, "OFFER страницы никто не отменил");
  });

  it("READY MAIN-мира гасится изолированным миром: слушатели страницы позже в цепочке его не видят", () => {
    const page = createFakePage();
    startIsolatedWorld(page);
    let pageSawReady = 0;
    page.win.addEventListener(PAGE_CHANNEL_READY_EVENT, () => (pageSawReady += 1), { capture: true });
    startHarness({ page });
    assert.equal(pageSawReady, 0);
  });

  it("сбой сигнала READY не уходит странице: одна строка отладки, слушатель рукопожатия стоит", async () => {
    const page = createFakePage();
    const oddities: string[] = [];
    const brokenRendezvous: RendezvousOriginals = {
      dispatch: () => {
        throw new Error("dispatchEvent replaced by a hostile page");
      },
      preventDefault: (event) => (event as Event).preventDefault(),
      createEvent: (type) => new Event(type),
    };
    let connected = 0;
    assert.doesNotThrow(() =>
      listenForPageChannel(page.win, captureChannelOriginals(page.win), brokenRendezvous, { onConnected: () => (connected += 1), onMessage: () => {} }, (what) =>
        oddities.push(what),
      ),
    );
    assert.deepEqual(oddities, ["page channel ready signal failed"]);
    // Изолированный мир, вставший позже, всё равно отдаёт порт через OFFER.
    const isolated = startIsolatedWorld(page);
    await oneTask();
    await oneTask();
    assert.equal(isolated.handshakesPosted, 1);
    assert.equal(connected, 1, "канал установлен");
  });
});
