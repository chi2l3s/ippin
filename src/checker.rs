use crate::transport::{Transport, full};
use anyhow::{Context, Result, bail, ensure};
use http_body_util::{BodyExt, Limited};
use hyper::Request;
use std::{future::Future, net::IpAddr, pin::Pin, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub ip: IpAddr,
    pub country: String,
}

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<Observation>> + Send + 'a>>;
pub trait IpProbe: Send + Sync {
    fn fetch(&self) -> ProbeFuture<'_>;
}

pub struct Checker {
    transport: Transport,
    endpoints: Vec<String>,
}

impl Checker {
    pub fn new(transport: Transport, ipv6: bool) -> Self {
        let endpoints = if ipv6 {
            vec![
                "https://www.cloudflare.com/cdn-cgi/trace".into(),
                "https://one.one.one.one/cdn-cgi/trace".into(),
            ]
        } else {
            vec![
                "https://1.1.1.1/cdn-cgi/trace".into(),
                "https://1.0.0.1/cdn-cgi/trace".into(),
            ]
        };
        Self {
            transport,
            endpoints,
        }
    }

    pub fn with_endpoints(transport: Transport, endpoints: Vec<String>) -> Self {
        Self {
            transport,
            endpoints,
        }
    }

    async fn fetch_inner(&self) -> Result<Observation> {
        let cancel = CancellationToken::new();
        let _cleanup = cancel.clone().drop_guard();
        let mut last = String::from("no trace endpoints configured");
        for endpoint in &self.endpoints {
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                let uri: hyper::Uri = endpoint.parse()?;
                let req = Request::builder()
                    .uri(&uri)
                    .header(
                        "host",
                        uri.authority().context("trace host missing")?.as_str(),
                    )
                    .header("user-agent", "ippin/2")
                    .header("connection", "close")
                    .body(full(""))?;
                let response = self.transport.request(req, cancel.child_token()).await?;
                ensure!(
                    response.status().is_success(),
                    "trace returned {}",
                    response.status()
                );
                let bytes = Limited::new(response.into_body(), 16 * 1024)
                    .collect()
                    .await
                    .map_err(|e| anyhow::anyhow!("trace body: {e}"))?
                    .to_bytes();
                parse_trace(std::str::from_utf8(&bytes)?)
            })
            .await;
            match result {
                Ok(Ok(observation)) => return Ok(observation),
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => last = "trace request timed out".into(),
            }
        }
        bail!("cannot verify external IP: {last}")
    }
}

impl IpProbe for Checker {
    fn fetch(&self) -> ProbeFuture<'_> {
        Box::pin(self.fetch_inner())
    }
}

pub fn parse_trace(raw: &str) -> Result<Observation> {
    let mut ip = None;
    let mut country = "UNKNOWN".to_owned();
    for line in raw.lines() {
        if let Some((key, value)) = line.trim().split_once('=') {
            match key {
                "ip" => {
                    ensure!(ip.is_none(), "duplicate IP in trace response");
                    ip = Some(
                        value
                            .parse::<IpAddr>()
                            .context("invalid IP in trace response")?,
                    );
                }
                "loc" => {
                    ensure!(
                        value.len() == 2 && value.bytes().all(|b| b.is_ascii_uppercase()),
                        "invalid trace country"
                    );
                    country = value.into();
                }
                _ => {}
            }
        }
    }
    Ok(Observation {
        ip: ip.context("trace response has no IP")?,
        country,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trace_requires_a_real_unambiguous_ip() {
        assert_eq!(
            parse_trace("ip=203.0.113.2\nloc=NL\n").unwrap().country,
            "NL"
        );
        assert!(parse_trace("ip=2001:db8::1").unwrap().ip.is_ipv6());
        for raw in [
            "",
            "<html>error</html>",
            "ip=garbage",
            "ip=1.1.1.1\nip=8.8.8.8",
            "ip=1.1.1.1\nloc=<script>",
        ] {
            assert!(parse_trace(raw).is_err());
        }
    }

    #[tokio::test]
    async fn endpoint_failures_invalid_bodies_and_oversized_responses_fail_closed() {
        use crate::transport::Route;
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for (status, body) in [
            (503, "ip=203.0.113.1".to_owned()),
            (200, "<html>error</html>".into()),
            (200, format!("ip=203.0.113.1\n{}", "x".repeat(16 * 1024))),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
            let checker = Checker::with_endpoints(
                Transport::new(Route::Direct, false, 0),
                vec![format!("http://{addr}/trace")],
            );
            assert!(checker.fetch().await.is_err());
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn failed_primary_endpoint_can_use_valid_secondary() {
        use crate::transport::Route;
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for body in ["invalid", "ip=203.0.113.1\nloc=NL\n"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let checker = Checker::with_endpoints(
            Transport::new(Route::Direct, false, 0),
            vec![
                format!("http://{addr}/primary"),
                format!("http://{addr}/secondary"),
            ],
        );
        assert_eq!(
            checker.fetch().await.unwrap().ip,
            "203.0.113.1".parse::<IpAddr>().unwrap()
        );
        task.await.unwrap();
    }
}
