// Решение режима 2 по задержанному видео (12a): таблица правил, срок кредита, проба рекламы.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MAX_PLAY_CREDIT_LEASE_MS } from "../../src/shared/auto-handoff-contract.ts";
import { AD_PROBE_LEASE_MS, AD_PROBE_WINDOW_MS, createAdProbe } from "../../src/background/auto-handoff/ad-probe.ts";
import type { SitePolicy } from "../../src/background/auto-handoff/site-policy.ts";
import { PLAY_CREDIT_GRACE_MS, candidatesAsFiltersSee, decideViewerPlay, playCreditLeaseMs, type AdProbeVerdict, type VideoFilters, type ViewerPlayDecision } from "../../src/background/auto-handoff/viewer-play-decision.ts";
import { classifySurvey, type ClassifiedVideo } from "../../src/background/handoff/video-choice.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import type { PageAdState } from "../../src/protocol/index.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock } from "./support.ts";

const FILM: VideoCandidate = { document: "doc-7f3a", element: 4, media: { kind: "media_source", in_worker: false }, playing: true, visible_area_px: 200_000, ...DEFAULT_CANDIDATE_FACTS };

/** Классифицировать одно видео тем же классификатором, что и фон. */
function held(overrides: Partial<VideoCandidate> = {}, neighbours: readonly VideoCandidate[] = []): ClassifiedVideo {
  const candidate = { ...FILM, ...overrides };
  const entry = classifySurvey([{ frameId: 0, candidates: [candidate, ...neighbours] }]).find((known) => known.candidate === candidate);
  assert.ok(entry !== undefined);
  return entry;
}

const HAND: SitePolicy = { kind: "hand_off" };
/** Фильтры по умолчанию: порог 60 с, оба фильтра включены. */
const FILTERS: VideoFilters = { minDurationS: 60, requireMinDuration: true, requireSound: true };
const decide = (video: ClassifiedVideo | undefined, adProbe: AdProbeVerdict = "not_probed", policy: SitePolicy = HAND, minDurationS = 60): ViewerPlayDecision =>
  decideViewerPlay({ policy, held: video, filters: { ...FILTERS, minDurationS }, adProbe });
/**
 * Решение с выбранными фильтрами (решение 260) — тем же путём, что у фона: кандидат сначала проходит
 * `candidatesAsFiltersSee`, потом общий классификатор, потом правила.
 */
function decideWith(overrides: Partial<VideoCandidate>, filterOverrides: Partial<VideoFilters>, adProbe: AdProbeVerdict = "not_probed", policy: SitePolicy = HAND): ViewerPlayDecision {
  const filters = { ...FILTERS, ...filterOverrides };
  const candidate = { ...FILM, ...overrides };
  const [seen] = candidatesAsFiltersSee([candidate], candidate.element, filters);
  assert.ok(seen !== undefined);
  const video = classifySurvey([{ frameId: 0, candidates: [seen] }])[0];
  return decideViewerPlay({ policy, held: video, filters, adProbe });
}

/** Короткий файл-ролик (20 с), не опознанный как реклама. */
const SHORT_FILE: Partial<VideoCandidate> = { media: { kind: "file" }, duration_s: 20, file_host: "site.example" };

describe("решение по задержанному видео", () => {
  it("политика пропустила или видео нет — release", () => {
    assert.deepEqual(decide(held(), "not_probed", { kind: "skip", reason: "mode_off" }), { kind: "release", reason: "site_skipped" });
    assert.deepEqual(decide(undefined), { kind: "release", reason: "no_held_video" });
  });

  it("DRM, неподдержанная форма, пустое и мусор не передаются", () => {
    for (const overrides of [{ drm: true }, { media: { kind: "unsupported", form: "blob_file" } }, { media: { kind: "none" } }] as const) {
      assert.deepEqual(decide(held(overrides)), { kind: "release", reason: "not_a_film" }, JSON.stringify(overrides));
    }
    // Беззвучная плитка-цикл — мусор по общему классификатору, и она же без звука.
    assert.equal(decide(held({ muted: true, loop: true, visible_area_px: 500 })).kind, "release");
  });

  it("без звука — release, даже длинный фильм (страница могла соврать сторожу)", () => {
    assert.deepEqual(decide(held({ muted: true })), { kind: "release", reason: "muted" });
  });

  it("не виден (площадь 0) — release", () => {
    assert.deepEqual(decide(held({ visible_area_px: 0 })), { kind: "release", reason: "hidden" });
  });

  it("длительность неизвестна (эфир) — передаём сразу, даже при огромном пороге", () => {
    assert.deepEqual(decide(held({ duration_s: null }), "not_probed", HAND, 3_600), { kind: "hand_off", reason: "duration_unknown" });
  });

  it("порог: ровно порог и длиннее — передаём, на секунду короче — нет; порог 0 передаёт всё", () => {
    assert.deepEqual(decide(held({ duration_s: 60 })), { kind: "hand_off", reason: "long_enough" });
    assert.equal(decide(held({ ...SHORT_FILE, duration_s: 59 })).kind, "probe_ads_first");
    assert.deepEqual(decide(held({ ...SHORT_FILE, duration_s: 3 }), "not_probed", HAND, 0), { kind: "hand_off", reason: "long_enough" });
  });

  it("короткий ролик с вердиктом ad передаётся сразу, без пробы", () => {
    const ad = held({ ...SHORT_FILE, file_host: "doubleclick.net" });
    assert.equal(ad.verdict, "ad");
    assert.deepEqual(decide(ad), { kind: "hand_off", reason: "known_ad" });
  });

  it("короткий ролик без вердикта: проба; реклама на экране — передаём; рекламы нет — отпускаем с кредитом", () => {
    const clip = held(SHORT_FILE);
    assert.equal(clip.verdict, "main");
    assert.deepEqual(decide(clip, "not_probed"), { kind: "probe_ads_first" });
    assert.deepEqual(decide(clip, "ad_on_screen"), { kind: "hand_off", reason: "ad_on_screen" });
    assert.deepEqual(decide(clip, "no_ad"), { kind: "release_with_credit", lease_ms: 20_000 + PLAY_CREDIT_GRACE_MS });
  });

  it("сайт ask: то, что ушло бы в плеер, становится вопросом с той же причиной; отпускаемое не спрашивается", () => {
    const ASK: SitePolicy = { kind: "ask" };
    assert.deepEqual(decide(held({ duration_s: 600 }), "not_probed", ASK), { kind: "ask_viewer", reason: "long_enough" });
    assert.deepEqual(decide(held({ duration_s: null }), "not_probed", ASK), { kind: "ask_viewer", reason: "duration_unknown" });
    assert.deepEqual(decide(held({ ...SHORT_FILE, file_host: "doubleclick.net" }), "not_probed", ASK), { kind: "ask_viewer", reason: "known_ad" });
    // Без звука и короткий ролик без рекламы — как на любом сайте: вопроса нет.
    assert.deepEqual(decide(held({ muted: true }), "not_probed", ASK), { kind: "release", reason: "muted" });
    assert.deepEqual(decide(held(SHORT_FILE), "not_probed", ASK), { kind: "probe_ads_first" });
    assert.equal(decide(held(SHORT_FILE), "no_ad", ASK).kind, "release_with_credit");
    assert.deepEqual(decide(held(SHORT_FILE), "ad_on_screen", ASK), { kind: "ask_viewer", reason: "ad_on_screen" });
  });

  it("фильтр звука снят (решение 260): видео без звука судится как звучащее", () => {
    assert.deepEqual(decideWith({ muted: true }, { requireSound: false }), { kind: "hand_off", reason: "long_enough" });
    // Беззвучный цикл и мелкая плитка, запущенные человеком, — тоже не «украшение».
    assert.deepEqual(decideWith({ muted: true, loop: true }, { requireSound: false }), { kind: "hand_off", reason: "long_enough" });
    // Остальные правила стоят: короткий беззвучный ролик идёт веткой рекламы, невидимый — отпускается.
    assert.deepEqual(decideWith({ ...SHORT_FILE, muted: true }, { requireSound: false }), { kind: "probe_ads_first" });
    assert.deepEqual(decideWith({ muted: true, visible_area_px: 0 }, { requireSound: false }), { kind: "release", reason: "hidden" });
  });

  it("фильтр звука включён: тот же беззвучный ролик отсеивается (классификатором или правилом звука)", () => {
    assert.deepEqual(decideWith({ muted: true }, {}), { kind: "release", reason: "muted" });
    assert.deepEqual(decideWith({ ...SHORT_FILE, muted: true }, { requireMinDuration: false }), { kind: "release", reason: "not_a_film" });
  });

  it("фильтр длины снят (решение 260): короткий ролик уходит сразу, без пробы рекламы и кредита", () => {
    assert.deepEqual(decideWith(SHORT_FILE, { requireMinDuration: false }), { kind: "hand_off", reason: "length_filter_off" });
    assert.deepEqual(decideWith({ ...SHORT_FILE, duration_s: 1 }, { requireMinDuration: false, minDurationS: 3_600 }), { kind: "hand_off", reason: "length_filter_off" });
    // Длина неизвестна (эфир) — по-прежнему своя причина.
    assert.deepEqual(decideWith({ duration_s: null }, { requireMinDuration: false }), { kind: "hand_off", reason: "duration_unknown" });
  });

  it("оба фильтра сняты: короткий беззвучный ролик уходит в плеер; на сайте ask — вопрос", () => {
    const clip = { ...SHORT_FILE, muted: true };
    const off = { requireMinDuration: false, requireSound: false };
    assert.deepEqual(decideWith(clip, off), { kind: "hand_off", reason: "length_filter_off" });
    assert.deepEqual(decideWith(clip, off, "not_probed", { kind: "ask" }), { kind: "ask_viewer", reason: "length_filter_off" });
    // DRM и прочий «не фильм» фильтры не отменяют.
    assert.deepEqual(decideWith({ ...clip, drm: true }, off), { kind: "release", reason: "not_a_film" });
  });

  it("candidatesAsFiltersSee: фильтр звука включён — те же объекты; снят — меняется только задержанный кандидат", () => {
    const heldOne = { ...FILM, muted: true };
    const neighbour = { ...FILM, element: 9, muted: true };
    const list = [heldOne, neighbour];
    assert.equal(candidatesAsFiltersSee(list, heldOne.element, FILTERS), list);
    const seen = candidatesAsFiltersSee(list, heldOne.element, { ...FILTERS, requireSound: false });
    assert.equal(seen[0]?.muted, false);
    assert.equal(seen[1], neighbour, "сосед не тронут");
    assert.equal(heldOne.muted, true, "донесение не изменено");
  });

  it("срок кредита: длина ролика + 10 с запаса, но не больше предела договора", () => {
    assert.equal(playCreditLeaseMs(20), 30_000);
    assert.equal(playCreditLeaseMs(20.2), 30_200);
    assert.equal(playCreditLeaseMs(10_000), MAX_PLAY_CREDIT_LEASE_MS);
  });
});

describe("проба рекламы", () => {
  function setup() {
    const clock = createFakeClock();
    const armings: { tabId: number; leaseMs: number }[] = [];
    const probe = createAdProbe({ scheduler: clock.scheduler, armAdWatch: (tabId, leaseMs) => void armings.push({ tabId, leaseMs }) });
    return { clock, armings, probe };
  }
  const SHOWING: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "unavailable" } };

  it("донесение «реклама показана» от фрейма вкладки — ad_on_screen сразу, окно закрывается", async () => {
    const { clock, armings, probe } = setup();
    const result = probe.probe(7);
    assert.deepEqual(armings, [{ tabId: 7, leaseMs: AD_PROBE_LEASE_MS }]);
    probe.acceptReport(7, 2, SHOWING);
    assert.equal(await result, "ad_on_screen");
    assert.equal(clock.liveTimers(), 0);
  });

  it("тишина до конца окна — no_ad; донесение «none» рекламой не считается", async () => {
    const { clock, probe } = setup();
    const result = probe.probe(7);
    probe.acceptReport(7, 0, { kind: "none" });
    clock.advance(AD_PROBE_WINDOW_MS);
    assert.equal(await result, "no_ad");
  });

  it("донесение чужой вкладки или после окна пробу не трогает", async () => {
    const { clock, probe } = setup();
    const result = probe.probe(7);
    probe.acceptReport(8, 0, SHOWING);
    clock.advance(AD_PROBE_WINDOW_MS);
    assert.equal(await result, "no_ad");
    probe.acceptReport(7, 0, SHOWING);
    const next = probe.probe(7);
    clock.advance(AD_PROBE_WINDOW_MS);
    assert.equal(await next, "no_ad");
  });

  it("две пробы одной вкладки делят окно: аренда одна, ответ один", async () => {
    const { armings, probe, clock } = setup();
    const first = probe.probe(7);
    const second = probe.probe(7);
    assert.equal(armings.length, 1);
    clock.advance(AD_PROBE_WINDOW_MS);
    assert.deepEqual(await Promise.all([first, second]), ["no_ad", "no_ad"]);
  });
});
