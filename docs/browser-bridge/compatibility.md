# Fastiplayer Bridge: site compatibility

**Status: development build, not released yet** (10 November 2026). The bridge lets the page keep downloading the video while Fastiplayer decodes and shows it. It is planned in [issue #2](https://github.com/Bogdan7c/fastiplayer/issues/2) and the public plan [`user/browser-bridge/`](../../user/browser-bridge/README.md). This file lists what is proven on the reference sites of that plan. It is updated when a session proves or breaks something, and it will name the release the bridge ships in.

Legend:
- **Live** — seen working on the real site, in the browsers named in brackets: **FF** = Firefox 158, **Cr** = Chromium 156, **CF** = Google Chrome 156 from Flatpak. The automatic runs use a fresh browser profile on an invisible desktop. Owner checks use the owner's own Firefox.
- **Bench** — proven on the project's local test pages (`scripts/browser-bridge-bench.sh`), with the same extension and player, but not yet on this site.
- **Not checked** — no run on this site yet. It may work; we do not claim it.
- **No** — the site does not offer it, or we refuse it on purpose (the reason is in the notes).
- **—** — does not apply.

A site that is not listed has no promises. The bridge still tries: any page that feeds its video element through Media Source Extensions, a plain `<video src>` file or native HLS can be handed off. Menus of dubs, qualities and episodes need a site adapter.

How to connect the browsers and what each status means: [README.md](README.md).

## What the columns mean

- **Hand-off** — mode 1: the toolbar button, the right-click menu on the video, the button over the video, "Reload and open". Mode 2 (auto-play when the person presses play on the page) is one rule for every site. It is proven live on archive.org, Wikimedia Commons and X; it is not checked per reference site.
- **Next** — the Next button, the N key and MPRIS Next in the player window open the site's next episode or video.
- **Dubs** — the site's dubs or audio tracks in the player's ♪ menu. Choosing one switches the page, and the window returns to the same second.
- **Quality** — the site's qualities in the quality button menu. The button itself always shows what is on screen (all sites).
- **CC** — subtitles the page downloads appear in the player's CC menu and are drawn in Fastiplayer's own style.
- **Ads** — ads play in the player with a plate ("Ad 1 of 2"). **Skip** means our "Skip ›" presses the site's real skip button.
- **Memory** — the extension remembers the chosen dub, quality and subtitles (per series, or per site where noted) and sets them again on the next episode. Private windows read the memory but never write it.

## Summary

| Site | Hand-off | Next | Dubs | Quality | CC | Ads | Memory |
|---|---|---|---|---|---|---|---|
| **YouTube** video | Live (FF, CF) | Live (FF, Cr) | Live: 13–25 dubs (FF) | Live (FF, Cr) | Live: native rows + 156 auto-translations (embedded player, FF, Cr) | Plate; "Skip on the page" hint (YouTube accepts only a real mouse) | One record for the whole site (FF) |
| YouTube live | Live (FF) | — | — | Live (FF) | Not checked | Plate | One record for the whole site |
| YouTube Shorts | Live (FF) | No (feeds come in session 16b) | — | Live (FF) | Not checked | Not checked | One record for the whole site |
| Embedded YouTube (e.g. Suspilne) | Not checked | Not checked | Not checked | Not checked | Live on a local wrapper page (FF, Cr) | Not checked | Written under the host site |
| **Twitch** live | Live (FF, CF) | — | — | Live, switches in place (FF, Cr) | Not checked | Plate only: Twitch's built-in ads have no skip button | One record for the whole site |
| Twitch recording | Live (FF, CF) | — | — | Not checked | Not checked | Not checked | — |
| Twitch clip | Live (FF, CF) | — | — | — | — | — | — |
| **uaserials.com** (Tortuga player) | Live (FF, Cr) | Live (FF, Cr) | Live: Tortuga dubs, same episode (FF) | Live (Cr) | Live (Cr) | Skip (FF, Cr) | Live: record per series (FF, Cr) |
| **klonua.com** (Ashdi player, series) | Live (FF); **Cr: No**, see below | Live (FF) | Live: three-step dub switch (FF) | Not checked here (same player as uakino) | Not checked | Skip (FF) | Live: record per series (FF) |
| klonua.com (Ashdi player, film) | Live (FF); **Cr: No**, see below | — | Live (FF) | Not checked here | Not checked | Skip (FF) | Not checked |
| **uakino** (Ashdi player) | Live (Cr) | — | No: the site's own tabs above the player are not PlayerJS | Live, switches in place (Cr) | Not checked | Skip (Cr) | Live: record per film (Cr) |
| **kinoukr.tv** (Ashdi and Tortuga players) | Live (FF, CF) | Not checked (same players as above) | Not checked (same players) | Not checked (same players) | Live (FF, Cr) | Skip (FF, CF) | Not checked |
| Megogo (free TV) | Live (FF) | — | — | Not checked | Not checked | Not checked | — |
| Takflix | Not checked (trailers are YouTube) | — | — | — | — | — | — |
| MDN `<video src>` examples | Live, file inside an iframe (FF) | — | — | — | — | — | — |
| archive.org | Live (FF, CF) | — | — | — | — | — | — |
| PeerTube (Framatube) | Live (FF) | — | — | Not checked | Not checked | — | — |
| DW (native HLS) | Bench only (Cr) | — | — | — | — | — | — |
| Vimeo | No: DRM, the video stays in the browser ("This video is protected") | — | — | — | — | — | — |
| DRM demos (Shaka, dash.js) | No: DRM is refused by design | — | — | — | — | — | — |
| Jitsi and other calls | No: on the built-in stop-list | — | — | — | — | — | — |

1+1 video now redirects to another service and is not checked. Russian sites, services and players are never listed or tested.

## Notes by site

### YouTube
- Dub labels are in the YouTube interface language. The original track has "(original)" added, so "English (original)" on one video does not match an "English" dub on another.
- YouTube itself carries the chosen dub to the next video inside one tab; the memory then sees "already set" and does nothing.
- Subtitle text is proven end to end on the embedded player. Under browser automation, youtube.com returns empty subtitle bodies, so the full page is checked by hand.
- The automatic runs on the invisible desktop cannot keep youtube.com longer than about 45–65 s ("Something went wrong"). This also happens without the extension, so long YouTube runs are checked by hand.

### Twitch
- The quality button shows the 160p stream as "240p" (the button names quality steps), while the menu shows "160p".
- Twitch has no dubs and no next episode. Its built-in ads have no skip button.

### Sites with the PlayerJS player (Ashdi, Tortuga and others)
- One adapter serves every site that embeds PlayerJS. On a site that is not checked it is careful: it offers qualities, the player's own audio-track list and Next only when the list items look like episodes ("Серія 5", "Episode 5"). It never treats playlist folders as dubs there.
- **Tortuga** (uaserials, kinoukr): dubs are the player's audio tracks. A switch keeps the episode and the window returns to the same second. Seasons and episodes are chosen on the page. Next goes to the next episode and from the last episode of a season to the next season.
- **Ashdi series** (klonua, `?multivoice`): dubs are playlist folders. A switch takes three presses (dub, the same season, the same episode). If the new dub does not have this season or episode, nothing changes: the video keeps playing and the page menu goes back. Next stays inside the current dub and is not offered after its last episode.
- **Ashdi film**: dubs are list items; the site restarts the film from 0 and shows ads again, and the window returns to the same second.
- The site's own tabs above the player (uakino "UA #1", "Trailer") are not part of the player and are not in our menus.
- The page plays its ads again before each episode and after each dub switch. Choices in our menus are refused while an ad is on screen and can be repeated after it.
- **Chromium on klonua: hand-off fails** with "the page did not answer" (the tab survey times out). This is older than the PlayerJS adapter and is limited to Chrome-family browsers on this site. The fix is planned right after this session (plan session 15c). Firefox works.

## How support is verified

1. **Automated tests:** about 3 000 extension tests (`npm run check` in `extensions/browser`), among them the PlayerJS adapter on recorded page structures of Ashdi and Tortuga (`tests/adapters/playerjs-*.test.ts`), YouTube and Twitch adapters, and the choice memory. End-to-end bench scenarios (`scripts/browser-bridge-bench.sh`) prove that frames reach the renderer.
2. **Live runs** on the real sites use the bench helper `live_handoff` (Chromium and Firefox, fresh profiles, invisible desktop). A page probe reads what the site's player really shows. Session reports are in `user/browser-bridge/results/` (15a, 15a2, 15b and earlier).
3. **Owner checks** in the owner's own Firefox at the end of each adapter session.
