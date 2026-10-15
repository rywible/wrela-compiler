//! Rooms over the network (language.md §12, `wrela-serve`): WebTransport sessions at
//! `/room/<name>`, one stream each, its messages framed by their length. The peers here are Rust
//! WebTransport clients that trust the server's development certificate by its hash, and a
//! program's client build in headless Chrome, which finds its server in `net.json`.

use crate::rooms::{Down, down, hello, propose, wasm};
use std::sync::Arc;
use std::time::Duration;
use wrela_host::server::{Limits, ServerBuild};
use wtransport::{ClientConfig, Endpoint};

/// A server of the rooms' test program on a free port, its data in a fresh directory: the port
/// and its certificate's hash.
fn start(name: &str, runtime: &tokio::runtime::Runtime) -> (u16, [u8; 32]) {
    start_build(name, wasm(), runtime)
}

/// A server of the server build `server` (its WASM), as [`start`] starts one.
fn start_build(name: &str, server: &[u8], runtime: &tokio::runtime::Runtime) -> (u16, [u8; 32]) {
    let data =
        wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("transport-{name}"));
    let _ = std::fs::remove_dir_all(&data);
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port();
    let (identity, hash) = wrela_serve::dev_identity().expect("a certificate");
    let config = wrela_serve::Config {
        build: Arc::new(ServerBuild::from_wasm(server).expect("a server build")),
        build_hash: wrela_abi::ticks::wasm_hash(server),
        data,
        port,
        identity,
        limits: Limits::default(),
    };
    runtime.spawn(async move {
        let _ = wrela_serve::serve(config).await;
    });
    (port, hash)
}

/// A peer's session and its stream.
struct Peer {
    _connection: wtransport::Connection,
    send: wtransport::SendStream,
    recv: wtransport::RecvStream,
}

async fn connect(port: u16, hash: [u8; 32], path: &str) -> Result<Peer, String> {
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([wtransport::tls::Sha256Digest::new(hash)])
        .build();
    let endpoint = Endpoint::client(config).map_err(|e| e.to_string())?;
    let url = format!("https://localhost:{port}{path}");
    // The server may be starting: try for a while.
    let mut last = String::new();
    for _ in 0..50 {
        match endpoint.connect(&url).await {
            Ok(connection) => {
                let (send, recv) = connection
                    .open_bi()
                    .await
                    .map_err(|e| e.to_string())?
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(Peer { _connection: connection, send, recv });
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last)
}

impl Peer {
    async fn put(&mut self, bytes: &[u8]) {
        self.send.write_all(&wrela_serve::frame(bytes)).await.expect("write");
    }

    /// The next message, or `None` if the stream ends.
    async fn next(&mut self) -> Option<Down> {
        let mut len = 0usize;
        for k in 0..3 {
            let mut b = [0u8];
            tokio::time::timeout(Duration::from_secs(10), self.recv.read_exact(&mut b))
                .await
                .ok()?
                .ok()?;
            len |= usize::from(b[0] & 127) << (7 * k);
            if b[0] < 128 {
                let mut bytes = vec![0u8; len];
                self.recv.read_exact(&mut bytes).await.ok()?;
                return Some(down(&bytes));
            }
        }
        None
    }
}

#[test]
fn peers_meet_in_a_room_over_webtransport() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash) = start("meet", &runtime);
    runtime.block_on(async {
        let mut a = connect(port, hash, "/room/meet").await.expect("a connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        let mut b = connect(port, hash, "/room/meet").await.expect("b connects");
        b.put(&hello(6, 1, 0, 0)).await;
        assert_eq!(b.next().await, Some(Down::Welcome { peer: 2, seq: 0 }));
        // A's proposal is committed, and both see it.
        a.put(&propose(1, 1, 1, 10)).await;
        assert_eq!(a.next().await, Some(Down::Committed { seq: 1, counter: 1, value: 10 }));
        assert_eq!(b.next().await, Some(Down::Committed { seq: 1, counter: 1, value: 10 }));
    });
}

#[test]
fn a_frame_too_long_closes_the_connection_and_a_wrong_path_is_refused() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash) = start("limits", &runtime);
    runtime.block_on(async {
        let mut a = connect(port, hash, "/room/limits").await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        // A frame's length past 64 KiB: the server stops reading and closes.
        let mut too_long = Vec::new();
        let mut x = wrela_serve::MAX_FRAME + 1;
        loop {
            let g = (x & 127) as u8;
            x >>= 7;
            if x == 0 {
                too_long.push(g);
                break;
            }
            too_long.push(g | 128);
        }
        let _ = a.send.write_all(&too_long).await;
        assert_eq!(a.next().await, None, "the connection closed");
        // A path that isn't a room's: refused.
        assert!(connect(port, hash, "/rooms/../x").await.is_err());
    });
}

#[test]
fn a_browser_joins_a_room_over_webtransport() {
    let (dir, rel) = wrela_tests::page("compiler/tests/rooms-client", "rooms-client-chrome");
    let server = std::fs::read(dir.join("server/game.wasm")).expect("the program's server build");
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash) = start_build("chrome", &server, &runtime);
    std::fs::write(dir.join("net.json"), wrela_serve::net_json(port, Some(&hash)))
        .expect("net.json");
    // Another peer's tag is in the room first: the browser catches up to it.
    runtime.block_on(async {
        let mut a = connect(port, hash, "/room/chrome").await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        a.put(&propose(1, 1, 1, 11)).await;
        assert_eq!(a.next().await, Some(Down::Committed { seq: 1, counter: 1, value: 11 }));
    });
    let run = wrela_tests::run_in_chrome(&rel, 240, 16, 16, 60.0);
    let printed: Vec<&str> = run.printed.iter().map(|(_, line)| line.as_str()).collect();
    assert_eq!(printed, ["tags 11 10"], "the browser's log has the room's tags, its own committed");
}
