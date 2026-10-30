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

# Audio-only Opus в Matroska для tests/matroska_audio_only_seek.rs.
# Тон 440 Гц + писк 60 мс в начале каждой секунды k частотой 1000 + 250·(k mod 8) Гц:
# по частоте писка тест восстанавливает, какую медиа-секунду он реально слышит после seek-а.
# ffmpeg для audio-only режет кластеры по 5 с и пишет Cues с CueRelativePosition — ровно
# раскладка, на которой seek раньше перепрыгивал целый кластер.
beeps="0.12*sin(2*PI*440*t)+0.5*lt(mod(t\,1)\,0.06)*sin(2*PI*(1000+250*mod(floor(t)\,8))*t)"
ffmpeg -hide_banner -loglevel error -y \
  -filter_complex "aevalsrc=exprs=${beeps}:s=48000:d=12:n=960[a]" -map "[a]" \
  -c:a libopus -b:a 16k -ac 1 \
  "$here/opus-audio-only-beeps.mka"
# Тот же сигнал, но дорожка начинается на 2-й секунде (atrim сохраняет pts, первая cue = 2.001 с).
ffmpeg -hide_banner -loglevel error -y \
  -filter_complex "aevalsrc=exprs=${beeps}:s=48000:d=12:n=960,atrim=start=2[a]" -map "[a]" \
  -c:a libopus -b:a 16k -ac 1 \
  "$here/opus-audio-only-beeps-late2.mka"
