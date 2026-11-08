//! One-request transport client. The server owns policy, readiness and identity.
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use ha_downloads::media::model::{
    ApiErrorCode, Component, ComponentHealth, CreateRequest, DownloadDetails, Episode,
    HealthSnapshot, ImportSince, ImportedItem, ManagerList, MediaItem, Operation, Page, QueueId,
    QueueItem, ReadinessState, RequestId, RequestRecord, SearchKind, SearchQuery, SystemResponse,
    TmdbId, TvdbId,
};
use reqwest::{Client, Method, RequestBuilder, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const MAX_INPUT: u64 = 1024 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

type Result<T> = std::result::Result<T, Failure>;

#[derive(Parser)]
#[command(
    name = "ha-downloads",
    version,
    about = "One-request JSON client for Downloads",
    long_about = "JSON client for Downloads. Status/doctor preserve the read-only local, native and Home Assistant health interface. Media commands use DOWNLOADS_URL and DOWNLOADS_API_TOKEN or HOME_ASSISTANT_URL and HOME_ASSISTANT_TOKEN. Select --transport for media when both origins are configured. No retries, redirects, credential fallback, or automatic pagination."
)]
struct Cli {
    #[command(flatten)]
    connection: ConnectionArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct ConnectionArgs {
    /// Status defaults to native; media requires this when both origins are configured.
    #[arg(long, value_enum, global = true)]
    transport: Option<Transport>,
    /// Native status URL or media HTTP(S) origin. Status HA access uses its environment.
    #[arg(long, global = true)]
    url: Option<String>,
    /// Credential file for the selected transport, instead of its token environment.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    /// Media request deadline in seconds. Legacy status retains its 10 second deadline.
    #[arg(long, default_value = "30", value_parser = clap::value_parser!(u64).range(1..=300), global = true)]
    timeout: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Read current protection readiness. HA access uses HOME_ASSISTANT_URL/TOKEN.
    #[command(visible_alias = "doctor")]
    Status(StatusFileArgs),
    #[command(flatten)]
    Media(MediaCommand),
}

#[derive(Args)]
struct StatusFileArgs {
    /// Native local snapshot, normally read inside the app.
    #[arg(
        long,
        default_value = "/run/downloads/status.json",
        conflicts_with = "url"
    )]
    status_file: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Transport {
    Native,
    HomeAssistant,
}

impl Transport {
    fn token_env(self) -> &'static str {
        match self {
            Self::Native => "DOWNLOADS_API_TOKEN",
            Self::HomeAssistant => "HOME_ASSISTANT_TOKEN",
        }
    }

    fn token_file_env(self) -> &'static str {
        match self {
            Self::Native => "DOWNLOADS_TOKEN_FILE",
            Self::HomeAssistant => "HOME_ASSISTANT_TOKEN_FILE",
        }
    }
}

#[derive(Args)]
struct Input {
    /// Strict request JSON from a file, or - for stdin. Limited to 1 MiB.
    #[arg(long, default_value = "-")]
    input: PathBuf,
}

impl Input {
    fn read<T: DeserializeOwned>(&self) -> Result<T> {
        let source: Box<dyn Read> = if self.input.as_os_str() == "-" {
            Box::new(io::stdin())
        } else {
            Box::new(
                File::open(&self.input).map_err(|_| Failure::local(ErrorCode::InputReadFailed))?,
            )
        };
        let mut bytes = Vec::new();
        source
            .take(MAX_INPUT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::local(ErrorCode::InputReadFailed))?;
        if bytes.len() as u64 > MAX_INPUT {
            return Err(Failure::local(ErrorCode::InputTooLarge));
        }
        // Decoder messages can contain private values and never reach stderr.
        serde_json::from_slice(&bytes).map_err(|_| Failure::local(ErrorCode::InvalidJsonInput))
    }
}

#[derive(Args)]
struct PageArgs {
    /// Fetch one page only. Follow the returned cursor explicitly.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    limit: Option<u32>,
    #[arg(long)]
    page_token: Option<String>,
}

impl PageArgs {
    fn apply(&self, mut request: RequestBuilder) -> RequestBuilder {
        if let Some(limit) = self.limit {
            request = request.query(&[("limit", limit)]);
        }
        if let Some(token) = &self.page_token {
            request = request.query(&[("page_token", token)]);
        }
        request
    }
}

#[derive(Serialize)]
struct Output<T> {
    status: u16,
    etag: Option<String>,
    body: T,
}

#[derive(Subcommand)]
enum MediaCommand {
    /// Show the selected credential's capabilities and server version.
    System,
    /// Read health, one component, or operation readiness.
    Health {
        #[command(subcommand)]
        command: Option<HealthCommand>,
    },
    /// Search movies and TV shows, one page at a time.
    Search {
        query: SearchQuery,
        #[arg(long, default_value = "all")]
        kind: SearchKind,
        #[command(flatten)]
        page: PageArgs,
    },
    #[command(subcommand)]
    Managers(Managers),
    #[command(subcommand)]
    Movies(Movies),
    #[command(subcommand)]
    Series(Series),
    #[command(subcommand)]
    Requests(Requests),
    #[command(subcommand)]
    Queue(Queue),
    /// Recent successful native imports, including titles added directly to a manager.
    #[command(subcommand)]
    Imports(Imports),
}

#[derive(Subcommand)]
enum HealthCommand {
    Component {
        component: Component,
    },
    /// Print the server snapshot; exit 3 when this operation is not ready.
    Check {
        #[arg(long)]
        operation: Operation,
    },
}

#[derive(Subcommand)]
enum Managers {
    List,
}

#[derive(Subcommand)]
enum Movies {
    List(PageArgs),
    Get { id: TmdbId },
}

#[derive(Subcommand)]
enum Series {
    List(PageArgs),
    Get {
        id: TvdbId,
    },
    Episodes {
        id: TvdbId,
        #[arg(long)]
        season: Option<u16>,
        #[command(flatten)]
        page: PageArgs,
    },
}

#[derive(Subcommand)]
enum Requests {
    List(PageArgs),
    Get {
        id: RequestId,
    },
    /// Recover an uncertain submission by its original caller-owned key.
    Lookup(Key),
    /// Submit exact CreateRequest JSON, without lookup, generated keys, or retry.
    Create {
        #[command(flatten)]
        input: Input,
        #[command(flatten)]
        key: Key,
    },
}

#[derive(Subcommand)]
enum Queue {
    List(PageArgs),
    /// Read one download's release, transfer, and import details.
    Get {
        id: QueueId,
    },
}

#[derive(Subcommand)]
enum Imports {
    List {
        #[command(flatten)]
        page: PageArgs,
        /// Include successful native imports at or after this UTC instant.
        #[arg(long)]
        since: Option<ImportSince>,
    },
}

#[derive(Args)]
struct Key {
    /// Stable caller-owned replay key. Reuse with the same body after uncertainty.
    #[arg(long, value_parser = parse_key)]
    idempotency_key: String,
}

fn parse_key(value: &str) -> std::result::Result<String, &'static str> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        Err("expected 1..128 ASCII letters, digits, hyphens or underscores")
    } else {
        Ok(value.to_owned())
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    SelectTransport,
    MissingUrl,
    InvalidUrl,
    InvalidEnvironment,
    MissingToken,
    CredentialReadFailed,
    InvalidCredential,
    ClientSetupFailed,
    InvalidRoute,
    InputReadFailed,
    InputTooLarge,
    InvalidJsonInput,
    RequestTimeoutOutcomeUnknown,
    RequestFailedOutcomeUnknown,
    ResponseReadFailedOutcomeUnknown,
    ResponseTooLargeOutcomeUnknown,
    InvalidJsonResponseOutcomeUnknown,
    InvalidResponseEtag,
    RedirectRefused,
    HttpError,
    OutputFailed,
}

#[derive(Serialize)]
#[serde(untagged)]
enum FailureCode {
    Local(ErrorCode),
    Api(ApiErrorCode),
}

#[derive(Serialize)]
struct Failure {
    status: Option<u16>,
    error_code: FailureCode,
}

impl Failure {
    fn local(error_code: ErrorCode) -> Self {
        Self {
            status: None,
            error_code: FailureCode::Local(error_code),
        }
    }

    fn http(status: u16, error_code: ErrorCode) -> Self {
        Self {
            status: Some(status),
            error_code: FailureCode::Local(error_code),
        }
    }
}

struct Api {
    client: Client,
    base: Url,
    token: header::HeaderValue,
}

fn variable(name: &str) -> Result<Option<String>> {
    std::env::var(name).map(Some).or_else(|error| match error {
        std::env::VarError::NotPresent => Ok(None),
        _ => Err(Failure::local(ErrorCode::InvalidEnvironment)),
    })
}

impl Api {
    fn new(cli: &ConnectionArgs) -> Result<Self> {
        let native = variable("DOWNLOADS_URL")?;
        let ha = variable("HOME_ASSISTANT_URL")?;
        let transport = match cli.transport {
            Some(transport) => transport,
            None if cli.url.is_none() => match (&native, &ha) {
                (Some(_), None) => Transport::Native,
                (None, Some(_)) => Transport::HomeAssistant,
                _ => return Err(Failure::local(ErrorCode::SelectTransport)),
            },
            None => return Err(Failure::local(ErrorCode::SelectTransport)),
        };
        let raw = cli
            .url
            .clone()
            .or(match transport {
                Transport::Native => native,
                Transport::HomeAssistant => ha,
            })
            .ok_or_else(|| Failure::local(ErrorCode::MissingUrl))?;
        let mut base = Url::parse(&raw).map_err(|_| Failure::local(ErrorCode::InvalidUrl))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Failure::local(ErrorCode::InvalidUrl));
        }
        if transport == Transport::HomeAssistant && base.scheme() != "https" {
            let loopback = base.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            });
            if !loopback {
                return Err(Failure::local(ErrorCode::InvalidUrl));
            }
        }
        base.set_path(match transport {
            Transport::Native => "/api/v1/",
            Transport::HomeAssistant => "/api/downloads/v1/",
        });
        let token_file = match &cli.token_file {
            Some(path) => Some(path.clone()),
            None => variable(transport.token_file_env())?.map(PathBuf::from),
        };
        let token = Zeroizing::new(if let Some(path) = token_file {
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(|_| Failure::local(ErrorCode::CredentialReadFailed))?
                .take(16385)
                .read_to_end(&mut bytes)
                .map_err(|_| Failure::local(ErrorCode::CredentialReadFailed))?;
            if bytes.len() > 16384 {
                return Err(Failure::local(ErrorCode::InvalidCredential));
            }
            String::from_utf8(bytes)
                .map_err(|_| Failure::local(ErrorCode::InvalidCredential))?
                .trim_end_matches(['\r', '\n'])
                .to_owned()
        } else {
            variable(transport.token_env())?
                .ok_or_else(|| Failure::local(ErrorCode::MissingToken))?
        });
        if token.is_empty() || token.len() > 16384 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Failure::local(ErrorCode::InvalidCredential));
        }
        let mut token = header::HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
            .map_err(|_| Failure::local(ErrorCode::InvalidCredential))?;
        token.set_sensitive(true);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(cli.timeout.min(10)))
            .timeout(Duration::from_secs(cli.timeout))
            .build()
            .map_err(|_| Failure::local(ErrorCode::ClientSetupFailed))?;
        Ok(Self {
            client,
            base,
            token,
        })
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self
            .base
            .join(path)
            .map_err(|_| Failure::local(ErrorCode::InvalidRoute))?;
        Ok(self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, self.token.clone())
            .header(header::ACCEPT, "application/json"))
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<Output<T>> {
        let mut response = request.send().await.map_err(|error| {
            Failure::local(if error.is_timeout() {
                ErrorCode::RequestTimeoutOutcomeUnknown
            } else {
                ErrorCode::RequestFailedOutcomeUnknown
            })
        })?;
        let status = response.status();
        let etag = response
            .headers()
            .get(header::ETAG)
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| Failure::http(status.as_u16(), ErrorCode::InvalidResponseEtag))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            Failure::http(status.as_u16(), ErrorCode::ResponseReadFailedOutcomeUnknown)
        })? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                return Err(Failure::http(
                    status.as_u16(),
                    ErrorCode::ResponseTooLargeOutcomeUnknown,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            #[derive(Deserialize)]
            struct ApiError {
                error_code: ApiErrorCode,
            }
            let code = if status.is_redirection() {
                FailureCode::Local(ErrorCode::RedirectRefused)
            } else {
                serde_json::from_slice::<ApiError>(&bytes)
                    .map(|error| FailureCode::Api(error.error_code))
                    .unwrap_or(FailureCode::Local(ErrorCode::HttpError))
            };
            return Err(Failure {
                status: Some(status.as_u16()),
                error_code: code,
            });
        }
        let body = serde_json::from_slice(&bytes).map_err(|_| {
            Failure::http(
                status.as_u16(),
                ErrorCode::InvalidJsonResponseOutcomeUnknown,
            )
        })?;
        Ok(Output {
            status: status.as_u16(),
            etag,
            body,
        })
    }
}

fn write_json(writer: impl Write, value: &impl Serialize) -> Result<()> {
    let mut writer = writer;
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| Failure::local(ErrorCode::OutputFailed))?;
    writeln!(writer).map_err(|_| Failure::local(ErrorCode::OutputFailed))
}

impl Api {
    async fn print<T: DeserializeOwned + Serialize>(
        &self,
        request: RequestBuilder,
    ) -> Result<ExitCode> {
        write_json(io::stdout().lock(), &self.send::<T>(request).await?)?;
        Ok(ExitCode::SUCCESS)
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let command = match cli.command {
        Command::Status(args) => {
            let transport = match cli.connection.transport.unwrap_or(Transport::Native) {
                Transport::Native => ha_downloads::cli::Transport::Native,
                Transport::HomeAssistant => ha_downloads::cli::Transport::HomeAssistant,
            };
            return Ok(ha_downloads::cli::status(
                ha_downloads::cli::StatusArgs {
                    url: cli.connection.url,
                    token_file: cli.connection.token_file,
                    status_file: args.status_file,
                },
                transport,
            )
            .await);
        }
        Command::Media(command) => command,
    };
    let api = Api::new(&cli.connection)?;
    match command {
        MediaCommand::System => {
            api.print::<SystemResponse>(api.request(Method::GET, "system")?)
                .await
        }
        MediaCommand::Health { command: None } => {
            api.print::<HealthSnapshot>(api.request(Method::GET, "health/stack")?)
                .await
        }
        MediaCommand::Health {
            command: Some(HealthCommand::Component { component }),
        } => {
            api.print::<ComponentHealth>(
                api.request(Method::GET, &format!("health/components/{component}"))?,
            )
            .await
        }
        MediaCommand::Health {
            command: Some(HealthCommand::Check { operation }),
        } => {
            let request = api
                .request(Method::GET, "health/stack")?
                .query(&[("operation", operation.to_string())]);
            let output = api.send::<HealthSnapshot>(request).await?;
            // Native managers own settings and the backend owns dependency policy.
            // Accept exactly one matching, unexpired operation verdict.
            let mut matches = output
                .body
                .readiness
                .iter()
                .filter(|item| item.operation == operation);
            let first = matches.next();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_millis()).ok());
            let ready = matches.next().is_none()
                && first.is_some_and(|item| {
                    now.is_some_and(|now| {
                        output.body.generated_at_unix_ms <= now && now < item.expires_at_unix_ms
                    }) && item.state == ReadinessState::Ready
                });
            write_json(io::stdout().lock(), &output)?;
            Ok(if ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            })
        }
        MediaCommand::Search { query, kind, page } => {
            let request = api.request(Method::GET, "search")?.query(&[
                ("query", query.as_str()),
                ("kind", kind.to_string().as_str()),
            ]);
            api.print::<Page<MediaItem>>(page.apply(request)).await
        }
        MediaCommand::Managers(Managers::List) => {
            api.print::<ManagerList>(api.request(Method::GET, "managers")?)
                .await
        }
        MediaCommand::Movies(Movies::List(page)) => {
            api.print::<Page<MediaItem>>(page.apply(api.request(Method::GET, "movies")?))
                .await
        }
        MediaCommand::Movies(Movies::Get { id }) => {
            api.print::<MediaItem>(api.request(Method::GET, &format!("movies/{id}"))?)
                .await
        }
        MediaCommand::Series(Series::List(page)) => {
            api.print::<Page<MediaItem>>(page.apply(api.request(Method::GET, "series")?))
                .await
        }
        MediaCommand::Series(Series::Get { id }) => {
            api.print::<MediaItem>(api.request(Method::GET, &format!("series/{id}"))?)
                .await
        }
        MediaCommand::Series(Series::Episodes { id, season, page }) => {
            let mut request = api.request(Method::GET, &format!("series/{id}/episodes"))?;
            if let Some(season) = season {
                request = request.query(&[("season", season)]);
            }
            api.print::<Page<Episode>>(page.apply(request)).await
        }
        MediaCommand::Requests(Requests::List(page)) => {
            api.print::<Page<RequestRecord>>(page.apply(api.request(Method::GET, "requests")?))
                .await
        }
        MediaCommand::Requests(Requests::Get { id }) => {
            api.print::<RequestRecord>(api.request(Method::GET, &format!("requests/{id}"))?)
                .await
        }
        MediaCommand::Requests(Requests::Lookup(key)) => {
            api.print::<RequestRecord>(api.request(
                Method::GET,
                &format!("request-keys/{}", key.idempotency_key),
            )?)
            .await
        }
        MediaCommand::Requests(Requests::Create { input, key }) => {
            let body = input.read::<CreateRequest>()?;
            api.print::<RequestRecord>(
                api.request(Method::POST, "requests")?
                    .header("Idempotency-Key", key.idempotency_key)
                    .json(&body),
            )
            .await
        }
        MediaCommand::Queue(Queue::List(page)) => {
            api.print::<Page<QueueItem>>(page.apply(api.request(Method::GET, "queue")?))
                .await
        }
        MediaCommand::Queue(Queue::Get { id }) => {
            api.print::<DownloadDetails>(api.request(Method::GET, &format!("queue/{id}"))?)
                .await
        }
        MediaCommand::Imports(Imports::List { page, since }) => {
            let mut request = page.apply(api.request(Method::GET, "imports")?);
            if let Some(since) = since {
                request = request.query(&[("since", since.as_str())]);
            }
            api.print::<Page<ImportedItem>>(request).await
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if std::env::args_os().len() == 1 {
        return if Cli::command().print_help().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return ha_downloads::cli::argument_error(error),
    };
    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            let _ = write_json(io::stderr().lock(), &error);
            ExitCode::FAILURE
        }
    }
}
