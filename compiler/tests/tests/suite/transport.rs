//! Rooms over the network (language.md §19, `wrela-serve`): WebTransport sessions at
//! `/room/<name>?pair=<id>`, one stream each, its messages framed by their length. The peers here
//! are Rust WebTransport clients that trust the server's development certificate by its hash, and
//! a program's client build in headless Chrome, which finds its server in `net.json`.

use crate::rooms::{DEAF, Down, down, hello, propose, server_wasm, wasm};
use std::sync::Arc;
use std::time::Duration;
use wrela_host::server::{Limits, ServerBuild};
use wtransport::{ClientConfig, Endpoint};

/// A server of the rooms' test program on a free port, its data in a fresh directory: the port
/// and its certificate's hash.
fn start(name: &str, runtime: &tokio::runtime::Runtime) -> (u16, [u8; 32]) {
    let (port, hash, _) = start_build(name, wasm(), runtime, wrela_serve::Bounds::default());
    (port, hash)
}

/// The path of room `room`, for a client of the rooms' test program.
fn room(name: &str) -> String {
    let pair = wrela_abi::read_pair(wasm()).expect("the program's pair id");
    format!("/room/{name}?pair={pair:016x}")
}

/// A server of the server build `server` (its WASM), as [`start`] starts one; and the server.
fn start_build(
    name: &str,
    server: &[u8],
    runtime: &tokio::runtime::Runtime,
    bounds: wrela_serve::Bounds,
) -> (u16, [u8; 32], wrela_serve::Server) {
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
        bounds,
    };
    (port, hash, wrela_serve::start(config, runtime).expect("the server starts"))
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
            // Refused: trying again won't change that.
            Err(e @ wtransport::error::ConnectingError::SessionRejected) => {
                return Err(e.to_string());
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

    /// Whether the stream ends within `within`, with no message before: not a wait that runs out.
    async fn ends(&mut self, within: Duration) -> bool {
        let mut b = [0u8];
        matches!(tokio::time::timeout(within, self.recv.read_exact(&mut b)).await, Ok(Err(_)))
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
        let mut a = connect(port, hash, &room("meet")).await.expect("a connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        let mut b = connect(port, hash, &room("meet")).await.expect("b connects");
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
        let mut a = connect(port, hash, &room("limits")).await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        // A frame's length past what a client may send: the server stops reading and closes.
        let mut too_long = Vec::new();
        let mut x = wrela_serve::MAX_UP + 1;
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
fn a_server_frame_past_the_transport_bound_closes_the_connection() {
    let wasm = server_wasm(
        "oversize-server-frame",
        r#"
use platform::net::send
use std::input::{Event, events}

pub fn serve(time: f32) {
    for e in events() {
        if let Event::Message { conn, .. } = e {
            var bytes: Vec<u8> = Vec::new()
            for i in 0..65536 {
                bytes.push(0)
            }
            send(conn, bytes)
            bytes.push(0)
            send(conn, bytes)
        }
    }
}
"#,
    );
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash, _server) =
        start_build("oversize-down", &wasm, &runtime, wrela_serve::Bounds::default());
    let path = format!("/room/large?pair={:016x}", wrela_abi::read_pair(&wasm).expect("pair"));
    runtime.block_on(async {
        let mut peer = connect(port, hash, &path).await.expect("connects");
        peer.put(b"hi").await;
        let mut header = [0u8; 3];
        tokio::time::timeout(Duration::from_secs(3), peer.recv.read_exact(&mut header))
            .await
            .expect("the valid frame arrives")
            .expect("its header");
        assert_eq!(header, [0x80, 0x80, 0x04]);
        let mut bytes = vec![1u8; wrela_serve::MAX_FRAME];
        peer.recv.read_exact(&mut bytes).await.expect("the maximum-size frame");
        assert!(bytes.iter().all(|&b| b == 0));
        assert!(peer.ends(Duration::from_secs(3)).await, "an oversized server frame was written");
    });
}

#[test]
fn a_room_delivers_its_last_frame_before_closing() {
    let wasm = server_wasm(
        "last-server-frame",
        r#"
use platform::net::{close, send}
use std::input::{Event, events}

pub fn serve(time: f32) {
    for e in events() {
        if let Event::Message { conn, .. } = e {
            var bytes: Vec<u8> = Vec::new()
            for i in 0..65536 {
                bytes.push(0)
            }
            send(conn, bytes)
            close(conn)
        }
    }
}
"#,
    );
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash, _server) =
        start_build("last-frame", &wasm, &runtime, wrela_serve::Bounds::default());
    let path = format!("/room/last?pair={:016x}", wrela_abi::read_pair(&wasm).expect("pair"));
    runtime.block_on(async {
        let mut peer = connect(port, hash, &path).await.expect("connects");
        peer.put(b"hi").await;
        let mut bytes = vec![1u8; wrela_serve::MAX_FRAME + 3];
        tokio::time::timeout(Duration::from_secs(3), peer.recv.read_exact(&mut bytes))
            .await
            .expect("the last frame arrives")
            .expect("the entire last frame");
        assert_eq!(&bytes[..3], [0x80, 0x80, 0x04]);
        assert!(bytes[3..].iter().all(|&b| b == 0));
        assert!(peer.ends(Duration::from_secs(3)).await, "the connection closes after its send");
    });
}

#[test]
fn a_browser_joins_a_room_over_webtransport() {
    let (dir, rel) = wrela_tests::page("compiler/tests/rooms-client", "rooms-client-chrome");
    let server = std::fs::read(dir.join("server/game.wasm")).expect("the program's server build");
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash, _server) =
        start_build("chrome", &server, &runtime, wrela_serve::Bounds::default());
    let path = format!("/room/chrome?pair={:016x}", wrela_abi::read_pair(&server).expect("pair"));
    std::fs::write(dir.join("net.json"), wrela_serve::net_json(port, Some(&hash)))
        .expect("net.json");
    // Another peer's tag is in the room first: the browser catches up to it.
    runtime.block_on(async {
        let mut a = connect(port, hash, &path).await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        a.put(&propose(1, 1, 1, 11)).await;
        assert_eq!(a.next().await, Some(Down::Committed { seq: 1, counter: 1, value: 11 }));
    });
    let run = wrela_tests::run_in_chrome(&rel, 240, 16, 16, 60.0);
    let printed: Vec<&str> = run.printed.iter().map(|(_, line)| line.as_str()).collect();
    assert_eq!(printed, ["tags 11 10"], "the browser's log has the room's tags, its own committed");
}

#[test]
fn a_new_build_closes_the_rooms_and_keeps_each_builds_apart() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash, server) =
        start_build("builds", wasm(), &runtime, wrela_serve::Bounds::default());
    let build = || Arc::new(ServerBuild::from_wasm(wasm()).expect("a server build"));
    let first = wrela_abi::ticks::wasm_hash(wasm());
    runtime.block_on(async {
        let mut a = connect(port, hash, &room("builds")).await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        a.put(&propose(1, 1, 1, 10)).await;
        assert_eq!(a.next().await, Some(Down::Committed { seq: 1, counter: 1, value: 10 }));
        // Another build (its hash says so): the connection closes, and the room it comes back
        // to is the new build's, with nothing in it.
        server.set_build(build(), first ^ 1);
        assert_eq!(a.next().await, None, "the connection closed");
        let mut b = connect(port, hash, &room("builds")).await.expect("connects again");
        b.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(b.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        // Back to the first: its room has what it had.
        server.set_build(build(), first);
        assert_eq!(b.next().await, None, "the connection closed");
        let mut c = connect(port, hash, &room("builds")).await.expect("connects again");
        c.put(&hello(5, 2, 1, 1)).await;
        assert_eq!(c.next().await, Some(Down::Welcome { peer: 1, seq: 1 }));
    });
}

#[test]
fn a_client_of_another_program_is_refused() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash) = start("pairs", &runtime);
    let pair = wrela_abi::read_pair(wasm()).expect("the program's pair id");
    runtime.block_on(async {
        for path in [
            format!("/room/pairs?pair={:016x}", pair ^ 1),
            "/room/pairs".to_string(),
            "/room/pairs?pair=12".to_string(),
        ] {
            let refused = connect(port, hash, &path).await.err();
            assert!(refused.is_some_and(|e| e.contains("rejected")), "{path}");
        }
        let mut a = connect(port, hash, &room("pairs")).await.expect("its own program's client");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
    });
}

#[test]
fn rooms_and_sessions_are_bounded_and_an_idle_room_stops() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let bounds = wrela_serve::Bounds {
        sessions: 3,
        rooms: 2,
        queued: 64 << 20,
        idle: Duration::from_millis(300),
    };
    let (port, hash, server) = start_build("bounds", wasm(), &runtime, bounds);
    runtime.block_on(async {
        let mut a = connect(port, hash, &room("one")).await.expect("a");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        let mut b = connect(port, hash, &room("two")).await.expect("b");
        b.put(&hello(6, 1, 0, 0)).await;
        assert_eq!(b.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        // A third room is past the rooms the server may run.
        let refused = connect(port, hash, &room("three")).await.err();
        assert!(refused.is_some_and(|e| e.contains("rejected")));
        // A third session to a room running is let in; a fourth is past the sessions.
        let c = connect(port, hash, &room("one")).await.expect("c");
        assert!(connect(port, hash, &room("one")).await.is_err());
        assert_eq!(server.held().0, 2);
        // The rooms' connections go: once idle, each stops, and the bytes queued are back.
        drop((a, b, c));
        let mut stopped = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if server.held() == (0, 0) {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "held {:?}", server.held());
        // A room that stopped starts again from its journal: the peer is known.
        let mut d = connect(port, hash, &room("one")).await.expect("d");
        d.put(&hello(5, 2, 0, 0)).await;
        assert_eq!(d.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
    });
}

#[test]
fn a_quarantined_room_closes_its_sessions_and_stops() {
    let deaf = server_wasm("deaf-transport", DEAF);
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let bounds = wrela_serve::Bounds { idle: Duration::from_millis(300), ..Default::default() };
    let (port, hash, server) = start_build("deaf", &deaf, &runtime, bounds);
    let path = format!("/room/deaf?pair={:016x}", wrela_abi::read_pair(&deaf).expect("pair"));
    runtime.block_on(async {
        let mut a = connect(port, hash, &path).await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert!(a.ends(Duration::from_secs(5)).await, "the session closed");
        let mut stopped = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if server.held() == (0, 0) {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "held {:?}", server.held());
    });
}

#[test]
fn a_room_past_its_limits_tries_again_and_lets_its_peer_go() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let (port, hash) = start("limits-again", &runtime);
    runtime.block_on(async {
        let mut a = connect(port, hash, &room("limits-again")).await.expect("connects");
        a.put(&hello(5, 1, 0, 0)).await;
        assert_eq!(a.next().await, Some(Down::Welcome { peer: 1, seq: 0 }));
        // 99 runs past every call's deadline. The peer sends nothing more: the room tries again
        // on its own, and after the retries lets the peer go.
        a.put(&propose(1, 1, 1, 99)).await;
        assert!(a.ends(Duration::from_secs(5)).await, "the session closed");
    });
}
