//! Дескрипторы elementary stream-а из PMT (ES_info loop) и классификация потока.
//!
//! `stream_type` сам по себе не всегда называет кодек: DVB кладёт AC-3/E-AC-3 в
//! «private PES» (`stream_type 0x06`) и сообщает кодек дескриптором. Этот модуль
//! владеет только разбором тех дескрипторов, которые меняют решение «играем ли
//! поток и как его называть», плюс языком дорожки. Остальные дескрипторы
//! пропускаются по длине, не влияя на результат.
//!
//! Битый descriptor loop (CRC сошёлся — значит, так записал muxer) не роняет
//! всю PMT: поток с самоописывающим `stream_type` (H.264, AAC, ATSC AC-3, …)
//! остаётся играемым без языка, а private PES, чей кодек известен только из
//! дескрипторов, становится нераспознанным потоком. Иначе одна кривая метка
//! чужого потока или языка лишала бы звука и картинки весь program.

use tracing::warn;

use crate::MpegTsDemuxError;
use crate::psi::StreamKind;

/// ISO/IEC 13818-1 `registration_descriptor`: 4-байтный `format_identifier`.
const REGISTRATION_DESCRIPTOR_TAG: u8 = 0x05;
/// ISO/IEC 13818-1 `ISO_639_language_descriptor`.
const ISO_639_LANGUAGE_DESCRIPTOR_TAG: u8 = 0x0a;
/// ETSI EN 300 468 `AC-3_descriptor` (DVB).
const DVB_AC3_DESCRIPTOR_TAG: u8 = 0x6a;
/// ETSI EN 300 468 `enhanced_AC-3_descriptor` (DVB).
const DVB_ENHANCED_AC3_DESCRIPTOR_TAG: u8 = 0x7a;

/// `format_identifier` из SMPTE-RA, которыми помечают AC-3 / E-AC-3.
const AC3_FORMAT_IDENTIFIER: [u8; 4] = *b"AC-3";
const ENHANCED_AC3_FORMAT_IDENTIFIER: [u8; 4] = *b"EAC3";
/// Program-level `format_identifier` Blu-ray (BDAV/HDMV): stream_type 0x80–0xFF
/// трактуются по таблице Blu-ray, а не как произвольные user private.
const HDMV_FORMAT_IDENTIFIER: [u8; 4] = *b"HDMV";

/// Таблица stream_type → codec family, которые не требуют дескрипторов.
mod stream_type {
    pub(super) const MPEG1_AUDIO: u8 = 0x03;
    pub(super) const MPEG2_AUDIO: u8 = 0x04;
    pub(super) const PRIVATE_PES: u8 = 0x06;
    pub(super) const AAC_ADTS: u8 = 0x0f;
    pub(super) const H264: u8 = 0x1b;
    pub(super) const H265: u8 = 0x24;
    /// ATSC A/52 Annex A: AC-3 audio.
    pub(super) const ATSC_AC3: u8 = 0x81;
    /// ATSC A/52 Annex G: E-AC-3 audio.
    pub(super) const ATSC_ENHANCED_AC3: u8 = 0x87;
    /// Blu-ray (под program registration `HDMV`): основной звук Dolby Digital
    /// Plus — AC-3 ядро + E-AC-3 dependent substream (7.1). Вне HDMV это user
    /// private и без явной AC-3 сигнализации не играется.
    ///
    /// Blu-ray 0xA1 (secondary audio E-AC-3) намеренно не поддерживается: это
    /// комментарий/PiP-звук, который по спецификации подмешивается к основному
    /// с метаданными микширования; сыгранный как самостоятельная дорожка он
    /// выглядел бы «рабочим», но звучал бы не так, как задумано.
    pub(super) const BLU_RAY_ENHANCED_AC3: u8 = 0x84;
}

/// Таблица, по которой читаются stream_type program-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamTypeRegistry {
    /// ISO/IEC 13818-1 + ATSC/DVB (обычный TS).
    Standard,
    /// Program помечен registration `HDMV`: действуют типы Blu-ray.
    BluRay,
}

/// Поддерживаемый поток, распознанный по `stream_type` и ES_info loop-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecognizedStream {
    /// Codec family.
    pub(crate) kind: StreamKind,
    /// Язык из `ISO_639_language_descriptor`, если loop его содержал.
    pub(crate) language: Option<Iso639LanguageCode>,
}

/// Трёхбуквенный код языка из `ISO_639_language_descriptor` («как в потоке»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Iso639LanguageCode([u8; 3]);

impl Iso639LanguageCode {
    /// Код как строка; конструктор допускает только печатаемый ASCII.
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

/// Факты из ES_info loop, важные для выбора и описания дорожки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ElementaryDescriptors {
    registration: Option<[u8; 4]>,
    signals_dvb_ac3: bool,
    signals_dvb_enhanced_ac3: bool,
    language: Option<Iso639LanguageCode>,
}

impl ElementaryDescriptors {
    /// Первый язык из `ISO_639_language_descriptor`, если поток его указал.
    pub(crate) const fn language(&self) -> Option<Iso639LanguageCode> {
        self.language
    }

    fn signals_ac3_family(&self) -> bool {
        self.signals_dvb_ac3
            || self.signals_dvb_enhanced_ac3
            || self.registration == Some(AC3_FORMAT_IDENTIFIER)
            || self.registration == Some(ENHANCED_AC3_FORMAT_IDENTIFIER)
    }
}

/// Нужно ли вообще разбирать дескрипторы: только у потоков, которые мы
/// можем играть. Дескрипторы неизвестных потоков (субтитры, SCTE, данные)
/// не читаются, чтобы их особенности не влияли на воспроизведение.
pub(crate) const fn is_candidate_stream_type(raw_stream_type: u8) -> bool {
    matches!(
        raw_stream_type,
        stream_type::MPEG1_AUDIO
            | stream_type::MPEG2_AUDIO
            | stream_type::PRIVATE_PES
            | stream_type::AAC_ADTS
            | stream_type::H264
            | stream_type::H265
            | stream_type::ATSC_AC3
            | stream_type::ATSC_ENHANCED_AC3
            | stream_type::BLU_RAY_ENHANCED_AC3
    )
}

/// Таблица stream_type по program-level дескрипторам (`program_info` PMT).
///
/// Битый program descriptor loop не роняет PMT: таблица остаётся стандартной.
pub(crate) fn stream_type_registry(program_info: &[u8], program_number: u16) -> StreamTypeRegistry {
    match parse_elementary_descriptors(program_info) {
        Ok(descriptors) if descriptors.registration == Some(HDMV_FORMAT_IDENTIFIER) => {
            StreamTypeRegistry::BluRay
        }
        Ok(_) => StreamTypeRegistry::Standard,
        Err(error) => {
            warn!(
                program_number,
                %error,
                "Malformed MPEG-TS program descriptor loop ignored"
            );
            StreamTypeRegistry::Standard
        }
    }
}

/// Распознаёт поток PMT: codec family и язык, либо `None` — поток не играем.
///
/// Битый ES_info loop не ошибка PMT: поток классифицируется так, будто
/// дескрипторов нет (см. документацию модуля).
pub(crate) fn recognize_elementary_stream(
    registry: StreamTypeRegistry,
    raw_stream_type: u8,
    pid: u16,
    descriptor_loop: &[u8],
) -> Option<RecognizedStream> {
    // Дескрипторы читаются только у потенциально играемых stream_type:
    // особенности чужих потоков (субтитры, SCTE, данные) не должны влиять.
    if !is_candidate_stream_type(raw_stream_type) {
        return None;
    }
    let descriptors = parse_elementary_descriptors(descriptor_loop).unwrap_or_else(|error| {
        warn!(
            pid,
            stream_type = raw_stream_type,
            %error,
            "Malformed MPEG-TS ES descriptor loop ignored; stream classified by stream_type only"
        );
        ElementaryDescriptors::default()
    });
    classify_stream(registry, raw_stream_type, &descriptors).map(|kind| RecognizedStream {
        kind,
        language: descriptors.language(),
    })
}

/// Определяет codec family по `stream_type` и дескрипторам.
///
/// AC-3 и E-AC-3 сводятся к одному [`StreamKind::Ac3Syncframes`]: точный кодек
/// (`A_AC3` или `A_EAC3`) доказывает `bsid` самого syncframe-а, а не PMT —
/// встречаются muxer-ы, кладущие E-AC-3 под AC-3 сигнализацию.
fn classify_stream(
    registry: StreamTypeRegistry,
    raw_stream_type: u8,
    descriptors: &ElementaryDescriptors,
) -> Option<StreamKind> {
    match raw_stream_type {
        stream_type::H264 => Some(StreamKind::H264),
        stream_type::H265 => Some(StreamKind::H265),
        stream_type::AAC_ADTS => Some(StreamKind::AacAdts),
        stream_type::MPEG1_AUDIO | stream_type::MPEG2_AUDIO => Some(StreamKind::MpegAudio),
        stream_type::ATSC_AC3 | stream_type::ATSC_ENHANCED_AC3 => Some(StreamKind::Ac3Syncframes),
        stream_type::PRIVATE_PES if descriptors.signals_ac3_family() => {
            Some(StreamKind::Ac3Syncframes)
        }
        stream_type::BLU_RAY_ENHANCED_AC3
            if registry == StreamTypeRegistry::BluRay || descriptors.signals_ac3_family() =>
        {
            Some(StreamKind::Ac3Syncframes)
        }
        _ => None,
    }
}

/// Разбирает ES_info loop одного elementary stream-а.
///
/// Длина всего loop-а уже проверена `parse_pmt`; дескриптор, вылезающий за
/// пределы loop-а, — typed `Malformed`, а что с ним делать, решает вызывающий
/// (`recognize_elementary_stream` / `stream_type_registry`).
fn parse_elementary_descriptors(
    mut descriptor_loop: &[u8],
) -> Result<ElementaryDescriptors, MpegTsDemuxError> {
    let mut descriptors = ElementaryDescriptors::default();
    while !descriptor_loop.is_empty() {
        let [tag, length, rest @ ..] = descriptor_loop else {
            return Err(malformed("PMT ES descriptor header is truncated"));
        };
        let length = usize::from(*length);
        if length > rest.len() {
            return Err(malformed("PMT ES descriptor goes beyond its ES_info loop"));
        }
        let (body, remainder) = rest.split_at(length);
        record_descriptor(&mut descriptors, *tag, body);
        descriptor_loop = remainder;
    }
    Ok(descriptors)
}

fn record_descriptor(descriptors: &mut ElementaryDescriptors, tag: u8, body: &[u8]) {
    match tag {
        REGISTRATION_DESCRIPTOR_TAG => {
            // Дескриптор короче 4 байт не содержит format_identifier — он не
            // может ничего утверждать о кодеке и потому не учитывается.
            if let Some(identifier) = body.first_chunk::<4>() {
                descriptors.registration = Some(*identifier);
            }
        }
        ISO_639_LANGUAGE_DESCRIPTOR_TAG => {
            // Записи по 4 байта (3 буквы + audio_type); берём первую.
            // Не-ASCII «код» — это не язык, а мусор muxer-а: метка не ставится.
            if descriptors.language.is_none()
                && let Some(code) = body.first_chunk::<3>()
                && code.iter().all(u8::is_ascii_graphic)
            {
                descriptors.language = Some(Iso639LanguageCode(*code));
            }
        }
        DVB_AC3_DESCRIPTOR_TAG => descriptors.signals_dvb_ac3 = true,
        DVB_ENHANCED_AC3_DESCRIPTOR_TAG => descriptors.signals_dvb_enhanced_ac3 = true,
        // Прочие дескрипторы не влияют на выбор/описание дорожки.
        _ => {}
    }
}

fn malformed(reason: &str) -> MpegTsDemuxError {
    MpegTsDemuxError::Malformed {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(raw_stream_type: u8, descriptor_loop: &[u8]) -> Option<StreamKind> {
        let descriptors = parse_elementary_descriptors(descriptor_loop).expect("valid loop");
        classify_stream(StreamTypeRegistry::Standard, raw_stream_type, &descriptors)
    }

    #[test]
    fn atsc_stream_types_are_ac3_family_without_descriptors() {
        assert_eq!(classify(0x81, &[]), Some(StreamKind::Ac3Syncframes));
        assert_eq!(classify(0x87, &[]), Some(StreamKind::Ac3Syncframes));
    }

    #[test]
    fn private_pes_needs_ac3_descriptor_or_registration() {
        assert_eq!(
            classify(0x06, &[0x6a, 0x01, 0x00]),
            Some(StreamKind::Ac3Syncframes)
        );
        assert_eq!(
            classify(0x06, &[0x7a, 0x01, 0x00]),
            Some(StreamKind::Ac3Syncframes)
        );
        assert_eq!(
            classify(0x06, b"\x05\x04AC-3"),
            Some(StreamKind::Ac3Syncframes)
        );
        assert_eq!(
            classify(0x06, b"\x05\x04EAC3"),
            Some(StreamKind::Ac3Syncframes)
        );
        // Teletext (0x56), DVB subtitles (0x59), чужая регистрация — не аудио AC-3.
        assert_eq!(
            classify(0x06, &[0x56, 0x05, b'e', b'n', b'g', 0x09, 0x00]),
            None
        );
        assert_eq!(classify(0x06, &[0x59, 0x00]), None);
        assert_eq!(classify(0x06, b"\x05\x04Opus"), None);
        assert_eq!(classify(0x06, &[]), None);
        // Обрезанная регистрация не утверждает кодек.
        assert_eq!(classify(0x06, b"\x05\x02AC"), None);
    }

    #[test]
    fn unknown_descriptors_are_skipped_by_length() {
        let descriptor_loop = [0x52, 0x01, 0x07, 0xfe, 0x00, 0x6a, 0x00];
        assert_eq!(
            classify(0x06, &descriptor_loop),
            Some(StreamKind::Ac3Syncframes)
        );
    }

    #[test]
    fn first_language_entry_is_recorded_and_non_ascii_is_rejected() {
        let descriptors =
            parse_elementary_descriptors(b"\x0a\x08rus\x00eng\x00").expect("valid loop");
        assert_eq!(
            descriptors.language().map(|code| code.as_str().to_owned()),
            Some("rus".to_owned())
        );
        let garbage = parse_elementary_descriptors(&[0x0a, 0x04, 0x00, 0xff, 0x20, 0x00])
            .expect("valid loop");
        assert_eq!(garbage.language(), None);
    }

    #[test]
    fn descriptor_overrunning_loop_is_malformed() {
        assert!(matches!(
            parse_elementary_descriptors(&[0x6a, 0x05, 0x00]),
            Err(MpegTsDemuxError::Malformed { .. })
        ));
        assert!(matches!(
            parse_elementary_descriptors(&[0x6a]),
            Err(MpegTsDemuxError::Malformed { .. })
        ));
    }

    #[test]
    fn candidate_filter_keeps_unknown_streams_out_of_descriptor_parsing() {
        assert!(is_candidate_stream_type(0x06));
        assert!(is_candidate_stream_type(0x81));
        assert!(!is_candidate_stream_type(0x86)); // SCTE-35
        assert!(!is_candidate_stream_type(0x15)); // ID3 metadata
    }

    #[test]
    fn malformed_es_loop_falls_back_to_stream_type_only() {
        let overrunning = [0x0a, 0x09, b'e', b'n', b'g'];
        let recognize = |stream_type| {
            recognize_elementary_stream(
                StreamTypeRegistry::Standard,
                stream_type,
                0x100,
                &overrunning,
            )
        };
        assert_eq!(
            recognize(0x1b),
            Some(RecognizedStream {
                kind: StreamKind::H264,
                language: None
            })
        );
        assert_eq!(
            recognize(0x81).map(|stream| stream.kind),
            Some(StreamKind::Ac3Syncframes)
        );
        // Кодек private PES доказывается только дескрипторами — их нет.
        assert_eq!(recognize(0x06), None);
    }

    #[test]
    fn valid_es_loop_still_reports_language() {
        let recognized = recognize_elementary_stream(
            StreamTypeRegistry::Standard,
            0x0f,
            0x100,
            b"\x0a\x04deu\x00",
        )
        .expect("AAC stream");
        assert_eq!(recognized.kind, StreamKind::AacAdts);
        assert_eq!(
            recognized.language.map(|code| code.as_str().to_owned()),
            Some("deu".to_owned())
        );
    }

    #[test]
    fn program_registration_selects_blu_ray_stream_types() {
        assert_eq!(
            stream_type_registry(b"\x05\x04HDMV", 1),
            StreamTypeRegistry::BluRay
        );
        assert_eq!(
            stream_type_registry(b"\x05\x04CUEI", 1),
            StreamTypeRegistry::Standard
        );
        assert_eq!(stream_type_registry(&[], 1), StreamTypeRegistry::Standard);
        // Битый program loop не превращается в ошибку PMT.
        assert_eq!(
            stream_type_registry(&[0x05, 0x09, b'H'], 1),
            StreamTypeRegistry::Standard
        );
    }

    #[test]
    fn stream_type_0x84_is_ac3_family_only_under_blu_ray_or_explicit_signalling() {
        let classify_in = |registry, descriptors: &[u8]| {
            recognize_elementary_stream(registry, 0x84, 0x100, descriptors)
                .map(|stream| stream.kind)
        };
        assert_eq!(
            classify_in(StreamTypeRegistry::BluRay, &[]),
            Some(StreamKind::Ac3Syncframes)
        );
        assert_eq!(classify_in(StreamTypeRegistry::Standard, &[]), None);
        assert_eq!(
            classify_in(StreamTypeRegistry::Standard, b"\x05\x04EAC3"),
            Some(StreamKind::Ac3Syncframes)
        );
        // Secondary audio Blu-ray (0xA1) не играется самостоятельно.
        assert_eq!(
            recognize_elementary_stream(StreamTypeRegistry::BluRay, 0xa1, 0x100, &[]),
            None
        );
    }
}
