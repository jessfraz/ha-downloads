//! Fixed-message diagnostics for untrusted upstream output.
//!
//! Never forward an upstream line, even after credential redaction. Proxy logs
//! can contain API keys that this app has never seen. Only the enums below and
//! their fixed messages cross the logging boundary.
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    os::fd::AsRawFd,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch},
    time::{Instant, timeout},
};

const MAX_LINE_BYTES: usize = 4096;
const READ_BYTES: usize = 2048;
const EVENT_CAPACITY: usize = 16;
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const KNOWN_INTERVAL: Duration = Duration::from_secs(5);
const GENERIC_INTERVAL: Duration = Duration::from_secs(30);
const MAX_RECORD_BYTES: usize = 4096;

/// Best-effort journal output that cannot wait for a stderr pipe to become writable.
///
/// Linux opens its own pipe description rather than changing inherited fd 2 or
/// a dup of it. Non-pipe targets and unavailable descriptors disable this sink;
/// O_NONBLOCK cannot guarantee nonblocking writes to regular files.
#[derive(Default)]
pub struct NonblockingStderr {
    file: Option<File>,
}

impl NonblockingStderr {
    pub fn open() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::open_descriptor(libc::STDERR_FILENO)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::default()
        }
    }

    #[cfg(target_os = "linux")]
    fn open_descriptor(descriptor: std::os::fd::RawFd) -> Self {
        use std::{fs::OpenOptions, mem::MaybeUninit, os::unix::fs::OpenOptionsExt};
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes the supplied stat structure on success.
        if unsafe { libc::fstat(descriptor, metadata.as_mut_ptr()) } != 0 {
            return Self::default();
        }
        // SAFETY: fstat succeeded above.
        if unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO {
            return Self::default();
        }
        let Ok(file) = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(format!("/proc/self/fd/{descriptor}"))
        else {
            return Self::default();
        };
        // The checked size makes every accepted pipe write atomic, including
        // WouldBlock: no partial JSON prefix is emitted when the pipe is full.
        // SAFETY: the file owns a live descriptor; fpathconf only reads its limit.
        if unsafe { libc::fpathconf(file.as_raw_fd(), libc::_PC_PIPE_BUF) } < 4096 {
            return Self::default();
        }
        Self { file: Some(file) }
    }

    /// Write an app-owned typed JSON record, or drop the entire record.
    /// Never pass raw upstream text: upstream output belongs through `consume`.
    pub fn record<T: Serialize>(&mut self, event: &T) -> bool {
        let mut record = RecordBuffer::default();
        if serde_json::to_writer(&mut record, event).is_err() {
            return false;
        }
        record.bytes[record.len] = b'\n';
        self.write_bytes(&record.bytes[..record.len + 1])
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        if bytes.len() > MAX_RECORD_BYTES {
            return false;
        }
        let Some(file) = self.file.as_ref() else {
            return false;
        };
        // SAFETY: the owned nonblocking pipe descriptor and byte slice are live.
        // A single write at or below PIPE_BUF either succeeds in full or fails.
        let written = unsafe { libc::write(file.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if usize::try_from(written).ok() == Some(bytes.len()) {
            return true;
        }
        if written < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return false;
            }
        }
        // A closed or invalid sink stays disabled. Its error never becomes an
        // app failure or gets recursively written back into the same journal.
        self.file = None;
        false
    }
}

impl AsyncWrite for NonblockingStderr {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        // consume supplies complete bounded, fixed-message JSON records. A
        // dropped record is acknowledged so pipe capture continues normally.
        self.write_bytes(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct RecordBuffer {
    bytes: [u8; MAX_RECORD_BYTES],
    len: usize,
}

impl Default for RecordBuffer {
    fn default() -> Self {
        Self {
            bytes: [0; MAX_RECORD_BYTES],
            len: 0,
        }
    }
}

impl io::Write for RecordBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RECORD_BYTES - 1 - self.len {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Vpn,
    Openvpn,
    Dns,
    Health,
    Proxy,
    Firewall,
    Routing,
    Configuration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    VpnAuthenticationFailed,
    VpnTlsFailed,
    VpnConnectionFailed,
    VpnConnectionTimedOut,
    VpnConnected,
    VpnReconnecting,
    DnsUpstreamFailed,
    HealthPingUnavailable,
    HealthPingFailed,
    HealthDnsFailed,
    HealthTlsFailed,
    HealthConnectionFailed,
    InvalidProviderConfiguration,
    InvalidCountryConfiguration,
    InvalidVpnConfiguration,
    FirewallConfigurationFailed,
    RoutingFailed,
    ProxyRequestFailed,
    UpstreamWarning,
    UpstreamError,
    UpstreamLineDiscarded,
    UpstreamReadFailed,
}

impl Code {
    fn message(self) -> &'static str {
        match self {
            Self::VpnAuthenticationFailed => {
                "VPN authentication failed. Check the VPN service credentials in the app settings."
            }
            Self::VpnTlsFailed => "The VPN TLS handshake or certificate check failed.",
            Self::VpnConnectionFailed => "The VPN server could not be reached.",
            Self::VpnConnectionTimedOut => "The VPN connection timed out.",
            Self::VpnConnected => "The VPN tunnel initialization completed.",
            Self::VpnReconnecting => "The VPN is reconnecting.",
            Self::DnsUpstreamFailed => "The VPN DNS resolver could not reach its upstream service.",
            Self::HealthPingUnavailable => {
                "ICMP health checks are unavailable. Check the app's network capabilities."
            }
            Self::HealthPingFailed => "The VPN ping health check failed.",
            Self::HealthDnsFailed => "The VPN DNS health check failed.",
            Self::HealthTlsFailed => "The VPN TLS health check failed.",
            Self::HealthConnectionFailed => "The VPN connection health check failed.",
            Self::InvalidProviderConfiguration => {
                "The VPN provider configuration is invalid. Check the app settings."
            }
            Self::InvalidCountryConfiguration => {
                "The VPN country selection is invalid. Check the app settings."
            }
            Self::InvalidVpnConfiguration => {
                "The VPN configuration is invalid. Check the app settings."
            }
            Self::FirewallConfigurationFailed => "The VPN firewall configuration failed.",
            Self::RoutingFailed => "The VPN route configuration failed.",
            Self::ProxyRequestFailed => "An external request through the VPN proxy failed.",
            Self::UpstreamWarning => {
                "The VPN service reported a warning. Check the app status and connection settings."
            }
            Self::UpstreamError => {
                "The VPN service reported an error. Check the app status and connection settings."
            }
            Self::UpstreamLineDiscarded => {
                "An oversized or invalid upstream log line was discarded."
            }
            Self::UpstreamReadFailed => {
                "The app could not continue reading the VPN service output."
            }
        }
    }

    fn level(self) -> Level {
        match self {
            Self::VpnConnected => Level::Info,
            Self::VpnReconnecting
            | Self::HealthPingUnavailable
            | Self::ProxyRequestFailed
            | Self::UpstreamWarning
            | Self::UpstreamLineDiscarded => Level::Warning,
            _ => Level::Error,
        }
    }

    fn interval(self) -> Duration {
        match self {
            Self::UpstreamWarning | Self::UpstreamError | Self::UpstreamLineDiscarded => {
                GENERIC_INTERVAL
            }
            _ => KNOWN_INTERVAL,
        }
    }
}

/// An owned event whose text can only come from a fixed diagnostic code.
#[derive(Debug, Serialize)]
pub struct Event {
    epoch_ms: u64,
    source: Source,
    level: Level,
    code: Code,
    message: String,
}

impl Event {
    fn new(source: Source, code: Code, level: Level) -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            epoch_ms: elapsed
                .as_secs()
                .saturating_mul(1000)
                .saturating_add(u64::from(elapsed.subsec_millis())),
            source,
            level,
            code,
            message: code.message().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CaptureError {
    #[error("Cannot read VPN service diagnostics.")]
    Read,
    #[error("Cannot write app diagnostics.")]
    Write,
}

/// Emit one JSON event and newline to the supplied journal writer.
/// A stalled or failed sink returns a fixed error without exposing its details.
pub async fn emit<W: AsyncWrite + Unpin>(
    writer: &mut W,
    source: Source,
    code: Code,
) -> Result<(), CaptureError> {
    write_event(writer, &Event::new(source, code, code.level())).await
}

async fn write_event<W: AsyncWrite + Unpin>(
    writer: &mut W,
    event: &Event,
) -> Result<(), CaptureError> {
    let mut bytes = serde_json::to_vec(event).map_err(|_| CaptureError::Write)?;
    bytes.push(b'\n');
    timeout(WRITE_TIMEOUT, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| CaptureError::Write)?
    .map_err(|_| CaptureError::Write)
}

/// Drain one upstream stdout/stderr stream until EOF or cancellation.
///
/// The reader never waits for the log sink: a bounded queue drops events under
/// pressure, and a failed or stalled writer disables logging while reading
/// continues. Lines are bounded in bytes, including a final unterminated line.
/// Closing the cancellation channel also cancels capture. Stop the owned child
/// before cancelling so its output can normally finish draining to EOF.
pub async fn consume<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: R,
    mut writer: W,
    cancellation: watch::Receiver<bool>,
) -> Result<(), CaptureError> {
    let (sender, mut receiver) = mpsc::channel(EVENT_CAPACITY);
    let drain = drain(reader, sender, cancellation);
    let output = async {
        while let Some(event) = receiver.recv().await {
            write_event(&mut writer, &event).await?;
        }
        Ok(())
    };
    let (read_result, write_result) = tokio::join!(drain, output);
    read_result.and(write_result)
}

#[derive(Default)]
struct Reporter {
    // The key space is the closed Code enum, never upstream text or addresses.
    reported: BTreeMap<Code, Instant>,
}

impl Reporter {
    fn report(&mut self, sender: &mpsc::Sender<Event>, source: Source, code: Code, level: Level) {
        let now = Instant::now();
        if self
            .reported
            .get(&code)
            .is_some_and(|previous| now.duration_since(*previous) < code.interval())
        {
            return;
        }
        self.reported.insert(code, now);
        // Full or disconnected output must never block the service's pipe.
        let _ = sender.try_send(Event::new(source, code, level));
    }

    fn line(&mut self, sender: &mpsc::Sender<Event>, bytes: &[u8]) {
        match std::str::from_utf8(bytes) {
            Ok(line)
                if !line
                    .chars()
                    .any(|value| value.is_control() && value != '\t' && value != '\r') =>
            {
                if let Some((source, code, level)) = classify(line) {
                    self.report(sender, source, code, level);
                }
            }
            _ => self.report(
                sender,
                Source::Vpn,
                Code::UpstreamLineDiscarded,
                Level::Warning,
            ),
        }
    }
}

async fn drain<R: AsyncRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<Event>,
    mut cancellation: watch::Receiver<bool>,
) -> Result<(), CaptureError> {
    let mut chunk = [0_u8; READ_BYTES];
    let mut line = Vec::with_capacity(MAX_LINE_BYTES);
    let mut discarding = false;
    let mut reporter = Reporter::default();
    if *cancellation.borrow() {
        return Ok(());
    }
    loop {
        let count = tokio::select! {
            biased;
            changed = cancellation.changed() => {
                if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                continue;
            }
            result = reader.read(&mut chunk) => match result {
                Ok(count) => count,
                Err(_) => {
                    reporter.report(&sender, Source::Vpn, Code::UpstreamReadFailed, Level::Error);
                    return Err(CaptureError::Read);
                }
            },
        };
        if count == 0 {
            if !discarding && !line.is_empty() {
                reporter.line(&sender, &line);
            }
            return Ok(());
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                if !discarding && !line.is_empty() {
                    reporter.line(&sender, &line);
                }
                line.clear();
                discarding = false;
            } else if !discarding {
                if line.len() == MAX_LINE_BYTES {
                    reporter.report(
                        &sender,
                        Source::Vpn,
                        Code::UpstreamLineDiscarded,
                        Level::Warning,
                    );
                    line.clear();
                    discarding = true;
                } else {
                    line.push(*byte);
                }
            }
        }
    }
}

fn source(line: &str) -> Source {
    let Some((_, rest)) = line.split_once('[') else {
        return Source::Vpn;
    };
    match rest.split_once(']').map(|(tag, _)| tag.trim()) {
        Some("openvpn") => Source::Openvpn,
        Some("dns") => Source::Dns,
        Some("healthcheck" | "health") => Source::Health,
        Some("http proxy" | "proxy") => Source::Proxy,
        Some("firewall") => Source::Firewall,
        Some("routing") => Source::Routing,
        Some("settings" | "configuration") => Source::Configuration,
        _ => Source::Vpn,
    }
}

enum ParsedLevel {
    Diagnostic(Level),
    Info,
    Quiet,
    Missing,
}

fn level(line: &str) -> ParsedLevel {
    for token in line.split_ascii_whitespace().take(6) {
        let token = token.trim_matches(['[', ']', ':']);
        if token.eq_ignore_ascii_case("warn") || token.eq_ignore_ascii_case("warning") {
            return ParsedLevel::Diagnostic(Level::Warning);
        }
        if token.eq_ignore_ascii_case("error")
            || token.eq_ignore_ascii_case("fatal")
            || token.eq_ignore_ascii_case("panic")
        {
            return ParsedLevel::Diagnostic(Level::Error);
        }
        if token.eq_ignore_ascii_case("info") {
            return ParsedLevel::Info;
        }
        if token.eq_ignore_ascii_case("debug") || token.eq_ignore_ascii_case("trace") {
            return ParsedLevel::Quiet;
        }
    }
    ParsedLevel::Missing
}

fn contains_any(line: &str, values: &[&str]) -> bool {
    values.iter().any(|value| line.contains(value))
}

fn lifecycle_milestone(line: &str) -> Option<(Source, Code)> {
    let (_, rest) = line.split_once('[')?;
    let (component, message) = rest.split_once(']')?;
    let message = message.trim();
    // These upstream milestones describe the tunnel, not verified app readiness.
    // Match component and message rather than searching INFO settings or URLs.
    match component {
        "openvpn" if message == "initialization sequence completed" => {
            Some((Source::Openvpn, Code::VpnConnected))
        }
        "vpn" | "openvpn"
            if matches!(
                message,
                "restarting"
                    | "restarting the vpn"
                    | "reconnecting"
                    | "restart requested"
                    | "reconnect requested"
            ) =>
        {
            Some((source(line), Code::VpnReconnecting))
        }
        "openvpn"
            if message.starts_with("sigusr1[soft,")
                && message.ends_with("] received, process restarting") =>
        {
            Some((Source::Openvpn, Code::VpnReconnecting))
        }
        _ => None,
    }
}

fn classify(line: &str) -> Option<(Source, Code, Level)> {
    let lower = line.to_ascii_lowercase();
    let raw_openvpn = contains_any(
        &lower,
        &[
            "auth_failed",
            "tls error:",
            "verify error:",
            "sigusr1[soft,",
        ],
    );
    let severity = match level(line) {
        ParsedLevel::Diagnostic(level) => level,
        ParsedLevel::Info => {
            return lifecycle_milestone(&lower).map(|(source, code)| (source, code, Level::Info));
        }
        ParsedLevel::Quiet => return None,
        ParsedLevel::Missing if raw_openvpn => Level::Error,
        ParsedLevel::Missing => return None,
    };
    let mut source = source(&lower);
    if source == Source::Vpn && raw_openvpn {
        source = Source::Openvpn;
    }
    let invalid = contains_any(
        &lower,
        &[
            "invalid",
            "not valid",
            "unsupported",
            "not supported",
            "unknown",
            "not found",
        ],
    );
    let code = match source {
        // Never classify proxy URLs or response text as VPN credentials/errors.
        Source::Proxy => Code::ProxyRequestFailed,
        Source::Dns => Code::DnsUpstreamFailed,
        Source::Firewall => Code::FirewallConfigurationFailed,
        Source::Routing => Code::RoutingFailed,
        Source::Health => {
            if contains_any(&lower, &["ping", "icmp"]) {
                if contains_any(&lower, &["operation not permitted", "permission denied"]) {
                    Code::HealthPingUnavailable
                } else {
                    Code::HealthPingFailed
                }
            } else if contains_any(&lower, &["dns", "lookup", "resolver"]) {
                Code::HealthDnsFailed
            } else if contains_any(&lower, &["tls", "x509", "certificate"]) {
                Code::HealthTlsFailed
            } else if contains_any(&lower, &["connection", "dial tcp", "timeout", "timed out"]) {
                Code::HealthConnectionFailed
            } else if severity == Level::Error {
                Code::UpstreamError
            } else {
                Code::UpstreamWarning
            }
        }
        Source::Vpn | Source::Openvpn | Source::Configuration => {
            if invalid && contains_any(&lower, &["country", "countries"]) {
                Code::InvalidCountryConfiguration
            } else if invalid && lower.contains("provider") {
                Code::InvalidProviderConfiguration
            } else if contains_any(
                &lower,
                &["auth_failed", "authentication failed", "auth failed"],
            ) {
                Code::VpnAuthenticationFailed
            } else if contains_any(&lower, &["timed out", "timeout", "time-out"]) {
                Code::VpnConnectionTimedOut
            } else if contains_any(
                &lower,
                &[
                    "tls error:",
                    "tls handshake",
                    "verify error:",
                    "certificate verify failed",
                ],
            ) {
                Code::VpnTlsFailed
            } else if contains_any(&lower, &["retrying", "restarting", "reconnecting"]) {
                Code::VpnReconnecting
            } else if contains_any(
                &lower,
                &[
                    "connection refused",
                    "network is unreachable",
                    "cannot resolve host",
                    "socket error",
                    "connection failed",
                ],
            ) {
                Code::VpnConnectionFailed
            } else if invalid && (source == Source::Configuration || lower.contains("settings:")) {
                Code::InvalidVpnConfiguration
            } else if severity == Level::Error {
                Code::UpstreamError
            } else {
                Code::UpstreamWarning
            }
        }
    };
    Some((source, code, severity))
}

#[cfg(test)]
mod tests {
    use crate::diagnostics::{
        CaptureError, Code, GENERIC_INTERVAL, KNOWN_INTERVAL, Level, MAX_LINE_BYTES, Reporter,
        Source, consume, emit,
    };
    use serde::Deserialize;
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
        time::Duration,
    };
    use tokio::{
        io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf, duplex},
        sync::{mpsc, watch},
        time::{Instant, timeout},
    };

    #[derive(Debug, Deserialize)]
    struct RecordedEvent {
        epoch_ms: u64,
        source: String,
        level: String,
        code: String,
        message: String,
    }

    fn events(bytes: &[u8]) -> Vec<RecordedEvent> {
        std::str::from_utf8(bytes)
            .expect("JSON output is UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("structured event"))
            .collect()
    }

    async fn capture(bytes: &[u8]) -> Vec<u8> {
        let (_keep_alive, cancellation) = watch::channel(false);
        let mut output = Vec::new();
        consume(bytes, &mut output, cancellation)
            .await
            .expect("capture");
        output
    }

    #[tokio::test]
    async fn classifies_known_failures_without_ever_emitting_upstream_values() {
        let lines = [
            "2026-01-01T00:00:00Z ERROR [openvpn] AUTH: Received control message: AUTH_FAILED user=private-user-123 password=private-password-456",
            "2026-01-01T00:00:00Z ERROR [openvpn] TLS Error: certificate verify failed https://vpn.example/?apikey=private-key-789",
            "2026-01-01T00:00:00Z WARN [openvpn] connection timed out private-token-012",
            "2026-01-01T00:00:00Z WARN [vpn] restarting the VPN",
            "2026-01-01T00:00:00Z ERROR [dns] upstream unavailable https://dns.example/private-path",
            "2026-01-01T00:00:00Z WARN [healthcheck] ping: operation not permitted",
            "2026-01-01T00:00:00Z ERROR [healthcheck] DNS lookup failed private-user-123",
            "2026-01-01T00:00:00Z ERROR [healthcheck] TLS certificate failed private-password-456",
            "2026-01-01T00:00:00Z ERROR [settings] unsupported VPN provider private-provider-345",
            "2026-01-01T00:00:00Z ERROR [settings] country selection not valid private-country-678",
            "2026-01-01T00:00:00Z ERROR [http proxy] GET https://indexer.example/api?apikey=unknown-indexer-key-901&password=private-password-456 AUTH_FAILED",
            "2026-01-01T00:00:00Z ERROR [vpn] novel failure -----BEGIN PRIVATE KEY----- private-pem-234",
        ];
        let output = capture(lines.join("\n").as_bytes()).await;
        let parsed = events(&output);
        let codes: Vec<_> = parsed.iter().map(|event| event.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                "vpn_authentication_failed",
                "vpn_tls_failed",
                "vpn_connection_timed_out",
                "vpn_reconnecting",
                "dns_upstream_failed",
                "health_ping_unavailable",
                "health_dns_failed",
                "health_tls_failed",
                "invalid_provider_configuration",
                "invalid_country_configuration",
                "proxy_request_failed",
                "upstream_error",
            ]
        );
        assert!(
            parsed
                .iter()
                .all(|event| event.epoch_ms > 0 && !event.message.is_empty())
        );
        assert_eq!(parsed[0].source, "openvpn");
        assert_eq!(parsed[5].level, "warning");
        assert_eq!(parsed[10].source, "proxy");
        let text = String::from_utf8(output).expect("output");
        for secret in [
            "private-user-123",
            "private-password-456",
            "private-key-789",
            "private-token-012",
            "unknown-indexer-key-901",
            "private-pem-234",
            "private-provider-345",
            "private-country-678",
            "https://",
            "PRIVATE KEY",
        ] {
            assert!(!text.contains(secret), "upstream value escaped: {secret}");
        }
    }

    #[tokio::test]
    async fn info_lifecycle_milestones_use_fixed_codes_without_exposing_input() {
        for (line, source, code) in [
            (
                "2026-01-01T00:00:00Z INFO [openvpn] Initialization Sequence Completed",
                "openvpn",
                "vpn_connected",
            ),
            (
                "2026-01-01T00:00:00Z INFO [vpn] restarting",
                "vpn",
                "vpn_reconnecting",
            ),
            (
                "2026-01-01T00:00:00Z INFO [vpn] reconnect requested",
                "vpn",
                "vpn_reconnecting",
            ),
            (
                "2026-01-01T00:00:00Z INFO [openvpn] SIGUSR1[soft,private-cause-123] received, process restarting",
                "openvpn",
                "vpn_reconnecting",
            ),
        ] {
            let output = capture(line.as_bytes()).await;
            let parsed = events(&output);
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].level, "info");
            assert_eq!(parsed[0].source, source);
            assert_eq!(parsed[0].code, code);
            assert!(
                !String::from_utf8(output)
                    .expect("output")
                    .contains("private-cause-123")
            );
        }
    }

    #[tokio::test]
    async fn unknown_info_settings_and_quiet_milestone_lookalikes_are_discarded() {
        let lines = [
            "INFO [vpn] VPN settings: username=private-user-123 password=private-password-456",
            "INFO [http proxy] GET https://indexer.example/Initialization Sequence Completed?apikey=private-key-789",
            "INFO [settings] Initialization Sequence Completed",
            "INFO [vpn] settings: restarting the vpn private-user-123",
            "INFO [openvpn] username: Initialization Sequence Completed",
            "INFO [openvpn] AUTH_FAILED private-password-456",
            "DEBUG [openvpn] Initialization Sequence Completed",
            "TRACE [vpn] restarting",
        ];
        assert!(capture(lines.join("\n").as_bytes()).await.is_empty());
    }

    #[tokio::test]
    async fn bounds_invalid_oversized_and_multibyte_lines_and_recovers_at_next_line() {
        let mut input = vec![b'x'; MAX_LINE_BYTES * 20];
        input.extend_from_slice(b"\nERROR [vpn] \xff\xfeprivate-invalid-123\n");
        input.extend_from_slice(b"ERROR [vpn] \x1b[31mprivate-control-456\0\n");
        let prefix = "ERROR [openvpn] ";
        let multibyte = format!(
            "{prefix}{}☃ AUTH_FAILED private-unicode-789\n",
            "x".repeat(2047 - prefix.len())
        );
        input.extend_from_slice(multibyte.as_bytes());
        input.extend_from_slice(b"ERROR [dns] upstream unavailable private-eof-012");
        let output = capture(&input).await;
        let parsed = events(&output);
        assert_eq!(
            parsed
                .iter()
                .map(|event| event.code.as_str())
                .collect::<Vec<_>>(),
            [
                "upstream_line_discarded",
                "vpn_authentication_failed",
                "dns_upstream_failed",
            ]
        );
        assert_eq!(parsed[1].message, Code::VpnAuthenticationFailed.message());
        let text = String::from_utf8(output).expect("output");
        for value in ["private-", "☃", "\\u001b", "\\u0000", "\u{fffd}"] {
            assert!(!text.contains(value));
        }
    }

    #[tokio::test]
    async fn drops_quiet_lines_and_limits_unknown_events_by_code_instead_of_input() {
        let mut lines = String::from(
            "INFO [http proxy] https://example.test/?error=private-info-123\n-----BEGIN PRIVATE KEY-----\nprivate-pem-body\n",
        );
        for index in 0..1000 {
            lines.push_str(&format!("WARN [unknown] private-value-{index}\n"));
        }
        lines.push_str("ERROR [unknown] another-private-value\nAUTH_FAILED raw-private-credential");
        let parsed = events(&capture(lines.as_bytes()).await);
        assert_eq!(
            parsed
                .iter()
                .map(|event| event.code.as_str())
                .collect::<Vec<_>>(),
            [
                "upstream_warning",
                "upstream_error",
                "vpn_authentication_failed",
            ]
        );
    }

    #[tokio::test]
    async fn limiter_allows_later_reports_without_keeping_input_strings() {
        let (sender, mut receiver) = mpsc::channel(16);
        let mut reporter = Reporter::default();
        reporter.report(&sender, Source::Vpn, Code::UpstreamWarning, Level::Warning);
        reporter.report(&sender, Source::Vpn, Code::UpstreamWarning, Level::Warning);
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        reporter
            .reported
            .insert(Code::UpstreamWarning, Instant::now() - GENERIC_INTERVAL);
        reporter.report(&sender, Source::Vpn, Code::UpstreamWarning, Level::Warning);
        assert!(receiver.try_recv().is_ok());
        reporter.reported.insert(
            Code::VpnAuthenticationFailed,
            Instant::now() - KNOWN_INTERVAL,
        );
        reporter.report(
            &sender,
            Source::Openvpn,
            Code::VpnAuthenticationFailed,
            Level::Error,
        );
        assert!(receiver.try_recv().is_ok());
    }

    struct FailingWriter;
    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "private-sink-error",
            )))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct StalledWriter;
    impl AsyncWrite for StalledWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    async fn drain_with_sink<W: AsyncWrite + Unpin>(writer: W) {
        let (reader, mut producer) = duplex(32);
        let (_keep_alive, cancellation) = watch::channel(false);
        let input = "ERROR [openvpn] AUTH_FAILED private-backpressure-123\n".repeat(10000);
        let feed = async {
            producer
                .write_all(input.as_bytes())
                .await
                .expect("service pipe keeps draining");
            producer.shutdown().await.expect("EOF");
        };
        let capture = consume(reader, writer, cancellation);
        let (_, result) = timeout(Duration::from_secs(3), async {
            tokio::join!(feed, capture)
        })
        .await
        .expect("bounded stream completion");
        assert_eq!(result, Err(CaptureError::Write));
    }

    #[tokio::test]
    async fn failed_sink_does_not_stop_draining_the_service() {
        drain_with_sink(FailingWriter).await;
    }

    #[tokio::test]
    async fn stalled_sink_does_not_stop_draining_the_service() {
        drain_with_sink(StalledWriter).await;
    }

    #[tokio::test]
    async fn cancellation_stops_an_open_stream_without_emitting_partial_input() {
        let (reader, mut producer) = duplex(32);
        let (cancel, cancellation) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut output = Vec::new();
            let result = consume(reader, &mut output, cancellation).await;
            (result, output)
        });
        producer
            .write_all(b"partial-private-credential-123")
            .await
            .expect("partial line");
        cancel.send(true).expect("cancel");
        let (result, output) = timeout(Duration::from_secs(1), task)
            .await
            .expect("cancelled promptly")
            .expect("capture task");
        assert_eq!(result, Ok(()));
        assert!(output.is_empty());
    }

    struct FailingReader;
    impl AsyncRead for FailingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("private-read-error-123")))
        }
    }

    #[tokio::test]
    async fn stream_errors_emit_fixed_diagnostics_and_never_include_io_details() {
        let (_keep_alive, cancellation) = watch::channel(false);
        let mut output = Vec::new();
        assert_eq!(
            consume(FailingReader, &mut output, cancellation).await,
            Err(CaptureError::Read)
        );
        let parsed = events(&output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].code, "upstream_read_failed");
        assert!(
            !String::from_utf8(output)
                .expect("output")
                .contains("private-read-error-123")
        );
    }

    #[tokio::test]
    async fn reusable_emitter_uses_the_same_fixed_owned_json_contract() {
        let mut output = Vec::new();
        emit(&mut output, Source::Health, Code::HealthTlsFailed)
            .await
            .expect("emit");
        let parsed = events(&output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].source, "health");
        assert_eq!(parsed[0].code, "health_tls_failed");
        assert_eq!(parsed[0].level, "error");
        assert_eq!(parsed[0].message, Code::HealthTlsFailed.message());
    }

    #[cfg(target_os = "linux")]
    mod nonblocking {
        use crate::{
            diagnostics::{Code, MAX_RECORD_BYTES, NonblockingStderr, Source, consume, emit},
            monitor::{Checks, Tracker},
        };
        use serde::{Deserialize, Serialize, Serializer};
        use std::{
            fs::File,
            io::{self, Read},
            os::fd::{AsRawFd, FromRawFd},
            process::Stdio,
            time::Duration,
        };
        use tokio::{io::AsyncReadExt, process::Command, sync::watch, time::timeout};

        fn pipe() -> (File, File) {
            let mut descriptors = [-1; 2];
            // SAFETY: pipe2 writes the two descriptors into this array.
            assert_eq!(
                unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            // SAFETY: each successful pipe descriptor is transferred to one File.
            unsafe {
                (
                    File::from_raw_fd(descriptors[0]),
                    File::from_raw_fd(descriptors[1]),
                )
            }
        }

        fn flags(file: &File) -> (i32, i32) {
            // SAFETY: fcntl only reads flags from these owned live descriptors.
            let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
            assert!(status >= 0 && descriptor >= 0);
            (status, descriptor)
        }

        fn fill(sink: &NonblockingStderr) {
            let descriptor = sink.file.as_ref().expect("enabled pipe").as_raw_fd();
            // SAFETY: this is a live pipe descriptor; querying capacity does not mutate it.
            let capacity = unsafe { libc::fcntl(descriptor, libc::F_GETPIPE_SZ) };
            assert!(capacity > 0);
            let block = [0_u8; MAX_RECORD_BYTES];
            for _ in 0..=usize::try_from(capacity).expect("capacity") / block.len() + 1 {
                // SAFETY: block and the owned nonblocking descriptor are live.
                let written =
                    unsafe { libc::write(descriptor, block.as_ptr().cast(), block.len()) };
                if written < 0 {
                    assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
                    return;
                }
                assert_eq!(usize::try_from(written).expect("write count"), block.len());
            }
            panic!("owned pipe did not fill within its reported capacity");
        }

        #[derive(Serialize, Deserialize)]
        struct Record {
            payload: String,
        }

        struct InvalidRecord;
        impl Serialize for InvalidRecord {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("private-serialization-error"))
            }
        }

        #[test]
        fn bounded_records_are_atomic_and_invalid_or_large_records_emit_nothing() {
            let (mut reader, writer) = pipe();
            let before = flags(&writer);
            assert_eq!(before.0 & libc::O_NONBLOCK, 0);
            let mut sink = NonblockingStderr::open_descriptor(writer.as_raw_fd());
            assert!(sink.file.is_some());
            assert_eq!(flags(&writer), before);
            assert_ne!(
                flags(sink.file.as_ref().expect("enabled")).0 & libc::O_NONBLOCK,
                0
            );
            let empty = Record {
                payload: String::new(),
            };
            let overhead = serde_json::to_vec(&empty).expect("size").len();
            let maximum = Record {
                payload: "x".repeat(MAX_RECORD_BYTES - 1 - overhead),
            };
            assert!(sink.record(&maximum));
            assert!(!sink.record(&Record {
                payload: "x".repeat(MAX_RECORD_BYTES * 4)
            }));
            assert!(!sink.record(&InvalidRecord));
            assert_eq!(flags(&writer), before);
            drop(sink);
            drop(writer);
            let mut output = String::new();
            reader.read_to_string(&mut output).expect("record output");
            assert_eq!(output.len(), MAX_RECORD_BYTES);
            let recorded: Record =
                serde_json::from_str(output.trim_end()).expect("complete record");
            assert_eq!(recorded.payload, maximum.payload);
            assert!(!output.contains("private-serialization-error"));
        }

        #[test]
        fn unavailable_non_pipe_and_closed_sinks_disable_without_app_errors() {
            let mut unavailable = NonblockingStderr::open_descriptor(-1);
            assert!(!unavailable.record(&InvalidRecord));
            let regular = tempfile::tempfile().expect("owned regular file");
            let original = flags(&regular);
            let mut sink = NonblockingStderr::open_descriptor(regular.as_raw_fd());
            assert!(sink.file.is_none());
            assert!(!sink.record(&Record {
                payload: "discarded".into()
            }));
            assert_eq!(flags(&regular), original);
            let (reader, writer) = pipe();
            let mut sink = NonblockingStderr::open_descriptor(writer.as_raw_fd());
            drop(reader);
            assert!(!sink.record(&Record {
                payload: "closed".into()
            }));
            assert!(sink.file.is_none());
        }

        #[tokio::test]
        async fn full_pipe_child_probe() {
            // The parent test gives this subprocess its own full stderr pipe.
            // No test changes the test runner's shared inherited stderr.
            if std::env::var_os("DOWNLOADS_DIAGNOSTICS_PIPE_TEST").as_deref()
                != Some(std::ffi::OsStr::new("1"))
            {
                return;
            }
            // SAFETY: this reads the child process's inherited stderr flags.
            let before = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL) };
            assert!(before >= 0);
            assert_eq!(before & libc::O_NONBLOCK, 0);
            let mut sink = NonblockingStderr::open();
            assert!(sink.file.is_some());
            assert!(!sink.record(&Record {
                payload: "fixed app record".into()
            }));
            assert!(!sink.record(&Record {
                payload: "x".repeat(MAX_RECORD_BYTES * 4)
            }));
            assert!(!sink.record(&InvalidRecord));
            emit(&mut sink, Source::Health, Code::HealthTlsFailed)
                .await
                .expect("best-effort emission");
            let (_keep_alive, cancellation) = watch::channel(false);
            consume(
                &b"ERROR [http proxy] https://indexer.example/?apikey=private-key\n"[..],
                sink,
                cancellation,
            )
            .await
            .expect("blocked journal does not fail capture");
            let mut tracker = Tracker::new();
            let observed = tracker.observe(Checks::default());
            assert_eq!(observed.incident_count, 1);
            // SAFETY: readback must show no change to the original descriptor.
            assert_eq!(
                unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL) },
                before
            );
        }

        #[tokio::test]
        async fn full_pipe_never_blocks_records_capture_or_real_tracker_observation() {
            let (_reader, writer) = pipe();
            let before = flags(&writer);
            let sink = NonblockingStderr::open_descriptor(writer.as_raw_fd());
            fill(&sink);
            assert_eq!(flags(&writer), before);
            let mut child = Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "diagnostics::tests::nonblocking::full_pipe_child_probe",
                    "--nocapture",
                ])
                .env("DOWNLOADS_DIAGNOSTICS_PIPE_TEST", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(writer.try_clone().expect("owned stderr pipe")))
                .kill_on_drop(true)
                .spawn()
                .expect("child test process");
            let status = match timeout(Duration::from_secs(2), child.wait()).await {
                Ok(result) => result.expect("child exit"),
                Err(_) => {
                    child.kill().await.expect("kill blocked owned test child");
                    child.wait().await.expect("reap blocked test child");
                    panic!("journal output blocked the protection observation loop");
                }
            };
            let mut output = String::new();
            child
                .stdout
                .take()
                .expect("child output")
                .read_to_string(&mut output)
                .await
                .expect("read output");
            assert!(status.success(), "child probe failed: {output}");
            assert_eq!(flags(&writer), before);
        }
    }
}
