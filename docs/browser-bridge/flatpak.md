# Flatpak browsers: allowing Fastiplayer in 3 steps

A browser installed from Flatpak lives in a sandbox with a *filtered* connection to your desktop. It is not allowed to call arbitrary programs, including Fastiplayer. You have to give it one permission. Fastiplayer can do it for you with the **Allow** button, or you can do it by hand as described here.

Firefox from Flatpak is not supported yet; this guide is for Google Chrome, Chromium, Brave, Vivaldi, Edge and similar Flatpak browsers.

## The 3 steps

1. **Connect.** In Fastiplayer open **Settings > Browser integration** and press **Connect** on the row of your Flatpak browser. Its status becomes "Needs Flatpak permission".
2. **Give the permission** (pick one):
   - **Button:** press **Allow** on the same row. It runs exactly the command from the command-line option below, once, because you clicked.
   - **Flatseal** (a free permissions manager for Flatpak apps):
     1. Open Flatseal. The list on the left shows your Flatpak apps.
     2. Click your browser (for example "Google Chrome").
     3. Scroll to the **Session Bus** group, then to **Talk**.
     4. Press the **+** button in the "Talk" box and type `io.github.Bogdan7c.Fastiplayer.*`, then confirm. The new line appears in the list.
   - **Command line:**
     ```
     flatpak override --user --talk-name='io.github.Bogdan7c.Fastiplayer.*' <app id>
     ```
     For example `com.google.Chrome`, `org.chromium.Chromium`, `com.brave.Browser`. Find ids with `flatpak list --app`.
3. **Restart the browser** completely (close all its windows). Permissions apply when the app starts. Back in Settings the status becomes **Connected**.

## Why the name ends in `.*`

Fastiplayer's name on the desktop bus is `io.github.Bogdan7c.Fastiplayer.BrowserSession`. A permission for the exact name `io.github.Bogdan7c.Fastiplayer` covers **only that exact name**, not names under it; we tested this and it fails with "ServiceUnknown". The mask `io.github.Bogdan7c.Fastiplayer.*` covers the name and everything below it, so it works. Only Fastiplayer's own namespace is opened, nothing else.

## If you already gave the browser the full session bus

If the browser already has the full **session bus** socket (in Flatseal it shows as the session bus option in the Socket group), no extra permission is needed; this also works, and Fastiplayer will show Connected. The narrow `talk-name` permission is the safer choice, so we recommend it if you are setting things up fresh.

## Taking the permission back

- Flatseal: remove the `io.github.Bogdan7c.Fastiplayer.*` line from Session Bus > Talk.
- Command line, only our permission:
  ```
  flatpak override --user --no-talk-name='io.github.Bogdan7c.Fastiplayer.*' <app id>
  ```
  This leaves an explicit "not allowed" entry for that name, which is harmless.
- `flatpak override --user --reset <app id>` also works but removes **all** your user overrides for that app (including ones you made for other things), so prefer the command above.

To see what a browser is currently allowed to do: `flatpak info --show-permissions <app id>`.
