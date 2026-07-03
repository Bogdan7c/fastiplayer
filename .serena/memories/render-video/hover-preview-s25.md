# Obsolete: Render Hover Preview

- OBSOLETE since 2026-07-03: render hover preview overlay input/pass and related resource-provider plumbing were removed.
- Renderer still presents main playback frames and any retained scrub/main visual override paths required by live scrub, but it must not expose a hover-preview overlay API.
- Future frame-server/playback-rate rendering work should use neutral frame identity/resource-provider contracts without resurrecting hover-specific overlay terminology.