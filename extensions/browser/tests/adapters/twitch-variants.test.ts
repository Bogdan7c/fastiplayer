// Адаптер вариантов Twitch (15a) с НАСТОЯЩИМИ клиентом и исполнителем руки-функции и белым списком Twitch.
// «Окно» страницы — подделка: документ → элемент `.video-player` → цепочка узлов React fiber → плеер-класс.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { VariantPageView } from "../../src/adapters/capabilities/variants/contract.ts";
import { TWITCH_SITE } from "../../src/adapters/sites/twitch/identity.ts";
import { TWITCH_PLAYER_HAND } from "../../src/adapters/sites/twitch/player-hand.ts";
import { createTwitchVariants, TwitchPageError } from "../../src/adapters/sites/twitch/variants.ts";
import { createAdapterHandClient } from "../../src/content/adapter-hand-client.ts";
import { createAdapterHandExecutor } from "../../src/page/adapter-hand.ts";
import { createFakeClock } from "../background/support.ts";
import { createFakeAdView, FakeNode } from "./fake-ad-dom.ts";

interface QualityEntry {
  name: string;
  group: string;
  height: number;
  isDefault: boolean;
}

const QUALITIES: readonly QualityEntry[] = [
  { name: "1080p60 (Source)", group: "chunked", height: 1080, isDefault: true },
  { name: "720p60", group: "720p60", height: 720, isDefault: false },
  { name: "480p", group: "480p30", height: 480, isDefault: false },
];

/** Плеер как у Twitch: методы на прототипе класса, состояние в полях. */
class FakeTwitchPlayer {
  calls: string[] = [];
  auto = false;
  current: QualityEntry = QUALITIES[0] as QualityEntry;
  list: unknown = QUALITIES;
  login: unknown = "some_channel";
  failQualities = false;
  getQualities(): unknown {
    this.calls.push("getQualities");
    if (this.failQualities) throw new Error("player crashed");
    return this.list;
  }
  getQuality(): QualityEntry {
    return this.current;
  }
  setQuality(quality: unknown): void {
    this.calls.push("setQuality");
    this.received = quality;
    this.current = quality as QualityEntry;
    this.auto = false;
  }
  received: unknown = undefined;
  isAutoQualityMode(): boolean {
    return this.auto;
  }
  setAutoQualityMode(value: boolean): void {
    this.calls.push(`setAuto:${value}`);
    this.auto = value;
  }
  getLoadedChannelLogin(): unknown {
    return this.login;
  }
}

/** Цепочка fiber-узлов длиной `depth`: плеер лежит в свойствах узла с номером `depth` (1 — сам fiber элемента). */
function fiberChain(depth: number, player: object | undefined, firstNodeTrap = false): object {
  let node: object | undefined;
  for (let index = depth; index >= 1; index -= 1) {
    const props: Record<string, unknown> = index === depth && player !== undefined ? { mediaPlayerInstance: player } : {};
    if (index === 1 && firstNodeTrap) Object.defineProperty(props, "mediaPlayerInstance", { enumerable: true, get: () => { throw new Error("trap"); } });
    node = { memoizedProps: props, return: node };
  }
  return node as object;
}

/** «Окно» страницы с документом, чей `.video-player` несёт fiber под ключом `fiberKey`. */
function twitchWindow(options: { fiber?: object; fiberKey?: string; noElement?: boolean }): object {
  const element: Record<string, unknown> = {};
  if (options.fiber !== undefined) element[options.fiberKey ?? "__reactFiber$abc"] = options.fiber;
  const document = {
    querySelector: (selector: string) => (selector === ".video-player" && options.noElement !== true ? element : null),
    querySelectorAll: () => [],
  };
  return { document };
}

function playerWindow(player: FakeTwitchPlayer, depth = 24): object {
  return twitchWindow({ fiber: fiberChain(depth, player) });
}

function view(hostname: string, withPlayer = true): VariantPageView {
  const root = new FakeNode("html");
  if (withPlayer) root.add(new FakeNode("div", { classes: ["video-player"] }));
  return { ...createFakeAdView(root, { hostname }), attributeOf: () => null };
}

function adapterFor(page: object, hostname = "www.twitch.tv") {
  const clock = createFakeClock();
  let client: ReturnType<typeof createAdapterHandClient> | undefined;
  const executor = createAdapterHandExecutor({
    page,
    actionsOf: (siteId) => (siteId === TWITCH_SITE.id ? TWITCH_PLAYER_HAND : undefined),
    send: (reply) => queueMicrotask(() => client?.acceptPageMessage(structuredClone(reply))),
  });
  client = createAdapterHandClient({
    sendToPage: (call) => queueMicrotask(() => void executor.handle(structuredClone(call))),
    timers: clock.scheduler,
    now: clock.scheduler.now,
  });
  return createTwitchVariants({ dom: view(hostname), player: client.handFor(TWITCH_SITE.id) });
}

describe("адаптер Twitch: опознание", () => {
  it("свой хост и поддомены с плеером; чужой хост, похожий суффикс и страница без плеера — нет", () => {
    const place = (hostname: string, withPlayer = true) => adapterFor({}).matches(view(hostname, withPlayer));
    for (const host of ["twitch.tv", "www.twitch.tv", "m.twitch.tv", "player.twitch.tv"]) assert.equal(place(host), true, host);
    assert.equal(place("eviltwitch.tv"), false);
    assert.equal(place("twitch.tv.evil.example"), false);
    assert.equal(place("www.example.com"), false);
    assert.equal(place("www.twitch.tv", false), false);
  });
});

describe("адаптер Twitch: описание", () => {
  it("«Auto» первым, качества по списку плеера (подпись — ступень, 15a), выбранное — по группе, ключ эфира — логин; озвучек и серии нет", async () => {
    const player = new FakeTwitchPlayer();
    player.current = QUALITIES[1] as QualityEntry;
    assert.deepEqual(await adapterFor(playerWindow(player)).describe(), {
      seriesKey: "twitch",
      episodeKey: "some_channel",
      audio: [],
      quality: [
        { id: "auto", label: "Auto", selected: false, automatic: true },
        // «1080p60 (Source)» и «720p60» сайта — ступени: память выбора найдёт их на любом канале.
        { id: "chunked", label: "1080p", selected: false },
        { id: "720p60", label: "720p", selected: true },
        { id: "480p30", label: "480p", selected: false },
      ],
      nextEpisode: false,
    });
  });

  it("годных качеств нет (плеер строится или все с мусорной группой) — меню качеств пустое, одинокого «Auto» нет", async () => {
    const player = new FakeTwitchPlayer();
    player.list = [{ name: "720p", group: "720 p60", height: 720, isDefault: false }];
    const description = await adapterFor(playerWindow(player)).describe();
    assert.deepEqual(description?.quality, []);
  });

  it("автоматика включена — выбран «Auto», ни одно обычное качество не выбрано", async () => {
    const player = new FakeTwitchPlayer();
    player.auto = true;
    const described = await adapterFor(playerWindow(player)).describe();
    assert.deepEqual(described?.quality.map((option) => option.selected), [true, false, false, false]);
  });

  it("плеер на 24-м узле цепочки найден, на 40-м найден, на 41-м — нет (меню нет: null)", async () => {
    assert.notEqual(await adapterFor(playerWindow(new FakeTwitchPlayer(), 24)).describe(), null);
    assert.notEqual(await adapterFor(playerWindow(new FakeTwitchPlayer(), 40)).describe(), null);
    assert.equal(await adapterFor(playerWindow(new FakeTwitchPlayer(), 41)).describe(), null);
  });

  it("нет ключа fiber, нет элемента, нет документа — null, а не срыв", async () => {
    assert.equal(await adapterFor(twitchWindow({})).describe(), null);
    assert.equal(await adapterFor(twitchWindow({ fiber: fiberChain(3, new FakeTwitchPlayer()), fiberKey: "__reactProps$abc" })).describe(), null);
    assert.equal(await adapterFor(twitchWindow({ noElement: true })).describe(), null);
    assert.equal(await adapterFor({}).describe(), null);
  });

  it("бросающий геттер в memoizedProps узла не роняет поиск: идём дальше и находим плеер выше", async () => {
    const player = new FakeTwitchPlayer();
    const chain = fiberChain(5, player, true);
    assert.notEqual(await adapterFor(twitchWindow({ fiber: chain })).describe(), null);
  });

  it("бросающий геттер `return` обрывает подъём: плеера выше не достать, но и срыва нет — null", async () => {
    const trapped = { memoizedProps: {} } as Record<string, unknown>;
    Object.defineProperty(trapped, "return", { enumerable: true, get: () => { throw new Error("trap"); } });
    assert.equal(await adapterFor(twitchWindow({ fiber: trapped })).describe(), null);
  });

  it("узел без функции getQualities — не плеер", async () => {
    const impostor = { mediaPlayerInstance: { getQuality: () => ({}) } };
    assert.equal(await adapterFor(twitchWindow({ fiber: { memoizedProps: impostor, return: undefined } })).describe(), null);
  });

  it("getQualities бросает или отвечает не списком — описание срывается целиком (полменю не бывает)", async () => {
    const crashing = new FakeTwitchPlayer();
    crashing.failQualities = true;
    await assert.rejects(adapterFor(playerWindow(crashing)).describe(), TwitchPageError);
    const garbage = new FakeTwitchPlayer();
    garbage.list = "oops";
    await assert.rejects(adapterFor(playerWindow(garbage)).describe(), TwitchPageError);
  });

  it("группа с пробелом, кириллицей, слишком длинная, пустая и чужой «auto» пропускаются; мусорные записи тоже", async () => {
    const player = new FakeTwitchPlayer();
    player.list = [
      { name: "bad space", group: "720p 60" },
      { name: "кириллица", group: "высокое" },
      { name: "long", group: "a".repeat(65) },
      { name: "empty", group: "" },
      { name: "fake auto", group: "auto" },
      { name: "no group" },
      { group: "no_name" },
      42,
      null,
      { name: "good", group: "good_1-x" },
    ];
    const described = await adapterFor(playerWindow(player)).describe();
    assert.deepEqual(described?.quality.map((option) => option.id), ["auto", "good_1-x"]);
  });

  it("логин не строка или пустой — episodeKey null; описание при этом есть", async () => {
    for (const login of [undefined, 7, ""]) {
      const player = new FakeTwitchPlayer();
      player.login = login;
      assert.equal((await adapterFor(playerWindow(player)).describe())?.episodeKey, null);
    }
  });
});

describe("адаптер Twitch: действия", () => {
  it("качество: плееру уходит ОБЪЕКТ из getQualities(), а не строка; «Auto» включает автоматику", async () => {
    const player = new FakeTwitchPlayer();
    player.auto = true;
    const adapter = adapterFor(playerWindow(player));
    assert.equal(await adapter.select("quality", "480p30"), "done");
    assert.equal(player.received, QUALITIES[2]);
    assert.equal(player.auto, false);
    assert.equal(await adapter.select("quality", "auto"), "done");
    assert.equal(player.auto, true);
    assert.ok(player.calls.includes("setAuto:true"));
  });

  it("такого качества нет — no_such_variant, плеер не тронут; id с мусором плеера не касается вообще", async () => {
    const player = new FakeTwitchPlayer();
    const adapter = adapterFor(playerWindow(player));
    assert.equal(await adapter.select("quality", "1440p"), "no_such_variant");
    for (const junk of ["720p 60", "высокое", "", "a".repeat(65), '"], x']) assert.equal(await adapter.select("quality", junk), "no_such_variant");
    assert.equal(player.received, undefined);
    assert.deepEqual(player.calls, ["getQualities"], "только поиск по списку, ни одного setQuality");
  });

  it("озвучки нет — no_such_variant; следующей серии нет — no_such_variant без обращения к странице", async () => {
    const player = new FakeTwitchPlayer();
    const adapter = adapterFor(playerWindow(player));
    assert.equal(await adapter.select("audio", "720p60"), "no_such_variant");
    assert.equal(await adapter.next(), "no_such_variant");
    assert.deepEqual(player.calls, []);
  });

  it("плеера нет или getQualities бросила — failed", async () => {
    assert.equal(await adapterFor(twitchWindow({})).select("quality", "720p60"), "failed");
    const crashing = new FakeTwitchPlayer();
    crashing.failQualities = true;
    assert.equal(await adapterFor(playerWindow(crashing)).select("quality", "720p60"), "failed");
  });
});
