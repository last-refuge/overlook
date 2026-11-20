//! Opt-in, authenticated Streamable HTTP over a numeric loopback listener.

mod auth;
mod protocol;

use std::convert::Infallible;
use std::fs::OpenOptions;
use std::io::Read;
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body, Bytes};
use hyper::header::{self, HeaderMap};
use hyper::server::conn::http1;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use url::Url;

use super::service;
use auth::{Auth, Denial, METADATA_PATH};

const DEFAULT_BIND: &str = "127.0.0.1:8765";
const MCP_PATH: &str = "/mcp";
const MAX_CONNECTIONS: usize = 16;
const MAX_ACTIVE_CALLS: usize = 8;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
// Allow the owner IPC's 60-second deadline plus HTTP receipt and introspection.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(80);

type HttpResponse = Response<Full<Bytes>>;
type OwnerCall = dyn Fn(super::contract::Request) -> super::contract::Reply + Send + Sync;

pub(super) fn run(profile: &Path, args: &[String]) -> anyhow::Result<()> {
    let (bind, config_path) = options(args)?;
    let config = Config::read(&config_path)?;
    let profile = profile
        .canonicalize()
        .context("Could not resolve the MCP profile")?;
    let Config {
        grant_client,
        grant_token,
        allowed_origins,
        auth,
    } = config;
    let auth = Auth::new(auth, &grant_token)?;
    let bridge = Arc::new(Bridge {
        host: match &auth {
            Auth::Loopback(_) => bind.to_string(),
            Auth::OAuth(oauth) => oauth.host.clone(),
        },
        allowed_origins,
        auth,
        grant_token: grant_token.clone(),
        call: Box::new(move |request| {
            service::call(&profile, &grant_client, &grant_token, request)
        }),
        active_calls: Arc::new(Semaphore::new(MAX_ACTIVE_CALLS)),
    });

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(MAX_ACTIVE_CALLS)
        .enable_all()
        .build()?
        .block_on(async {
            let listener = TcpListener::bind(bind)
                .await
                .context("Could not bind MCP HTTP")?;
            eprintln!("Overlook MCP HTTP listening on {bind}{MCP_PATH}");
            serve(listener, bridge).await
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    grant_client: String,
    grant_token: String,
    #[serde(default)]
    allowed_origins: Vec<String>,
    auth: auth::Settings,
}

impl Config {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .context("Could not open the MCP HTTP configuration")?;
        let meta = file.metadata()?;
        // SAFETY: geteuid takes no arguments and cannot fail.
        let uid = unsafe { libc::geteuid() };
        ensure!(
            meta.is_file() && meta.uid() == uid && meta.mode() & 0o077 == 0,
            "MCP HTTP configuration must be an owner-only regular file"
        );
        let mut bytes = Vec::new();
        file.take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_BODY_BYTES,
            "MCP HTTP configuration exceeds 64 KiB"
        );
        // Serde's diagnostic can include the text of a malformed credential.
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid MCP HTTP configuration JSON or fields"))?;
        ensure!(
            !config.grant_client.trim().is_empty()
                && config.grant_client.len() <= 128
                && !config.grant_client.chars().any(char::is_control),
            "Invalid MCP HTTP grant_client"
        );
        ensure!(
            auth::is_local_secret(&config.grant_token),
            "Invalid MCP HTTP grant_token"
        );
        ensure!(
            config
                .allowed_origins
                .iter()
                .all(|origin| Url::parse(origin).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.origin().ascii_serialization() == *origin
                })),
            "Each allowed_origins entry must be an exact HTTP or HTTPS origin"
        );
        Ok(config)
    }
}

fn options(args: &[String]) -> anyhow::Result<(SocketAddr, PathBuf)> {
    let mut bind = None;
    let mut config = None;
    let mut flags = args.iter();
    while let Some(flag) = flags.next() {
        let value = flags
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with('-'))
            .context("Each MCP HTTP option requires a value")?;
        match flag.as_str() {
            "--bind" if bind.is_none() => bind = Some(value.as_str()),
            "--config" if config.is_none() => config = Some(PathBuf::from(value)),
            _ => bail!("Unknown or repeated MCP HTTP option"),
        }
    }
    let bind: SocketAddr = bind
        .unwrap_or(DEFAULT_BIND)
        .parse()
        .ok()
        .context("--bind requires a numeric loopback address and port")?;
    ensure!(
        bind.ip().is_loopback() && bind.port() != 0,
        "--bind requires a numeric loopback address and nonzero port"
    );
    Ok((bind, config.context("serve-http requires --config PATH")?))
}

struct Bridge {
    host: String,
    allowed_origins: Vec<String>,
    auth: Auth,
    grant_token: String,
    call: Box<OwnerCall>,
    active_calls: Arc<Semaphore>,
}

async fn serve(listener: TcpListener, bridge: Arc<Bridge>) -> anyhow::Result<()> {
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let slot = connections.clone().acquire_owned().await?;
        let (socket, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let bridge = bridge.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let service = hyper::service::service_fn(move |request| {
                let bridge = bridge.clone();
                async move { Ok::<_, Infallible>(bridge.handle(request).await) }
            });
            let mut http = http1::Builder::new();
            http.keep_alive(false)
                .max_buf_size(MAX_HEADER_BYTES)
                .max_headers(64)
                .timer(TokioTimer::new())
                .header_read_timeout(READ_TIMEOUT);
            let _ = timeout(
                CONNECTION_TIMEOUT,
                http.serve_connection(TokioIo::new(socket), service),
            )
            .await;
        });
    }
}

impl Bridge {
    async fn handle<B>(self: Arc<Self>, request: Request<B>) -> HttpResponse
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let (parts, body) = request.into_parts();
        let headers = parts.headers;
        match single_header(&headers, "origin") {
            Ok(None) => {}
            Ok(Some(origin)) if self.allowed_origins.iter().any(|allowed| allowed == origin) => {}
            _ => return empty(StatusCode::FORBIDDEN),
        }
        if !matches!(single_header(&headers, "host"), Ok(Some(host)) if host.eq_ignore_ascii_case(&self.host))
            || parts.uri.authority().is_some()
            || (matches!(self.auth, Auth::Loopback(_))
                && headers.keys().any(|name| {
                    name.as_str() == "forwarded" || name.as_str().starts_with("x-forwarded-")
                }))
        {
            return empty(StatusCode::MISDIRECTED_REQUEST);
        }

        match (parts.uri.path(), parts.uri.query()) {
            (METADATA_PATH, None) if let Auth::OAuth(oauth) = &self.auth => {
                return if parts.method == Method::GET {
                    json_response(StatusCode::OK, oauth.metadata())
                } else {
                    method_not_allowed("GET")
                };
            }
            (MCP_PATH, None) => {}
            _ => return empty(StatusCode::NOT_FOUND),
        }
        let token = match single_header(&headers, "authorization") {
            Ok(Some(value)) => match value
                .split_once(' ')
                .map(|(scheme, token)| (scheme, token.trim_start_matches(' ')))
            {
                Some((scheme, token))
                    if scheme.eq_ignore_ascii_case("Bearer") && auth::valid_bearer(token) =>
                {
                    token
                }
                _ => return self.deny(Denial::Invalid),
            },
            Ok(None) => return self.deny(Denial::Missing),
            Err(()) => return self.deny(Denial::Invalid),
        };

        let body = if parts.method == Method::POST {
            match timeout(READ_TIMEOUT, Limited::new(body, MAX_BODY_BYTES).collect()).await {
                Ok(Ok(collected)) => collected.to_bytes(),
                Ok(Err(err)) if err.is::<LengthLimitError>() => {
                    return empty(StatusCode::PAYLOAD_TOO_LARGE)
                }
                Ok(Err(_)) => return empty(StatusCode::BAD_REQUEST),
                Err(_) => return empty(StatusCode::REQUEST_TIMEOUT),
            }
        } else {
            Bytes::new()
        };
        let Ok(slot) = self.active_calls.clone().try_acquire_owned() else {
            return empty(StatusCode::SERVICE_UNAVAILABLE);
        };
        let token = token.to_owned();
        tokio::task::spawn_blocking(move || {
            // Keep the worker slot even if the client disconnects mid-call.
            let _slot = slot;
            let access = match self.auth.authenticate(&token, &self.grant_token) {
                Ok(access) => access,
                Err(denial) => return self.deny(denial),
            };
            if parts.method != Method::POST {
                return method_not_allowed("POST");
            }
            self.post(headers, &body, access)
        })
        .await
        .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
    }

    fn deny(&self, denial: Denial) -> HttpResponse {
        let status = match denial {
            Denial::Missing | Denial::Invalid => StatusCode::UNAUTHORIZED,
            Denial::Scope | Denial::WriteScope => StatusCode::FORBIDDEN,
            Denial::Unavailable => return empty(StatusCode::SERVICE_UNAVAILABLE),
        };
        let mut response = empty(status);
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            self.auth
                .challenge(&denial)
                .parse()
                .expect("validated OAuth resource authority"),
        );
        response
    }
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ()> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    match (values.next(), values.next()) {
        (None, _) => Ok(None),
        (Some(value), None) => value.to_str().map(Some).map_err(|_| ()),
        _ => Err(()),
    }
}

fn response(status: StatusCode) -> hyper::http::response::Builder {
    Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
}

fn empty(status: StatusCode) -> HttpResponse {
    response(status).body(Full::default()).unwrap()
}

fn json_response(status: StatusCode, message: Value) -> HttpResponse {
    response(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(message.to_string())))
        .unwrap()
}

fn method_not_allowed(allow: &'static str) -> HttpResponse {
    response(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::ALLOW, allow)
        .body(Full::default())
        .unwrap()
}

#[cfg(test)]
mod tests;
