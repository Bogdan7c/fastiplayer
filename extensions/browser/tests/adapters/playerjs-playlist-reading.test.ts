// Чтение плейлиста PlayerJS (15b): «что играет» узнаётся по трём независимым признакам, и каждый из них
// сам по себе обязан признать меню «на полпути». Подделка страницы проверяет их вместе; здесь — по одному.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { playingEntriesOf, playlistRolesOf, readPlaylist } from "../../src/adapters/sites/playerjs/playlist-reading.ts";
import { PLAYERJS_HOST_PROFILES, PLAYERJS_UNKNOWN_HOST_PROFILE } from "../../src/adapters/sites/playerjs/variant-markup.ts";
import { createFakeVariantView, FakeNode } from "./fake-ad-dom.ts";

const ASHDI = PLAYERJS_HOST_PROFILES[0]!;
const TORTUGA = PLAYERJS_HOST_PROFILES[1]!;
const ACTIVE = "pjs-videoplayer42-active-pl";

/** Документ из пунктов `[путь, подпись, выбран]`. */
function pageOf(items: readonly (readonly [string, string, boolean])[]) {
  const root = new FakeNode("html");
  for (const [path, label, active] of items) root.add(new FakeNode("pjsdiv", { text: label, attributes: active ? { me: path, class: `other ${ACTIVE}` } : { me: path } }));
  return readPlaylist(createFakeVariantView(root));
}

/** Сериал Ashdi в устоявшемся виде: озвучка 0, сезон 1, серия 0 (файл 200) играет. */
const SETTLED_ASHDI: readonly (readonly [string, string, boolean])[] = [
  ["head_x-0", "DniproFilm", false],
  ["x-0", "DniproFilm", true],
  ["x-1", "Sunnysiders", false],
  ["head_xx-0-1", "Сезон 2", false],
  ["xx-0-0", "Сезон 1", false],
  ["xx-0-1", "Сезон 2", true],
  ["head_xxx-0-1-0-200", "Серія 1", false],
  ["xxx-0-1-0-200", "Серія 1", true],
  ["xxx-0-1-1-201", "Серія 2", false],
];

describe("PlayerJS: что играет", () => {
  it("устоявшийся сериал Ashdi: озвучка, сезон и серия — выбранные пункты", () => {
    const reading = pageOf(SETTLED_ASHDI);
    const roles = playlistRolesOf(reading, ASHDI, "?multivoice");
    assert.deepEqual(roles, { dubDepth: 1, seasonDepth: 2, episodeDepth: 3 });
    const playing = playingEntriesOf(reading, roles, ASHDI, "200");
    assert.equal(playing.kind, "settled");
    assert.deepEqual(playing.kind === "settled" ? [playing.entries.dub?.label, playing.entries.season?.label, playing.entries.episode?.label] : [], ["DniproFilm", "Сезон 2", "Серія 1"]);
  });

  it("заголовок «...» один признаёт меню «на полпути», даже если выбранные пункты на месте", () => {
    const reading = pageOf(SETTLED_ASHDI.map(([path, label, active]) => [path, path === "head_xx-0-1" ? "..." : label, active] as const));
    assert.equal(playingEntriesOf(reading, playlistRolesOf(reading, ASHDI, "?multivoice"), ASHDI, "200").kind, "unsettled");
  });

  it("выбранный лист Ashdi — не играющий файл (`playlist_id` другой): «на полпути»", () => {
    const reading = pageOf(SETTLED_ASHDI);
    assert.equal(playingEntriesOf(reading, playlistRolesOf(reading, ASHDI, "?multivoice"), ASHDI, "201").kind, "unsettled");
  });

  it("у Tortuga лист не несёт id файла: `playlist_id` «1-1» с путём не сверяется", () => {
    const reading = pageOf([
      ["x-0", "Сезон 1", true],
      ["xx-0-0-1-1", "Серія 1", true],
    ]);
    const roles = playlistRolesOf(reading, TORTUGA, "");
    assert.deepEqual(roles, { seasonDepth: 1, episodeDepth: 2 });
    assert.equal(playingEntriesOf(reading, roles, TORTUGA, "1-1").kind, "settled");
  });

  it("в уровне два выбранных пункта или ни одного — «на полпути»", () => {
    const twoActive = pageOf([
      ["x-0", "Сезон 1", true],
      ["x-1", "Сезон 2", true],
      ["xx-0-0-1-1", "Серія 1", true],
    ]);
    assert.equal(playingEntriesOf(twoActive, playlistRolesOf(twoActive, TORTUGA, ""), TORTUGA, "1-1").kind, "unsettled");
  });

  it("незнакомый сайт: признак `?multivoice` папки озвучками не делает; три уровня — устройство незнакомо", () => {
    const reading = pageOf(SETTLED_ASHDI);
    assert.deepEqual(playlistRolesOf(reading, PLAYERJS_UNKNOWN_HOST_PROFILE, "?multivoice"), {});
  });

  it("чужие и негодные `me` в плейлист не попадают", () => {
    const reading = pageOf([
      ["x-0", "Сезон 1", true],
      ["menu-settings", "Якість", false],
      ["x-0-<script>", "мусор", false],
      ["audio_1", "Субтитри", false],
    ]);
    assert.deepEqual([...reading.levels.keys()], [1]);
    assert.equal(reading.levels.get(1)?.length, 1);
    assert.deepEqual(reading.audioTracks.map((track) => [track.path, track.numbers]), [["audio_1", [1]]]);
  });
});
