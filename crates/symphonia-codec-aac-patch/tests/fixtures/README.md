# Фикстура AAC с Program Config Element

`pce-5p1-fc-880hz.adts` (~7 КБ): AAC-LC, 48 кГц, раскладка 5.1(side), синус 880 Гц только в
канале FC, длительность 1 с. ADTS-заголовки несут `channel_configuration = 0`, а каждый
raw_data_block начинается с in-band PCE (front SCE, front CPE, back CPE, LFE). Тот же PCE лежит в
ASC (extradata, 25 байт), он зашит в тесты как `FFMPEG_PCE_5P1_EXTRA_DATA`.

Пересоздание: `crates/symphonia-codec-aac-patch/tests/fixtures/generate.sh` (нужен `ffmpeg`).
Файл не детерминирован побитово (версия ffmpeg), поэтому тесты проверяют только тон 880 Гц в FC
и тишину в остальных каналах.

Лицензия: сигнал сгенерирован ffmpeg-источником `sine`, авторских прав не несёт.

# Фикстура неявного HE-AAC

`he-aac-implicit-sbr-4-frames.adts` (~1 КБ): первые 4 кадра ADTS HE-AAC v1 (стерео, ядро
22,05 кГц, выход 44,1 кГц). Заголовок ADTS говорит «AAC-LC»; данные SBR лежат только внутри
кадров (`ID_FIL` с `EXT_SBR_DATA`). Источник — `crates/audio-ffmpeg/tests/fixtures/he-aac/`
(кодер Fraunhofer `fdkaac`, свой синтетический сигнал). Тест проверяет, что декодер не выдаёт
глухое ядро, а сообщает `IN_BAND_SBR_UNSUPPORTED` (решение 349 плана моста).
