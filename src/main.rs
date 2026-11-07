use anyhow::{Context as _, Result};
use clap::{Args, Parser, Subcommand};
use ha_downloads::{
    api::{self, Access},
    config::Config,
    monitor::{LifecycleEvent, ServiceFailure, log_lifecycle},
    status::Status,
};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};
use tokio::sync::watch;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "downloads",
    version,
    about = "VPN-protected downloads for Home Assistant"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Start the app using Home Assistant's options by default.
    Serve(Serve),
    /// Read sanitized local status, or query a remote status API.
    Doctor(Doctor),
    /// Print the OpenAPI description without starting the app.
    Openapi,
}
#[derive(Args)]
struct Serve {
    #[arg(long, default_value = "/data/options.json")]
    config: PathBuf,
    #[arg(long, env = "DOWNLOADS_INGRESS_LISTEN", default_value = "0.0.0.0:8099")]
    ingress_listen: SocketAddr,
    #[arg(long, env = "DOWNLOADS_API_LISTEN", default_value = "0.0.0.0:8080")]
    api_listen: SocketAddr,
    #[arg(long, env = "DOWNLOADS_INGRESS_PEER", default_value = "172.30.32.2")]
    ingress_peer: IpAddr,
}
#[derive(Args)]
struct Doctor {
    #[arg(long, env = "DOWNLOADS_URL")]
    url: Option<reqwest::Url>,
    #[arg(long, env = "DOWNLOADS_TOKEN_FILE", requires = "url")]
    token_file: Option<PathBuf>,
    #[arg(
        long,
        default_value = "/run/downloads/status.json",
        conflicts_with = "url"
    )]
    status_file: PathBuf,
}
async fn doctor(args: Doctor) -> Result<ExitCode> {
    #[derive(serde::Deserialize)]
    struct RemoteStatus {
        #[serde(flatten)]
        status: Status,
        health: ha_downloads::status::Health,
    }
    let (status, ready) = if let Some(base) = args.url {
        anyhow::ensure!(
            matches!(base.scheme(), "http" | "https")
                && base.username().is_empty()
                && base.password().is_none(),
            "Use an HTTP(S) API URL without credentials."
        );
        let path = args
            .token_file
            .context("Set DOWNLOADS_TOKEN_FILE or pass --token-file for the status API.")?;
        let token = Zeroizing::new(
            tokio::fs::read_to_string(path)
                .await
                .context("Cannot read status API token file.")?,
        );
        let url = base
            .join("api/v1/status")
            .context("Invalid status API URL.")?;
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()?
            .get(url)
            .bearer_auth(token.trim())
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Status API is unavailable."))?;
        anyhow::ensure!(
            response.status().is_success(),
            "Status API rejected the request (HTTP {}).",
            response.status().as_u16()
        );
        let snapshot = response
            .json::<RemoteStatus>()
            .await
            .map_err(|_| anyhow::anyhow!("Status API returned an invalid response."))?;
        // The app owns the check clock. A remote CLI's wall clock may differ.
        (snapshot.status, snapshot.health.ready)
    } else {
        let bytes = tokio::fs::read(args.status_file)
            .await
            .context("No local app status is available. Start the app or use --url.")?;
        let status: Status =
            serde_json::from_slice(&bytes).context("Local app status is invalid.")?;
        let ready = status.health().ready;
        (status, ready)
    };
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(if ready {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}
async fn persist_status(path: &Path, status: &Status) -> Result<()> {
    let parent = path.parent().context("Status path has no parent.")?;
    tokio::fs::create_dir_all(parent).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let temp = parent.join(".status.json.tmp");
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).await?;
    use tokio::io::AsyncWriteExt as _;
    file.write_all(&serde_json::to_vec(status)?).await?;
    file.flush().await?;
    drop(file);
    tokio::fs::rename(temp, path).await?;
    Ok(())
}
async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result?,_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
async fn serve(args: Serve) -> Result<ExitCode> {
    let loaded = Config::load(&args.config);
    let (config, initial) = match loaded {
        Ok(config) => match config.validate() {
            Ok(()) => {
                let status = Status::starting(&config.vpn.provider);
                (Some(config), status)
            }
            Err(error) => (
                Some(config),
                Status::needs_configuration(&error.to_string()),
            ),
        },
        Err(error) => (None, Status::needs_configuration(&error.to_string())),
    };
    let token_hash = config
        .as_ref()
        .filter(|c| c.api_token.len() >= 32)
        .and_then(|c| api::Context::token_hash(&c.api_token));
    let (tx, rx) = watch::channel(initial);
    let (stop_tx, stop_rx) = watch::channel(false);
    let ingress = api::start(
        args.ingress_listen,
        Arc::new(api::Context::new(
            Access::Ingress {
                peer: args.ingress_peer,
            },
            rx.clone(),
        )?),
    )?;
    let direct = match api::start(
        args.api_listen,
        Arc::new(api::Context::new(
            Access::Direct { token_hash },
            rx.clone(),
        )?),
    ) {
        Ok(server) => server,
        Err(error) => {
            let _ = ingress.close().await;
            return Err(error);
        }
    };
    let mut status_rx = rx.clone();
    let writer = tokio::spawn(async move {
        loop {
            let snapshot = status_rx.borrow_and_update().clone();
            if persist_status(Path::new("/run/downloads/status.json"), &snapshot)
                .await
                .is_err()
            {
                log_lifecycle(LifecycleEvent::StatusPersistenceFailed);
            }
            if status_rx.changed().await.is_err() {
                break;
            }
        }
    });
    let mut runtime = match config {
        Some(config) if config.validate().is_ok() => {
            let status = tx.clone();
            Some(tokio::spawn(ha_downloads::runtime::run(
                config, status, stop_rx,
            )))
        }
        _ => None,
    };
    if let Some(task) = runtime.as_mut() {
        tokio::select! {
            signal=shutdown_signal()=>signal?,
            result=task=>{
                match result {
                    Ok(Err(error))=>log_lifecycle(LifecycleEvent::ProtectionServiceFailed {
                        reason: error.downcast_ref::<ServiceFailure>().copied().unwrap_or(ServiceFailure::RuntimeFailed),
                    }),
                    Err(_)=>{
                        log_lifecycle(LifecycleEvent::ProtectionTaskStopped);
                        tx.send_replace(Status::degraded("","The protection service stopped. Restart the app.",false));
                    }
                    Ok(Ok(()))=>{}
                }
                shutdown_signal().await?;
                runtime=None;
            }
        }
    } else {
        shutdown_signal().await?;
    }
    let _ = stop_tx.send(true);
    if let Some(task) = runtime {
        task.await.context("Cannot join protection service.")??;
    }
    let _ = direct.close().await;
    let _ = ingress.close().await;
    drop(tx);
    drop(rx);
    writer.await.context("Cannot join status writer.")?;
    Ok(ExitCode::SUCCESS)
}
fn openapi() -> Result<ExitCode> {
    let api = api::description()?;
    let version = env!("CARGO_PKG_VERSION")
        .parse()
        .context("Invalid application version.")?;
    let spec = api.openapi("Downloads", version).json()?;
    println!("{}", serde_json::to_string_pretty(&spec)?);
    Ok(ExitCode::SUCCESS)
}
#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args_os().collect::<Vec<_>>();
    if args.len() == 1 {
        args.push("serve".into());
    }
    let result = match Cli::parse_from(args).command {
        None => Err(anyhow::anyhow!("Choose an app command.")),
        Some(Command::Serve(args)) => serve(args).await,
        Some(Command::Doctor(args)) => doctor(args).await,
        Some(Command::Openapi) => openapi(),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
