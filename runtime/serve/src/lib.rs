//! The server host (language.md §12): a server build's rooms, over WebTransport. A session at
//! `/room/<name>` is a connection of room `<name>`; it opens one bidirectional stream, on which
//! each message is a frame: its length (a varint, at most [`MAX_FRAME`] bytes), then its bytes.
//! Each room runs on a thread of its own ([`wrela_host::rooms::RoomHost`]), which keeps it
//! durably in the data directory and releases what it sends only once its records are kept.
//!
//! Every queue is bounded: a connection whose messages the room can't take fast enough, or that
//! can't take the room's fast enough, is closed, and its peer comes back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wrela_host::rooms::{Out, RoomHost, Status};
use wrela_host::server::{Limits, ServerBuild};
use wtransport::endpoint::IncomingSession;
use wtransport::{Endpoint, Identity, ServerConfig};

/// The largest frame, in bytes.
pub const MAX_FRAME: usize = 64 * 1024;
/// How many messages may wait for a room, and for a connection, before it's closed.
pub const QUEUE: usize = 1024;
/// How long a session may take to open its stream.
const OPENING: Duration = Duration::from_secs(10);

/// What the server runs.
pub struct Config {
    /// The server build (`server/game.wasm` of a build directory), its WASM's hash, and where
    /// its rooms' journals go.
    pub build: Arc<ServerBuild>,
    pub build_hash: u64,
    pub data: PathBuf,
    pub port: u16,
    pub identity: Identity,
    pub limits: Limits,
}

/// What a room's thread is told.
enum ToRoom {
    Opened(u32, tokio::sync::mpsc::Sender<Out>),
    Closed(u32),
    Message(u32, Vec<u8>),
}

/// The server's rooms, each a thread, started when its first connection comes.
struct Rooms {
    build: Arc<ServerBuild>,
    build_hash: u64,
    data: PathBuf,
    limits: Limits,
    rooms: Mutex<HashMap<String, std::sync::mpsc::SyncSender<ToRoom>>>,
    next_conn: AtomicU32,
}

/// Whether `name` names a room: 1 to 64 of a-z, 0-9, `-` and `_`.
pub fn room_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

impl Rooms {
    /// The queue to room `name`'s thread, started if it isn't running.
    fn room(self: &Arc<Self>, name: &str) -> std::sync::mpsc::SyncSender<ToRoom> {
        let mut rooms = self.rooms.lock().expect("the rooms' lock");
        if let Some(tx) = rooms.get(name) {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        rooms.insert(name.to_string(), tx.clone());
        let me = Arc::clone(self);
        let name = name.to_string();
        std::thread::Builder::new()
            .name(format!("room {name}"))
            .spawn(move || me.run(&name, &rx))
            .expect("can't start a room's thread");
        tx
    }

    /// Room `name`'s thread: it takes what has come, wakes the room, and gives each connection
    /// what the room released.
    fn run(&self, name: &str, rx: &std::sync::mpsc::Receiver<ToRoom>) {
        let started = Instant::now();
        let mut room = match RoomHost::open(
            Arc::clone(&self.build),
            self.build_hash,
            &self.data,
            name,
            self.limits,
        ) {
            Ok(room) => Some(room),
            Err(e) => {
                eprintln!("wrela-serve: room {name} doesn't open: {e}");
                None
            }
        };
        let mut outs: HashMap<u32, tokio::sync::mpsc::Sender<Out>> = HashMap::new();
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            let Some(host) = room.as_mut() else {
                // A room that didn't open closes what comes to it.
                for e in batch {
                    if let ToRoom::Opened(_, tx) = e {
                        let _ = tx.try_send(Out::Close(0));
                    }
                }
                continue;
            };
            for e in batch {
                match e {
                    ToRoom::Opened(conn, tx) => {
                        outs.insert(conn, tx);
                        host.connect(conn);
                    }
                    ToRoom::Closed(conn) => {
                        outs.remove(&conn);
                        host.disconnect(conn);
                    }
                    ToRoom::Message(conn, bytes) => host.receive(conn, bytes),
                }
            }
            if let Err(e) = host.wake(started.elapsed().as_secs_f32()) {
                eprintln!("wrela-serve: room {name}: {e}");
            }
            for o in host.take_outbox() {
                let conn = match &o {
                    Out::Send(c, _) | Out::Close(c) => *c,
                };
                let Some(tx) = outs.get(&conn) else { continue };
                if tx.try_send(o).is_err() {
                    // It can't take what it's sent fast enough: it goes, and comes back.
                    outs.remove(&conn);
                    host.disconnect(conn);
                }
            }
            if let Status::Quarantined(why) = host.status() {
                eprintln!("wrela-serve: room {name} is quarantined: {why}");
            }
        }
    }
}

/// Writes `x` as a varint.
fn varint(out: &mut Vec<u8>, mut x: usize) {
    loop {
        let group = (x & 127) as u8;
        x >>= 7;
        if x == 0 {
            out.push(group);
            return;
        }
        out.push(group | 128);
    }
}

/// A frame: `bytes`' length, then them.
pub fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 3);
    varint(&mut out, bytes.len());
    out.extend_from_slice(bytes);
    out
}

/// Reads one frame from `recv`: `None` at the stream's end, or for a frame too long or whose
/// length isn't a varint.
async fn read_frame(recv: &mut wtransport::RecvStream) -> Option<Vec<u8>> {
    let mut len = 0usize;
    for k in 0..3 {
        let mut byte = [0u8];
        recv.read_exact(&mut byte).await.ok()?;
        len |= usize::from(byte[0] & 127) << (7 * k);
        if byte[0] < 128 {
            if len > MAX_FRAME {
                return None;
            }
            let mut bytes = vec![0u8; len];
            recv.read_exact(&mut bytes).await.ok()?;
            return Some(bytes);
        }
    }
    None
}

/// One session: a connection of the room its path names.
async fn session(incoming: IncomingSession, rooms: Arc<Rooms>) {
    let Ok(request) = incoming.await else { return };
    let Some(name) = request.path().strip_prefix("/room/").map(str::to_string) else {
        request.not_found().await;
        return;
    };
    if !room_name(&name) {
        request.not_found().await;
        return;
    }
    let Ok(connection) = request.accept().await else { return };
    let conn = rooms.next_conn.fetch_add(1, Ordering::Relaxed);
    let to_room = rooms.room(&name);
    let (tx, mut from_room) = tokio::sync::mpsc::channel::<Out>(QUEUE);
    if to_room.try_send(ToRoom::Opened(conn, tx)).is_err() {
        return;
    }
    let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(OPENING, connection.accept_bi()).await
    else {
        let _ = to_room.try_send(ToRoom::Closed(conn));
        return;
    };
    let reading = async {
        while let Some(bytes) = read_frame(&mut recv).await {
            if to_room.try_send(ToRoom::Message(conn, bytes)).is_err() {
                // The room can't take this connection's messages fast enough.
                break;
            }
        }
    };
    let writing = async {
        while let Some(o) = from_room.recv().await {
            match o {
                Out::Send(_, bytes) => {
                    if send.write_all(&frame(&bytes)).await.is_err() {
                        break;
                    }
                }
                Out::Close(_) => break,
            }
        }
    };
    tokio::select! {
        () = reading => {}
        () = writing => {}
    }
    let _ = rooms.room(&name).try_send(ToRoom::Closed(conn));
    connection.close(0u32.into(), b"");
}

/// Serves `config`'s rooms until the process ends.
pub async fn serve(config: Config) -> std::io::Result<()> {
    let server = ServerConfig::builder()
        .with_bind_default(config.port)
        .with_identity(config.identity)
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .build();
    let endpoint = Endpoint::server(server)?;
    let rooms = Arc::new(Rooms {
        build: config.build,
        build_hash: config.build_hash,
        data: config.data,
        limits: config.limits,
        rooms: Mutex::new(HashMap::new()),
        next_conn: AtomicU32::new(1),
    });
    loop {
        let incoming = endpoint.accept().await;
        tokio::spawn(session(incoming, Arc::clone(&rooms)));
    }
}

/// A development certificate (self-signed, valid under 14 days, as browsers take one by its hash)
/// for `localhost`, and its SHA-256 hash.
pub fn dev_identity() -> std::io::Result<(Identity, [u8; 32])> {
    let identity =
        Identity::self_signed(["localhost", "127.0.0.1", "::1"]).map_err(std::io::Error::other)?;
    let hash = identity.certificate_chain().as_slice()[0].hash();
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_ref());
    Ok((identity, out))
}

/// What a client build reads to find its server (`net.json` beside it): the server's address and,
/// for a development certificate, its hash.
pub fn net_json(port: u16, certificate_hash: Option<&[u8; 32]>) -> String {
    let hash = certificate_hash
        .map(|h| {
            format!(
                ",\n  \"certificateHash\": [{}]",
                h.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")
            )
        })
        .unwrap_or_default();
    format!("{{\n  \"url\": \"https://localhost:{port}\"{hash}\n}}\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn room_names_are_plain() {
        assert!(super::room_name("board-7f3a_1"));
        assert!(!super::room_name("") && !super::room_name("../etc") && !super::room_name("Big"));
        assert!(!super::room_name(&"a".repeat(65)));
    }

    #[test]
    fn a_frame_is_its_length_then_its_bytes() {
        assert_eq!(super::frame(b"hi"), [2, b'h', b'i']);
        assert_eq!(&super::frame(&[0; 300])[..2], [0xAC, 0x02]);
    }
}
