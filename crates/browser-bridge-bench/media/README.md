# Bench media

`generate.sh <out_dir>` builds every test clip with ffmpeg (about 15 s, about 15 MB) and writes
`catalog.json` (format version 1). Rust calls it through `media::ensure_media`, which caches the
result under `<cache>/<sha256 of generate.sh>/`; editing the script invalidates the cache.

Needs: `ffmpeg`/`ffprobe` with libx264, aac, libvpx-vp9, libopus, libsvtav1 and the dash/hls
muxers, plus `python3` (standard library only). A monospace font found by `fc-match` is burnt in
as frame number and time; without it the clips are generated without text.

Invariants: 25 fps, GOP of 2 s, segment boundaries of the video qualities are identical, init
segments hold only headers, every segment starts with a keyframe.
