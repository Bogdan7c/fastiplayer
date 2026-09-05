# Runtime assets

`fastiplayer-main.png` is an unedited Spectacle capture of the actual Fastiplayer
window at approximately 00:24, sized 1280×720. It was captured on 2026-09-05
on the current development computer: AMD Ryzen 7 7840HS with Radeon 780M,
Linux 7.2.2-1-cachyos, KDE Wayland, WGPU rendering and FFmpeg software decoding.
A separate temporary profile used volume 0.0 with the audio pipeline active;
the actual output initialized at 48 kHz, six channels. The source is AV1
3840×2160 at 60 fps, SDR BT.709, with AAC audio. This is a runtime demonstration,
not a performance measurement and not the historical T480s screenshot.

Build source: `56f856690a8abc65221e6c88931370127c667aab` plus the Fastiplayer
identity changes in this rename commit; package versions remain `0.1.0-alpha.1`.
Build command: `cargo build --release -p app-egui --locked` (Rust 1.96.0).

| Artifact | SHA-256 |
| --- | --- |
| Screenshot | `b8545cdd297e375de2be33e2309382278ffb03a5704c8bf606a192cd216cd1ef` |
| Release executable used for capture | `3adf9c1f7b93157f0464825de6f90eb3ad184d915a378be2536b148b0b4125f6` |
| Owner-supplied AV1 source | `e647620fa682a1ca46dcc0c02465f97513241e13e998afd68cdf39c842f00c3b` |

The historical T480s capture and its own provenance remain available in the
[tagged asset directory](https://github.com/Bogdan7c/fastiplayer/tree/v0.1.0-alpha.1/docs/assets)
and the [T480s report](../benchmarks/thinkpad-t480s.md). Its checksum and machine
identity do not describe this new image. The original logo is unchanged.

The visible movie frame is from **Big Buck Bunny**:
© 2008 Blender Foundation / www.bigbuckbunny.org.
Movie imagery is licensed under
[Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/),
as stated by the [Blender Foundation](https://peach.blender.org/about/).
The image combines the movie frame with the running player interface;
it does not imply endorsement by the Blender Foundation.
