#!/usr/bin/env bash
# Генерация фикстуры для crates/symphonia-demux/tests/matroska_chapters.rs.
#
# Запуск: crates/symphonia-demux/tests/fixtures/matroska/generate.sh
# Требуется ffmpeg (проверено на n9.0.2). ffmpeg не пишет необязательный EditionUID
# (RFC 9559), поэтому файл воспроизводит реальный случай «MKV с главами после ремукса».
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Две главы: 0–1 с «Intro», 1–2 с «Main» (время в мс, TIMEBASE=1/1000).
cat > "$work/chapters.ffmeta" <<'META'
;FFMETADATA1
[CHAPTER]
TIMEBASE=1/1000
START=0
END=1000
title=Intro
[CHAPTER]
TIMEBASE=1/1000
START=1000
END=2000
title=Main
META

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc=size=64x64:rate=10:duration=2" -i "$work/chapters.ffmeta" \
  -map 0:v -map_metadata 1 -map_chapters 1 \
  -c:v libvpx -b:v 40k \
  "$here/chapters-without-edition-uid.mkv"
