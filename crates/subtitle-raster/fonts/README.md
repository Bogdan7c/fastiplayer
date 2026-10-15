# Встроенные шрифты subtitle-raster

Шрифты семейства Noto (Noto Project Authors), без хинтинга (unhinted TTF, таблиц `fpgm`/`prep` нет).
Лицензия: SIL Open Font License 1.1, текст — в `OFL.txt` (получен из репозитория
`notofonts/latin-greek-cyrillic`; у каждого семейства своя строка Copyright
«The Noto Project Authors» с адресом репозитория семейства, она записана в самом шрифте, таблица `name`, id 0).

Источник: https://github.com/notofonts/notofonts.github.io, коммит
`5c3913063bb189df4edc94b766584dfb4c04a187` (14 октября 2026),
путь `fonts/<Family>/unhinted/ttf/<Family>-<Style>.ttf`. Файлы не изменялись.

Состав определён решением владельца: основной шрифт Noto Sans (латиница + греческий + кириллица),
Regular и Bold, плюс Regular основных алфавитов. CJK и эмодзи сознательно не встроены (берутся из системы).

| Файл | Версия шрифта | Размер, байт |
|---|---|---|
| `NotoSans-Bold.ttf` | 2.015 | 432376 |
| `NotoSans-Regular.ttf` | 2.015 | 431364 |
| `NotoSansArabic-Regular.ttf` | 2.013 | 142140 |
| `NotoSansArmenian-Regular.ttf` | 2.008 | 18772 |
| `NotoSansBengali-Regular.ttf` | 3.011 | 103704 |
| `NotoSansDevanagari-Regular.ttf` | 2.007 | 184228 |
| `NotoSansEthiopic-Regular.ttf` | 2.102 | 289244 |
| `NotoSansGeorgian-Regular.ttf` | 2.005 | 35300 |
| `NotoSansGujarati-Regular.ttf` | 2.106 | 142760 |
| `NotoSansGurmukhi-Regular.ttf` | 2.004 | 35956 |
| `NotoSansHebrew-Regular.ttf` | 3.001 | 16836 |
| `NotoSansKannada-Regular.ttf` | 2.006 | 135488 |
| `NotoSansKhmer-Regular.ttf` | 2.004 | 67784 |
| `NotoSansLao-Regular.ttf` | 2.003 | 21720 |
| `NotoSansMalayalam-Regular.ttf` | 2.104 | 77312 |
| `NotoSansMyanmar-Regular.ttf` | 2.107 | 148628 |
| `NotoSansSinhala-Regular.ttf` | 3.000 | 90296 |
| `NotoSansSymbols-Regular.ttf` | 2.003 | 145508 |
| `NotoSansSymbols2-Regular.ttf` | 2.008 | 671568 |
| `NotoSansTamil-Regular.ttf` | 2.004 | 46600 |
| `NotoSansTelugu-Regular.ttf` | 2.005 | 165212 |
| `NotoSansThai-Regular.ttf` | 2.002 | 20960 |

Итого: 3423756 байт (3.27 МиБ).

Шрифты подключаются через `include_bytes!` в `src/fonts/embedded.rs`; тесты проверяют, что каждый файл разбирается.
