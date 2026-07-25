//! Проверенные FTP(S) values для service-neutral progressive transport policy.
//!
//! Модуль не выполняет FTP команды. Его задача — один раз разобрать untrusted
//! locator, сохранить exact secret identity и наружу отдать только безопасные
//! структурные доказательства: scheme, host и effective port.

use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

use crate::SecretFtpUrl;

/// Разрешённая схема progressive FTP transport-а без alias expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FtpScheme {
    /// Незашифрованный FTP.
    Ftp,
    /// Explicit TLS FTPS (`AUTH TLS` после connect). Implicit `:990` сюда не входит.
    Ftps,
}

impl FtpScheme {
    /// Возвращает canonical scheme label без пользовательского payload.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ftp => "ftp",
            Self::Ftps => "ftps",
        }
    }

    /// Возвращает effective default port scheme-а.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Ftp => 21,
            // Explicit FTPS использует control port 21; implicit 990 не поддерживается.
            Self::Ftps => 21,
        }
    }

    /// Классифицирует уже распарсенную scheme без alias expansion.
    fn parse(value: &str) -> Result<Self, FtpRequestTargetError> {
        match value {
            "ftp" => Ok(Self::Ftp),
            "ftps" => Ok(Self::Ftps),
            _ => Err(FtpRequestTargetError::UnsupportedScheme),
        }
    }
}

/// Нормализованный FTP endpoint: scheme + host + effective port.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FtpEndpoint {
    /// Exact admitted scheme family.
    scheme: FtpScheme,
    /// WHATWG-normalized host без userinfo/path/query/fragment.
    host: String,
    /// Explicit либо scheme-default port.
    effective_port: u16,
}

impl FtpEndpoint {
    /// Возвращает admitted FTP scheme.
    #[must_use]
    pub const fn scheme(&self) -> FtpScheme {
        self.scheme
    }

    /// Возвращает normalized host для diagnostics без path/userinfo.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Возвращает explicit либо scheme-default port.
    #[must_use]
    pub const fn effective_port(&self) -> u16 {
        self.effective_port
    }
}

impl fmt::Debug for FtpEndpoint {
    /// Показывает только endpoint; secret path/query/userinfo сюда не входят.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpEndpoint")
            .field("scheme", &self.scheme)
            .field("host", &self.host)
            .field("effective_port", &self.effective_port)
            .finish()
    }
}

/// Exact secret FTP request target плюс проверенные policy attributes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FtpRequestTarget {
    /// Exact caller serialization для единственной реальной request boundary.
    exact: SecretFtpUrl,
    /// Normalized endpoint для safe diagnostics и connect.
    endpoint: FtpEndpoint,
    /// Decoded absolute FTP command path без query/fragment.
    path: String,
    /// Decoded login username; пустой username уже заменён anonymous identity.
    username: String,
    /// Decoded login password с anonymous default-ом.
    password: String,
    /// Есть ли userinfo в exact locator (credentials не публикуются).
    has_userinfo: bool,
}

impl FtpRequestTarget {
    /// Проверяет absolute hierarchical FTP(S) URL, сохраняя exact input отдельно.
    pub fn parse_exact(exact: impl Into<String>) -> Result<Self, FtpRequestTargetError> {
        let exact = exact.into();
        let parsed = Url::parse(&exact).map_err(|_| FtpRequestTargetError::InvalidSyntax)?;
        let scheme = FtpScheme::parse(parsed.scheme())?;
        if parsed.cannot_be_a_base() {
            return Err(FtpRequestTargetError::NonHierarchical);
        }
        let host = parsed
            .host_str()
            .ok_or(FtpRequestTargetError::MissingHost)?
            .to_owned();
        let effective_port = parsed.port().unwrap_or_else(|| scheme.default_port());
        let has_userinfo = !parsed.username().is_empty() || parsed.password().is_some();
        let path = decode_ftp_command_text(parsed.path())?;
        if path.is_empty() || path == "/" {
            return Err(FtpRequestTargetError::MissingPath);
        }
        let parsed_username = decode_ftp_command_text(parsed.username())?;
        let parsed_password = parsed.password().map(decode_ftp_command_text).transpose()?;
        let (username, password) = if parsed_username.is_empty() {
            (
                "anonymous".to_owned(),
                parsed_password.unwrap_or_else(|| "anonymous@".to_owned()),
            )
        } else {
            (parsed_username, parsed_password.unwrap_or_default())
        };

        Ok(Self {
            exact: SecretFtpUrl::from_secret_for_open(exact),
            endpoint: FtpEndpoint {
                scheme,
                host,
                effective_port,
            },
            path,
            username,
            password,
            has_userinfo,
        })
    }

    /// Возвращает admitted scheme без раскрытия locator-а.
    #[must_use]
    pub const fn scheme(&self) -> FtpScheme {
        self.endpoint.scheme()
    }

    /// Возвращает normalized endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &FtpEndpoint {
        &self.endpoint
    }

    /// Сообщает только факт наличия userinfo, не раскрывая credentials.
    #[must_use]
    pub const fn has_userinfo(&self) -> bool {
        self.has_userinfo
    }

    /// Раскрывает exact locator только concrete FTP request owner-у.
    #[must_use]
    pub fn expose_secret_for_request(&self) -> &str {
        self.exact.expose_secret_for_open()
    }

    /// Возвращает opaque hash exact identity без раскрытия locator-а.
    #[must_use]
    pub fn stable_identity_hash(&self) -> u64 {
        self.exact.stable_identity_hash()
    }

    /// Возвращает remote path для RETR/SIZE без публикации в Debug.
    #[must_use]
    pub(crate) fn remote_path_for_command(&self) -> &str {
        &self.path
    }

    /// Возвращает decoded credentials только FTP login boundary.
    #[must_use]
    pub(crate) fn login_credentials(&self) -> (&str, &str) {
        (&self.username, &self.password)
    }
}

impl fmt::Debug for FtpRequestTarget {
    /// Не допускает утечку userinfo/path/query через diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpRequestTarget")
            .field("endpoint", &self.endpoint)
            .field("path", &"<redacted>")
            .field("has_userinfo", &self.has_userinfo)
            .finish()
    }
}

impl fmt::Display for FtpRequestTarget {
    /// Display намеренно совпадает с безопасным endpoint-only представлением.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}://{}:{}<redacted>",
            self.endpoint.scheme().as_str(),
            self.endpoint.host(),
            self.endpoint.effective_port()
        )
    }
}

/// Secret-safe ошибка проверки FTP request target-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FtpRequestTargetError {
    /// Locator не является syntactically valid absolute URL.
    #[error("некорректный absolute FTP request target")]
    InvalidSyntax,
    /// Scheme не входит в exact FTP(S) vocabulary.
    #[error("FTP request target использует неподдерживаемую схему")]
    UnsupportedScheme,
    /// Opaque URL нельзя использовать как hierarchical network target.
    #[error("FTP request target не является hierarchical URL")]
    NonHierarchical,
    /// Network target обязан иметь host.
    #[error("FTP request target не содержит host")]
    MissingHost,
    /// Progressive FTP path обязан указывать remote file.
    #[error("FTP request target не содержит remote path")]
    MissingPath,
    /// Percent-decoded command value обязан быть корректным UTF-8.
    #[error("FTP request target содержит не-UTF-8 command value")]
    InvalidCommandUtf8,
    /// FTP command arguments не должны содержать control characters.
    #[error("FTP request target содержит небезопасный command value")]
    UnsafeCommandText,
}

/// Декодирует URL component ровно один раз и не допускает FTP command injection.
fn decode_ftp_command_text(encoded: &str) -> Result<String, FtpRequestTargetError> {
    let decoded = percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|_| FtpRequestTargetError::InvalidCommandUtf8)?
        .into_owned();
    if decoded.chars().any(char::is_control) {
        return Err(FtpRequestTargetError::UnsafeCommandText);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::{FtpRequestTarget, FtpRequestTargetError, FtpScheme};

    #[test]
    fn parse_exact_admits_ftp_and_ftps_without_alias_expansion() {
        let ftp = FtpRequestTarget::parse_exact("ftp://media.invalid/video.webm").expect("ftp");
        assert_eq!(ftp.scheme(), FtpScheme::Ftp);
        assert_eq!(ftp.endpoint().effective_port(), 21);
        assert!(!ftp.has_userinfo());

        let ftps =
            FtpRequestTarget::parse_exact("ftps://media.invalid:2121/a/b.webm").expect("ftps");
        assert_eq!(ftps.scheme(), FtpScheme::Ftps);
        assert_eq!(ftps.endpoint().effective_port(), 2121);
    }

    #[test]
    fn parse_exact_rejects_http_and_implicit_aliases() {
        assert_eq!(
            FtpRequestTarget::parse_exact("https://media.invalid/video.webm"),
            Err(FtpRequestTargetError::UnsupportedScheme)
        );
        assert_eq!(
            FtpRequestTarget::parse_exact("ftpes://media.invalid/video.webm"),
            Err(FtpRequestTargetError::UnsupportedScheme)
        );
    }

    #[test]
    fn parse_exact_rejects_endpoint_without_remote_file_path() {
        assert_eq!(
            FtpRequestTarget::parse_exact("ftp://media.invalid").expect_err("missing file path"),
            FtpRequestTargetError::MissingPath
        );
        assert_eq!(
            FtpRequestTarget::parse_exact("ftps://media.invalid/").expect_err("root is not a file"),
            FtpRequestTargetError::MissingPath
        );
    }

    #[test]
    fn debug_and_display_redact_credentials_and_path() {
        let target = FtpRequestTarget::parse_exact(
            "ftp://user:password@media.invalid/private/video.webm?token=secret",
        )
        .expect("valid ftp");
        assert!(target.has_userinfo());
        let formatted = format!("{target:?} {target}");
        for secret in ["user:password", "private/video", "token=secret"] {
            assert!(!formatted.contains(secret));
        }
        assert!(formatted.contains("media.invalid"));
    }

    #[test]
    fn expose_secret_preserves_exact_caller_string() {
        let exact = "ftp://media.invalid/video.webm?keep=1";
        let target = FtpRequestTarget::parse_exact(exact).expect("valid");
        assert_eq!(target.expose_secret_for_request(), exact);
    }

    #[test]
    fn command_path_and_credentials_are_percent_decoded_once() {
        let target = FtpRequestTarget::parse_exact(
            "ftp://media%20user:p%C3%A4ss@media.invalid/My%20Video-%D1%82%D0%B5%D1%81%D1%82.webm",
        )
        .expect("valid encoded FTP target");
        assert_eq!(target.remote_path_for_command(), "/My Video-тест.webm");
        assert_eq!(target.login_credentials(), ("media user", "päss"));
    }

    #[test]
    fn percent_decoded_control_characters_are_rejected_before_command_boundary() {
        for exact in [
            "ftp://media.invalid/video%0D%0ANOOP.webm",
            "ftp://user%0Aother:pass@media.invalid/video.webm",
            "ftp://user:pass%00word@media.invalid/video.webm",
        ] {
            assert_eq!(
                FtpRequestTarget::parse_exact(exact).expect_err("control character must fail"),
                FtpRequestTargetError::UnsafeCommandText
            );
        }
    }
}
