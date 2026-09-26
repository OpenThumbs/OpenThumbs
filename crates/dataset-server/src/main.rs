mod api;
mod auth;
mod error;
mod web;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use clap::{Parser, Subcommand};
use dataset_meta::MetaStore;
use dataset_storage::{BlobStore, LocalBlobStore, UploadManager};
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Versioned dataset store: immutable manifests, shared blobs, lineage")]
struct Cli {
    /// `sqlite://datasets.db?mode=rwc` (workstation) or `postgres://…` (team server).
    #[arg(long, env = "DATABASE_URL", default_value = "sqlite://datasets.db?mode=rwc", global = true)]
    database_url: String,

    /// Root directory of the local blob store and upload staging area.
    #[arg(long, env = "DS_STORE", default_value = "./store", global = true)]
    store: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server (default).
    Serve {
        /// Default port 62541 = 0xF44D, from 👍 (U+1F44D).
        #[arg(long, env = "DS_BIND", default_value = "127.0.0.1:62541")]
        bind: String,
        /// Override just the port of --bind.
        #[arg(short, long, env = "DS_PORT")]
        port: Option<u16>,
        /// Public URL used in `resolve` responses.
        #[arg(long, env = "DS_BASE_URL")]
        base_url: Option<String>,
        /// Allow unauthenticated reads.
        #[arg(long, env = "DS_PUBLIC_READ", default_value_t = false)]
        public_read: bool,
        /// Comma-separated usernames allowed to run GC.
        #[arg(long, env = "DS_ADMINS", default_value = "admin", value_delimiter = ',')]
        admins: Vec<String>,
    },
    /// Create a user. Password is read from DS_PASSWORD.
    Useradd {
        username: String,
        #[arg(long, env = "DS_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Issue an API token for a user and print it.
    Token {
        username: String,
        #[arg(long, default_value = "cli")]
        name: String,
    },
}

pub struct Config {
    pub base_url: String,
    pub public_read: bool,
    pub admins: Vec<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub meta: MetaStore,
    pub store: Arc<dyn BlobStore>,
    pub uploads: Arc<UploadManager>,
    pub config: Arc<Config>,
    /// Writers that may create blob references hold a read lock; GC holds the write lock.
    pub gc_lock: Arc<RwLock<()>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into()))
        .init();
    // `serve` is the default subcommand, so its env/flag defaults still apply.
    let mut args: Vec<String> = std::env::args().collect();
    const KNOWN: &[&str] = &["serve", "useradd", "token", "help", "--help", "-h", "--version", "-V"];
    if !args.iter().skip(1).any(|a| KNOWN.contains(&a.as_str())) {
        // Insert right after the binary name so serve flags (`--port 9000`) parse.
        args.insert(1, "serve".into());
    }
    let cli = Cli::parse_from(args);
    let meta = MetaStore::connect(&cli.database_url).await?;

    match cli.command.expect("defaulted to serve") {
        Command::Useradd { username, password } => {
            if !dataset_core::validate_name(&username) || username.contains('/') {
                anyhow::bail!("invalid username");
            }
            meta.create_user(&username, &auth::hash_password(&password)?).await?;
            println!("created user {username}");
        }
        Command::Token { username, name } => {
            let user = meta.user_by_name(&username).await?.ok_or_else(|| anyhow::anyhow!("no such user"))?;
            println!("{}", auth::issue_token(&meta, &user, &name).await?);
        }
        Command::Serve { bind, port, base_url, public_read, admins } => {
            let bind = match port {
                Some(p) => {
                    let host = bind.rsplit_once(':').map(|(h, _)| h).unwrap_or(&bind);
                    format!("{host}:{p}")
                }
                None => bind,
            };
            let root = std::path::PathBuf::from(&cli.store);
            let state = AppState {
                meta,
                store: Arc::new(LocalBlobStore::new(&root)?),
                uploads: Arc::new(UploadManager::new(root.join("uploads"))?),
                config: Arc::new(Config {
                    base_url: base_url.unwrap_or_else(|| format!("http://{bind}")).trim_end_matches('/').to_string(),
                    public_read,
                    admins,
                }),
                gc_lock: Arc::new(RwLock::new(())),
            };
            let app = axum::Router::new()
                .nest("/api", api::routes())
                .merge(web::routes())
                .layer(DefaultBodyLimit::disable())
                .layer(TraceLayer::new_for_http())
                .with_state(state);
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            tracing::info!("listening on http://{bind}, store at {}", root.display());
            axum::serve(listener, app).await?;
        }
    }
    Ok(())
}
