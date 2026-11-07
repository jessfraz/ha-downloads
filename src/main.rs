use anyhow::{Context as _, Result};
use clap::{Args, Parser, Subcommand};
use ha_downloads::{
    api::{self, Access},
    cli::{StatusArgs, Transport},
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

#[derive(Parser)]
#[command(
    name = "downloads",
    version,
    about = "VPN-protected downloads for Home Assistant"
)]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = Transport::Native)]
    transport: Transport,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Start the app using Home Assistant's options by default.
    Serve(Serve),
    /// Read sanitized local status, or query a remote status API.
    #[command(visible_alias = "status")]
    Doctor(StatusArgs),
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
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => return ha_downloads::cli::argument_error(error),
    };
    let result = match cli.command {
        None => Err(anyhow::anyhow!("Choose an app command.")),
        Some(Command::Serve(_)) if cli.transport != Transport::Native => Err(anyhow::anyhow!(
            "Serving the app requires native transport."
        )),
        Some(Command::Serve(args)) => serve(args).await,
        Some(Command::Doctor(args)) => Ok(ha_downloads::cli::status(args, cli.transport).await),
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
