//! A read-only client; starting app services belongs to the downloads binary.
use clap::{CommandFactory, Parser, Subcommand};
use ha_downloads::cli::{StatusArgs, Transport};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "ha-downloads",
    version,
    about = "Read Downloads health as typed JSON"
)]
struct Cli {
    /// Choose native app access or the Home Assistant health bridge.
    #[arg(long, global = true, value_enum, default_value_t = Transport::Native)]
    transport: Transport,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Read current readiness. HA access uses HOME_ASSISTANT_URL/TOKEN.
    #[command(visible_alias = "doctor")]
    Status(StatusArgs),
}

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args_os().len() == 1 {
        return if Cli::command().print_help().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let args = match Cli::try_parse() {
        Ok(args) => args,
        Err(error) => return ha_downloads::cli::argument_error(error),
    };
    match args.command {
        Command::Status(status) => ha_downloads::cli::status(status, args.transport).await,
    }
}
