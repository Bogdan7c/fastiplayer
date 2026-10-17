#!/usr/bin/env bash
# Генерация фикстур AC-3/E-AC-3 в MPEG-TS для crates/mpeg-ts-demux/tests/ac3_transport_stream.rs.
#
# Запуск: crates/mpeg-ts-demux/tests/fixtures/ac3/generate.sh
# Требуется ffmpeg с libx264 и энкодерами ac3/eac3. Источники синтетические
# (testsrc2, синус), ничего не скачивается. Байты зависят от версии ffmpeg,
# поэтому тесты опираются только на свойства из README.md.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
video=(-f lavfi -i "testsrc2=size=64x64:rate=10:duration=0.5")
tone() { echo "sine=frequency=440:sample_rate=$1:duration=0.5"; }
x264=(-c:v libx264 -preset ultrafast -tune zerolatency -g 5 -pix_fmt yuv420p)
common=(-hide_banner -loglevel error -y)
output=(-fflags +bitexact -map_metadata -1)

# ATSC-сигнализация: stream_type 0x81 + registration "AC-3".
ffmpeg "${common[@]}" "${video[@]}" -f lavfi -i "$(tone 48000)" \
    "${x264[@]}" -ac 2 -c:a ac3 -b:a 96k \
    "${output[@]}" -f mpegts "$here/h264-ac3-stereo-48k.ts"

# ATSC-сигнализация: stream_type 0x87 + registration "EAC3"; 5.1, 44.1 кГц.
ffmpeg "${common[@]}" "${video[@]}" -f lavfi -i "$(tone 44100)" \
    "${x264[@]}" -ac 6 -c:a eac3 -b:a 192k \
    "${output[@]}" -f mpegts "$here/h264-eac3-5p1-44k1.ts"

# DVB-сигнализация (System B): private PES 0x06 + AC-3_descriptor 0x6a, язык rus.
ffmpeg "${common[@]}" -f lavfi -i "$(tone 48000)" \
    -ac 1 -c:a ac3 -b:a 64k -metadata:s:a:0 language=rus \
    "${output[@]}" -mpegts_flags system_b -f mpegts "$here/audio-only-ac3-dvb-mono.ts"

# DVB-сигнализация (System B): private PES 0x06 + enhanced_AC-3_descriptor 0x7a, язык eng.
ffmpeg "${common[@]}" -f lavfi -i "$(tone 32000)" \
    -ac 2 -c:a eac3 -b:a 96k -metadata:s:a:0 language=eng \
    "${output[@]}" -mpegts_flags system_b -f mpegts "$here/audio-only-eac3-dvb-stereo-32k.ts"

# Вещательная смена формата посреди потока (реклама ↔ фильм): 16 кадров stereo,
# 16 кадров 5.1(side), 16 кадров stereo — склейка готовых тоновых фикстур
# crates/audio/tests/fixtures/ac3 (свой синус в каждом канале), один PID, без видео.
audio_fixtures="$here/../../../../audio/tests/fixtures/ac3"
switch_raw="$(mktemp --suffix=.ac3)"
trap 'rm -f "$switch_raw"' EXIT
cat "$audio_fixtures/tones-stereo.ac3" "$audio_fixtures/tones-5.1.ac3" \
    "$audio_fixtures/tones-stereo.ac3" > "$switch_raw"
ffmpeg "${common[@]}" -f ac3 -i "$switch_raw" -c:a copy \
    "${output[@]}" -f mpegts "$here/ac3-broadcast-2p0-5p1-2p0-48k.ts"
