//! Read-only status queries shared by the app's doctor and the native CLI.
use crate::status::{Health, State, Status, StatusSnapshot};
use clap::{Args, ValueEnum, error::ErrorKind};
use reqwest::{Client, Response, Url};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};
use zeroize::Zeroizing;

const STATUS_FILE: &str = "/run/downloads/status.json";
const RESPONSE_LIMIT: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Transport {
    #[default]
    Native,
    HomeAssistant,
}

#[derive(Args)]
pub struct StatusArgs {
    /// Native app origin (DOWNLOADS_URL); credentials use a token file.
    #[arg(long)]
    pub url: Option<String>,
    /// Native API bearer token file (DOWNLOADS_TOKEN_FILE).
    #[arg(long)]
    pub token_file: Option<PathBuf>,
    /// Native local snapshot, normally read inside the app.
    #[arg(long, default_value = STATUS_FILE, conflicts_with = "url")]
    pub status_file: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "kebab-case")]
pub enum StatusReport {
    Native {
        #[serde(flatten)]
        status: Box<Status>,
        health: Health,
    },
    HomeAssistant {
        health: Health,
    },
}
impl StatusReport {
    fn ready(&self) -> bool {
        match self {
            Self::Native { health, .. } | Self::HomeAssistant { health } => health.ready,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Configuration,
    Authentication,
    PermissionDenied,
    NotExposed,
    Unavailable,
    InvalidResponse,
    HttpError,
    Timeout,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct StatusError {
    pub error: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}
impl StatusError {
    fn new(error: ErrorCode, message: &'static str) -> Self {
        Self {
            error,
            message: message.into(),
            http_status: None,
        }
    }
    fn transport(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::new(ErrorCode::Timeout, "Status request timed out.")
        } else {
            Self::new(ErrorCode::Unavailable, "Status API is unavailable.")
        }
    }
}

#[derive(Deserialize)]
struct AccessError {
    error: AccessErrorCode,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccessErrorCode {
    NotExposed,
    PermissionDenied,
    Unavailable,
    #[serde(other)]
    Unknown,
}

fn origin(input: &str) -> Result<Url, StatusError> {
    let mut url = Url::parse(input).map_err(|_| {
        StatusError::new(
            ErrorCode::Configuration,
            "Use an HTTP(S) API URL without credentials or query parameters.",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(StatusError::new(
            ErrorCode::Configuration,
            "Use an HTTP(S) API URL without credentials or query parameters.",
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn home_assistant_origin(input: &str) -> Result<Url, StatusError> {
    let url = origin(input)?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() != "https" && !loopback {
        return Err(StatusError::new(
            ErrorCode::Configuration,
            "Home Assistant requires HTTPS, except for HTTP on loopback.",
        ));
    }
    Ok(url)
}

async fn response_bytes(mut response: Response) -> Result<Vec<u8>, StatusError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| StatusError::transport(&error))?
    {
        if chunk.len() > RESPONSE_LIMIT.saturating_sub(bytes.len()) {
            return Err(StatusError::new(
                ErrorCode::InvalidResponse,
                "Status response is too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn request(base: Url, path: &str, token: &str) -> Result<Vec<u8>, StatusError> {
    let url = base
        .join(path)
        .map_err(|_| StatusError::new(ErrorCode::Configuration, "Invalid API URL."))?;
    let response = Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| {
            StatusError::new(ErrorCode::Configuration, "Cannot create the status client.")
        })?
        .get(url)
        .bearer_auth(token.trim())
        .header("cache-control", "no-cache")
        .send()
        .await
        .map_err(|error| StatusError::transport(&error))?;
    let code = response.status();
    let bytes = response_bytes(response).await?;
    if !code.is_success() {
        let (error, message) =
            match serde_json::from_slice::<AccessError>(&bytes).map(|body| body.error) {
                Ok(AccessErrorCode::NotExposed) => (
                    ErrorCode::NotExposed,
                    "Expose Downloads Problem to Assist before querying through Home Assistant.",
                ),
                Ok(AccessErrorCode::PermissionDenied) => (
                    ErrorCode::PermissionDenied,
                    "The current user cannot read Downloads health.",
                ),
                Ok(AccessErrorCode::Unavailable) => (
                    ErrorCode::Unavailable,
                    "Downloads health could not be verified.",
                ),
                _ if code == reqwest::StatusCode::UNAUTHORIZED => (
                    ErrorCode::Authentication,
                    "The API rejected the credential.",
                ),
                _ if code == reqwest::StatusCode::FORBIDDEN => (
                    ErrorCode::PermissionDenied,
                    "The API denied access to Downloads health.",
                ),
                _ => (ErrorCode::HttpError, "The status API rejected the request."),
            };
        return Err(StatusError {
            error,
            message: message.into(),
            http_status: Some(code.as_u16()),
        });
    }
    Ok(bytes)
}

fn validate_health(health: &Health) -> Result<(), StatusError> {
    // Check the server's evidence, without comparing its clock to this host's.
    if health.max_age_ms != 30_000
        || (health.ready
            && (health.state != State::Ready
                || !health.reasons.is_empty()
                || health.checked_at_unix_ms.is_none()
                || health.age_ms.is_none_or(|age| age > health.max_age_ms)))
    {
        return Err(StatusError::new(
            ErrorCode::InvalidResponse,
            "The API returned inconsistent readiness evidence.",
        ));
    }
    Ok(())
}

pub async fn query(args: StatusArgs, transport: Transport) -> Result<StatusReport, StatusError> {
    if transport == Transport::HomeAssistant {
        if args.url.is_some()
            || args.token_file.is_some()
            || args.status_file != Path::new(STATUS_FILE)
        {
            return Err(StatusError::new(
                ErrorCode::Configuration,
                "Native status options require --transport native.",
            ));
        }
        let base = std::env::var("HOME_ASSISTANT_URL").map_err(|_| {
            StatusError::new(
                ErrorCode::Configuration,
                "Set HOME_ASSISTANT_URL through the existing Home Assistant credential wrapper.",
            )
        })?;
        let token = Zeroizing::new(std::env::var("HOME_ASSISTANT_TOKEN").map_err(|_| {
            StatusError::new(
                ErrorCode::Configuration,
                "Set HOME_ASSISTANT_TOKEN through the existing Home Assistant credential wrapper.",
            )
        })?);
        if token.trim().is_empty() {
            return Err(StatusError::new(
                ErrorCode::Configuration,
                "The Home Assistant credential is empty.",
            ));
        }
        let body = request(
            home_assistant_origin(&base)?,
            "api/downloads/health",
            &token,
        )
        .await?;
        let health: Health = serde_json::from_slice(&body).map_err(|_| {
            StatusError::new(
                ErrorCode::InvalidResponse,
                "Home Assistant returned an invalid health response.",
            )
        })?;
        validate_health(&health)?;
        return Ok(StatusReport::HomeAssistant { health });
    }
    let url = args.url.or_else(|| std::env::var("DOWNLOADS_URL").ok());
    let token_file = args
        .token_file
        .or_else(|| std::env::var_os("DOWNLOADS_TOKEN_FILE").map(PathBuf::from));
    if url.is_some() && args.status_file != Path::new(STATUS_FILE) {
        return Err(StatusError::new(
            ErrorCode::Configuration,
            "Choose a native API URL or a local status file.",
        ));
    }
    if token_file.is_some() && url.is_none() {
        return Err(StatusError::new(
            ErrorCode::Configuration,
            "A native token file requires a native API URL.",
        ));
    }
    if let Some(base) = url {
        let base = origin(&base)?;
        let path = token_file.ok_or_else(|| {
            StatusError::new(
                ErrorCode::Configuration,
                "Set DOWNLOADS_TOKEN_FILE or pass --token-file for the status API.",
            )
        })?;
        let token = Zeroizing::new(tokio::fs::read_to_string(path).await.map_err(|_| {
            StatusError::new(
                ErrorCode::Configuration,
                "Cannot read the status API token file.",
            )
        })?);
        if token.trim().is_empty() {
            return Err(StatusError::new(
                ErrorCode::Configuration,
                "The native API credential is empty.",
            ));
        }
        let body = request(base, "api/v1/status", &token).await?;
        let snapshot: StatusSnapshot = serde_json::from_slice(&body).map_err(|_| {
            StatusError::new(
                ErrorCode::InvalidResponse,
                "Status API returned an invalid response.",
            )
        })?;
        validate_health(&snapshot.health)?;
        // Remote readiness uses the server's clock, never the CLI host's clock.
        return Ok(StatusReport::Native {
            status: Box::new(snapshot.status),
            health: snapshot.health,
        });
    }
    let bytes = tokio::fs::read(args.status_file).await.map_err(|_| {
        StatusError::new(
            ErrorCode::Unavailable,
            "No local app status is available. Use --url or --transport home-assistant.",
        )
    })?;
    let status: Status = serde_json::from_slice(&bytes).map_err(|_| {
        StatusError::new(ErrorCode::InvalidResponse, "Local app status is invalid.")
    })?;
    let health = status.health();
    Ok(StatusReport::Native {
        status: Box::new(status),
        health,
    })
}

fn write_json(mut output: impl Write, value: &impl Serialize) -> Result<(), serde_json::Error> {
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n").map_err(serde_json::Error::io)
}

/// Clap's default errors include argument values, which may contain credentials.
pub fn argument_error(error: clap::Error) -> ExitCode {
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        return if error.print().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let _ = write_json(
        std::io::stderr().lock(),
        &StatusError::new(
            ErrorCode::Configuration,
            "Invalid arguments. Use --help for usage.",
        ),
    );
    ExitCode::FAILURE
}

pub async fn status(args: StatusArgs, transport: Transport) -> ExitCode {
    match query(args, transport).await {
        Ok(report) => {
            if write_json(std::io::stdout().lock(), &report).is_err() {
                return ExitCode::FAILURE;
            }
            if report.ready() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Err(error) => {
            let _ = write_json(std::io::stderr().lock(), &error);
            ExitCode::FAILURE
        }
    }
}
