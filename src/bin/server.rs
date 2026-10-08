use clap::Parser;
use overpass::cache::{Cache, Config as CacheConfig};
use overpass::client::{DEFAULT_ENDPOINT, DEFAULT_RETRIES};
use overpass::history::History;
use overpass::{Client, Error};
use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

/// Serve the Overpass fetcher over HTTP.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long, env = "OVERPASS_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,

    /// Upstream Overpass interpreter URL
    #[arg(long, env = "OVERPASS_ENDPOINT", default_value = DEFAULT_ENDPOINT)]
    endpoint: String,

    /// Retries when the upstream is busy (429/504)
    #[arg(long, env = "OVERPASS_RETRIES", default_value_t = DEFAULT_RETRIES)]
    retries: u32,

    /// AMQP URL of the RabbitMQ broker feeding the history service
    /// (e.g. amqp://app:app@rabbitmq:5672/%2f); unset disables logging
    #[arg(long, env = "RABBITMQ_URL", hide_env_values = true)]
    rabbitmq_url: Option<String>,

    /// Redis URL for the response cache and limits (e.g. redis://redis:6379); unset disables them
    #[arg(long, env = "REDIS_URL", hide_env_values = true)]
    redis_url: Option<String>,

    /// How long a cached response stays valid, seconds
    #[arg(long, env = "CACHE_TTL", default_value_t = 600)]
    cache_ttl: u64,

    /// Upstream-bound requests per minute per client IP; 0 disables the limit
    #[arg(long, env = "RATE_LIMIT", default_value_t = 30)]
    rate_limit: u32,

    /// Refuse all upstream requests for this many seconds after Overpass answers 429/504
    #[arg(long, env = "BREAKER_SECS", default_value_t = 30)]
    breaker_secs: u64,
}

fn report_retry(err: &Error, delay: Duration) {
    eprintln!("upstream busy ({err}), retrying in {}s", delay.as_secs());
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let client = match Client::with_endpoint(&cli.endpoint) {
        Ok(c) => c.retries(cli.retries).on_retry(report_retry),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let history = cli.rabbitmq_url.as_deref().map(History::new);
    let cache = match cli.redis_url.as_deref().map(|url| {
        Cache::new(
            url,
            CacheConfig {
                ttl_secs: cli.cache_ttl,
                rate_limit: cli.rate_limit,
                breaker_secs: cli.breaker_secs,
            },
        )
    }) {
        None => None,
        Some(Ok(c)) => Some(c),
        Some(Err(e)) => {
            eprintln!("error: REDIS_URL: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(cli.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: bind {}: {e}", cli.listen);
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "listening on {} → {}, history: {}, cache: {}",
        cli.listen,
        cli.endpoint,
        if history.is_some() { "rabbitmq" } else { "off" },
        if cache.is_some() { "redis" } else { "off" }
    );

    let app = overpass::server::router(client, history, cache);
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
