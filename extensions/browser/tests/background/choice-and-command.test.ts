// Выбор видео (пункт 9 09a), опрос фреймов и приём команды «отдать вкладку N» (решение 175).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { describeFrameVideos, surveyTabVideos, type ExecuteInAllFrames, type FrameInjectionResult } from "../../src/background/handoff/frame-survey.ts";
import { createHandOffCommandListener, createHeldVideoListener, isOwnExtensionPage, type FrameSender } from "../../src/background/handoff/hand-off-command.ts";
import { pageIdentityOf, siteDomainOf, UNKNOWN_SITE_DOMAIN } from "../../src/background/handoff/page-identity.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import { chooseMainVideo, chooseVideo, classifySurvey } from "../../src/background/handoff/video-choice.ts";
import { VIDEO_FILTER_RULES } from "../../src/background/tab-model/video-filter-rules.ts";
import { HELD_VIDEO_REPORT_KIND, type HandOffTabOutcome, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME } from "../../src/shared/video-candidates-probe.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock, flushPromises } from "./support.ts";

function video(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { document: "doc-top", element, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 1000, ...DEFAULT_CANDIDATE_FACTS, ...overrides };
}

/** Итог «передать этот элемент». */
function handOff(frameId: number, document: string, element: number) {
  return { kind: "hand_off", video: { frameId, document, element, frameHost: DEFAULT_CANDIDATE_FACTS.frame_host, fileHost: DEFAULT_CANDIDATE_FACTS.file_host, role: "film", mayBeAd: false } };
}

/** Сеанс начинается на рекламе (решение 201): рекламный файл короткий, поэтому и `mayBeAd`. */
function handedAd(frameId: number, element: number, fileHost: string) {
  return { kind: "hand_off", video: { frameId, document: "doc-top", element, frameHost: DEFAULT_CANDIDATE_FACTS.frame_host, fileHost, role: "ad", mayBeAd: true } };
}

describe("выбор видео", () => {
  it("играющий элемент с живым MSE побеждает больший неиграющий, из любого фрейма", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { visible_area_px: 900_000 })] },
      { frameId: 7, candidates: [video(2, { document: "doc-iframe", playing: true, visible_area_px: 50_000 })] },
    ]);
    assert.deepEqual(choice, handOff(7, "doc-iframe", 2), "метка документа выбранного элемента запомнена");
  });

  it("никто не играет — самый большой видимый живой", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { visible_area_px: 100 }), video(2, { visible_area_px: 5000 })] },
      { frameId: 4, candidates: [video(3, { visible_area_px: 4000 })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("элемент без источника не выбирается, даже играющий", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { media: { kind: "none" }, playing: true }), video(2)] }]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("файл `<video src>` (09b) выбирается, когда живого MSE нет", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { media: { kind: "none" }, playing: true }), video(2, { media: { kind: "file" } })] }]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("решение 8: играющий файл побеждает видимый, но стоящий MSE (находка 9 ревью 09b)", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { visible_area_px: 900_000 }), video(2, { media: { kind: "file" }, playing: true, visible_area_px: 50_000 })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("никто не играет — большой файл выше мелкого MSE: площадь важнее формы (решение 8)", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { media: { kind: "file" }, visible_area_px: 900_000 }), video(2, { visible_area_px: 10 })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 1));
  });

  it("играют оба: беззвучный тизер MSE 300×170 без цикла не обгоняет большой фильм-файл", () => {
    const teaser = video(1, { playing: true, muted: true, loop: false, visible_area_px: 300 * 170 });
    const film = video(2, { playing: true, media: { kind: "file" }, visible_area_px: 1280 * 720, duration_s: 5400 });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [teaser, film] }]), handOff(0, "doc-top", 2));
  });

  it("при равной площади длиннее побеждает, даже если он файл", () => {
    const shorter = video(1, { playing: true, duration_s: 60 });
    const longer = video(2, { playing: true, media: { kind: "file" }, duration_s: 5400 });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [shorter, longer] }]), handOff(0, "doc-top", 2));
  });

  it("площадь и длительность равны — форма разрешает равенство: MSE выше файла", () => {
    const file = video(1, { playing: true, media: { kind: "file" }, duration_s: 5400 });
    const mse = video(2, { playing: true, duration_s: 5400 });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [file, mse] }]), handOff(0, "doc-top", 2));
  });

  it("MSE не играет и не виден — ярус пуст, решает файл", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { visible_area_px: 0 }), video(2, { media: { kind: "file" }, visible_area_px: 10 })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("передать нечего, но играет нативный HLS — честное «not supported yet» с формой (решение 185)", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { media: { kind: "unsupported", form: "native_hls" }, playing: true })] },
      { frameId: 2, candidates: [video(2, { media: { kind: "unsupported", form: "media_stream" }, visible_area_px: 5 })] },
    ]);
    assert.deepEqual(choice, { kind: "not_supported_yet", form: "native_hls" });
  });

  it("неподдержанная форма проигрывает любому передаваемому видео", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { media: { kind: "unsupported", form: "native_hls" }, playing: true }), video(2, { media: { kind: "file" } })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("живые, но не играют и не видны — передавать нечего", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [video(1, { visible_area_px: 0 })] }]), { kind: "nothing" });
    assert.deepEqual(
      chooseVideo([{ frameId: 0, candidates: [video(1, { media: { kind: "unsupported", form: "data_url" }, visible_area_px: 0 })] }]),
      { kind: "nothing" },
      "неподдержанная форма, которую не видно и которая не играет, — не повод для «not supported yet»",
    );
    assert.deepEqual(chooseVideo([]), { kind: "nothing" });
  });

  it("при равенстве — верхний фрейм и порядок документа, независимо от порядка ответов", () => {
    const choice = chooseVideo([
      { frameId: 9, candidates: [video(5, { playing: true })] },
      { frameId: 0, candidates: [video(1, { playing: true }), video(2, { playing: true })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 1));
  });
});

describe("выбор видео: реклама, мусор, DRM, скрытые игроки (09c)", () => {
  const AD_FILE = { media: { kind: "file" } as const, file_host: "securepubads.g.doubleclick.net", duration_s: 15 };

  it("реклама на месте плеера уходит первой, но главным видео вкладки остаётся фильм (решение 201)", () => {
    const frames = [{ frameId: 0, candidates: [video(1, { visible_area_px: 400_000 }), video(2, { ...AD_FILE, playing: true, visible_area_px: 400_000 })] }];
    assert.deepEqual(chooseVideo(frames), handedAd(0, 2, AD_FILE.file_host));
    assert.deepEqual(chooseMainVideo(frames), handOff(0, "doc-top", 1));
  });

  it("реклама из iframe рекламного домена уходит первой и в другом фрейме, даже при MSE", () => {
    const frames = [
      { frameId: 0, candidates: [video(1, { media: { kind: "file" }, visible_area_px: 400_000 })] },
      { frameId: 3, candidates: [video(2, { document: "doc-ima", playing: true, frame_host: "imasdk.googleapis.com", visible_area_px: 400_000 })] },
    ];
    const choice = chooseVideo(frames);
    assert.equal(choice.kind === "hand_off" ? choice.video.frameId : null, 3);
    assert.equal(choice.kind === "hand_off" ? choice.video.role : null, "ad");
  });

  it("играющий фильм важнее рекламы рядом: это не преролл", () => {
    const frames = [{ frameId: 0, candidates: [video(1, { playing: true, visible_area_px: 400_000 }), video(2, { ...AD_FILE, playing: true, visible_area_px: 400_000 })] }];
    assert.deepEqual(chooseVideo(frames), handOff(0, "doc-top", 1));
  });

  it("одинокий беззвучный рекламный ролик на странице без плеера — баннер, передавать нечего", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [video(1, { ...AD_FILE, playing: true, muted: true, visible_area_px: 90_000 })] }]), { kind: "nothing" });
  });

  it("беззвучный рекламный баннер меньше половины фильма не считается рекламой на месте плеера", () => {
    const banner = video(2, { ...AD_FILE, playing: true, muted: true, visible_area_px: 90_000 });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [video(1, { visible_area_px: 400_000 }), banner] }]), handOff(0, "doc-top", 1));
  });

  it("скрытый играющий не побеждает видимый на паузе (Ashdi, решение 68)", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { playing: true, visible_area_px: 0 }), video(2, { visible_area_px: 300_000 })] },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("если видимых нет, скрытый играющий всё же выбирается", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { visible_area_px: 0 }), video(2, { playing: true, visible_area_px: 0 })] }]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("играющий и видимый побеждает видимый на паузе", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { visible_area_px: 900_000 }), video(2, { playing: true, visible_area_px: 1000 })] }]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("беззвучный баннер-автоплей и превью по наведению не главные (banner-autoplay)", () => {
    const choice = chooseVideo([
      {
        frameId: 0,
        candidates: [
          video(1, { playing: true, muted: true, loop: true, visible_area_px: 300 * 250, duration_s: 12 }),
          video(2, { playing: true, muted: true, visible_area_px: 120 * 68, duration_s: null }),
          video(3, { visible_area_px: 640_000 }),
        ],
      },
    ]);
    assert.deepEqual(choice, handOff(0, "doc-top", 3));
  });

  it("при равной площади выигрывает более длинный ролик, неизвестная длительность — последней", () => {
    const longer = chooseVideo([{ frameId: 0, candidates: [video(1, { duration_s: 600 }), video(2, { duration_s: 5400 }), video(3, { duration_s: null })] }]);
    assert.deepEqual(longer, handOff(0, "doc-top", 2));
    const unknownLast = chooseVideo([{ frameId: 0, candidates: [video(1, { duration_s: null }), video(2, { duration_s: 90 })] }]);
    assert.deepEqual(unknownLast, handOff(0, "doc-top", 2));
  });

  it("DRM-видео: единственное играющее — исход protected, а не передача", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { drm: true, playing: true })] }]);
    assert.deepEqual(choice, { kind: "protected" });
  });

  it("DRM-видео не мешает передать другое обычное видео страницы", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { drm: true, playing: true, visible_area_px: 900_000 }), video(2, { visible_area_px: 100 })] }]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });

  it("только играющая реклама, фильма нет — передача рекламы (решение 201); стоящая реклама — nothing", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [video(1, { ...AD_FILE, playing: true })] }]), handedAd(0, 1, AD_FILE.file_host));
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [video(1, { ...AD_FILE, playing: false })] }]), { kind: "nothing" });
  });

  it("при играющем защищённом видео реклама рядом не передаётся, неподдержанная форма не мешает рекламе", () => {
    const unsupported = video(3, { media: { kind: "unsupported", form: "native_hls" }, playing: true });
    const ad = video(2, { ...AD_FILE, playing: true });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [unsupported, ad, video(1, { drm: true, playing: true })] }]), { kind: "protected" });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [unsupported, ad] }]), handedAd(0, 2, AD_FILE.file_host));
  });
});

describe("выбор видео: преролл с неизвестного домена (живые формы Ashdi и Tortuga)", () => {
  // Числа и домен взяты из живой проверки: домена нет в списке, опознать его можно только по странице.
  const PLAYER_AREA = 296_000;
  const UNKNOWN_AD_HOST = "video.unknown-ad-cdn.example";
  /** Итог «файл не реклама, передаётся он». */
  const keptFile = { kind: "hand_off", video: { frameId: 0, document: "doc-top", element: 2, frameHost: DEFAULT_CANDIDATE_FACTS.frame_host, fileHost: UNKNOWN_AD_HOST, role: "film", mayBeAd: true } };
  /** Итог «реклама на месте плеера»: сеанс начинается на ней. */
  const keptAd = { kind: "hand_off", video: { ...keptFile.video, role: "ad", mayBeAd: true } };
  const preroll = (element: number, overrides: Partial<VideoCandidate> = {}) =>
    video(element, { media: { kind: "file" }, file_host: UNKNOWN_AD_HOST, playing: true, visible_area_px: PLAYER_AREA, duration_s: 24, ...overrides });
  const waitingFilm = (element: number, overrides: Partial<VideoCandidate> = {}) => video(element, { visible_area_px: PLAYER_AREA, duration_s: 6381, ...overrides });
  const emptySlot = (element: number, overrides: Partial<VideoCandidate> = {}) => video(element, { media: { kind: "none" }, visible_area_px: PLAYER_AREA, duration_s: null, ...overrides });

  it("Ashdi: играет реклама-файл, рядом ждёт загруженный фильм — сеанс начинается на рекламе (решение 201)", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1), preroll(2)] }]), keptAd);
  });

  it("Tortuga: играет реклама-файл, рядом пустой элемент той же площади — тоже передаётся реклама", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [emptySlot(1), preroll(2)] }]), keptAd);
  });

  it("незапущенный адрес плейлиста той же площади — тоже пустой плеер: передаётся реклама", () => {
    const playlistSlot = emptySlot(1, { media: { kind: "hls" } });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [playlistSlot, preroll(2)] }]), keptAd);
  });

  it("играющий нативный HLS рядом с файлом — не пустой плеер, рекламы по соседству не видно", () => {
    const playingPlaylist = emptySlot(1, { media: { kind: "hls" }, playing: true, duration_s: null });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [playingPlaylist, preroll(2)] }]), keptFile);
  });

  it("MediaSource без длительности тоже ждёт фильм", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1, { duration_s: null }), preroll(2)] }]), keptAd);
  });

  it("беззвучный преролл на месте плеера — всё равно реклама, а не баннер: передаётся", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [emptySlot(1), preroll(2, { muted: true })] }]), keptAd);
  });

  it("длинный файл рядом с ожидающим MSE остаётся видео (порог преролла, решение 8)", () => {
    const longFile = preroll(2, { duration_s: 181 });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1), longFile] }]), { kind: "hand_off", video: { ...keptFile.video, mayBeAd: false } });
  });

  it("ролик ровно на пороге ещё преролл, порог берётся из правил, а не из кода", () => {
    const atLimit = preroll(2, { duration_s: 180 });
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1), atLimit] }]), keptAd);
    const strict = { ...VIDEO_FILTER_RULES, prerollMaxSeconds: 20 };
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1), preroll(2)] }], strict), { kind: "hand_off", video: { ...keptFile.video, mayBeAd: false } });
  });

  it("файл с неизвестной длительностью не считается прероллом: рано судить", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1), preroll(2, { duration_s: null })] }]), { kind: "hand_off", video: { ...keptFile.video, mayBeAd: false } });
  });

  it("сосед MSE короче играющего файла — не фильм, файл остаётся видео", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [waitingFilm(1, { duration_s: 10 }), preroll(2)] }]), keptFile);
  });

  it("сосед тоже играет — он ничего не ждёт", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [waitingFilm(1, { playing: true }), preroll(2, { visible_area_px: PLAYER_AREA + 1 })] }]);
    assert.deepEqual(choice, keptFile);
  });

  it("пустой элемент другой площади — чужая плитка ленты, файл остаётся видео", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [emptySlot(1, { visible_area_px: PLAYER_AREA - 1 }), preroll(2)] }]), keptFile);
  });

  it("скрытые нулевой площади не считаются одним плеером", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [emptySlot(1, { visible_area_px: 0 }), preroll(2, { visible_area_px: 0 })] }]);
    assert.deepEqual(choice, keptFile);
  });

  it("зацикленный файл — не реклама: это баннер, рядом с плиткой той же площади", () => {
    assert.deepEqual(chooseVideo([{ frameId: 0, candidates: [emptySlot(1), preroll(2, { loop: true })] }]), keptFile);
  });

  it("файл на паузе рядом с фильмом — не преролл: реклама сейчас не идёт", () => {
    const classified = classifySurvey([{ frameId: 0, candidates: [waitingFilm(1), preroll(2, { playing: false, duration_s: 60 })] }]);
    assert.deepEqual(classified.map((entry) => entry.verdict), ["main", "main"]);
  });

  it("сосед из другого документа того же фрейма не считается: старый документ ещё в опросе", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [waitingFilm(1, { document: "doc-old" }), preroll(2)] }]);
    assert.deepEqual(choice, keptFile);
  });

  it("сосед из другого фрейма не считается: фильм и преролл живут в одном плеере", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [preroll(2)] },
      { frameId: 7, candidates: [emptySlot(1, { document: "doc-iframe" })] },
    ]);
    assert.deepEqual(choice, keptFile);
  });

  it("MSE-реклама поверх MSE-фильма правилом не трогается: прероллы бывают только файлом", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [waitingFilm(1), video(2, { playing: true, visible_area_px: PLAYER_AREA, duration_s: 24 })] }]);
    assert.deepEqual(choice, handOff(0, "doc-top", 2));
  });
});

describe("выбор видео: адрес плейлиста в `src` до старта (живая форма Ashdi)", () => {
  const playlist = { media: { kind: "hls" } as const };

  it("до нажатия play это не «не поддерживается», а «видео ещё нет»", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { ...playlist, duration_s: null })] }]);
    assert.deepEqual(choice, { kind: "nothing" });
  });

  it("играющий нативный HLS — честный not_supported_yet", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { ...playlist, playing: true, duration_s: null })] }]);
    assert.deepEqual(choice, { kind: "not_supported_yet", form: "native_hls" });
  });

  it("нативный HLS на паузе с известной длительностью — запись: передаётся как видео", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { ...playlist, duration_s: 60 })] }]);
    assert.equal(choice.kind, "hand_off");
  });

  it("другие неподдержанные формы до старта остаются неподдержанными", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { media: { kind: "unsupported", form: "media_stream" }, duration_s: null })] }]);
    assert.deepEqual(choice, { kind: "not_supported_yet", form: "media_stream" });
  });
});

/** Версия фона в этих тестах и ответ фрейма с нашим скриптом этой версии. */
const SCRIPT_VERSION = "1.2.3";
const scripted = (candidates: unknown): unknown => ({ script_version: SCRIPT_VERSION, candidates });
const survey = (execute: ExecuteInAllFrames) => surveyTabVideos(5, execute, createFakeClock().scheduler, SCRIPT_VERSION);

/** Подменить `location` внедряемой функции: в Node его нет. */
function withLocation<Result>(protocol: string, body: () => Result): Result {
  const global = globalThis as Record<string, unknown>;
  global["location"] = { protocol };
  try {
    return body();
  } finally {
    delete global["location"];
  }
}

describe("опрос фреймов", () => {
  it("внедряемая функция зовёт функцию изолированного глобала и возвращает её результат", async () => {
    const answer = scripted([video(1)]);
    const global = globalThis as Record<string, unknown>;
    global[VIDEO_CANDIDATES_FUNCTION_NAME] = () => Promise.resolve(answer);
    try {
      assert.deepEqual(await describeFrameVideos(VIDEO_CANDIDATES_FUNCTION_NAME), answer);
    } finally {
      delete global[VIDEO_CANDIDATES_FUNCTION_NAME];
    }
  });

  it("фрейм без нашего скрипта говорит об этом явно: на веб-странице и на служебном документе по-разному", () => {
    assert.deepEqual(withLocation("https:", () => describeFrameVideos(VIDEO_CANDIDATES_FUNCTION_NAME)), { script_absent: true, on_web_page: true });
    assert.deepEqual(withLocation("about:", () => describeFrameVideos(VIDEO_CANDIDATES_FUNCTION_NAME)), { script_absent: true, on_web_page: false });
  });

  it("внедряемая функция самодостаточна: её текст работает без замыканий модуля", () => {
    const rebuilt = new Function(`return (${describeFrameVideos.toString()})`)() as (name: string) => unknown;
    const global = globalThis as Record<string, unknown>;
    global["probeName"] = () => "ok";
    try {
      assert.equal(rebuilt("probeName"), "ok");
      assert.deepEqual(withLocation("http:", () => rebuilt("noSuchProbe")), { script_absent: true, on_web_page: true });
    } finally {
      delete global["probeName"];
    }
  });

  it("собирает договорные ответы фреймов, кривые и пустые отбрасывает", async () => {
    const execute: ExecuteInAllFrames = (tabId, func, args) => {
      assert.equal(tabId, 5);
      assert.equal(func, describeFrameVideos);
      assert.deepEqual(args, [VIDEO_CANDIDATES_FUNCTION_NAME]);
      return Promise.resolve([
        { frameId: 0, result: scripted([video(1)]) },
        { frameId: 1, result: null },
        { frameId: 2, result: scripted([{ ...video(2), extra: true }]) },
        { frameId: 3, result: scripted([]) },
        { frameId: 4 },
      ]);
    };
    assert.deepEqual(await survey(execute), { kind: "surveyed", frames: [{ frameId: 0, candidates: [video(1)] }], truncated: false, scriptGap: null });
  });

  it("Firefox вернул пустой элемент вместо фрейма: остальные фреймы вкладки опрошены", async () => {
    const execute: ExecuteInAllFrames = () => Promise.resolve([{ frameId: 0, result: scripted([video(1)]) }, undefined]);
    assert.deepEqual(await survey(execute), { kind: "surveyed", frames: [{ frameId: 0, candidates: [video(1)] }], truncated: false, scriptGap: null });
  });

  it("ни один фрейм не принял внедрение (страница Firefox): вкладка закрыта, а не сбой", async () => {
    const execute: ExecuteInAllFrames = () => Promise.resolve([undefined]);
    assert.equal((await survey(execute)).kind, "not_scriptable");
  });

  it("пустой ответ браузера без фреймов — вкладка без видео, а не закрытая", async () => {
    const execute: ExecuteInAllFrames = () => Promise.resolve([]);
    assert.deepEqual(await survey(execute), { kind: "surveyed", frames: [], truncated: false, scriptGap: null });
  });
});

describe("опрос: вкладка старше скрипта (сессия 10d)", () => {
  const gapOf = async (results: readonly FrameInjectionResult[]) => {
    const reply = await survey(() => Promise.resolve(results));
    assert.equal(reply.kind, "surveyed");
    return reply.kind === "surveyed" ? reply : undefined;
  };
  const SCRIPTLESS_WEB = { script_absent: true, on_web_page: true };
  const SCRIPTLESS_BLANK = { script_absent: true, on_web_page: false };

  it("скрипта нет в верхнем фрейме веб-страницы — script_missing", async () => {
    assert.equal((await gapOf([{ frameId: 0, result: SCRIPTLESS_WEB }]))?.scriptGap, "script_missing");
  });

  it("скрипт есть, видео нет — это не «вкладка старше скрипта»", async () => {
    assert.equal((await gapOf([{ frameId: 0, result: scripted([]) }]))?.scriptGap, null);
  });

  it("верхний фрейм со скриптом: подфрейм без скрипта просто ещё грузится", async () => {
    const reply = await gapOf([{ frameId: 0, result: scripted([]) }, { frameId: 9, result: SCRIPTLESS_WEB }]);
    assert.equal(reply?.scriptGap, null);
  });

  it("пустой about:blank наверху: перезагрузка не поможет, случая нет", async () => {
    assert.equal((await gapOf([{ frameId: 0, result: SCRIPTLESS_BLANK }]))?.scriptGap, null);
  });

  it("свежий подфрейм старой вкладки отдаёт видео, а разрыв скрипта остаётся виден", async () => {
    const reply = await gapOf([{ frameId: 0, result: SCRIPTLESS_WEB }, { frameId: 3, result: scripted([video(1)]) }]);
    assert.equal(reply?.scriptGap, "script_missing");
    assert.deepEqual(reply?.frames, [{ frameId: 3, candidates: [video(1)] }]);
  });

  it("другая версия скрипта: его список не принимается, причина script_outdated", async () => {
    const reply = await gapOf([{ frameId: 0, result: { script_version: "1.2.2", candidates: [video(1)] } }]);
    assert.equal(reply?.scriptGap, "script_outdated");
    assert.deepEqual(reply?.frames, [], "со старым скриптом не разговариваем: его видео не передаются");
  });
});

describe("команда «отдать вкладку N»", () => {
  const identity = { extensionId: "ourid", extensionBaseUrl: "chrome-extension://ourid/" };

  it("страница экстеншена — своя; контент-скрипт, чужой экстеншен и подделка адреса — нет", () => {
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "chrome-extension://ourid/popup.html" }, identity), true);
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "https://evil.example/chrome-extension://ourid/" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "other", url: "chrome-extension://ourid/popup.html" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "chrome-extension://other/popup.html" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "ourid" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "chrome-extension://ourid/x" }, { ...identity, extensionBaseUrl: "" }), false);
  });

  it("от своей страницы команда исполняется и исход уходит в ответ", async () => {
    const asked: number[] = [];
    const listener = createHandOffCommandListener(identity, (tabId) => {
      asked.push(tabId);
      return Promise.resolve({ kind: "started", player_window: "new" });
    });
    const responses: HandOffTabOutcome[] = [];
    const keepsChannel = listener({ kind: "hand_off_tab", tab_id: 31 }, { id: "ourid", url: "chrome-extension://ourid/bench.html" }, (response) =>
      responses.push(response),
    );
    assert.equal(keepsChannel, true);
    await flushPromises();
    assert.deepEqual(asked, [31]);
    assert.deepEqual(responses, [{ kind: "started", player_window: "new" }]);
  });

  it("от контент-скрипта (адрес сайта) и кривые команды не исполняются", () => {
    const asked: number[] = [];
    const listener = createHandOffCommandListener(identity, (tabId) => {
      asked.push(tabId);
      return Promise.resolve({ kind: "started", player_window: "new" });
    });
    const ignore = () => assert.fail("ответа быть не должно");
    assert.equal(listener({ kind: "hand_off_tab", tab_id: 1 }, { id: "ourid", url: "https://www.youtube.com/watch" }, ignore), false);
    for (const malformed of [
      { kind: "hand_off_tab" },
      { kind: "hand_off_tab", tab_id: -1 },
      { kind: "hand_off_tab", tab_id: 1.5 },
      { kind: "hand_off_tab", tab_id: "1" },
      { kind: "hand_off_tab", tab_id: 1, extra: 0 },
      { kind: "other", tab_id: 1 },
      null,
    ]) {
      assert.equal(listener(malformed, { id: "ourid", url: "chrome-extension://ourid/popup.html" }, ignore), false);
    }
    assert.deepEqual(asked, []);
  });
});

describe("страница для open_session и сообщения плеера", () => {
  it("заголовок чистится, домен — только допустимый протоколом", () => {
    assert.deepEqual(pageIdentityOf({ title: "A‮B\nC", url: "https://Пример.укр/x?y" }), { title: "AB C", domain: "xn--e1afmkfd.xn--j1amh" });
    assert.equal(siteDomainOf("file:///home/video.html"), UNKNOWN_SITE_DOMAIN);
    assert.equal(siteDomainOf("not a url"), UNKNOWN_SITE_DOMAIN);
    assert.equal(siteDomainOf("http://[::1]:8080/"), "[::1]");
  });

  it("часы плеера теряют seq, return_to_page даёт точный возврат, fetch_range уходит загрузчику (09b)", () => {
    assert.deepEqual(translatePlayerMessage({ type: "clock", seq: 3, position_us: 10, state: "waiting", rate: 500 }, 1), {
      kind: "shadow",
      command: { kind: "clock", position_us: 10, state: "waiting", rate: 500 },
    });
    assert.deepEqual(translatePlayerMessage({ type: "return_to_page", position_us: 7, resume: "paused" }, 1), {
      kind: "end_handoff",
      position: { kind: "at", position_us: 7 },
      resume: { kind: "exactly", resume: "paused" },
      cause: "returned_to_page",
    });
    assert.deepEqual(translatePlayerMessage({ type: "fetch_range", request_id: 1, offset: 0, length: 10 }, 1), {
      kind: "fetch_range",
      request: { request_id: 1, offset: 0, length: 10 },
    });
    assert.deepEqual(translatePlayerMessage({ type: "stream_rejected", generation: 1, reason: "unsupported_codec" }, undefined), { kind: "ignore" });
  });
});

describe("донесение сторожа следующего видео (сессия 09c2)", () => {
  const candidate = video(4, { playing: true, visible_area_px: 1_000 });
  const report = { kind: HELD_VIDEO_REPORT_KIND, element: 4, candidates: [candidate] };
  const contentSender = { id: "ext", tab: { id: 12 }, frameId: 3 };

  function listen() {
    const calls: [number, number][] = [];
    const replies: string[] = [];
    const listener = createHeldVideoListener("ext", (tabId, frameId) => {
      calls.push([tabId, frameId]);
      return "adopt";
    });
    return { calls, replies, send: (message: unknown, sender: FrameSender) => listener(message, sender, (reply) => void replies.push(reply)) };
  }

  it("вкладку и фрейм называет браузер; фон отвечает синхронно", () => {
    const heard = listen();
    assert.equal(heard.send(report, contentSender), false);
    assert.deepEqual(heard.calls, [[12, 3]]);
    assert.deepEqual(heard.replies, ["adopt"]);
  });

  it("чужой экстеншен, служебная страница без вкладки и мусор не проходят", () => {
    const heard = listen();
    heard.send(report, { ...contentSender, id: "other" });
    heard.send(report, { id: "ext" });
    heard.send({ ...report, extra: 1 }, contentSender);
    heard.send({ ...report, candidates: [{ forged: true }] }, contentSender);
    assert.deepEqual(heard.calls, []);
    assert.deepEqual(heard.replies, []);
  });
});
