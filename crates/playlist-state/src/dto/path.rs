use std::path::{Path, PathBuf};

use playlist_core::{
    ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath, LocalLocator, PlaylistLocator,
};
use serde::{Deserialize, Serialize};

use super::{
    DtoLoadError, MAX_LOCATOR_TEXT_BYTES, SerializationResourceBudget, StateSerializationError,
};

/// Unknown path encodings сохраняются opaque, но число units остаётся bounded.
const MAX_PATH_UNITS: usize = 256 * 1024;
/// Имя unknown platform/encoding — короткий protocol identifier, не payload.
const MAX_PROTOCOL_NAME_BYTES: usize = 128;

/// Каждый local variant содержит origin platform даже при valid UTF-8.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LocalPathV1Dto {
    platform: PathPlatformV1Dto,
    encoding: PathEncodingV1Dto,
}

/// Exact OS tag; Linux и macOS намеренно не объединены в Unix.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PathPlatformV1Dto {
    Linux,
    MacOs,
    Windows,
    Other { name: String },
}

/// Reversible path units, пригодные для JSON.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PathEncodingV1Dto {
    Utf8 {
        value: String,
    },
    Bytes {
        raw_units: Vec<u8>,
    },
    Wide {
        raw_units: Vec<u16>,
    },
    Opaque {
        encoding_name: String,
        raw_units: Vec<u32>,
    },
}

impl LocalPathV1Dto {
    pub(super) fn from_domain(locator: &LocalLocator) -> Result<Self, StateSerializationError> {
        match locator {
            LocalLocator::Native(path) => native_path_to_dto(path),
            LocalLocator::Foreign(path) => Ok(Self {
                platform: path.platform_for_persistence().into(),
                encoding: path.encoding_for_persistence().into(),
            }),
        }
    }

    pub(super) fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        if let PathPlatformV1Dto::Other { name } = &self.platform {
            validate_protocol_name(name)?;
        }
        self.encoding.validate_resource_limits()
    }

    pub(super) fn into_domain(self) -> Result<LocalLocator, DtoLoadError> {
        let platform = ForeignPathPlatform::from(self.platform.clone());
        let encoding = ForeignPathEncoding::from(self.encoding);

        if self.platform == current_platform_dto()
            && let Some(native_path) = native_path_from_matching_encoding(&encoding)
        {
            return Ok(LocalLocator::Native(native_path));
        }
        Ok(LocalLocator::Foreign(ForeignPlatformPath::new(
            platform, encoding,
        )))
    }
}

impl PathEncodingV1Dto {
    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        match self {
            Self::Utf8 { value } => validate_text(value, MAX_LOCATOR_TEXT_BYTES),
            Self::Bytes { raw_units } => validate_units(raw_units.len()),
            Self::Wide { raw_units } => validate_units(raw_units.len()),
            Self::Opaque {
                encoding_name,
                raw_units,
            } => {
                validate_protocol_name(encoding_name)?;
                validate_units(raw_units.len())
            }
        }
    }
}

pub(super) fn validate_domain_locator(
    locator: &PlaylistLocator,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    match locator {
        PlaylistLocator::Url(secret_url) => {
            let raw_url = secret_url.expose_secret_for_persistence();
            validate_domain_text(raw_url, MAX_LOCATOR_TEXT_BYTES, budget)
        }
        PlaylistLocator::Local(LocalLocator::Native(path)) => {
            validate_native_path_size(path, budget)
        }
        PlaylistLocator::Local(LocalLocator::Foreign(path)) => {
            if let ForeignPathPlatform::Other(name) = path.platform_for_persistence() {
                validate_domain_protocol_name(name, budget)?;
            }
            match path.encoding_for_persistence() {
                ForeignPathEncoding::Utf8(value) => {
                    validate_domain_text(value, MAX_LOCATOR_TEXT_BYTES, budget)
                }
                ForeignPathEncoding::Bytes(raw_units) => {
                    validate_domain_units(raw_units.len(), 1, budget)
                }
                ForeignPathEncoding::Wide(raw_units) => {
                    validate_domain_units(raw_units.len(), 2, budget)
                }
                ForeignPathEncoding::Opaque {
                    encoding_name,
                    raw_units,
                } => {
                    validate_domain_protocol_name(encoding_name, budget)?;
                    validate_domain_units(raw_units.len(), 4, budget)
                }
            }
        }
    }
}

fn validate_native_path_size(
    path: &Path,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    if let Some(value) = path.to_str() {
        return validate_domain_text(value, MAX_LOCATOR_TEXT_BYTES, budget);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        validate_domain_units(path.as_os_str().as_bytes().len(), 1, budget)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let unit_count = path.as_os_str().encode_wide().count();
        validate_domain_units(unit_count, 2, budget)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = budget;
        Err(StateSerializationError::UnsupportedNativePathEncoding)
    }
}

fn validate_protocol_name(value: &str) -> Result<(), DtoLoadError> {
    if value.is_empty() {
        return Err(DtoLoadError::DomainValue);
    }
    validate_text(value, MAX_PROTOCOL_NAME_BYTES)
}

fn validate_units(unit_count: usize) -> Result<(), DtoLoadError> {
    if unit_count > MAX_PATH_UNITS {
        return Err(DtoLoadError::ResourceLimit);
    }
    Ok(())
}

fn validate_text(value: &str, maximum_bytes: usize) -> Result<(), DtoLoadError> {
    if value.len() > maximum_bytes {
        return Err(DtoLoadError::ResourceLimit);
    }
    Ok(())
}

fn validate_domain_protocol_name(
    value: &str,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    if value.is_empty() {
        return Err(StateSerializationError::ResourceLimitExceeded);
    }
    validate_domain_text(value, MAX_PROTOCOL_NAME_BYTES, budget)
}

fn validate_domain_units(
    unit_count: usize,
    unit_size: usize,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    if unit_count > MAX_PATH_UNITS {
        return Err(StateSerializationError::ResourceLimitExceeded);
    }
    budget.add(unit_count.saturating_mul(unit_size))
}

fn validate_domain_text(
    value: &str,
    maximum_bytes: usize,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    if value.len() > maximum_bytes {
        return Err(StateSerializationError::ResourceLimitExceeded);
    }
    budget.add(value.len())
}

impl From<&ForeignPathPlatform> for PathPlatformV1Dto {
    fn from(value: &ForeignPathPlatform) -> Self {
        match value {
            ForeignPathPlatform::Linux => Self::Linux,
            ForeignPathPlatform::MacOs => Self::MacOs,
            ForeignPathPlatform::Windows => Self::Windows,
            ForeignPathPlatform::Other(name) => Self::Other { name: name.clone() },
        }
    }
}

impl From<PathPlatformV1Dto> for ForeignPathPlatform {
    fn from(value: PathPlatformV1Dto) -> Self {
        match value {
            PathPlatformV1Dto::Linux => Self::Linux,
            PathPlatformV1Dto::MacOs => Self::MacOs,
            PathPlatformV1Dto::Windows => Self::Windows,
            PathPlatformV1Dto::Other { name } => Self::Other(name),
        }
    }
}

impl From<&ForeignPathEncoding> for PathEncodingV1Dto {
    fn from(value: &ForeignPathEncoding) -> Self {
        match value {
            ForeignPathEncoding::Utf8(value) => Self::Utf8 {
                value: value.clone(),
            },
            ForeignPathEncoding::Bytes(raw_units) => Self::Bytes {
                raw_units: raw_units.clone(),
            },
            ForeignPathEncoding::Wide(raw_units) => Self::Wide {
                raw_units: raw_units.clone(),
            },
            ForeignPathEncoding::Opaque {
                encoding_name,
                raw_units,
            } => Self::Opaque {
                encoding_name: encoding_name.clone(),
                raw_units: raw_units.clone(),
            },
        }
    }
}

impl From<PathEncodingV1Dto> for ForeignPathEncoding {
    fn from(value: PathEncodingV1Dto) -> Self {
        match value {
            PathEncodingV1Dto::Utf8 { value } => Self::Utf8(value),
            PathEncodingV1Dto::Bytes { raw_units } => Self::Bytes(raw_units),
            PathEncodingV1Dto::Wide { raw_units } => Self::Wide(raw_units),
            PathEncodingV1Dto::Opaque {
                encoding_name,
                raw_units,
            } => Self::Opaque {
                encoding_name,
                raw_units,
            },
        }
    }
}

fn current_platform_dto() -> PathPlatformV1Dto {
    #[cfg(target_os = "linux")]
    {
        PathPlatformV1Dto::Linux
    }
    #[cfg(target_os = "macos")]
    {
        PathPlatformV1Dto::MacOs
    }
    #[cfg(windows)]
    {
        PathPlatformV1Dto::Windows
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        PathPlatformV1Dto::Other {
            name: std::env::consts::OS.to_owned(),
        }
    }
}

fn native_path_to_dto(path: &Path) -> Result<LocalPathV1Dto, StateSerializationError> {
    let platform = current_platform_dto();
    if let Some(value) = path.to_str() {
        return Ok(LocalPathV1Dto {
            platform,
            encoding: PathEncodingV1Dto::Utf8 {
                value: value.to_owned(),
            },
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(LocalPathV1Dto {
            platform,
            encoding: PathEncodingV1Dto::Bytes {
                raw_units: path.as_os_str().as_bytes().to_vec(),
            },
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        Ok(LocalPathV1Dto {
            platform,
            encoding: PathEncodingV1Dto::Wide {
                raw_units: path.as_os_str().encode_wide().collect(),
            },
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = platform;
        Err(StateSerializationError::UnsupportedNativePathEncoding)
    }
}

fn native_path_from_matching_encoding(encoding: &ForeignPathEncoding) -> Option<PathBuf> {
    match encoding {
        ForeignPathEncoding::Utf8(value) => Some(PathBuf::from(value)),
        #[cfg(unix)]
        ForeignPathEncoding::Bytes(raw_units) => {
            use std::os::unix::ffi::OsStringExt;
            Some(PathBuf::from(std::ffi::OsString::from_vec(
                raw_units.clone(),
            )))
        }
        #[cfg(windows)]
        ForeignPathEncoding::Wide(raw_units) => {
            use std::os::windows::ffi::OsStringExt;
            Some(PathBuf::from(std::ffi::OsString::from_wide(raw_units)))
        }
        _ => None,
    }
}
