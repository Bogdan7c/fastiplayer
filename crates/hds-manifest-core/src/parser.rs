//! S04X-backed bounded parser F4M hierarchy documents.

use std::num::NonZeroUsize;
use std::time::Duration;

use bounded_xml_reader::{BoundedXmlReader, XmlBudgets, XmlElement, XmlEvent};
use thiserror::Error;

use crate::model::{
    F4M_NAMESPACES, F4mBootstrapSource, F4mManifest, F4mManifestLimits, F4mMediaEntryRejection,
    F4mStreamType, bootstrap_info, media_entry,
};

/// Typed parser failure без quick-xml leakage.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum F4mManifestError {
    /// XML security/schema reader отклонил input.
    #[error("F4M XML boundary rejected the manifest")]
    Xml(#[source] bounded_xml_reader::XmlReadError),
    /// Well-formed XML root не является supported Adobe F4M manifest.
    #[error("F4M manifest has an unsupported root namespace or element")]
    InvalidRoot,
    /// Manifest явно требует DRM/protected-video semantics.
    #[error("F4M manifest requires unsupported DRM feature: {0}")]
    DrmProtected(&'static str),
    /// Namespaced/private extension нельзя принять как Adobe public profile.
    #[error("F4M manifest contains a private extension: {0}")]
    PrivateExtension(&'static str),
    /// Unsupported profile feature не может быть безопасно проигнорирован.
    #[error("F4M manifest contains an unsupported feature: {0}")]
    UnsupportedFeature(&'static str),
    /// Required value отсутствует или содержит неверный scalar.
    #[error("F4M manifest contains an invalid value for {field}")]
    InvalidValue { field: &'static str },
    /// One domain string вышел за caller-owned bound.
    #[error("F4M manifest string exceeds the configured limit")]
    StringTooLong,
    /// Media/bootstrap count вышел за caller-owned bound.
    #[error("F4M manifest contains too many {kind} entries")]
    CountExceeded { kind: &'static str },
    /// Inline bootstrap is too large before/after decoding.
    #[error("F4M inline bootstrap exceeds the configured limit")]
    BootstrapTooLarge,
    /// F4M requires at least one media row.
    #[error("F4M manifest has no media rows")]
    MissingMedia,
}

/// Разбирает F4M через единственный project XML boundary S04X.
pub fn parse_f4m_manifest(
    input: &[u8],
    xml_budgets: XmlBudgets,
    limits: F4mManifestLimits,
) -> Result<F4mManifest, F4mManifestError> {
    let mut reader = BoundedXmlReader::new(input, xml_budgets).map_err(F4mManifestError::Xml)?;
    let mut state = ParserState::new(limits);
    while let Some(event) = reader.next_event().map_err(F4mManifestError::Xml)? {
        state.consume(event)?;
    }
    state.finish()
}

/// Mutable parser state держит только bounded transient fields.
struct ParserState {
    limits: F4mManifestLimits,
    root_seen: bool,
    stack: Vec<String>,
    stream_type: F4mStreamType,
    duration: Option<Duration>,
    base_url: Option<String>,
    media: Vec<crate::model::F4mMediaEntry>,
    rejected_media: Vec<F4mMediaEntryRejection>,
    media_entry_count: usize,
    bootstrap_info: Vec<crate::model::F4mBootstrapInfo>,
    active_media: Option<Result<ActiveMedia, F4mMediaEntryRejection>>,
    active_bootstrap: Option<ActiveBootstrap>,
    text_buffer: String,
}

impl ParserState {
    /// Создаёт пустой document state.
    fn new(limits: F4mManifestLimits) -> Self {
        Self {
            limits,
            root_seen: false,
            stack: Vec::new(),
            stream_type: F4mStreamType::Unspecified,
            duration: None,
            base_url: None,
            media: Vec::new(),
            rejected_media: Vec::new(),
            media_entry_count: 0,
            bootstrap_info: Vec::new(),
            active_media: None,
            active_bootstrap: None,
            text_buffer: String::new(),
        }
    }

    /// Применяет один bounded XML event.
    fn consume(&mut self, event: XmlEvent) -> Result<(), F4mManifestError> {
        match event {
            XmlEvent::StartElement(element) => self.start(element, false)?,
            XmlEvent::EmptyElement(element) => self.start(element, true)?,
            XmlEvent::EndElement(name) => self.end(name.local_name())?,
            XmlEvent::Text(text) => {
                if self
                    .stack
                    .last()
                    .is_some_and(|name| name == "bootstrapInfo")
                {
                    if let Some(bootstrap) = self.active_bootstrap.as_mut() {
                        bootstrap.text.push_str(text.content());
                    }
                } else {
                    self.text_buffer.push_str(text.content());
                }
            }
        }
        Ok(())
    }

    /// Открывает element и materializes только schema-owned attributes.
    fn start(&mut self, element: XmlElement, is_empty: bool) -> Result<(), F4mManifestError> {
        let local_name = element.name().local_name();
        if !self.root_seen {
            self.root_seen = true;
            if local_name != "manifest"
                || !element
                    .name()
                    .namespace_uri()
                    .is_some_and(|namespace| F4M_NAMESPACES.contains(&namespace))
            {
                return Err(F4mManifestError::InvalidRoot);
            }
        }
        if is_known_f4m_element(local_name)
            && !element
                .name()
                .namespace_uri()
                .is_some_and(|namespace| F4M_NAMESPACES.contains(&namespace))
        {
            return Err(F4mManifestError::PrivateExtension(
                "foreign namespace element",
            ));
        }

        match local_name {
            "media" => {
                if self.active_media.is_some() {
                    return Err(F4mManifestError::InvalidValue { field: "media" });
                }
                if self.media_entry_count >= self.limits.maximum_media_entries().get() {
                    return Err(F4mManifestError::CountExceeded { kind: "media" });
                }
                self.media_entry_count += 1;
                self.active_media = Some(ActiveMedia::from_element(&element, self.limits));
            }
            "bootstrapInfo" => {
                if self.active_bootstrap.is_some() {
                    return Err(F4mManifestError::InvalidValue {
                        field: "bootstrapInfo",
                    });
                }
                self.active_bootstrap = Some(ActiveBootstrap::from_element(&element, self.limits)?);
            }
            "drmAdditionalHeader" => {
                return Err(F4mManifestError::DrmProtected("drmAdditionalHeader"));
            }
            "signature" => return Err(F4mManifestError::DrmProtected("signature")),
            "pv-2.0" => return Err(F4mManifestError::DrmProtected("pv-2.0")),
            "cueInfo" => return Err(F4mManifestError::UnsupportedFeature("cueInfo")),
            _ => {}
        }

        self.stack.push(local_name.to_owned());
        if is_empty {
            self.end(local_name)?;
        }
        Ok(())
    }

    /// Закрывает element и commits complete media/bootstrap rows.
    fn end(&mut self, local_name: &str) -> Result<(), F4mManifestError> {
        let opened = self.stack.pop().ok_or(F4mManifestError::InvalidValue {
            field: "element nesting",
        })?;
        if opened != local_name {
            return Err(F4mManifestError::InvalidValue {
                field: "element nesting",
            });
        }

        let text = std::mem::take(&mut self.text_buffer);
        match local_name {
            "streamType" => {
                self.stream_type = match text.trim() {
                    "recorded" | "vod" | "VOD" => F4mStreamType::Vod,
                    "live" | "LIVE" => F4mStreamType::Live,
                    "" => F4mStreamType::Unspecified,
                    _ => {
                        return Err(F4mManifestError::InvalidValue {
                            field: "streamType",
                        });
                    }
                };
            }
            "duration" => {
                let seconds = text
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| F4mManifestError::InvalidValue { field: "duration" })?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err(F4mManifestError::InvalidValue { field: "duration" });
                }
                self.duration = Some(Duration::from_secs_f64(seconds));
            }
            "baseURL" => {
                self.base_url = Some(bounded_string(text.trim(), self.limits)?);
            }
            "media" => {
                let media = self
                    .active_media
                    .take()
                    .ok_or(F4mManifestError::InvalidValue { field: "media" })?
                    .and_then(ActiveMedia::finish);
                match media {
                    Ok(media) => self.media.push(media),
                    Err(rejection) => self.rejected_media.push(rejection),
                }
            }
            "bootstrapInfo" => {
                let bootstrap = self
                    .active_bootstrap
                    .take()
                    .ok_or(F4mManifestError::InvalidValue {
                        field: "bootstrapInfo",
                    })?
                    .finish(self.limits)?;
                if self.bootstrap_info.len() >= self.limits.maximum_bootstrap_entries().get() {
                    return Err(F4mManifestError::CountExceeded {
                        kind: "bootstrapInfo",
                    });
                }
                self.bootstrap_info.push(bootstrap);
            }
            _ => {}
        }
        Ok(())
    }

    /// Проверяет document-level invariants после fused EOF.
    fn finish(self) -> Result<F4mManifest, F4mManifestError> {
        if !self.root_seen
            || !self.stack.is_empty()
            || (self.media.is_empty() && self.rejected_media.is_empty())
        {
            return Err(F4mManifestError::MissingMedia);
        }
        Ok(F4mManifest::new(
            self.stream_type,
            self.duration,
            self.base_url,
            self.media,
            self.rejected_media,
            self.bootstrap_info,
        ))
    }
}

/// Незавершённая media row с validated scalar attributes.
struct ActiveMedia {
    url: Option<String>,
    href: Option<String>,
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    bootstrap_info_id: Option<String>,
}

impl ActiveMedia {
    /// Снимает только известные F4M attributes.
    fn from_element(
        element: &XmlElement,
        limits: F4mManifestLimits,
    ) -> Result<Self, F4mMediaEntryRejection> {
        Ok(Self {
            url: media_attribute_string(element, "url", limits)?,
            href: media_attribute_string(element, "href", limits)?,
            bitrate: media_attribute_u64(
                element,
                "bitrate",
                F4mMediaEntryRejection::InvalidBitrate,
            )?,
            width: media_attribute_u32(element, "width", F4mMediaEntryRejection::InvalidWidth)?,
            height: media_attribute_u32(element, "height", F4mMediaEntryRejection::InvalidHeight)?,
            bootstrap_info_id: media_attribute_string(element, "bootstrapInfoId", limits)?,
        })
    }

    /// Доказывает, что row является либо hierarchy edge, либо media locator.
    fn finish(self) -> Result<crate::model::F4mMediaEntry, F4mMediaEntryRejection> {
        if self.url.is_none() == self.href.is_none()
            || self.url.as_deref().is_some_and(str::is_empty)
            || self.href.as_deref().is_some_and(str::is_empty)
        {
            return Err(F4mMediaEntryRejection::InvalidLocatorShape);
        }
        Ok(media_entry(
            self.url,
            self.href,
            self.bitrate,
            self.width,
            self.height,
            self.bootstrap_info_id,
        ))
    }
}

/// Считывает bounded media attribute и не превращает локальную ошибку row в document failure.
fn media_attribute_string(
    element: &XmlElement,
    name: &str,
    limits: F4mManifestLimits,
) -> Result<Option<String>, F4mMediaEntryRejection> {
    attribute_string(element, name, limits).map_err(|_| F4mMediaEntryRejection::StringTooLong)
}

/// Разбирает media `u64`, сохраняя named безопасную причину отбрасывания.
fn media_attribute_u64(
    element: &XmlElement,
    name: &'static str,
    rejection: F4mMediaEntryRejection,
) -> Result<Option<u64>, F4mMediaEntryRejection> {
    attribute_u64(element, name).map_err(|_| rejection)
}

/// Разбирает media `u32`, сохраняя named безопасную причину отбрасывания.
fn media_attribute_u32(
    element: &XmlElement,
    name: &'static str,
    rejection: F4mMediaEntryRejection,
) -> Result<Option<u32>, F4mMediaEntryRejection> {
    attribute_u32(element, name).map_err(|_| rejection)
}

/// Возвращает true для F4M schema elements, а не arbitrary extension nodes.
fn is_known_f4m_element(local_name: &str) -> bool {
    matches!(
        local_name,
        "manifest"
            | "streamType"
            | "duration"
            | "baseURL"
            | "media"
            | "bootstrapInfo"
            | "drmAdditionalHeader"
            | "signature"
            | "pv-2.0"
            | "cueInfo"
    )
}

/// Незавершённый bootstrapInfo row.
struct ActiveBootstrap {
    id: Option<String>,
    url: Option<String>,
    text: String,
}

impl ActiveBootstrap {
    /// Снимает id/url без доступа к raw XML.
    fn from_element(
        element: &XmlElement,
        limits: F4mManifestLimits,
    ) -> Result<Self, F4mManifestError> {
        Ok(Self {
            id: attribute_string(element, "id", limits)?,
            url: attribute_string(element, "url", limits)?,
            text: String::new(),
        })
    }

    /// Завершает inline/url bootstrap source.
    fn finish(
        self,
        limits: F4mManifestLimits,
    ) -> Result<crate::model::F4mBootstrapInfo, F4mManifestError> {
        if self.url.is_some() && !self.text.trim().is_empty() {
            return Err(F4mManifestError::InvalidValue {
                field: "bootstrapInfo source",
            });
        }
        let source = if let Some(url) = self.url {
            F4mBootstrapSource::Url(url)
        } else {
            let decoded = decode_base64(self.text.trim(), limits.maximum_bootstrap_bytes())?;
            F4mBootstrapSource::Inline(decoded.into_boxed_slice())
        };
        Ok(bootstrap_info(self.id, source))
    }
}

/// Возвращает bounded non-secret attribute string.
fn attribute_string(
    element: &XmlElement,
    name: &str,
    limits: F4mManifestLimits,
) -> Result<Option<String>, F4mManifestError> {
    let value = element
        .attributes()
        .iter()
        .find(|attribute| {
            attribute.name().namespace_uri().is_none() && attribute.name().local_name() == name
        })
        .map(|attribute| attribute.value().to_owned());
    value
        .map(|value| {
            if value.len() > limits.maximum_string_bytes().get() {
                Err(F4mManifestError::StringTooLong)
            } else {
                Ok(value)
            }
        })
        .transpose()
}

/// Разбирает integer attribute и сохраняет malformed distinction.
fn attribute_u64(
    element: &XmlElement,
    name: &'static str,
) -> Result<Option<u64>, F4mManifestError> {
    element
        .attributes()
        .iter()
        .find(|attribute| {
            attribute.name().namespace_uri().is_none() && attribute.name().local_name() == name
        })
        .map(|attribute| {
            attribute
                .value()
                .parse()
                .map_err(|_| F4mManifestError::InvalidValue { field: name })
        })
        .transpose()
}

/// Разбирает bounded u32 attribute.
fn attribute_u32(
    element: &XmlElement,
    name: &'static str,
) -> Result<Option<u32>, F4mManifestError> {
    attribute_u64(element, name)?
        .map(|value| {
            u32::try_from(value).map_err(|_| F4mManifestError::InvalidValue { field: name })
        })
        .transpose()
}

/// Проверяет decoded base64 без добавления внешней dependency.
fn decode_base64(value: &str, maximum_bytes: NonZeroUsize) -> Result<Vec<u8>, F4mManifestError> {
    let compact = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if compact.len() % 4 != 0 || compact.len() > maximum_bytes.get().saturating_mul(2) {
        return Err(F4mManifestError::BootstrapTooLarge);
    }
    let mut output = Vec::with_capacity(compact.len() / 4 * 3);
    let chunk_count = compact.len() / 4;
    for (chunk_index, chunk) in compact.chunks_exact(4).enumerate() {
        let has_padding = chunk[2] == b'=' || chunk[3] == b'=';
        let padding_is_terminal = chunk_index + 1 == chunk_count;
        let padding_shape_is_valid =
            !has_padding || (padding_is_terminal && (chunk[2] != b'=' || chunk[3] == b'='));
        if !padding_shape_is_valid {
            return Err(F4mManifestError::InvalidValue {
                field: "bootstrapInfo",
            });
        }
        let a = base64_value(chunk[0]).ok_or(F4mManifestError::InvalidValue {
            field: "bootstrapInfo",
        })?;
        let b = base64_value(chunk[1]).ok_or(F4mManifestError::InvalidValue {
            field: "bootstrapInfo",
        })?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2]).ok_or(F4mManifestError::InvalidValue {
                field: "bootstrapInfo",
            })?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3]).ok_or(F4mManifestError::InvalidValue {
                field: "bootstrapInfo",
            })?
        };
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
        if output.len() > maximum_bytes.get() {
            return Err(F4mManifestError::BootstrapTooLarge);
        }
    }
    Ok(output)
}

/// Переводит один стандартный base64 alphabet byte в 6-bit value.
fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Конвертирует text в bounded string.
fn bounded_string(value: &str, limits: F4mManifestLimits) -> Result<String, F4mManifestError> {
    if value.len() > limits.maximum_string_bytes().get() {
        return Err(F4mManifestError::StringTooLong);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budgets() -> XmlBudgets {
        XmlBudgets::builder()
            .maximum_document_bytes(16 * 1024)
            .maximum_depth(16)
            .maximum_tokens(512)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(512)
            .maximum_attribute_bytes(16 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(16)
            .maximum_namespace_bytes(1024)
            .maximum_text_bytes(16 * 1024)
            .build()
            .expect("test XML budgets")
    }

    fn limits() -> F4mManifestLimits {
        F4mManifestLimits::new(
            NonZeroUsize::new(8).expect("media limit"),
            NonZeroUsize::new(8).expect("bootstrap limit"),
            NonZeroUsize::new(4096).expect("bootstrap bytes"),
            NonZeroUsize::new(1024).expect("string bytes"),
        )
    }

    #[test]
    fn parses_hierarchy_and_quality_attributes() {
        let input = br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><baseURL>media/</baseURL><media href="low.f4m" bitrate="100"/><media url="high" bitrate="200" width="1920" height="1080" bootstrapInfoId="boot"/><bootstrapInfo id="boot">YWJzdA==</bootstrapInfo></manifest>"#;
        let parsed = parse_f4m_manifest(input, budgets(), limits()).expect("F4M parses");
        assert_eq!(parsed.media().len(), 2);
        assert_eq!(parsed.media()[0].href(), Some("low.f4m"));
        assert_eq!(parsed.media()[1].height(), Some(1080));
        assert_eq!(parsed.bootstrap_info()[0].id(), Some("boot"));
    }

    #[test]
    fn rejects_live_only_features_and_missing_media() {
        let missing =
            br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><duration>1</duration></manifest>"#;
        assert_eq!(
            parse_f4m_manifest(missing, budgets(), limits()),
            Err(F4mManifestError::MissingMedia)
        );
        let drm =
            br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><drmAdditionalHeader/></manifest>"#;
        assert!(matches!(
            parse_f4m_manifest(drm, budgets(), limits()),
            Err(F4mManifestError::DrmProtected("drmAdditionalHeader"))
        ));
    }

    #[test]
    fn distinguishes_foreign_root_drm_private_extension_and_malformed_f4m() {
        let parse = |bytes: &[u8]| parse_f4m_manifest(bytes, budgets(), limits());
        assert!(matches!(
            parse(b"<html/>"),
            Err(F4mManifestError::InvalidRoot)
        ));
        assert!(matches!(
            parse(br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><drmAdditionalHeader/><media url="video"/></manifest>"#),
            Err(F4mManifestError::DrmProtected("drmAdditionalHeader"))
        ));
        assert!(matches!(
            parse(br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><x:media xmlns:x="urn:private" url="video"/></manifest>"#),
            Err(F4mManifestError::PrivateExtension("foreign namespace element"))
        ));
        assert!(matches!(
            parse(br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0">"#),
            Err(F4mManifestError::Xml(_))
        ));
    }

    #[test]
    fn rejects_non_terminal_or_inconsistent_base64_padding() {
        let non_terminal =
            br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><media url="stream"/><bootstrapInfo>YQ==AAAA</bootstrapInfo></manifest>"#;
        let inconsistent =
            br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><media url="stream"/><bootstrapInfo>YQ=A</bootstrapInfo></manifest>"#;

        assert!(matches!(
            parse_f4m_manifest(non_terminal, budgets(), limits()),
            Err(F4mManifestError::InvalidValue {
                field: "bootstrapInfo"
            })
        ));
        assert!(matches!(
            parse_f4m_manifest(inconsistent, budgets(), limits()),
            Err(F4mManifestError::InvalidValue {
                field: "bootstrapInfo"
            })
        ));
    }

    #[test]
    fn rejects_foreign_namespace_and_isolates_empty_media_locator() {
        let foreign =
            br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0" xmlns:x="urn:foreign"><x:media url="stream"/></manifest>"#;
        let empty = br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><media url=""/></manifest>"#;

        assert_eq!(
            parse_f4m_manifest(foreign, budgets(), limits()),
            Err(F4mManifestError::PrivateExtension(
                "foreign namespace element"
            ))
        );
        let parsed = parse_f4m_manifest(empty, budgets(), limits())
            .expect("malformed sibling row remains document-local evidence");
        assert!(parsed.media().is_empty());
        assert_eq!(
            parsed.rejected_media(),
            &[F4mMediaEntryRejection::InvalidLocatorShape]
        );
    }

    #[test]
    fn malformed_media_sibling_does_not_hide_valid_row() {
        let input = br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><media url="broken" width="wide"/><media url="valid" width="1280" height="720"/></manifest>"#;

        let parsed = parse_f4m_manifest(input, budgets(), limits()).expect("document parses");

        assert_eq!(parsed.media().len(), 1);
        assert_eq!(parsed.media()[0].url(), Some("valid"));
        assert_eq!(
            parsed.rejected_media(),
            &[F4mMediaEntryRejection::InvalidWidth]
        );
    }
}
