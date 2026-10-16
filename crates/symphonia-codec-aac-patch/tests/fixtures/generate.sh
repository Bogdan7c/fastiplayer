#!/usr/bin/env bash
# Генерация фикстуры AAC-LC 5.1 с Program Config Element (channel_configuration = 0).
#
# Запуск: crates/symphonia-codec-aac-patch/tests/fixtures/generate.sh
# Требуется ffmpeg (проверено на n9.0.2). Звук: синус 880 Гц, 1 с, 48 кГц, ТОЛЬКО в канале FC
# (остальные пять каналов — тишина). Раскладка `5.1(side)` ffmpeg-кодер aac пишет не индексом,
# а PCE: ASC = 11 80 04 c8 05 00 01 08 80 0c "Lavc63.1.102" 56 e5 00 (25 байт).
# ADTS-вариант ffmpeg дополнительно кладёт PCE внутрь каждого raw_data_block.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=1" \
  -af "pan=5.1(side)|FC=c0" -c:a aac -b:a 96k "$work/pce.mkv"
ffmpeg -hide_banner -loglevel error -y -i "$work/pce.mkv" -c copy "$here/pce-5p1-fc-880hz.adts"
