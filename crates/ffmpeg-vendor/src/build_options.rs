//! Аргументы configure/meson для vendored-сборки.
//!
//! Модуль чистый: только формирует списки аргументов, ничего не запускает.
//! Поэтому политику сборки (лицензия, набор декодеров, отсутствие
//! `-march=native`) закрепляют обычные unit-тесты ниже.

use std::path::Path;

/// Программные декодеры, которые нужны плееру.
///
/// Список обязан совпадать с `FfmpegDecoderId` в `video-ffmpeg::codec_adapter`:
/// H.264, H.265, VP8, VP9 и AV1. Встроенный декодер `av1` у FFmpeg работает
/// только через аппаратное ускорение, поэтому программный AV1 даёт `libdav1d`.
pub const ENABLED_DECODERS: [&str; 5] = ["h264", "hevc", "vp8", "vp9", "libdav1d"];

/// Суффикс имён наших библиотек: `libavcodec_fastiplayer.so.62` вместо `libavcodec.so.62`.
///
/// Без него линковщик и загрузчик путают наш FFmpeg с системным: в путях
/// поиска `/usr/lib` от других зависимостей стоит раньше нашего каталога,
/// и бинарь молча линкуется с системным `libavcodec.so`. Уникальное имя
/// исключает подмену и при сборке, и при запуске (даже если системный
/// FFmpeg той же версии уже загружен в процесс другой библиотекой).
pub const LIBRARY_NAME_SUFFIX: &str = "_fastiplayer";

/// Библиотеки FFmpeg, которые собирает vendored-сборка (без суффикса).
pub const BUILT_LIBRARIES: [&str; 2] = ["avcodec", "avutil"];

/// Имена для `rustc-link-lib` (`-l`): `avcodec_fastiplayer`, `avutil_fastiplayer`.
#[must_use]
pub fn link_library_names() -> Vec<String> {
    BUILT_LIBRARIES
        .iter()
        .map(|library| format!("{library}{LIBRARY_NAME_SUFFIX}"))
        .collect()
}

/// Аргументы `meson setup` для dav1d.
///
/// dav1d собирается статической библиотекой с PIC: она целиком уходит внутрь
/// `libavcodec.so`, и рядом с плеером не нужен отдельный `libdav1d.so`.
pub fn dav1d_meson_setup_arguments(
    build_directory: &Path,
    source_directory: &Path,
    install_prefix: &Path,
) -> Vec<String> {
    vec![
        "setup".to_owned(),
        build_directory.display().to_string(),
        source_directory.display().to_string(),
        format!("--prefix={}", install_prefix.display()),
        // Фиксированный libdir, иначе на Debian meson выберет lib/x86_64-linux-gnu.
        "--libdir=lib".to_owned(),
        "--buildtype=release".to_owned(),
        "--default-library=static".to_owned(),
        // PIC обязателен: статическая dav1d линкуется в разделяемую libavcodec.
        "-Db_staticpic=true".to_owned(),
        // Утилиты, тесты и примеры dav1d плееру не нужны.
        "-Denable_tools=false".to_owned(),
        "-Denable_tests=false".to_owned(),
        "-Denable_examples=false".to_owned(),
    ]
}

/// Аргументы `configure` для минимального LGPL FFmpeg.
pub fn ffmpeg_configure_arguments(install_prefix: &Path) -> Vec<String> {
    let mut configure_arguments = vec![
        format!("--prefix={}", install_prefix.display()),
        // Разделяемые библиотеки кладутся рядом с бинарём (решение владельца);
        // так пользователь может подменить FFmpeg, как требует LGPL.
        "--enable-shared".to_owned(),
        "--disable-static".to_owned(),
        "--enable-pic".to_owned(),
        // Без --enable-gpl/--enable-nonfree сборка остаётся LGPL 2.1+.
        "--disable-gpl".to_owned(),
        "--disable-nonfree".to_owned(),
        // Ничего не подхватываем из системы молча: только явно включённое ниже.
        "--disable-autodetect".to_owned(),
        "--disable-iconv".to_owned(),
        // Всё выключаем, затем включаем ровно нужные декодеры. Зависимые
        // компоненты (например, vp9_superframe_split) configure добавит сам.
        "--disable-everything".to_owned(),
        "--disable-programs".to_owned(),
        "--disable-doc".to_owned(),
        // Плееру нужны только libavcodec и libavutil.
        "--disable-avformat".to_owned(),
        "--disable-avdevice".to_owned(),
        "--disable-avfilter".to_owned(),
        "--disable-swscale".to_owned(),
        "--disable-swresample".to_owned(),
        "--enable-libdav1d".to_owned(),
        // Статическая dav1d требует -pthread/-ldl из своего .pc-файла.
        "--pkg-config-flags=--static".to_owned(),
        format!("--build-suffix={LIBRARY_NAME_SUFFIX}"),
    ];
    // Порядок важен: --enable-decoder должен идти после --disable-everything.
    configure_arguments.push(format!("--enable-decoder={}", ENABLED_DECODERS.join(",")));
    configure_arguments
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ENABLED_DECODERS, dav1d_meson_setup_arguments, ffmpeg_configure_arguments,
        link_library_names,
    };

    #[test]
    fn ffmpeg_configure_stays_lgpl_and_portable() {
        let configure_arguments = ffmpeg_configure_arguments(Path::new("/prefix"));

        // LGPL: GPL/nonfree компоненты не включаются.
        assert!(configure_arguments.contains(&"--disable-gpl".to_owned()));
        assert!(configure_arguments.contains(&"--disable-nonfree".to_owned()));
        for argument in &configure_arguments {
            assert!(
                !argument.contains("--enable-gpl"),
                "GPL enabled: {argument}"
            );
            assert!(
                !argument.contains("--enable-nonfree"),
                "nonfree enabled: {argument}"
            );
            // Бинарь должен работать на любом x86-64, а не только на машине сборки.
            assert!(
                !argument.contains("march=native"),
                "host-tuned flags: {argument}"
            );
            assert!(
                !argument.contains("fast-math"),
                "fast-math changes output: {argument}"
            );
        }
        // Разделяемые библиотеки рядом с бинарём, а не статика внутри него.
        assert!(configure_arguments.contains(&"--enable-shared".to_owned()));
        assert!(configure_arguments.contains(&"--disable-static".to_owned()));
        assert!(configure_arguments.contains(&"--prefix=/prefix".to_owned()));
    }

    #[test]
    fn ffmpeg_configure_enables_exactly_the_player_decoders_after_disabling_everything() {
        let configure_arguments = ffmpeg_configure_arguments(Path::new("/prefix"));

        let disable_everything_position = configure_arguments
            .iter()
            .position(|argument| argument == "--disable-everything")
            .expect("--disable-everything present");
        let decoder_arguments: Vec<(usize, &String)> = configure_arguments
            .iter()
            .enumerate()
            .filter(|(_, argument)| argument.starts_with("--enable-decoder="))
            .collect();

        assert_eq!(decoder_arguments.len(), 1, "exactly one decoder allowlist");
        let (decoder_position, decoder_argument) = decoder_arguments[0];
        // Иначе --disable-everything выключит только что включённые декодеры.
        assert!(decoder_position > disable_everything_position);
        assert_eq!(
            decoder_argument,
            "--enable-decoder=h264,hevc,vp8,vp9,libdav1d"
        );
        // Программный AV1 возможен только через libdav1d.
        assert!(ENABLED_DECODERS.contains(&"libdav1d"));
        assert!(configure_arguments.contains(&"--enable-libdav1d".to_owned()));
    }

    #[test]
    fn vendored_libraries_get_a_name_that_cannot_collide_with_system_ffmpeg() {
        let configure_arguments = ffmpeg_configure_arguments(Path::new("/prefix"));

        assert!(configure_arguments.contains(&"--build-suffix=_fastiplayer".to_owned()));
        assert_eq!(
            link_library_names(),
            [
                "avcodec_fastiplayer".to_owned(),
                "avutil_fastiplayer".to_owned()
            ]
        );
    }

    #[test]
    fn dav1d_is_built_static_with_pic_for_linking_into_libavcodec() {
        let meson_arguments = dav1d_meson_setup_arguments(
            Path::new("/build/dav1d"),
            Path::new("/sources/dav1d"),
            Path::new("/prefix/dav1d"),
        );

        assert_eq!(meson_arguments[0], "setup");
        assert!(meson_arguments.contains(&"--default-library=static".to_owned()));
        assert!(meson_arguments.contains(&"-Db_staticpic=true".to_owned()));
        assert!(meson_arguments.contains(&"--buildtype=release".to_owned()));
        assert!(meson_arguments.contains(&"--prefix=/prefix/dav1d".to_owned()));
    }
}
