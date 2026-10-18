#!/bin/sh
# Пересоздаёт крошечные фикстуры дубляжей: одно немое видео и три аудио с разными тонами.
# Тон на слух не нужен: вертикальный тест измеряет частоту декодированного PCM.
set -eu
cd "$(dirname "$0")"

# Немое видео H.264 64x64, 2 секунды; moov в начале файла (faststart) для чтения по HTTP.
ffmpeg -y -loglevel error -f lavfi -i "color=c=blue:s=64x64:r=10:d=2" \
  -c:v libx264 -profile:v baseline -pix_fmt yuv420p -an \
  -movflags +faststart video-only.mp4

# Три аудио-дорожки (2 секунды, моно, 22.05 кГц): «оригинал» 440 Гц, «дубляж» 880 Гц и 1320 Гц.
for pair in original:440 dub-a:880 dub-b:1320; do
  name="${pair%%:*}"
  hertz="${pair##*:}"
  ffmpeg -y -loglevel error -f lavfi -i "sine=frequency=${hertz}:sample_rate=22050:duration=2" \
    -ac 1 -c:a libmp3lame -b:a 32k "audio-${name}.mp3"
done
