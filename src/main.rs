use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Args, Parser};
use futures_util::future::FutureExt;
use tokio::signal::unix::{SignalKind, signal};

mod acl;
mod config;
mod proxy;
mod reply;
mod request;
mod socks;
mod stats;
mod stats_socket;
mod util;

use config::Config;

#[derive(Parser)]
#[command(version, about, long_about)]
struct Cli {
    #[clap(
        short = 'c',
        long = "config",
        required_unless_present("dump_config"),
        value_name = "PATH"
    )]
    /// Path to configuration TOML file
    config: Option<PathBuf>,
    #[clap(short = 'C', long = "check-config", conflicts_with("dump_config"))]
    /// Only check the config syntax and then exit
    check_config: bool,
    #[clap(long = "dump-config", value_name = "PATH")]
    /// Dump out config (with defaults interpolated) to the given path; - means stdout
    dump_config: Option<PathBuf>,
    #[command(flatten, next_help_heading = "LOGGING OPTIONS")]
    logging: CliLogging,
}

#[derive(
    Debug,
    PartialEq,
    Eq,
    Hash,
    Copy,
    Clone,
    PartialOrd,
    Ord,
    strum::Display,
    strum::IntoStaticStr,
    strum::EnumString,
    serde::Serialize,
    serde::Deserialize,
)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for tracing_subscriber::filter::LevelFilter {
    fn from(value: LogLevel) -> Self {
        use tracing_subscriber::filter::LevelFilter;
        match value {
            LogLevel::Error => LevelFilter::ERROR,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Trace => LevelFilter::TRACE,
        }
    }
}

#[derive(Args)]
struct CliLogging {
    #[clap(short = 'L', long = "log-level")]
    /// Log level; overrides any value in the config
    log_level: Option<LogLevel>,
}

async fn any_shutdown_signal() {
    tracing::info!("Will shut down on HUP, QUIT, INT, or TERM");
    futures_util::future::select_all(vec![
        signal(SignalKind::hangup()).unwrap().recv().boxed(),
        signal(SignalKind::quit()).unwrap().recv().boxed(),
        signal(SignalKind::interrupt()).unwrap().recv().boxed(),
        signal(SignalKind::terminate()).unwrap().recv().boxed(),
    ])
    .await;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let conf = if let Some(config_path) = cli.config {
        Config::from_path(config_path)?
    } else {
        Config::default()
    };

    if let Some(dump_path) = cli.dump_config {
        let dump_path = if dump_path.to_str() == Some("-") {
            None
        } else {
            Some(&dump_path)
        };
        conf.dump(dump_path)?;
        if let Some(path) = dump_path {
            println!("Config dumped to {:?}", path);
        }
        return Ok(());
    };

    let conf = Arc::new(conf);

    if cli.check_config {
        println!("Config loaded successfully");
        return Ok(());
    }

    conf.initialize_logging(&cli.logging);

    let stats = Arc::new(stats::Stats::new());

    if conf.expect_proxy {
        tracing::info!("NOTE: Expecting PROXY protocol on streams");
    }

    let p = permit::Permit::new();

    // Set up the stats server
    if let Some(ref stats_socket_listen_address) = conf.stats_socket_listen_address {
        let server = stats_socket::StatsServer::new(Arc::clone(&stats), p.new_sub());
        if stats_socket_listen_address.starts_with('/')
            || stats_socket_listen_address.starts_with('.')
        {
            tracing::info!(
                listen_address = stats_socket_listen_address,
                "Stats socket listening",
            );
            let listener = stats_socket::bind_unix_listener(
                stats_socket_listen_address,
                conf.stats_socket_mode.clone(),
            )
            .with_context(|| {
                format!(
                    "failed to bind to domain socket {:?}",
                    stats_socket_listen_address,
                )
            })?;
            tokio::spawn(server.run_unix(listener));
        } else {
            tracing::info!(
                listen_address = stats_socket_listen_address,
                "Stats socket listening",
            );
            let listener = tokio::net::TcpListener::bind(stats_socket_listen_address)
                .await
                .with_context(|| {
                    format!(
                        "failed to bind to TCP socket {:?}",
                        stats_socket_listen_address
                    )
                })?;
            tokio::spawn(server.run_tcp(listener));
        }
    }

    // Set up the main SOCKS server
    let (_, p) = socks::run(Arc::clone(&conf), stats, p).await?;

    // Shut down on signal (TODO: or if the socks server dies?)
    any_shutdown_signal().await;
    tracing::info!("got shutdown signal; attempting graceful shutdown");
    let shutdown_start = std::time::Instant::now();
    match p.revoke().wait_subs_timeout(conf.shutdown_timeout) {
        Ok(_) => tracing::debug!(elapsed = ?shutdown_start.elapsed(), "shutdown finished"),
        Err(err) => tracing::warn!(
            elapsed = ?shutdown_start.elapsed(),
            ?err,
            "shutdown timed out",
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn test_debug_assert_cli() {
        Cli::command().debug_assert()
    }
}
