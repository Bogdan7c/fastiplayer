# Permissions rationale (for store review)

Fastiplayer Bridge hands the video of a web page to the Fastiplayer desktop player running on the same computer.
It has no servers, no accounts and no analytics. Nothing leaves the user's machine.

| Permission | Why it is needed |
| --- | --- |
| `nativeMessaging` | Talks to the small local helper program (`fastiplayer-browser-host`) that the Fastiplayer desktop app installs. This is the only channel to the player. |
| `contextMenus` | Adds a "Play in Fastiplayer" item to the right-click menu of a video. |
| `storage` | Remembers the user's own choices (for example, which sites to hand over). Stored locally only. |
| `scripting` | Lets the extension attach to a page that was already open when the extension was installed or updated. |
| `host_permissions: <all_urls>` | A video can be on any site. The extension runs a content script at `document_start` in every frame, because it must see the page's media before the page's own player starts. The script does not read page content, cookies or form data; it only observes media elements and Media Source buffers of the page and passes the video bytes to the local player. |

Not requested: `cookies`, `webRequest`, `tabs` (page title and address come from the content script), remote code, any network access to the Internet.

The extension connects only to `ws://127.0.0.1` (loopback) to stream video to the local player.

Firefox: `data_collection_permissions` is `required: ["none"]` — the extension does not collect or transmit user data to anyone.
