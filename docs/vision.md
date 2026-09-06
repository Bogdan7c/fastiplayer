# The Fastiplayer vision

Watching a film, listening to an album, or opening a stream should feel easy. Fastiplayer is being built to make those everyday moments pleasant: a light, attractive player that responds to you, keeps useful controls close, and has room to grow.

**Early alpha · Active development · Linux-first · Source builds**

That is the direction, and the alpha is a working step toward it. Live timeline scrubbing, playback speed control, real-time video color adjustments, animated controls, local and supported network playback, and queue management already exist. The interface, compatibility, installation experience, and platform coverage are still developing. [Watch the 72-second trailer with sound](https://www.youtube.com/watch?v=eMfzBhpSF8M), [see the demonstrations in detail](demo.md), [build it](../README.md#quick-start), or check the [current limitations](../README.md#current-limitations).

## Lightness with evidence

A player should leave room for the rest of your computer. Fastiplayer treats resource use as something to design, measure, and improve: limit background work, bound queues and memory budgets, and avoid unnecessary copies or external processes where the native path can do the work.

These are engineering priorities, not a promise of universally lower resource use. The published ThinkPad T480s comparison found lower process RSS and higher process CPU than VLC on its two hardware workloads. The [benchmark reports](benchmarks/README.md) keep the method, revisions, raw evidence, and limitations alongside the numbers. Battery life has not been measured.

## Beauty and responsiveness in ordinary interactions

The interface should give the media space while making the next action easy to find. Playback controls, timeline interaction, the queue, and settings should feel like parts of one application. Small details matter: pausing a film should preserve the image without adding an irrelevant instruction to start playback.

The current application demonstrates that intent through live timeline scrubbing: the main picture follows a drag in either direction, and playback continues after release. Animated panels and controls connect the queue and settings to playback. The [trailer](demo.md) shows these interactions in continuous, real-time captures. Completing the cohesive redesign, including the prototype settings interface, remains an explicit roadmap item. Responsiveness also needs functional checks: a source opening successfully is useful only when its video reaches rendering or its audio reaches the consumer.

## Control without making every task complicated

People should be able to adjust the player to their needs. Playback speed uses pitch-preserving time stretching. Video saturation, contrast, and other color controls preview their effect during playback and can be reset. Runtime settings provide live previews where supported and explicit Apply/rollback behavior. The component that owns a setting validates and applies it; changes that require a rebuild use controlled reconfiguration.

Fuller control over application colors and appearance is planned separately from the existing video color correction. Localization and a smoother installation experience are also part of making the player approachable in everyday use. Easy use out of the box is a development direction; the current release requires a source build.

## Capable playback, understandable choices

Fastiplayer brings local media, supported network sources, queue persistence, playlists, and playback controls together. Future depth should help people do real things, such as watching with subtitles or handing media from a browser to the player, while keeping the ordinary playback path clear.

Support claims stay specific. A supported protocol can still contain an unsupported codec or profile, and a manifest listing subtitles does not mean the player can display them. The [compatibility matrix](web-media-compatibility-matrix.md) and dated acceptance reports define what has actually been checked.

## A multimedia foundation beneath the player

The player is built on an existing modular multimedia architecture. Source and demux owners handle input and packets; decoder backends produce frames through explicit contracts; rendering owns GPU conversion and presentation; audio owns its output path and clock. `player-core` coordinates playback scheduling and lifecycle, while the application composes these parts and translates user actions into intent.

Those boundaries make it possible to test and repair a specific area without rewriting its neighbors. Ownership, frame release, cancellation, backpressure, and errors are part of the contracts. [ARCHITECTURE.md](../ARCHITECTURE.md) explains the implementation and its trust boundaries.

This foundation could eventually support other multimedia applications, including a video editor. That is architectural potential, not a commitment to ship an editor or a standalone SDK. The working player remains the product and the place where the architecture earns its usefulness.

## The path to 1.0

The [ordered roadmap](../README.md#roadmap-to-10) and [GitHub milestone](https://github.com/Bogdan7c/fastiplayer/milestone/1) keep the agreed sequence:

1. Native Rust subtitles with a published format compatibility matrix.
2. Browser media handoff and an extension.
3. Cohesive application redesign, including settings.
4. Configurable application colors and appearance.
5. Localization infrastructure and initial translations.
6. An OpenGL ES 2.0 renderer for older Linux hardware.
7. Drag-and-drop for local files and URLs.
8. Native Windows application support.

These are planned capabilities. They do not imply that subtitles, browser integration, or Windows support already ship in the alpha.

## Beyond the current roadmap

NVDEC, native HDR display output, and macOS are possible future directions. Each depends on resources, suitable hardware, and real acceptance testing. There are no delivery dates or commitments for them, and they do not change the sequence through 1.0. Existing HDR-to-SDR processing is separate from future native HDR output.

## How the work is led

One maintainer, [Bogdan Korolyov](../MAINTAINERS.md), leads development and takes responsibility for product and architecture decisions. AI tools assist implementation and investigation; the workflow calls for explicit decisions, review, and functional checks of the result. [AI-assisted development](ai-development.md) explains that process and the optional tooling.

Trying the player and sharing a reproducible problem or a thoughtful account of its everyday use helps direct the work. The project is also open to support with development tools and equipment. [Discussions](https://github.com/Bogdan7c/fastiplayer/discussions) is the place for questions, ideas, and conversations about useful support.
