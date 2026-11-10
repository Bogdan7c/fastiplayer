# Connecting Fastiplayer to your browsers

Fastiplayer can receive video from a web page in your browser and play it in its own window. To do that, your browser has to know that Fastiplayer exists and is allowed to talk to it. This is what the **Browser integration** section in Settings sets up for you.

> **Status:** the player side (this section) is ready. The browser extension itself is **not published yet**, so for now connecting only prepares your browsers; nothing will play from a web page until the extension is released.

## What the section does

Open **Settings > Browser integration**. Fastiplayer looks for the browsers installed on your computer (regular packages and Flatpak) and shows one row per browser with a status and a button.

| Button | What it does |
| --- | --- |
| **Connect** | Copies a small helper program and writes the connection files for this browser. Safe to press again. |
| **Disconnect** | Removes the files Fastiplayer wrote for this browser. It never touches files that are not ours. |
| **Allow** | Only for Flatpak browsers. Gives the browser's sandbox permission to reach Fastiplayer. It runs one command, shown in [the Flatpak guide](flatpak.md), and only when you click it. |
| **Reconnect to this copy** | Re-writes the files so they point to the Fastiplayer copy you are running now. |

Nothing is installed in the background. Everything happens when you press a button, and the window stays responsive while it works. At the first start Fastiplayer may offer to set things up once; if you say "Not now" it will not ask again (you can still use Settings any time).

## Status meanings

| Status | Meaning | What to do |
| --- | --- | --- |
| Connected | Everything is in place and matches this copy of Fastiplayer. | Nothing. |
| Connected to another Fastiplayer copy | The files are ours but start a different copy of the player (for example a development build and an installed one). | Press **Reconnect to this copy** if you want this one to be used. |
| Not connected | No connection file for this browser. | Press **Connect**. |
| Outdated - reconnect | Our files are there but differ from what this version would write (older version, helper missing or not executable). | Press **Reconnect**. |
| Needs Flatpak permission | Files are fine, but the Flatpak sandbox of this browser cannot reach Fastiplayer yet. | Press **Allow**, or follow the [Flatpak guide](flatpak.md). |
| Firefox from Flatpak is not supported yet | Flatpak Firefox does not support the mechanism we use. Nothing is written. | Use Firefox installed as a regular package, or another browser. |
| Snap browsers are not supported | See [Snap](#snap-browsers). Nothing is written. | Use a package or Flatpak build. |
| A file with our name exists but belongs to another program | Something else already owns the connection file name. | **Connect** will overwrite it after warning you; remove it by hand if you are not sure. |

If a folder cannot be read, the row shows a plain-language reason instead of a status.

## Supported browsers

"Verified" means we ran a real end-to-end check (browser, helper and player) on a real machine. "Not verified" means the paths come from the browsers' documentation and should work, but nobody has run it yet. If you try one, a report is very welcome.

| Browser | Package | Flatpak |
| --- | --- | --- |
| Firefox | Verified | Not supported yet |
| Chromium | Verified | Not verified |
| Google Chrome | Not verified | Verified |
| Brave | Not verified | Not verified |
| Vivaldi | Not verified | Not verified |
| Microsoft Edge | Not verified | Not verified |
| LibreWolf | Not verified | Not supported yet |
| Waterfox | Not verified | Not supported yet |

Flatpak Chromium here also covers the Ungoogled Chromium Flatpak. A browser is listed in Settings only if it is actually installed (its program is found, or it appears in `flatpak list`); an empty profile folder left from an old install does not count.

## Where the files go

Connecting writes three kinds of files. All are small, and only inside your home folder.

1. **The helper program** (a copy of `fastiplayer-browser-host`).
   - Regular browsers: `~/.local/share/fastiplayer/browser-host/` (one copy shared by all of them).
   - Flatpak browser: `~/.var/app/<app id>/data/fastiplayer/browser-host/`, because the sandbox can only see its own folder.
2. **The native messaging manifest** (the "connection file", `io.github.bogdan7c.fastiplayer.bridge.json`) in the folder each browser reads:
   - Firefox: `~/.mozilla/native-messaging-hosts/` (and also `~/.config/mozilla/native-messaging-hosts/`).
   - LibreWolf: `~/.librewolf/native-messaging-hosts/`. Waterfox: `~/.waterfox/native-messaging-hosts/`.
   - Chromium family: `~/.config/<browser>/NativeMessagingHosts/` with `chromium`, `google-chrome`, `BraveSoftware/Brave-Browser`, `vivaldi` or `microsoft-edge`.
   - Flatpak Chromium family: `~/.var/app/<app id>/config/<browser>/NativeMessagingHosts/`.
3. **The D-Bus activation file** `~/.local/share/dbus-1/services/io.github.Bogdan7c.Fastiplayer.BrowserSession.service`. It lets your desktop start Fastiplayer when the browser asks for it. Fastiplayer also asks the session bus to re-read its service files, so no re-login is needed.

## Two Fastiplayer copies

The files point to one specific copy of Fastiplayer. **The copy that last pressed Connect owns them.** Other copies show "Connected to another Fastiplayer copy" and do not change anything until you press **Reconnect to this copy** there. Disconnect only removes the shared D-Bus file if it points to the copy you are using.

## Snap browsers

Browsers installed from Snap (for example the default Firefox or Chromium on some Ubuntu setups) run in a sandbox of their own. Chromium from Snap does not support the browser-to-program mechanism we rely on at all, and Firefox from Snap reaches programs through a portal or proxy mechanism that Fastiplayer does not use (we did not verify it). Rather than half-working, Fastiplayer detects a Snap browser, says "Snap browsers are not supported" and writes nothing. The honest workaround is to install the same browser as a regular package or from Flatpak. Supporting Snap may come later; it is not planned for the first release.

## Removing everything by hand

Press **Disconnect** on every row. If you cannot, delete these (adjust names for the browsers you used):

```
rm -r ~/.local/share/fastiplayer/browser-host
rm ~/.local/share/dbus-1/services/io.github.Bogdan7c.Fastiplayer.BrowserSession.service
rm ~/.mozilla/native-messaging-hosts/io.github.bogdan7c.fastiplayer.bridge.json
rm ~/.config/mozilla/native-messaging-hosts/io.github.bogdan7c.fastiplayer.bridge.json
rm ~/.config/chromium/NativeMessagingHosts/io.github.bogdan7c.fastiplayer.bridge.json
rm ~/.config/google-chrome/NativeMessagingHosts/io.github.bogdan7c.fastiplayer.bridge.json
# Flatpak browsers (example: Google Chrome):
rm -r ~/.var/app/com.google.Chrome/data/fastiplayer
rm ~/.var/app/com.google.Chrome/config/google-chrome/NativeMessagingHosts/io.github.bogdan7c.fastiplayer.bridge.json
flatpak override --user --no-talk-name='io.github.Bogdan7c.Fastiplayer.*' com.google.Chrome
```

`rm` complains about files that do not exist; that is fine. See the [Flatpak guide](flatpak.md) for taking the permission back.

## The browser extension (Fastiplayer Bridge)

The extension is the part that lives inside your browser. It sends the video of a web page to Fastiplayer. It is **not published in any store yet**; this section is for people who build it themselves.

### Installing a development build

Build it once (needs Node.js 26 or newer):

```
cd extensions/browser
npm ci
npm run build
```

This creates two folders: `dist/chrome` and `dist/firefox`.

- **Chromium, Chrome, Brave, Vivaldi, Edge:** open `chrome://extensions`, turn on **Developer mode**, press **Load unpacked** and pick `extensions/browser/dist/chrome`. The extension gets the same ID every time, so the connection files written by **Connect** keep matching it.
- **Firefox:** open `about:debugging#/runtime/this-firefox`, press **Load Temporary Add-on...** and pick `extensions/browser/dist/firefox/manifest.json`. A temporary add-on disappears when Firefox closes.

Then open Fastiplayer, go to **Settings > Browser integration** and press **Connect** (and **Allow** for a Flatpak browser).

### Installing the extension from Fastiplayer

Under each browser in **Settings > Browser integration** there is an **Extension** line. What its button does depends on the browser:

- **Chromium, Brave, Vivaldi, Edge (packages and Flatpak):** **Install extension** puts a small hint file into the browser's own folder in your home directory. The browser installs the extension by itself the next time it starts, so the line then says "restart the browser to finish installing". No clicks inside the browser are needed.
- **Google Chrome (package and Flatpak) and Firefox:** these browsers ignore such hint files. **Install in browser** opens the extension's store page inside that browser, and you press **Add** there.
- If you removed the extension in the browser yourself, the line says so and the button becomes **Open store page**: the browser remembers your choice and will not reinstall it silently.
- While the browser has not restarted yet, the line shows two buttons: **Install in browser** (opens the store page; use it if the browser ignored the hint, which can happen with browsers where silent install is not verified) and **Cancel**. **Cancel** only cancels an install that has not happened yet: it deletes the hint file Fastiplayer wrote, nothing else. A hint file left by another program is never touched or overwritten; the line only mentions it, and **Install in browser** opens the store page instead.
- **Removing an installed extension is done in the browser**, on its extensions page. Fastiplayer cannot remove it: deleting the hint file does not uninstall an extension that is already installed (checked in Chromium), so there is no such button once the extension is installed.
- If the extension is installed but turned off, turn it on on the browser's extensions page: Fastiplayer cannot do it for you.

Fastiplayer only reads your browser's profile files to find out the state; it never writes to them, and it **never asks for an administrator password**.

Until the extension is published in the stores, the line says **available after publication** and has no button. Use the development build instructions above in the meantime.

### What "Fastiplayer is not connected" means

Click the extension's toolbar button. If it says **Fastiplayer is not connected** (the icon also shows a red `!`), the extension could not reach the player. The usual reasons:

| Reason | What to do |
| --- | --- |
| The connection files are not written for this browser. | In Fastiplayer press **Settings > Browser integration > Connect**. |
| The browser is a Flatpak and the permission is missing. | Press **Allow** (see the [Flatpak guide](flatpak.md)) and restart the browser. |
| Fastiplayer was moved or removed. | Start the copy you want and press **Reconnect to this copy**. |
| Fastiplayer did not answer. | Restart Fastiplayer and open the popup again. |

If the popup asks you to update the extension or Fastiplayer, the two are too far apart in version to talk to each other; update the one it names.

### What the extension asks for

Every permission and the reason for it is listed in [`extensions/browser/manifest/PERMISSIONS.md`](../../extensions/browser/manifest/PERMISSIONS.md). It does not read cookies and does not talk to the Internet.

### Remembered dub, quality and subtitles

On sites the extension knows, Fastiplayer shows the site's dubs in the ♪ menu and its qualities in the quality button. Which sites these are, and what works on each, is in the [site compatibility table](compatibility.md). The extension remembers what you pick for each series — in the Fastiplayer menus or on the page itself — and sets it again when the next episode starts:

- the dub is set only at the start of an episode, because on many sites changing the dub starts the episode over;
- the quality is set at any moment, because the site keeps playing from the same second;
- subtitles are suggested to Fastiplayer once per episode; whatever you change during the episode stays as you set it.

The series is stored only as an irreversible fingerprint, never by its name or address. Private windows use remembered choices but never save new ones. The extension settings page shows how many series are remembered, lets you change the limit (500 by default) and has a "Forget remembered choices" button.
