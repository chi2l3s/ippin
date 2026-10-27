//! Offline end-to-end tests exercise real sockets and HTTP framing.
use anyhow::{Result, bail};
use ippin::{
    checker::{IpProbe, Observation, ProbeFuture},
    config::Config,
    gate::Gate,
    proxy::Server,
    service::Instance,
    transport::{Route, Transport},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

struct Probe(AtomicU8);
impl IpProbe for Probe {
    fn fetch(&self) -> ProbeFuture<'_> {
        Box::pin(async {
            match self.0.load(Ordering::SeqCst) {
                0 => Ok(Observation {
                    ip: "203.0.113.1".parse().unwrap(),
                    country: "NL".into(),
                }),
                1 => Ok(Observation {
                    ip: "203.0.113.2".parse().unwrap(),
                    country: "NL".into(),
                }),
                _ => bail!("probe unavailable"),
            }
        })
    }
}
struct Harness {
    port: u16,
    probe: Arc<Probe>,
    gate: Arc<Gate>,
    shutdown: CancellationToken,
    task: JoinHandle<Result<()>>,
}
impl Harness {
    async fn new(route: Route) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg = Config {
            port,
            locked_ip: "203.0.113.1".into(),
            ..Config::default()
        };
        let shutdown = CancellationToken::new();
        let probe = Arc::new(Probe(AtomicU8::new(0)));
        let gate = Arc::new(Gate::new(
            cfg.pinned_ip().unwrap(),
            probe.clone(),
            shutdown.clone(),
        ));
        let server = Server::new(
            cfg,
            gate.clone(),
            Transport::new(route, false, port),
            Instance::new(port),
            shutdown.clone(),
        );
        let task = tokio::spawn(server.serve(listener));
        Self {
            port,
            probe,
            gate,
            shutdown,
            task,
        }
    }
    async fn request(&self, raw: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        response
    }
    async fn close(mut self) {
        self.shutdown.cancel();
        timeout(Duration::from_secs(3), &mut self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn header(stream: &mut TcpStream) -> String {
    timeout(Duration::from_secs(3), async {
        let mut data = Vec::new();
        while !data.ends_with(b"\r\n\r\n") {
            data.push(stream.read_u8().await.unwrap());
            assert!(data.len() < 32 * 1024);
        }
        String::from_utf8(data).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn forward_preserves_method_path_query_body_and_end_to_end_headers() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let headers = header(&mut stream).await.to_ascii_lowercase();
        assert!(headers.starts_with("post /proxy.pac?x=1 http/1.1\r\n"));
        assert!(headers.contains(&format!("host: {addr}\r\n")));
        assert!(headers.contains("authorization: bearer api-key\r\n"));
        assert!(
            !headers.contains("proxy-authorization")
                && !headers.contains("x-private")
                && !headers.contains("x-forwarded-for")
        );
        let mut body = [0; 5];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"hello");
        stream.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\nConnection: close, x-private\r\nX-Private: remove-me\r\nX-Public: yes\r\n\r\nworld").await.unwrap();
    });
    let h = Harness::new(Route::Direct).await;
    let response = h.request(&format!("POST http://{addr}/proxy.pac?x=1 HTTP/1.1\r\nHost: ignored.example\r\nAuthorization: Bearer api-key\r\nProxy-Authorization: secret\r\nX-Forwarded-For: real-ip\r\nConnection: close, x-private\r\nX-Private: secret\r\nContent-Length: 5\r\n\r\nhello")).await;
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 201"));
    assert!(response.ends_with("world"));
    assert!(response.to_ascii_lowercase().contains("x-public: yes"));
    assert!(!response.contains("remove-me"));
    origin_task.await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn blocked_requests_never_connect_to_target() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let h = Harness::new(Route::Direct).await;
    for mode in [1, 2] {
        h.probe.0.store(mode, Ordering::SeqCst);
        for raw in [
            format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"),
            format!("GET http://{addr}/ HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"),
            format!(
                "POST /anthropic/v1/messages HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                h.port
            ),
        ] {
            let response = h.request(&raw).await;
            assert!(response.starts_with(b"HTTP/1.1 403"));
        }
    }
    assert!(
        timeout(Duration::from_millis(100), origin.accept())
            .await
            .is_err()
    );
    h.close().await;
}

#[tokio::test]
async fn connect_preserves_buffered_bytes_and_closes_both_peers_on_mismatch() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut data = [0; 5];
        stream.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"early");
        stream.write_all(b"reply").await.unwrap();
        let result = timeout(Duration::from_secs(3), stream.read_u8())
            .await
            .unwrap();
        assert!(result.is_err(), "outbound socket must close on mismatch");
    });
    let h = Harness::new(Route::Direct).await;
    let mut client = TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
    client
        .write_all(format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n\r\nearly").as_bytes())
        .await
        .unwrap();
    assert!(header(&mut client).await.starts_with("HTTP/1.1 200"));
    let mut data = [0; 5];
    client.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"reply");
    h.probe.0.store(1, Ordering::SeqCst);
    assert!(h.gate.admit().await.is_err());
    assert!(
        timeout(Duration::from_secs(3), client.read_u8())
            .await
            .unwrap()
            .is_err()
    );
    origin_task.await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn streaming_http_outbound_is_closed_on_probe_failure() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        header(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_secs(3), stream.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        let _ = closed_tx.send(());
    });
    let h = Harness::new(Route::Direct).await;
    let mut client = TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
    client
        .write_all(
            format!(
                "GET http://{addr}/events HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    assert!(header(&mut client).await.starts_with("HTTP/1.1 200"));
    let mut chunk = [0; 10];
    client.read_exact(&mut chunk).await.unwrap();
    assert_eq!(&chunk, b"5\r\nhello\r\n");
    h.probe.0.store(2, Ordering::SeqCst);
    assert!(h.gate.admit().await.is_err());
    timeout(Duration::from_secs(3), closed_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        timeout(Duration::from_secs(3), client.read_u8())
            .await
            .unwrap()
            .is_err()
    );
    origin_task.await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn explicit_proxy_failure_never_falls_back_to_direct() {
    let dead_proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = dead_proxy.local_addr().unwrap();
    drop(dead_proxy);
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let h = Harness::new(Route::Http(proxy_addr)).await;
    let response = h
        .request(&format!(
            "GET http://{addr}/ HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        ))
        .await;
    assert!(response.starts_with(b"HTTP/1.1 502"));
    assert!(
        timeout(Duration::from_millis(100), origin.accept())
            .await
            .is_err()
    );
    h.close().await;
}

#[tokio::test]
async fn local_endpoints_reject_untrusted_hosts_origins_and_missing_control_token() {
    let h = Harness::new(Route::Direct).await;
    for raw in [
        "GET /proxy.pac HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n".into(),
        format!(
            "GET /proxy.pac HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: https://evil.example\r\nConnection: close\r\n\r\n",
            h.port
        ),
        format!(
            "POST /_ippin/stop HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            h.port
        ),
    ] {
        assert!(h.request(&raw).await.starts_with(b"HTTP/1.1 403"));
    }
    let response = h
        .request(&format!(
            "GET /proxy.pac HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            h.port
        ))
        .await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    h.close().await;
}

#[tokio::test]
async fn shutdown_closes_idle_local_clients() {
    let h = Harness::new(Route::Direct).await;
    let mut client = TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
    h.close().await;
    let mut data = Vec::new();
    let result = timeout(Duration::from_secs(2), client.read_to_end(&mut data))
        .await
        .unwrap();
    assert!(result.is_err() || data.is_empty());
}

#[tokio::test]
async fn background_monitor_revokes_a_tunnel_without_another_request() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let h = Harness::new(Route::Direct).await;
    let mut client = TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
    client
        .write_all(format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    assert!(header(&mut client).await.starts_with("HTTP/1.1 200"));
    let (mut peer, _) = origin.accept().await.unwrap();
    h.probe.0.store(1, Ordering::SeqCst);
    assert!(
        timeout(Duration::from_secs(4), client.read_u8())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        timeout(Duration::from_secs(1), peer.read_u8())
            .await
            .unwrap()
            .is_err()
    );
    h.close().await;
}

#[tokio::test]
async fn revocation_while_upstream_connect_is_pending_never_returns_success() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = upstream.local_addr().unwrap();
    let h = Harness::new(Route::Http(addr)).await;
    let mut client = TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
    client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\nConnection: close\r\n\r\n").await.unwrap();
    let (mut peer, _) = upstream.accept().await.unwrap();
    assert!(
        header(&mut peer)
            .await
            .starts_with("CONNECT example.com:443")
    );
    h.probe.0.store(1, Ordering::SeqCst);
    assert!(h.gate.admit().await.is_err());
    let _ = peer.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
    assert!(header(&mut client).await.starts_with("HTTP/1.1 403"));
    h.close().await;
}
