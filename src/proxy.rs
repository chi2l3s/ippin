use crate::{
    config::Config,
    gate::Gate,
    pac,
    service::Instance,
    transport::{Body, Transport, full},
};
use anyhow::{Context, Result, ensure};
use http_body_util::BodyExt;
use hyper::{Method, Request, Response, StatusCode, body::Incoming, header, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, net::IpAddr, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub struct Server {
    cfg: Config,
    gate: Arc<Gate>,
    transport: Transport,
    instance: Instance,
    shutdown: CancellationToken,
    tasks: TaskTracker,
    tunnels: Arc<Semaphore>,
}

impl Server {
    pub fn new(
        cfg: Config,
        gate: Arc<Gate>,
        transport: Transport,
        instance: Instance,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            cfg,
            gate,
            transport,
            instance,
            shutdown,
            tasks: TaskTracker::new(),
            tunnels: Arc::new(Semaphore::new(256)),
        }
    }

    pub async fn serve(self, listener: TcpListener) -> Result<()> {
        let server = Arc::new(self);
        let _cleanup = server.shutdown.clone().drop_guard();
        let monitor = server.clone();
        server.tasks.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = monitor.shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(e) = monitor.gate.admit().await {
                            if monitor.shutdown.is_cancelled() { break; }
                            tracing::warn!(error = %e, "KILL active sessions / BLOCK new requests");
                        }
                    }
                }
            }
        });
        let clients = Arc::new(Semaphore::new(256));
        let result = loop {
            let accepted = tokio::select! {
                biased;
                _ = server.shutdown.cancelled() => break Ok(()),
                accepted = listener.accept() => accepted,
            };
            let (stream, _) = match accepted {
                Ok(pair) => pair,
                Err(e) => break Err(e).context("accept local connection"),
            };
            let Ok(permit) = clients.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let state = server.clone();
            server.tasks.spawn(async move {
                let _permit = permit;
                let state_for_request = state.clone();
                let service = service_fn(move |request| {
                    let state = state_for_request.clone();
                    async move { Ok::<_, Infallible>(state.handle(request).await) }
                });
                let mut builder = hyper::server::conn::http1::Builder::new();
                builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(32 * 1024);
                let connection = builder.serve_connection(TokioIo::new(stream), service).with_upgrades();
                tokio::select! {
                    biased;
                    _ = state.shutdown.cancelled() => {},
                    result = connection => { if let Err(e) = result { tracing::debug!(error = %e, "local HTTP connection closed"); } }
                }
            });
        };
        server.shutdown.cancel();
        server.tasks.close();
        tokio::time::timeout(Duration::from_secs(8), server.tasks.wait())
            .await
            .context("shutdown tasks timed out")?;
        result
    }

    async fn handle(self: Arc<Self>, mut request: Request<Incoming>) -> Response<Body> {
        if request.method() == Method::CONNECT {
            return self.connect(request).await;
        }

        // Absolute-form requests must never be intercepted by local route names.
        if request.uri().scheme().is_some() {
            return self.forward(request, false).await;
        }
        let local_host = format!("127.0.0.1:{}", self.cfg.port);
        if request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            != Some(&local_host)
            || request.headers().contains_key(header::ORIGIN)
        {
            return reply(
                StatusCode::FORBIDDEN,
                "Local endpoint requires the IPPin loopback host and no Origin header",
            );
        }
        match request.uri().path() {
            "/proxy.pac" if request.method() == Method::GET => {
                let mut response = reply(
                    StatusCode::OK,
                    pac::generate(&self.cfg, &self.transport.route),
                );
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/x-ns-proxy-autoconfig"),
                );
                response
            }
            "/_ippin/control" | "/_ippin/stop" => {
                let expected = format!("Bearer {}", self.instance.token);
                if request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .map(|h| h.as_bytes())
                    != Some(expected.as_bytes())
                {
                    return reply(StatusCode::FORBIDDEN, "Unauthorized");
                }
                let stopping = request.uri().path() == "/_ippin/stop";
                if request.method() != if stopping { Method::POST } else { Method::GET } {
                    return reply(StatusCode::METHOD_NOT_ALLOWED, "Invalid method");
                }
                let response = json_reply(
                    serde_json::json!({"pid": self.instance.pid, "status": "running", "route": self.transport.route.description()}),
                );
                if stopping {
                    let shutdown = self.shutdown.clone();
                    self.tasks.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        shutdown.cancel();
                    });
                }
                response
            }
            path if path == "/anthropic" || path.starts_with("/anthropic/") => {
                match anthropic_uri(request.uri()) {
                    Ok(uri) => {
                        *request.uri_mut() = uri;
                        self.forward(request, true).await
                    }
                    Err(_) => reply(StatusCode::BAD_REQUEST, "Invalid API path"),
                }
            }
            "/" if request.method() == Method::GET => json_reply(
                serde_json::json!({"status":"running", "version":env!("CARGO_PKG_VERSION")}),
            ),
            _ => reply(StatusCode::NOT_FOUND, "Not found"),
        }
    }

    async fn connect(self: Arc<Self>, mut request: Request<Incoming>) -> Response<Body> {
        let Some(authority) = request.uri().authority().cloned() else {
            return reply(StatusCode::BAD_REQUEST, "CONNECT requires host:port");
        };
        let Some(port) = authority.port_u16() else {
            return reply(StatusCode::BAD_REQUEST, "CONNECT requires a valid port");
        };
        if authority.as_str().contains('@') {
            return reply(StatusCode::BAD_REQUEST, "Target userinfo is not supported");
        }
        let host = authority.host().trim_matches(['[', ']']);
        if validate_target(host, port, self.cfg.port).is_err() {
            return reply(
                StatusCode::BAD_REQUEST,
                "Invalid CONNECT target or proxy loop",
            );
        }
        let Ok(permit) = self.tunnels.clone().try_acquire_owned() else {
            return reply(StatusCode::SERVICE_UNAVAILABLE, "Tunnel limit reached");
        };
        let (observation, cancel) = match self.gate.admit().await {
            Ok(admission) => admission,
            Err(e) => {
                tracing::warn!(target = %authority, error = %e, "BLOCK");
                return reply(
                    StatusCode::FORBIDDEN,
                    "Blocked by IPPin: external IP verification failed",
                );
            }
        };
        let stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => return reply(StatusCode::FORBIDDEN, "Session revoked"),
            result = self.transport.dial(host, port) => result,
        };
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "CONNECT failed");
                return reply(StatusCode::BAD_GATEWAY, "Upstream connection failed");
            }
        };
        if cancel.is_cancelled() {
            return reply(StatusCode::FORBIDDEN, "Session revoked");
        }
        tracing::info!(target = %authority, ip = %observation.ip, "PASS CONNECT");
        let upgrade = hyper::upgrade::on(&mut request);
        self.tasks.spawn(async move {
            let _permit = permit;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {},
                result = async {
                    let upgraded = upgrade.await?;
                    let mut client = TokioIo::new(upgraded);
                    tokio::io::copy_bidirectional(&mut client, &mut stream).await?;
                    Ok::<_, anyhow::Error>(())
                } => { if let Err(e) = result { tracing::debug!(error = %e, "tunnel closed"); } }
            }
        });
        // Empty 200 response is required by HTTP CONNECT; no entity headers.
        Response::new(full(""))
    }

    async fn forward(&self, request: Request<Incoming>, reverse: bool) -> Response<Body> {
        if !matches!(request.uri().scheme_str(), Some("http" | "https")) {
            return reply(StatusCode::BAD_REQUEST, "Only http and https are supported");
        }
        if request
            .uri()
            .authority()
            .is_some_and(|a| a.as_str().contains('@'))
        {
            return reply(StatusCode::BAD_REQUEST, "Target userinfo is not supported");
        }
        let Some(host) = request.uri().host() else {
            return reply(StatusCode::BAD_REQUEST, "Target host missing");
        };
        let port =
            request
                .uri()
                .port_u16()
                .unwrap_or(if request.uri().scheme_str() == Some("https") {
                    443
                } else {
                    80
                });
        if validate_target(host.trim_matches(['[', ']']), port, self.cfg.port).is_err() {
            return reply(StatusCode::BAD_REQUEST, "Invalid target or proxy loop");
        }
        if request.headers().contains_key(header::UPGRADE) {
            return reply(StatusCode::BAD_REQUEST, "Use CONNECT for protocol upgrades");
        }
        let (observation, cancel) = match self.gate.admit().await {
            Ok(admission) => admission,
            Err(e) => {
                tracing::warn!(host, error = %e, "BLOCK");
                return reply(
                    StatusCode::FORBIDDEN,
                    "Blocked by IPPin: external IP verification failed",
                );
            }
        };
        tracing::info!(host, ip = %observation.ip, reverse, "PASS HTTP");
        let (mut parts, body) = request.into_parts();
        strip_hop_headers(&mut parts.headers);
        parts.headers.remove("forwarded");
        parts.headers.remove("x-forwarded-for");
        let authority = parts.uri.authority().expect("validated host").as_str();
        let host_header = match header::HeaderValue::from_str(authority) {
            Ok(h) => h,
            Err(_) => return reply(StatusCode::BAD_REQUEST, "Invalid Host"),
        };
        parts.headers.insert(header::HOST, host_header);
        parts.headers.insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
        let request = Request::from_parts(
            parts,
            body.map_err(|e| Box::new(e) as crate::transport::BoxError)
                .boxed_unsync(),
        );
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return reply(StatusCode::FORBIDDEN, "Session revoked"),
            result = self.transport.request(request, cancel.clone()) => result,
        };
        match response {
            Ok(response) => {
                let (mut parts, body) = response.into_parts();
                strip_hop_headers(&mut parts.headers);
                // The transport driver owns the cancellation token through the body stream.
                Response::from_parts(
                    parts,
                    body.map_err(|e| Box::new(e) as crate::transport::BoxError)
                        .boxed_unsync(),
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "upstream HTTP request failed");
                reply(StatusCode::BAD_GATEWAY, "Upstream request failed")
            }
        }
    }
}

fn anthropic_uri(uri: &hyper::Uri) -> Result<hyper::Uri> {
    let suffix = uri
        .path_and_query()
        .context("API path missing")?
        .as_str()
        .strip_prefix("/anthropic")
        .context("API prefix missing")?;
    let path = if suffix.is_empty() {
        "/".to_owned()
    } else if suffix.starts_with('?') {
        format!("/{suffix}")
    } else {
        suffix.to_owned()
    };
    format!("https://api.anthropic.com{path}")
        .parse()
        .context("invalid API URI")
}

fn validate_target(host: &str, port: u16, own_port: u16) -> Result<()> {
    ensure!(
        !host.is_empty() && !host.contains(['@', '/', '\\', '\r', '\n', ' ']) && port != 0,
        "invalid authority"
    );
    let local = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(!(local && port == own_port), "proxy loop");
    Ok(())
}

pub fn strip_hop_headers(headers: &mut header::HeaderMap) {
    let connection_fields: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .filter_map(|s| header::HeaderName::from_bytes(s.trim().as_bytes()).ok())
        .collect();
    for field in connection_fields {
        headers.remove(field);
    }
    for field in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(field);
    }
}

fn reply(status: StatusCode, text: impl Into<bytes::Bytes>) -> Response<Body> {
    let mut response = Response::new(full(text));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
fn json_reply(value: serde_json::Value) -> Response<Body> {
    let mut response = reply(StatusCode::OK, value.to_string());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_connection_nominated_and_proxy_credentials_headers() {
        let mut headers = header::HeaderMap::new();
        headers.insert("connection", "x-secret, keep-alive".parse().unwrap());
        headers.insert("x-secret", "private".parse().unwrap());
        headers.insert("proxy-authorization", "secret".parse().unwrap());
        headers.insert("authorization", "Bearer api-key".parse().unwrap());
        strip_hop_headers(&mut headers);
        assert_eq!(headers.len(), 1);
        assert!(headers.contains_key("authorization"));
    }

    #[test]
    fn api_mapping_removes_only_one_prefix_and_preserves_query_encoding() {
        for (input, output) in [
            ("/anthropic", "https://api.anthropic.com/"),
            ("/anthropic?x=1", "https://api.anthropic.com/?x=1"),
            (
                "/anthropic/v1/messages?x=a%2Fb",
                "https://api.anthropic.com/v1/messages?x=a%2Fb",
            ),
            (
                "/anthropic/anthropic/v1",
                "https://api.anthropic.com/anthropic/v1",
            ),
            (
                "/anthropic//other.example/path",
                "https://api.anthropic.com//other.example/path",
            ),
        ] {
            assert_eq!(
                anthropic_uri(&input.parse().unwrap()).unwrap().to_string(),
                output
            );
        }
    }
}
