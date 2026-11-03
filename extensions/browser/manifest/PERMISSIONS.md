# Permissions rationale (for store review)

Fastiplayer Bridge hands the video of a web page to the Fastiplayer desktop player running on the same computer.
It has no servers, no accounts and no analytics. Nothing is sent to us or to any third party.

| Permission | Why it is needed |
| --- | --- |
| `nativeMessaging` | Talks to the small local helper program (`fastiplayer-browser-host`) that the Fastiplayer desktop app installs. This is the only channel to the player. |
| `contextMenus` | Adds a "Play in Fastiplayer" item to the right-click menu of a video. |
| `storage` | Remembers the user's own choices, locally only. Today it stores two things in `storage.local`: the "Video codec for sites" choice (`codecSetting`: auto / prefer_vp9 / always_h264 / off) and the list of video codecs the Fastiplayer app reported as cheap to play at its last connection (`cheapVideoCodecs`, derived from the app's own codec-priority and renderer settings). No page content, addresses or history are stored. |
| `scripting` | When the user hands a tab over, asks each frame of that tab which videos it has (to pick the playing one). Also lets the extension attach to a page that was already open when the extension was installed or updated. While a codec choice is active, the extension registers one dynamic content script (id `fastiplayer-codec-policy`, `document_start`, all frames, isolated world): its only content is the list of video codec families that pages are told are not supported (for example AV1), so that the page's player picks another codec before it starts. It is replaced or removed whenever the choice changes. |
| `host_permissions: <all_urls>` | A video can be on any site. The extension runs a content script at `document_start` in every frame, because it must see the page's media before the page's own player starts. The script does not read page content, cookies or form data; it only observes media elements and Media Source buffers of the page (in Chrome also inside the page's dedicated workers: the page's `Worker` is started with the same observer in front of the site's worker code) and passes the video bytes to the local player. The same host permission lets the extension background download a plain video file (`<video src>`) that the user handed over, see below. |

Not requested: `cookies`, `webRequest`, `declarativeNetRequest`, `tabs` (the title and site of a tab the user hands over are read with `tabs.get`, which the host permission already allows; only the title and the domain name are passed to the player), remote code.

Network access: when the user hands over a page whose video is a plain file (`<video src="https://…/movie.mp4">`), the extension background downloads that same file from the same address, piece by piece (HTTP `Range` requests), only while the player plays it and only the pieces the player asks for. The request goes through the browser's own network stack with the site's cookies, like the page's own `<video>` element would load it (the browser does not let the extension send the page address as `Referer`, so a site that checks it refuses, and the video simply stays in the page); the page's element stops loading the file meanwhile, so the file is not downloaded twice. The bytes go only to the local player. The extension contacts no other address on the Internet.

Chrome: `message_serialization: "structured_clone"` (Chrome 148+) lets the content script pass the video bytes to the extension background as binary data instead of JSON text; it does not change what is sent or to whom.

The extension streams video to the local player only over `ws://127.0.0.1` (loopback).

Firefox: the background page's `connect-src` allows `http:` and `https:` for the file download described above (Firefox applies this policy to `fetch` of the background page); scripts stay `'self'` only.

Firefox: `data_collection_permissions` is `required: ["none"]` — the extension does not collect or transmit user data to anyone.
