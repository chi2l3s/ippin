//! One immutable route shared by the IP probe and every forwarded connection.
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{Request, Response, body::Incoming};
use hyper_util::rt::TokioIo;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use tokio_util::sync::CancellationToken;
use url::Url;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = UnsyncBoxBody<Bytes, BoxError>;
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Stream = Box<dyn Io>;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn full(data: impl Into<Bytes>) -> Body {
    Full::new(data.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Direct,
    Http(SocketAddr),
    Socks5(SocketAddr),
}

impl Route {
    pub fn description(&self) -> String {
        match self {
            Self::Direct => "tun".into(),
            Self::Http(a) => format!("http://{a}"),
            Self::Socks5(a) => format!("socks5://{a}"),
        }
    }
    pub fn pac_fallback(&self) -> String {
        match self {
            Self::Direct => "DIRECT".into(),
            Self::Http(a) => format!("PROXY {a}"),
            Self::Socks5(a) => format!("SOCKS5 {a}"),
        }
    }
}

fn proxy_url(raw: &str, own_port: u16) -> Result<(Url, SocketAddr)> {
    let normalized = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let url = Url::parse(&normalized).context("invalid proxy URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "socks5" | "socks5h"),
        "proxy scheme must be http, socks5 or socks5h"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "proxy authentication is not supported"
    );
    ensure!(
        url.path() == "/" || url.path().is_empty(),
        "proxy URL cannot include a path"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "proxy URL cannot include query or fragment"
    );
    let host = url
        .host_str()
        .context("proxy host missing")?
        .trim_matches(['[', ']']);
    let ip: IpAddr = if host.eq_ignore_ascii_case("localhost") {
        Ipv4Addr::LOCALHOST.into()
    } else {
        host.parse()
            .context("proxy must use a loopback IP or localhost")?
    };
    ensure!(
        ip.is_loopback(),
        "only local loopback proxies are supported"
    );
    let port = url
        .port()
        .context("proxy port must be explicit and nonzero")?;
    ensure!(
        port != 0 && port != own_port,
        "proxy port must be nonzero and different from IPPin's port"
    );
    Ok((url, SocketAddr::new(ip, port)))
}

pub fn normalize_mode(raw: &str, own_port: u16) -> Result<String> {
    let raw = raw.trim();
    match raw.to_ascii_lowercase().as_str() {
        "auto" => Ok("auto".into()),
        "tun" | "direct" | "" => Ok("tun".into()),
        _ => {
            let (url, addr) = proxy_url(raw, own_port)?;
            Ok(format!(
                "{}://{addr}",
                if url.scheme() == "http" {
                    "http"
                } else {
                    "socks5"
                }
            ))
        }
    }
}

pub async fn resolve(mode: &str, own_port: u16) -> Result<Route> {
    let mode = normalize_mode(mode, own_port)?;
    if mode == "tun" {
        return Ok(Route::Direct);
    }
    if mode == "auto" {
        // Probe the protocol, not just whether an arbitrary process owns a port.
        for (port, socks) in [
            (2080, false),
            (12334, false),
            (7890, false),
            (10809, false),
            (10808, true),
        ] {
            if port == own_port {
                continue;
            }
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let route = if socks {
                Route::Socks5(addr)
            } else {
                Route::Http(addr)
            };
            if matches!(
                timeout(
                    Duration::from_millis(700),
                    connect_route(&route, "1.1.1.1", 443, false, own_port)
                )
                .await,
                Ok(Ok(_))
            ) {
                return Ok(route);
            }
        }
        return Ok(Route::Direct);
    }
    let (url, addr) = proxy_url(&mode, own_port)?;
    Ok(if url.scheme() == "http" {
        Route::Http(addr)
    } else {
        Route::Socks5(addr)
    })
}

#[derive(Clone)]
pub struct Transport {
    pub route: Route,
    ipv6: bool,
    own_port: u16,
    tls: TlsConnector,
}

impl Transport {
    pub fn new(route: Route, ipv6: bool, own_port: u16) -> Self {
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self {
            route,
            ipv6,
            own_port,
            tls: TlsConnector::from(Arc::new(config)),
        }
    }

    pub async fn dial(&self, host: &str, port: u16) -> Result<TcpStream> {
        timeout(
            CONNECT_TIMEOUT,
            connect_route(&self.route, host, port, self.ipv6, self.own_port),
        )
        .await
        .context("connection timed out")?
    }

    pub async fn request(
        &self,
        req: Request<Body>,
        cancel: CancellationToken,
    ) -> Result<Response<Incoming>> {
        let host = req
            .uri()
            .host()
            .context("request host missing")?
            .trim_matches(['[', ']'])
            .to_owned();
        let https = req.uri().scheme_str() == Some("https");
        let port = req.uri().port_u16().unwrap_or(if https { 443 } else { 80 });
        let stream = self.dial(&host, port).await?;
        let stream: Stream = if https {
            let name = ServerName::try_from(host).context("invalid TLS name")?;
            Box::new(
                timeout(CONNECT_TIMEOUT, self.tls.connect(name, stream))
                    .await
                    .context("TLS handshake timed out")??,
            )
        } else {
            Box::new(stream)
        };
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            tokio::select! { biased; _ = cancel.cancelled() => {}, result = conn => {
                if let Err(e) = result { tracing::debug!(error = %e, "outbound HTTP connection closed"); }
            } }
        });
        let (mut parts, body) = req.into_parts();
        // Origin form for a connection already established to the origin.
        parts.uri = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
            .parse()?;
        timeout(
            Duration::from_secs(120),
            sender.send_request(Request::from_parts(parts, body)),
        )
        .await
        .context("upstream response headers timed out")?
        .context("upstream HTTP request failed")
    }
}

async fn connect_route(
    route: &Route,
    host: &str,
    port: u16,
    ipv6: bool,
    own_port: u16,
) -> Result<TcpStream> {
    ensure!(
        port != 0 && !host.contains(['\r', '\n', ' ', '/', '@']),
        "invalid target authority"
    );
    match route {
        Route::Direct => {
            let addresses = tokio::net::lookup_host((host, port))
                .await
                .context("DNS lookup failed")?;
            let mut last = None;
            for addr in addresses.filter(|a| a.is_ipv6() == ipv6) {
                ensure!(
                    !(addr.ip().is_loopback() && addr.port() == own_port),
                    "proxy loop rejected"
                );
                match TcpStream::connect(addr).await {
                    Ok(s) => return Ok(s),
                    Err(e) => last = Some(e),
                }
            }
            bail!(
                "no reachable address in the pinned IP family: {}",
                last.map(|e| e.to_string()).unwrap_or_default()
            )
        }
        Route::Http(addr) => {
            let mut stream = TcpStream::connect(addr)
                .await
                .context("connect to HTTP proxy")?;
            let authority = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            stream
                .write_all(
                    format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
                )
                .await?;
            // Never consume tunneled bytes with a discarded buffered reader.
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                ensure!(headers.len() < 16 * 1024, "HTTP proxy headers too large");
                headers.push(stream.read_u8().await?);
            }
            let line = std::str::from_utf8(&headers)?
                .lines()
                .next()
                .context("empty proxy response")?;
            let mut fields = line.split_whitespace();
            ensure!(
                matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1"))
                    && fields.next() == Some("200"),
                "HTTP proxy rejected CONNECT: {line}"
            );
            Ok(stream)
        }
        Route::Socks5(addr) => {
            let mut stream = TcpStream::connect(addr)
                .await
                .context("connect to SOCKS5 proxy")?;
            stream.write_all(&[5, 1, 0]).await?;
            let mut greeting = [0; 2];
            stream.read_exact(&mut greeting).await?;
            ensure!(greeting == [5, 0], "SOCKS5 no-auth negotiation failed");
            let mut request = vec![5, 1, 0];
            match host.parse::<IpAddr>() {
                Ok(IpAddr::V4(ip)) => {
                    request.push(1);
                    request.extend(ip.octets());
                }
                Ok(IpAddr::V6(ip)) => {
                    request.push(4);
                    request.extend(ip.octets());
                }
                Err(_) => {
                    ensure!(
                        !host.is_empty() && host.len() <= 255,
                        "invalid SOCKS5 hostname length"
                    );
                    request.extend([3, host.len() as u8]);
                    request.extend(host.as_bytes());
                }
            }
            request.extend(port.to_be_bytes());
            stream.write_all(&request).await?;
            let mut reply = [0; 4];
            stream.read_exact(&mut reply).await?;
            ensure!(
                reply[0] == 5 && reply[1] == 0 && reply[2] == 0,
                "invalid/rejected SOCKS5 reply: {reply:?}"
            );
            let length = match reply[3] {
                1 => 4,
                4 => 16,
                3 => stream.read_u8().await? as usize,
                _ => bail!("invalid SOCKS5 address type"),
            };
            ensure!(length != 0, "empty SOCKS5 bound address");
            let mut discard = vec![0; length + 2];
            stream.read_exact(&mut discard).await?;
            Ok(stream)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn rejects_unsafe_or_unsupported_proxy_modes() {
        for bad in [
            "https://127.0.0.1:8880",
            "http://127.0.0.1:8888",
            "http://user:pass@127.0.0.1:7890",
            "http://8.8.8.8:80",
            "http://127.0.0.1:123/path",
            "socks5://127.0.0.1:0",
        ] {
            assert!(normalize_mode(bad, 8888).is_err(), "{bad}");
        }
        assert_eq!(
            normalize_mode("localhost:2080", 8888).unwrap(),
            "http://127.0.0.1:2080"
        );
    }

    #[tokio::test]
    async fn http_connect_preserves_eager_tunnel_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(s.read_u8().await.unwrap());
            }
            s.write_all(b"HTTP/1.1 200 OK\r\n\r\neager-data")
                .await
                .unwrap();
        });
        let mut stream = Transport::new(Route::Http(addr), false, 8888)
            .dial("example.com", 443)
            .await
            .unwrap();
        let mut data = Vec::new();
        stream.read_to_end(&mut data).await.unwrap();
        assert_eq!(data, b"eager-data");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_socks_reply_fails_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            s.read_exact(&mut greeting).await.unwrap();
            s.write_all(&[5, 0]).await.unwrap();
            let mut request = [0; 10];
            s.read_exact(&mut request).await.unwrap();
            s.write_all(&[4, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .await
                .unwrap();
        });
        assert!(
            Transport::new(Route::Socks5(addr), false, 8888)
                .dial("1.1.1.1", 443)
                .await
                .is_err()
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_encodes_ip_families_and_remote_dns_without_losing_payload() {
        for (host, address) in [
            ("203.0.113.7", vec![1, 203, 0, 113, 7]),
            ("2001:db8::1", {
                let mut v = vec![4];
                v.extend(
                    "2001:db8::1"
                        .parse::<std::net::Ipv6Addr>()
                        .unwrap()
                        .octets(),
                );
                v
            }),
            ("api.example.com", {
                let mut v = vec![3, 15];
                v.extend(b"api.example.com");
                v
            }),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 0]);
                stream.write_all(&[5, 0]).await.unwrap();
                let mut request = vec![0; 3 + address.len() + 2];
                stream.read_exact(&mut request).await.unwrap();
                let mut expected = vec![5, 1, 0];
                expected.extend(address);
                expected.extend(443u16.to_be_bytes());
                assert_eq!(request, expected);
                stream
                    .write_all(&[5, 0, 0, 3, 3, b'f', b'o', b'o', 0, 80, b'o', b'k'])
                    .await
                    .unwrap();
            });
            let mut stream = Transport::new(Route::Socks5(addr), false, 0)
                .dial(host, 443)
                .await
                .unwrap();
            let mut payload = Vec::new();
            stream.read_to_end(&mut payload).await.unwrap();
            assert_eq!(payload, b"ok");
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn http_connect_requires_a_successful_protocol_response() {
        for response in [
            "HTTP/1.1 407 Authentication Required\r\n\r\n",
            "HTTP/1.1 502 Bad Gateway\r\n\r\n",
            "garbage 200 ok\r\n\r\n",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            assert!(
                Transport::new(Route::Http(addr), false, 0)
                    .dial("example.com", 443)
                    .await
                    .is_err()
            );
            task.await.unwrap();
        }
    }
}
