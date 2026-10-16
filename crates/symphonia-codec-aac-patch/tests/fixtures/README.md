# Фикстура AAC с Program Config Element

`pce-5p1-fc-880hz.adts` (~7 КБ): AAC-LC, 48 кГц, раскладка 5.1(side), синус 880 Гц только в
канале FC, длительность 1 с. ADTS-заголовки несут `channel_configuration = 0`, а каждый
raw_data_block начинается с in-band PCE (front SCE, front CPE, back CPE, LFE). Тот же PCE лежит в
ASC (extradata, 25 байт), он зашит в тесты как `FFMPEG_PCE_5P1_EXTRA_DATA`.

Пересоздание: `crates/symphonia-codec-aac-patch/tests/fixtures/generate.sh` (нужен `ffmpeg`).
Файл не детерминирован побитово (версия ffmpeg), поэтому тесты проверяют только тон 880 Гц в FC
и тишину в остальных каналах.

Лицензия: сигнал сгенерирован ffmpeg-источником `sine`, авторских прав не несёт.
