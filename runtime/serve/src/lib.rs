//! The server host (language.md §19): a server build's rooms, over WebTransport. A session at
//! `/room/<name>?pair=<id>` is a connection of room `<name>`, from a client build whose pair id
//! is `<id>` (16 hex digits): one of another pair is refused before it reaches a room. The pair
//! identifies a build, not a trusted peer. A browser's Origin must be allowed; a room's program
//! decides who may use it. A session opens one bidirectional stream, on which each message is a
//! frame: its length (a varint, at most
//! [`MAX_FRAME`] bytes, and [`MAX_UP`] from a client), then its bytes. Each room runs on a
//! thread of its own
//! ([`wrela_host::rooms::RoomHost`]), which keeps it durably in the data directory and releases
//! what it sends only once its records are kept; a room with no connections for
//! [`Bounds::idle`] stops, and starts again from its journal when one comes.
//!
//! What it holds is bounded ([`Bounds`]): the sessions open or opening, the rooms running, and
//! the bytes queued, each message counted with [`OVERHEAD`] more. Each connection may hold
//! [`RESERVED`] bytes each way whatever the others hold; past that, its messages draw on what
//! every queue shares ([`Bounds::queued`]), so a few slow connections can't crowd out the rest.
//! A connection that sends faster than its room takes ([`CONN_IN`] waiting), or takes what its
//! room sends slower than the room sends it ([`CONN_OUT`] waiting, or a write that takes
//! [`WRITE`]), is closed, and its peer comes back. At most, then, the queues hold
//! `2 × sessions × RESERVED + queued` bytes. QUIC stream, receive and send windows bound its
//! buffers as well. An idle reader closes after 60 seconds; an unfinished frame after 10.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use url::{Host, Url};
use wrela_host::rooms::{Out, RoomHost, Status};
use wrela_host::server::{Limits, ServerBuild};
use wrela_host::services::Services;
use wtransport::config::{IpBindConfig, QuicTransportConfig};
use wtransport::endpoint::IncomingSession;
use wtransport::quinn::VarInt;
use wtransport::{Endpoint, Identity, ServerConfig};

/// The largest frame, in bytes.
pub const MAX_FRAME: usize = 64 * 1024;
/// The largest frame a client sends: a room's answer to a message adds a header to it (an
/// entry's number and time, say), and must still fit a frame.
pub const MAX_UP: usize = MAX_FRAME - 64;
/// The bytes each connection may hold each way whatever the others hold.
pub const RESERVED: usize = 128 << 10;
/// How many messages may wait for a room, and for a connection.
pub const QUEUE: usize = 1024;
/// The bytes a message is counted as past its own: what holding it costs.
pub const OVERHEAD: usize = 64;
/// The bytes a connection's messages may hold waiting for its room.
pub const CONN_IN: usize = 1 << 20;
/// The bytes a room's messages may hold waiting for a connection: a welcome's catch-up
/// (`std::net::CATCH_UP` entries) fits.
pub const CONN_OUT: usize = 16 << 20;
/// How long a session may take to open, and to open its stream.
const OPENING: Duration = Duration::from_secs(10);
/// How long an established connection may send no message, and how long it may take to finish a
/// frame after its first byte. QUIC keepalives do not count as messages.
const READ_IDLE: Duration = Duration::from_secs(60);
const FRAME_READ: Duration = Duration::from_secs(10);
/// How long a write to a connection may take.
pub const WRITE: Duration = Duration::from_secs(10);
/// How often a room with nothing to do looks at its connections and whether to stop.
const TICK: Duration = Duration::from_millis(250);

/// How much the server holds at most.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// Sessions open or opening.
    pub sessions: usize,
    /// Rooms running.
    pub rooms: usize,
    /// The bytes every queue holds together.
    pub queued: usize,
    /// How long a room with no connections runs before it stops.
    pub idle: Duration,
}

impl Default for Bounds {
    /// 256 sessions, 64 rooms, 64 MiB queued, and 30 s idle.
    fn default() -> Bounds {
        Bounds { sessions: 256, rooms: 64, queued: 64 << 20, idle: Duration::from_secs(30) }
    }
}

/// What the server runs.
pub struct Config {
    /// The server build (`server/game.wasm` of a build directory), its WASM's hash, and where
    /// its rooms' journals go.
    pub build: Arc<ServerBuild>,
    pub build_hash: u64,
    pub data: PathBuf,
    /// The services every room shares: its store, which outlives the build (none: a room's
    /// requests fail).
    pub services: Option<Arc<Services>>,
    pub port: u16,
    pub identity: Identity,
    pub limits: Limits,
    pub bounds: Bounds,
    /// Which browser page origins may open a session. Clients without an Origin header are not
    /// browsers and must be authorized by the room's own protocol.
    pub origins: Origins,
}

/// Browser origins allowed to open rooms. A production server names each page origin exactly;
/// a local development server accepts only localhost and loopback page origins, at any port.
pub enum Origins {
    Local,
    Exact(Vec<String>),
}

impl Origins {
    fn permits(&self, origin: Option<&str>) -> bool {
        let Some(origin) = origin else { return true };
        let Some(parsed) = parsed_origin(origin) else { return false };
        match self {
            Origins::Local => matches!(
                parsed.host(),
                Some(Host::Domain("localhost"))
                    | Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
                    | Some(Host::Ipv6(Ipv6Addr::LOCALHOST))
            ),
            Origins::Exact(allowed) => allowed.iter().any(|x| x == origin),
        }
    }

    fn validate(&self) -> std::io::Result<()> {
        if let Origins::Exact(allowed) = self {
            if allowed.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "at least one page origin is required",
                ));
            }
            if let Some(bad) = allowed.iter().find(|origin| parsed_origin(origin).is_none()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid page origin {bad:?}"),
                ));
            }
        }
        Ok(())
    }
}

fn parsed_origin(origin: &str) -> Option<Url> {
    let parsed = Url::parse(origin).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.origin().ascii_serialization() == origin)
        .then_some(parsed)
}

/// A share of a bound: `n` of what each of `counts` counts, given back when it's dropped.
struct Share {
    n: usize,
    counts: Vec<Arc<AtomicUsize>>,
}

impl Share {
    /// `n` of each counter, if none passes its cap.
    fn take(n: usize, counters: &[(&Arc<AtomicUsize>, usize)]) -> Option<Share> {
        let mut share = Share { n, counts: Vec::new() };
        for &(count, cap) in counters {
            count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| {
                    (x + n <= cap).then_some(x + n)
                })
                .ok()?;
            share.counts.push(Arc::clone(count));
        }
        Some(share)
    }

    /// `n` bytes for a connection whose messages hold `conn` (at most `cap`): from its reserve
    /// while it holds less than [`RESERVED`], else from what every queue shares too (`shared`,
    /// at most `shared_cap`).
    fn bytes(
        n: usize,
        (conn, cap): (&Arc<AtomicUsize>, usize),
        (shared, shared_cap): (&Arc<AtomicUsize>, usize),
    ) -> Option<Share> {
        // Only this connection's one sender adds to `conn`, so what it reads can only fall.
        if conn.load(Ordering::Acquire) + n <= RESERVED {
            Share::take(n, &[(conn, cap)])
        } else {
            Share::take(n, &[(conn, cap), (shared, shared_cap)])
        }
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        for c in &self.counts {
            c.fetch_sub(self.n, Ordering::AcqRel);
        }
    }
}

/// What carries a room's messages to a connection, and the bytes they hold.
struct Link {
    tx: tokio::sync::mpsc::Sender<(Out, Option<Share>)>,
    stop: tokio::sync::watch::Sender<bool>,
    held: Arc<AtomicUsize>,
}

impl Link {
    /// Tells its session to close even when its outbound queue is full.
    fn close(&self) {
        self.stop.send_replace(true);
    }
}

/// What a room's thread is told.
enum ToRoom {
    Opened(u32, Link),
    Closed(u32),
    Message(u32, Vec<u8>, Share),
    /// The server has a new build: close every connection, and stop.
    Stop,
}

/// A running room: its queue, and whether it's told to stop.
struct Running {
    tx: SyncSender<ToRoom>,
    stop: Arc<AtomicBool>,
}

/// The build new rooms run, and the rooms running it.
struct State {
    build: Arc<ServerBuild>,
    hash: u64,
    rooms: HashMap<String, Running>,
}

/// The server's rooms, each a thread, started when its first connection comes.
struct Rooms {
    state: Mutex<State>,
    data: PathBuf,
    services: Option<Arc<Services>>,
    limits: Limits,
    bounds: Bounds,
    origins: Origins,
    next_conn: AtomicU32,
    /// When it started: its rooms' time, which a room that stops and starts again goes on with.
    started: Instant,
    sessions: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
    queued: Arc<AtomicUsize>,
}

/// Why a session isn't let in.
enum Refusal {
    /// Its client is another program's build.
    Pair,
    /// The server holds all it may.
    Busy,
}

/// Whether `name` names a room: 1 to 64 of a-z, 0-9, `-` and `_`.
pub fn room_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A session's path: its room, and its client's pair id (none if it's missing or isn't 16 hex
/// digits). None if it isn't a room's.
pub fn session_path(path: &str) -> Option<(&str, Option<u64>)> {
    let rest = path.strip_prefix("/room/")?;
    let (name, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !room_name(name) {
        return None;
    }
    let pair = query.split('&').find_map(|kv| kv.strip_prefix("pair=")).and_then(|hex| {
        (hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u64::from_str_radix(hex, 16).ok())
            .flatten()
    });
    Some((name, pair))
}

/// The next non-reserved connection number, if the server still has one.
fn connection_id(next: &AtomicU32) -> Option<u32> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
        (id < wrela_host::rooms::POISONED).then(|| id + 1)
    })
    .ok()
}

impl Rooms {
    /// Lets a client of pair `pair` into room `name`, started if it isn't running: its
    /// connection number and the room's queue. The build is checked under the same lock as a
    /// new build.
    fn join(
        self: &Arc<Self>,
        name: &str,
        pair: Option<u64>,
        link: Link,
    ) -> Result<(u32, SyncSender<ToRoom>), Refusal> {
        let mut state = self.state.lock().expect("the rooms' lock");
        if pair.is_none() || state.build.pair != pair {
            return Err(Refusal::Pair);
        }
        let conn = connection_id(&self.next_conn).ok_or(Refusal::Busy)?;
        let tx = match state.rooms.get(name) {
            Some(r) => r.tx.clone(),
            None => {
                let permit =
                    Share::take(1, &[(&self.running, self.bounds.rooms)]).ok_or(Refusal::Busy)?;
                let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
                let stop = Arc::new(AtomicBool::new(false));
                let (rooms, build, hash) = (Arc::clone(self), state.build.clone(), state.hash);
                let (room, halt) = (name.to_string(), Arc::clone(&stop));
                std::thread::Builder::new()
                    .name(format!("room {name}"))
                    .spawn(move || {
                        let _permit = permit;
                        run(&rooms, build, hash, &room, &rx, &halt);
                    })
                    .map_err(|_| Refusal::Busy)?;
                state.rooms.insert(name.to_string(), Running { tx: tx.clone(), stop });
                tx
            }
        };
        tx.try_send(ToRoom::Opened(conn, link)).map_err(|_| Refusal::Busy)?;
        Ok((conn, tx))
    }

    /// From now on, rooms run `build`: each room running closes its connections and stops.
    fn set_build(&self, build: Arc<ServerBuild>, hash: u64) {
        let mut state = self.state.lock().expect("the rooms' lock");
        state.build = build;
        state.hash = hash;
        for (_, r) in state.rooms.drain() {
            // The flag stops it even if its queue is full; the message wakes it at once.
            r.stop.store(true, Ordering::Release);
            let _ = r.tx.try_send(ToRoom::Stop);
        }
    }

    /// Whether room `name`'s thread, told to stop by `stop`, may stop for want of connections:
    /// nothing waits in its queue, and nothing can, as a session joins under the same lock. What
    /// did wait goes to `carried`.
    fn retire(
        &self,
        name: &str,
        stop: &Arc<AtomicBool>,
        rx: &Receiver<ToRoom>,
        carried: &mut Vec<ToRoom>,
    ) -> bool {
        let mut state = self.state.lock().expect("the rooms' lock");
        match rx.try_recv() {
            Ok(e) => {
                carried.push(e);
                false
            }
            Err(_) => {
                if state.rooms.get(name).is_some_and(|r| Arc::ptr_eq(&r.stop, stop)) {
                    state.rooms.remove(name);
                }
                true
            }
        }
    }
}

/// Room `name`'s thread: it takes what has come, wakes the room, and gives each connection what
/// the room released, until it's told to stop, has had no connection for the idle time, or
/// nothing can reach it.
fn run(
    rooms: &Rooms,
    build: Arc<ServerBuild>,
    hash: u64,
    name: &str,
    rx: &Receiver<ToRoom>,
    stop: &Arc<AtomicBool>,
) {
    let data = rooms.data.join(format!("{hash:016x}"));
    let mut room =
        match RoomHost::open(build, hash, &data, name, rooms.limits, rooms.services.clone()) {
            Ok(room) => Some(room),
            Err(e) => {
                eprintln!("wrela-serve: room {name} doesn't open: {e}");
                None
            }
        };
    let mut outs: HashMap<u32, Link> = HashMap::new();
    let mut carried: Vec<ToRoom> = Vec::new();
    // The bytes of the messages waiting in the room, held until it has none waiting.
    let mut shares: Vec<Share> = Vec::new();
    let mut last_conn = Instant::now();
    loop {
        let mut batch = std::mem::take(&mut carried);
        if batch.is_empty() {
            match rx.recv_timeout(TICK) {
                Ok(e) => batch.push(e),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        while batch.len() < QUEUE {
            match rx.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        if stop.load(Ordering::Acquire) || batch.iter().any(|e| matches!(e, ToRoom::Stop)) {
            outs.values().for_each(Link::close);
            for e in batch {
                if let ToRoom::Opened(_, link) = e {
                    link.close();
                }
            }
            return;
        }
        let Some(host) = room.as_mut() else {
            // A room that didn't open, or was quarantined, closes what comes to it, and stops
            // once nothing does.
            let mut joined = false;
            for e in batch {
                if let ToRoom::Opened(_, link) = e {
                    link.close();
                    joined = true;
                }
            }
            if !joined && rooms.retire(name, stop, rx, &mut carried) {
                return;
            }
            continue;
        };
        // A room with events waiting (a call past the host's limits) tries again each tick.
        let mut woken = !batch.is_empty() || host.waiting() > 0;
        for e in batch {
            match e {
                ToRoom::Opened(conn, link) => {
                    outs.insert(conn, link);
                    host.connect(conn);
                }
                ToRoom::Closed(conn) => {
                    if outs.remove(&conn).is_some() {
                        host.disconnect(conn);
                    }
                }
                // A message from a connection the room let go goes nowhere.
                ToRoom::Message(conn, bytes, share) => {
                    if outs.contains_key(&conn) {
                        host.receive(conn, bytes);
                        shares.push(share);
                    }
                }
                ToRoom::Stop => {}
            }
        }
        // A session that ended without telling the room (its queue was full) has dropped its
        // end of the link.
        outs.retain(|&conn, link| {
            let gone = link.tx.is_closed();
            if gone {
                host.disconnect(conn);
                woken = true;
            }
            !gone
        });
        if woken {
            if let Err(e) = host.wake(rooms.started.elapsed().as_secs_f32()) {
                eprintln!("wrela-serve: room {name}: {e}");
            }
            if host.waiting() == 0 {
                shares.clear();
            }
            for o in host.take_outbox() {
                let conn = match &o {
                    Out::Send(c, _) | Out::Close(c) => *c,
                };
                let Some(link) = outs.get(&conn) else { continue };
                let sent = match o {
                    Out::Send(c, bytes) => Share::bytes(
                        bytes.len() + OVERHEAD,
                        (&link.held, CONN_OUT),
                        (&rooms.queued, rooms.bounds.queued),
                    )
                    .is_some_and(|share| {
                        link.tx.try_send((Out::Send(c, bytes), Some(share))).is_ok()
                    }),
                    close => link.tx.try_send((close, None)).is_ok(),
                };
                if !sent {
                    // It can't take what it's sent fast enough: it goes, and comes back.
                    link.close();
                    outs.remove(&conn);
                    host.disconnect(conn);
                }
            }
            if let Status::Quarantined(why) = host.status() {
                // Its connections were closed (above): it takes no more, until it opens again.
                eprintln!("wrela-serve: room {name} is quarantined: {why}");
                outs.drain().for_each(|(_, link)| link.close());
                shares.clear();
                room = None;
            }
        }
        if !outs.is_empty() {
            last_conn = Instant::now();
        } else if last_conn.elapsed() >= rooms.bounds.idle
            && rooms.retire(name, stop, rx, &mut carried)
        {
            return;
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

/// Reads one frame from a client: `None` at the stream's end, on a deadline, or for a frame past
/// [`MAX_UP`] or whose length isn't a varint.
async fn read_frame<R: AsyncRead + Unpin>(
    recv: &mut R,
    idle: Duration,
    frame_read: Duration,
) -> Option<Vec<u8>> {
    let mut first = [0u8];
    tokio::time::timeout(idle, recv.read_exact(&mut first)).await.ok()?.ok()?;
    tokio::time::timeout(frame_read, async {
        let mut len = 0usize;
        let mut byte = first;
        for k in 0..3 {
            len |= usize::from(byte[0] & 127) << (7 * k);
            if byte[0] < 128 {
                if len > MAX_UP {
                    return None;
                }
                let mut bytes = vec![0u8; len];
                recv.read_exact(&mut bytes).await.ok()?;
                return Some(bytes);
            }
            if k < 2 {
                recv.read_exact(&mut byte).await.ok()?;
            }
        }
        None
    })
    .await
    .ok()?
}

/// One session: a connection of the room its path names. `_permit` is its share of the
/// sessions the server may hold.
async fn session(incoming: IncomingSession, rooms: Arc<Rooms>, _permit: Share) {
    let Ok(Ok(request)) = tokio::time::timeout(OPENING, incoming).await else { return };
    if !rooms.origins.permits(request.origin()) {
        request.forbidden().await;
        return;
    }
    let Some((name, pair)) = session_path(request.path()).map(|(n, p)| (n.to_string(), p)) else {
        request.not_found().await;
        return;
    };
    let (tx, mut from_room) = tokio::sync::mpsc::channel(QUEUE);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let link = Link { tx, stop, held: Arc::new(AtomicUsize::new(0)) };
    let (conn, to_room) = match rooms.join(&name, pair, link) {
        Ok(joined) => joined,
        Err(Refusal::Pair) => return request.forbidden().await,
        Err(Refusal::Busy) => return request.too_many_requests().await,
    };
    // From here, however it ends, the room hears: from `Closed`, or, if its queue is full, as
    // it finds this end of the link dropped.
    let Ok(Ok(connection)) = tokio::time::timeout(OPENING, request.accept()).await else {
        return;
    };
    let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(OPENING, connection.accept_bi()).await
    else {
        let _ = to_room.try_send(ToRoom::Closed(conn));
        return;
    };
    let held = Arc::new(AtomicUsize::new(0));
    let reading = async {
        while let Some(bytes) = read_frame(&mut recv, READ_IDLE, FRAME_READ).await {
            let share = Share::bytes(
                bytes.len() + OVERHEAD,
                (&held, CONN_IN),
                (&rooms.queued, rooms.bounds.queued),
            );
            // The room can't take this connection's messages fast enough.
            let Some(share) = share else { break };
            if to_room.try_send(ToRoom::Message(conn, bytes, share)).is_err() {
                break;
            }
        }
    };
    let writing = async {
        while let Some((o, _share)) = from_room.recv().await {
            match o {
                Out::Send(_, bytes) => {
                    if bytes.len() > MAX_FRAME {
                        break;
                    }
                    let wrote = tokio::time::timeout(WRITE, send.write_all(&frame(&bytes))).await;
                    if !matches!(wrote, Ok(Ok(()))) {
                        return false;
                    }
                }
                Out::Close(_) => break,
            }
        }
        true
    };
    let finish = tokio::select! {
        biased;
        _ = stopped.changed() => false,
        () = reading => false,
        finish = writing => finish,
    };
    if finish {
        // A successful write only queued its bytes. Let the peer acknowledge them before
        // closing the session, which would otherwise drop the last frame's tail.
        let _ = tokio::time::timeout(WRITE, send.finish()).await;
    }
    // What waited for it goes now, and with it the bytes it held.
    from_room.close();
    while from_room.try_recv().is_ok() {}
    let _ = to_room.try_send(ToRoom::Closed(conn));
    connection.close(0u32.into(), b"");
}

/// A running server: what changes the build its rooms run.
pub struct Server {
    rooms: Arc<Rooms>,
    local_addr: SocketAddr,
}

impl Server {
    /// The address the server socket actually uses.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// From now on, rooms run `build` (its WASM's hash `hash`), kept apart from the last build's:
    /// each room running closes its connections, and its peers come back to the new build's.
    pub fn set_build(&self, build: Arc<ServerBuild>, hash: u64) {
        self.rooms.set_build(build, hash);
    }

    /// How many rooms are running, and how many bytes every queue holds: what a test checks
    /// the bounds by.
    pub fn held(&self) -> (usize, usize) {
        (self.rooms.running.load(Ordering::Acquire), self.rooms.queued.load(Ordering::Acquire))
    }
}

/// Starts serving `config`'s rooms on `runtime`, until it ends: the server, or why its port can't
/// be had.
pub fn start(config: Config, runtime: &tokio::runtime::Runtime) -> std::io::Result<Server> {
    start_with_bind(config, runtime, IpBindConfig::InAddrAnyDual)
}

/// Starts a development server on loopback addresses only.
pub fn start_local(config: Config, runtime: &tokio::runtime::Runtime) -> std::io::Result<Server> {
    start_with_bind(config, runtime, IpBindConfig::LocalDual)
}

fn transport_config() -> QuicTransportConfig {
    let mut transport = QuicTransportConfig::default();
    // HTTP/3 uses a CONNECT stream and control streams. The room uses one more bidirectional
    // stream. Unused streams and datagrams must not bypass the room's queue bounds.
    transport.max_concurrent_bidi_streams(VarInt::from_u32(2));
    transport.max_concurrent_uni_streams(VarInt::from_u32(4));
    transport.stream_receive_window(VarInt::from_u32(128 << 10));
    transport.receive_window(VarInt::from_u32(512 << 10));
    transport.send_window(512 << 10);
    transport.datagram_receive_buffer_size(Some(1024));
    transport
}

fn start_with_bind(
    config: Config,
    runtime: &tokio::runtime::Runtime,
    bind: IpBindConfig,
) -> std::io::Result<Server> {
    config.origins.validate()?;
    let _entered = runtime.enter();
    let server = ServerConfig::builder()
        .with_bind_config(bind, config.port)
        .with_custom_transport(config.identity, transport_config())
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .build();
    let endpoint = Endpoint::server(server)?;
    let local_addr = endpoint.local_addr()?;
    let rooms = Arc::new(Rooms {
        state: Mutex::new(State {
            build: config.build,
            hash: config.build_hash,
            rooms: HashMap::new(),
        }),
        data: config.data,
        services: config.services,
        limits: config.limits,
        bounds: config.bounds,
        origins: config.origins,
        next_conn: AtomicU32::new(1),
        started: Instant::now(),
        sessions: Arc::new(AtomicUsize::new(0)),
        running: Arc::new(AtomicUsize::new(0)),
        queued: Arc::new(AtomicUsize::new(0)),
    });
    let accepting = Arc::clone(&rooms);
    runtime.spawn(async move {
        loop {
            let incoming = endpoint.accept().await;
            // Past the sessions it may hold, a session is dropped before its handshake.
            let permit = Share::take(1, &[(&accepting.sessions, accepting.bounds.sessions)]);
            if let Some(permit) = permit {
                tokio::spawn(session(incoming, Arc::clone(&accepting), permit));
            }
        }
    });
    Ok(Server { rooms, local_addr })
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
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn a_full_outbound_queue_does_not_block_session_shutdown() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        tx.try_send((wrela_host::rooms::Out::Send(1, vec![0]), None))
            .expect("fill the outbound queue");
        let link = super::Link { tx, stop, held: Arc::new(AtomicUsize::new(0)) };
        link.close();
        tokio::time::timeout(Duration::from_secs(1), stopped.changed())
            .await
            .expect("shutdown signal is not queued")
            .expect("shutdown sender remains open");
        assert!(*stopped.borrow());
    }

    #[test]
    fn browser_origins_are_checked_before_a_room_opens() {
        let local = super::Origins::Local;
        for origin in ["http://localhost:8000", "https://127.0.0.1", "http://[::1]:1234"] {
            assert!(local.permits(Some(origin)), "{origin}");
        }
        for origin in [
            "https://attacker.example",
            "http://localhost.attacker.example",
            "http://localhost:12/path",
            "http://localhost:99999",
            "null",
        ] {
            assert!(!local.permits(Some(origin)), "{origin}");
        }
        let exact = super::Origins::Exact(vec!["https://game.example:8443".to_string()]);
        assert!(exact.validate().is_ok());
        assert!(exact.permits(Some("https://game.example:8443")));
        assert!(!exact.permits(Some("https://game.example")));
        assert!(!exact.permits(Some("https://other.example:8443")));
        assert!(exact.permits(None), "non-browser clients have no Origin header");
    }

    #[test]
    fn a_production_origin_must_be_a_serialized_http_origin() {
        assert!(super::Origins::Exact(Vec::new()).validate().is_err());
        for invalid in [
            "null",
            "file:///tmp/game.html",
            "https://user@game.example",
            "https://game.example/path",
            "https://game.example?query",
            "https://game.example#fragment",
            "https://game.example:443",
            "HTTPS://GAME.EXAMPLE",
            "http://localhost:99999",
        ] {
            let policy = super::Origins::Exact(vec![invalid.to_string()]);
            assert!(policy.validate().is_err(), "accepted {invalid}");
            assert!(!policy.permits(Some(invalid)), "permitted {invalid}");
        }
        for valid in ["http://localhost:8080", "https://game.example", "https://[::1]:4433"] {
            assert!(super::Origins::Exact(vec![valid.to_string()]).validate().is_ok(), "{valid}");
        }
    }

    #[tokio::test]
    async fn an_idle_or_partly_sent_frame_cannot_hold_a_session_forever() {
        let (mut peer, mut recv) = tokio::io::duplex(256);
        let idle = Duration::from_millis(30);
        let frame = Duration::from_millis(30);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), super::read_frame(&mut recv, idle, frame))
                .await
                .expect("read deadline")
                .is_none()
        );
        peer.write_all(&[0x80, 0x01]).await.expect("partial frame");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), super::read_frame(&mut recv, idle, frame))
                .await
                .expect("frame deadline")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_complete_frame_is_read_with_the_deadline() {
        let (mut peer, mut recv) = tokio::io::duplex(256);
        peer.write_all(&super::frame(b"hello")).await.expect("frame");
        assert_eq!(
            super::read_frame(&mut recv, Duration::from_secs(1), Duration::from_secs(1)).await,
            Some(b"hello".to_vec())
        );
    }

    #[test]
    fn a_sessions_path_names_its_room_and_pair() {
        let path = super::session_path;
        assert_eq!(path("/room/a?pair=00000000000000ff"), Some(("a", Some(255))));
        assert_eq!(path("/room/a"), Some(("a", None)));
        assert_eq!(path("/room/a?pair=ff"), Some(("a", None)));
        assert_eq!(path("/room/a?pair=zz00000000000000"), Some(("a", None)));
        assert_eq!(path("/room/A?pair=00000000000000ff"), None);
        assert_eq!(path("/other/a"), None);
    }

    #[test]
    fn a_connection_holds_its_reserve_whatever_the_others_hold() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (shared, cap) = (Arc::new(AtomicUsize::new(0)), 1 << 20);
        let (hog, other) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        // One connection takes its reserve, then all that's shared.
        let mut held = Vec::new();
        while let Some(s) = super::Share::bytes(4096, (&hog, 16 << 20), (&shared, cap)) {
            held.push(s);
        }
        assert_eq!(hog.load(Ordering::Acquire), super::RESERVED + cap);
        // Another still has its reserve, and no more.
        let mine = super::Share::bytes(super::RESERVED, (&other, 16 << 20), (&shared, cap));
        assert!(mine.is_some());
        assert!(super::Share::bytes(1, (&other, 16 << 20), (&shared, cap)).is_none());
        // Every byte comes back.
        drop((held, mine));
        assert_eq!((hog.load(Ordering::Acquire), shared.load(Ordering::Acquire)), (0, 0));
    }

    #[test]
    fn connection_numbers_stop_before_the_reserved_range() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let next = AtomicU32::new(wrela_host::rooms::POISONED - 1);
        assert_eq!(super::connection_id(&next), Some(wrela_host::rooms::POISONED - 1));
        assert_eq!(super::connection_id(&next), None);
        assert_eq!(super::connection_id(&next), None);
        assert_eq!(next.load(Ordering::Relaxed), wrela_host::rooms::POISONED);
    }

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
