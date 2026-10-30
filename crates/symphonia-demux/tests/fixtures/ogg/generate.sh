#!/usr/bin/env bash
# Генерация фикстуры для crates/symphonia-demux/tests/ogg_opus_pre_skip.rs.
#
# Запуск: crates/symphonia-demux/tests/fixtures/ogg/generate.sh
# Требуется ffmpeg с libopus (проверено на n9.0.2).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

# Тот же сигнал, что у ../matroska/opus-audio-only-beeps.mka (тон 440 Гц + писк 60 мс в начале
# каждой секунды k частотой 1000 + 250·(k mod 8) Гц), те же настройки libopus, но в Ogg и 4 с:
# тест сравнивает звук одной и той же медиа-секунды в двух контейнерах. ffmpeg пишет
# OpusHead pre_skip = 312 (6.5 мс), а гранулы страниц включают эти 312 отсчётов (RFC 7845).
beeps="0.12*sin(2*PI*440*t)+0.5*lt(mod(t\,1)\,0.06)*sin(2*PI*(1000+250*mod(floor(t)\,8))*t)"
ffmpeg -hide_banner -loglevel error -y \
  -filter_complex "aevalsrc=exprs=${beeps}:s=48000:d=4:n=960[a]" -map "[a]" \
  -c:a libopus -b:a 16k -ac 1 \
  "$here/opus-audio-only-beeps.opus"
