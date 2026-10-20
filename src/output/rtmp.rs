//! RTMP / RTMPS publisher: handshake, chunk stream, `connect` → `createStream` → `publish`, and
//! FLV-tagged H.264 / AAC from the MKFF encoders. Reconnects on its own and drops video under
//! congestion (whole GOPs, so the picture never smears).

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use super::amf::{decode_all, encode_all, Amf};

const OUT_CHUNK: usize = 4096;
/// Queue span after which the oldest video is dropped.
const MAX_LATENCY_MS: u32 = 2500;
const MAX_RECONNECTS: u32 = 25;

/// What the encoder knows about the streams (FLV metadata + sequence headers).
#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    /// AVCDecoderConfigurationRecord.
    pub avcc: Vec<u8>,
    /// AAC AudioSpecificConfig.
    pub asc: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Clone, Debug)]
pub enum StreamPacket {
    /// Length-prefixed H.264 access unit. `ts` is the DTS in ms, `cts` the PTS − DTS.
    Video { ts: u32, cts: i32, key: bool, data: Arc<Vec<u8>> },
    /// Raw AAC access unit.
    Audio { ts: u32, data: Arc<Vec<u8>> },
}

impl StreamPacket {
    fn ts(&self) -> u32 {
        match self {
            StreamPacket::Video { ts, .. } | StreamPacket::Audio { ts, .. } => *ts,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamState {
    Connecting,
    Live,
    Reconnecting { attempt: u32 },
    Failed(String),
    Stopped,
}

/// Live counters shared with the UI.
pub struct StreamStatus {
    pub state: Mutex<StreamState>,
    pub bytes_sent: AtomicU64,
    pub frames_sent: AtomicU64,
    pub frames_dropped: AtomicU64,
    pub live_since: Mutex<Option<Instant>>,
}

impl StreamStatus {
    fn new() -> Self {
        StreamStatus {
            state: Mutex::new(StreamState::Connecting),
            bytes_sent: AtomicU64::new(0),
            frames_sent: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
            live_since: Mutex::new(None),
        }
    }
    fn set(&self, s: StreamState) {
        log::info!("stream: {s:?}");
        *self.state.lock() = s;
    }
}

/// A running publish session (one thread).
pub struct Streamer {
    tx: Sender<StreamPacket>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub status: Arc<StreamStatus>,
}

impl Streamer {
    pub fn start(url: String, key: String, info: StreamInfo) -> Streamer {
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(StreamStatus::new());
        let (s, st) = (Arc::clone(&stop), Arc::clone(&status));
        let thread = std::thread::Builder::new()
            .name("rtmp".into())
            .spawn(move || publisher(url, key, info, rx, s, st))
            .ok();
        Streamer { tx, stop, thread, status }
    }

    pub fn send(&self, p: StreamPacket) {
        let _ = self.tx.send(p);
    }

    /// Whether the thread has given up (or finished).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Ask the thread to unpublish and close; returns without waiting.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            std::thread::spawn(move || {
                let _ = t.join();
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// URL

#[derive(Clone, Debug, PartialEq)]
pub struct RtmpUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub app: String,
    pub tc_url: String,
}

pub fn parse_url(url: &str) -> Result<RtmpUrl, String> {
    let url = url.trim().trim_end_matches('/');
    let (tls, rest) = if let Some(r) = url.strip_prefix("rtmps://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("rtmp://") {
        (false, r)
    } else {
        return Err(format!("not an rtmp:// or rtmps:// URL: {url}"));
    };
    let (authority, app) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => {
            (h.trim_matches(['[', ']']).to_string(), p.parse::<u16>().map_err(|_| format!("bad port in {url}"))?)
        }
        _ => (authority.to_string(), if tls { 443 } else { 1935 }),
    };
    if host.is_empty() {
        return Err(format!("no host in {url}"));
    }
    Ok(RtmpUrl { tls, host, port, app: app.to_string(), tc_url: url.to_string() })
}

// ---------------------------------------------------------------------------------------------
// Publisher thread

fn publisher(url: String, key: String, info: StreamInfo, rx: Receiver<StreamPacket>, stop: Arc<AtomicBool>, status: Arc<StreamStatus>) {
    let target = match parse_url(&url) {
        Ok(t) => t,
        Err(e) => {
            status.set(StreamState::Failed(e));
            return;
        }
    };
    let mut attempt = 0u32;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Whatever piled up while we were offline is stale.
        while rx.try_recv().is_ok() {}
        match Session::open(&target, &key, &status) {
            Ok(mut session) => {
                status.set(StreamState::Live);
                status.live_since.lock().get_or_insert_with(Instant::now);
                attempt = 0;
                match session.run(&info, &rx, &stop, &status) {
                    Ok(()) => break,
                    Err(e) => log::warn!("stream: connection lost: {e}"),
                }
            }
            Err(e) => {
                log::warn!("stream: connect failed: {e}");
                if attempt == 0 && status.live_since.lock().is_none() {
                    // Never got on air: a bad URL / key isn't worth retrying 25 times.
                    status.set(StreamState::Failed(e));
                    return;
                }
            }
        }
        attempt += 1;
        if attempt > MAX_RECONNECTS {
            status.set(StreamState::Failed("too many reconnect attempts".into()));
            return;
        }
        status.set(StreamState::Reconnecting { attempt });
        let wait = Instant::now() + Duration::from_secs(2u64.min(attempt as u64) * 2);
        while Instant::now() < wait {
            if stop.load(Ordering::Relaxed) {
                status.set(StreamState::Stopped);
                return;
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    status.set(StreamState::Stopped);
}

/// The connection, plain or TLS, with access to its socket for timeouts. (Timeouts must be set
/// on this very handle: on Windows a `try_clone`d socket keeps its own.)
trait Io: Read + Write + Send {
    fn tcp(&self) -> &TcpStream;
}

impl Io for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl Io for rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    fn tcp(&self) -> &TcpStream {
        &self.sock
    }
}

struct InState {
    ts: u32,
    delta: u32,
    len: usize,
    typ: u8,
    sid: u32,
    ext: bool,
    partial: Vec<u8>,
}

struct Message {
    typ: u8,
    payload: Vec<u8>,
}

struct Session {
    io: Box<dyn Io>,
    inbuf: Vec<u8>,
    in_chunk: usize,
    in_states: HashMap<u32, InState>,
    bytes_in: u64,
    last_ack: u64,
    window: u64,
    stream_id: u32,
    txn: f64,
    key: String,
    out: Vec<u8>,
    status: Arc<StreamStatus>,
}

impl Session {
    fn open(t: &RtmpUrl, key: &str, status: &Arc<StreamStatus>) -> Result<Session, String> {
        let addr = (t.host.as_str(), t.port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {}: {e}", t.host))?
            .next()
            .ok_or_else(|| format!("no address for {}", t.host))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("connect {}:{}: {e}", t.host, t.port))?;
        let _ = tcp.set_nodelay(true);
        tcp.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        tcp.set_write_timeout(Some(Duration::from_secs(15))).map_err(|e| e.to_string())?;
        let io: Box<dyn Io> = if t.tls {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
            let name = rustls::pki_types::ServerName::try_from(t.host.clone()).map_err(|e| format!("TLS name: {e}"))?;
            let conn = rustls::ClientConnection::new(Arc::new(cfg), name).map_err(|e| format!("TLS: {e}"))?;
            Box::new(rustls::StreamOwned::new(conn, tcp))
        } else {
            Box::new(tcp)
        };
        let mut s = Session {
            io,
            inbuf: Vec::new(),
            in_chunk: 128,
            in_states: HashMap::new(),
            bytes_in: 0,
            last_ack: 0,
            window: 2_500_000,
            stream_id: 0,
            txn: 1.0,
            key: key.to_string(),
            out: Vec::new(),
            status: Arc::clone(status),
        };
        s.handshake().map_err(|e| format!("handshake: {e}"))?;
        s.connect(t)?;
        Ok(s)
    }

    fn handshake(&mut self) -> io::Result<()> {
        let mut c0c1 = vec![0u8; 1537];
        c0c1[0] = 3;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u32;
        c0c1[1..5].copy_from_slice(&now.to_be_bytes());
        use rand::RngCore;
        rand::rng().fill_bytes(&mut c0c1[9..]);
        self.io.write_all(&c0c1)?;
        self.io.flush()?;
        let mut s0s1 = vec![0u8; 1537];
        self.io.read_exact(&mut s0s1)?;
        if s0s1[0] != 3 {
            return Err(io::Error::other(format!("server speaks RTMP version {}", s0s1[0])));
        }
        // C2 echoes S1.
        self.io.write_all(&s0s1[1..])?;
        self.io.flush()?;
        let mut s2 = vec![0u8; 1536];
        self.io.read_exact(&mut s2)?;
        Ok(())
    }

    fn connect(&mut self, t: &RtmpUrl) -> Result<(), String> {
        let e = |e: io::Error| e.to_string();
        self.send(2, 0, 1, 0, &(OUT_CHUNK as u32).to_be_bytes()).map_err(e)?;
        let txn = self.next_txn();
        let cmd = encode_all(&[
            Amf::str("connect"),
            Amf::Number(txn),
            Amf::obj(&[
                ("app", Amf::str(&t.app)),
                ("type", Amf::str("nonprivate")),
                ("flashVer", Amf::str("FMLE/3.0 (compatible; FMSc/1.0)")),
                ("swfUrl", Amf::str(&t.tc_url)),
                ("tcUrl", Amf::str(&t.tc_url)),
            ]),
        ]);
        self.send(3, 0, 20, 0, &cmd).map_err(e)?;
        let r = self.wait_result(txn)?;
        if r.first().and_then(Amf::as_str) == Some("_error") {
            return Err(format!("connect rejected: {}", describe_status(&r)));
        }

        let key = self.key.clone();
        let t1 = self.next_txn();
        self.send(3, 0, 20, 0, &encode_all(&[Amf::str("releaseStream"), Amf::Number(t1), Amf::Null, Amf::str(&key)])).map_err(e)?;
        let t2 = self.next_txn();
        self.send(3, 0, 20, 0, &encode_all(&[Amf::str("FCPublish"), Amf::Number(t2), Amf::Null, Amf::str(&key)])).map_err(e)?;
        let t3 = self.next_txn();
        self.send(3, 0, 20, 0, &encode_all(&[Amf::str("createStream"), Amf::Number(t3), Amf::Null])).map_err(e)?;
        let r = self.wait_result(t3)?;
        if r.first().and_then(Amf::as_str) != Some("_result") {
            return Err(format!("createStream failed: {}", describe_status(&r)));
        }
        self.stream_id = r.get(3).and_then(Amf::as_f64).unwrap_or(1.0) as u32;

        let t4 = self.next_txn();
        let publish = encode_all(&[Amf::str("publish"), Amf::Number(t4), Amf::Null, Amf::str(&key), Amf::str("live")]);
        self.send(4, 0, 20, self.stream_id, &publish).map_err(e)?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if Instant::now() > deadline {
                return Err("no answer to publish".into());
            }
            let Some(m) = self.read_message(true).map_err(e)? else { continue };
            if m.typ != 20 && m.typ != 17 {
                continue;
            }
            let v = command_values(&m);
            match v.first().and_then(Amf::as_str) {
                Some("onStatus") => {
                    let code = v.get(3).and_then(|o| o.get("code")).and_then(Amf::as_str).unwrap_or("");
                    if code == "NetStream.Publish.Start" {
                        return Ok(());
                    }
                    if code.contains("Error") || code.contains("BadName") || code.contains("Failed") || code.contains("Rejected") {
                        return Err(format!("publish rejected: {}", describe_status(&v)));
                    }
                }
                Some("_error") => return Err(format!("publish rejected: {}", describe_status(&v))),
                _ => {}
            }
        }
    }

    fn next_txn(&mut self) -> f64 {
        self.txn += 1.0;
        self.txn
    }

    fn wait_result(&mut self, txn: f64) -> Result<Vec<Amf>, String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let Some(m) = self.read_message(true).map_err(|e| e.to_string())? else { continue };
            if m.typ != 20 && m.typ != 17 {
                continue;
            }
            let v = command_values(&m);
            let name = v.first().and_then(Amf::as_str);
            if matches!(name, Some("_result") | Some("_error")) && v.get(1).and_then(Amf::as_f64) == Some(txn) {
                return Ok(v);
            }
        }
        Err("server did not answer".into())
    }

    /// The streaming loop: metadata, sequence headers, then packets until `stop`.
    fn run(&mut self, info: &StreamInfo, rx: &Receiver<StreamPacket>, stop: &AtomicBool, status: &StreamStatus) -> io::Result<()> {
        let sid = self.stream_id;
        let meta = encode_all(&[
            Amf::str("@setDataFrame"),
            Amf::str("onMetaData"),
            Amf::EcmaArray(vec![
                ("duration".into(), Amf::Number(0.0)),
                ("width".into(), Amf::Number(info.width as f64)),
                ("height".into(), Amf::Number(info.height as f64)),
                ("videodatarate".into(), Amf::Number(info.video_kbps as f64)),
                ("framerate".into(), Amf::Number(info.fps as f64)),
                ("videocodecid".into(), Amf::Number(7.0)),
                ("audiodatarate".into(), Amf::Number(info.audio_kbps as f64)),
                ("audiosamplerate".into(), Amf::Number(info.sample_rate as f64)),
                ("audiosamplesize".into(), Amf::Number(16.0)),
                ("audiochannels".into(), Amf::Number(info.channels as f64)),
                ("stereo".into(), Amf::Bool(info.channels == 2)),
                ("audiocodecid".into(), Amf::Number(10.0)),
                ("encoder".into(), Amf::str(concat!("LiveBroadcaster ", env!("CARGO_PKG_VERSION"), " (MKFF)"))),
                ("fileSize".into(), Amf::Number(0.0)),
            ]),
        ]);
        self.send(5, 0, 18, sid, &meta)?;
        let mut seq = vec![0x17, 0x00, 0, 0, 0];
        seq.extend_from_slice(&info.avcc);
        self.send(6, 0, 9, sid, &seq)?;
        let mut aseq = vec![0xAF, 0x00];
        aseq.extend_from_slice(&info.asc);
        self.send(7, 0, 8, sid, &aseq)?;

        self.io.tcp().set_read_timeout(Some(Duration::from_millis(1)))?;
        let mut queue: VecDeque<StreamPacket> = VecDeque::new();
        let mut need_key = true;
        while !stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(p) => queue.push_back(p),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(p) = rx.try_recv() {
                queue.push_back(p);
            }
            // Congestion: drop to the newest keyframe once the backlog is too long.
            if let (Some(f), Some(b)) = (queue.front(), queue.back()) {
                if b.ts().saturating_sub(f.ts()) > MAX_LATENCY_MS {
                    let last_key = queue.iter().rposition(|p| matches!(p, StreamPacket::Video { key: true, .. }));
                    let cut = last_key.unwrap_or(queue.len());
                    let dropped = queue.drain(..cut).filter(|p| matches!(p, StreamPacket::Video { .. })).count();
                    status.frames_dropped.fetch_add(dropped as u64, Ordering::Relaxed);
                    if last_key.is_none() {
                        need_key = true;
                    }
                    log::warn!("stream: congestion, dropped {dropped} frames");
                }
            }
            while let Some(p) = queue.pop_front() {
                match &p {
                    StreamPacket::Video { ts, cts, key, data } => {
                        if need_key && !key {
                            status.frames_dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        need_key = false;
                        let mut body = Vec::with_capacity(data.len() + 5);
                        body.push(if *key { 0x17 } else { 0x27 });
                        body.push(0x01);
                        let c = (*cts).clamp(-0x80_0000, 0x7F_FFFF) as u32;
                        body.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
                        body.extend_from_slice(data);
                        self.send(6, *ts, 9, sid, &body)?;
                        status.frames_sent.fetch_add(1, Ordering::Relaxed);
                    }
                    StreamPacket::Audio { ts, data } => {
                        if need_key {
                            continue;
                        }
                        let mut body = Vec::with_capacity(data.len() + 2);
                        body.extend_from_slice(&[0xAF, 0x01]);
                        body.extend_from_slice(data);
                        self.send(7, *ts, 8, sid, &body)?;
                    }
                }
                // Keep an eye on the server between packets (acks, pings, errors).
                if queue.len() % 8 == 0 {
                    self.poll_incoming()?;
                }
            }
            self.poll_incoming()?;
        }
        // Polite goodbye; errors don't matter any more.
        let key = self.key.clone();
        let t = self.next_txn();
        let _ = self.send(3, 0, 20, 0, &encode_all(&[Amf::str("FCUnpublish"), Amf::Number(t), Amf::Null, Amf::str(&key)]));
        let t = self.next_txn();
        let _ = self.send(3, 0, 20, 0, &encode_all(&[Amf::str("deleteStream"), Amf::Number(t), Amf::Null, Amf::Number(sid as f64)]));
        Ok(())
    }

    fn poll_incoming(&mut self) -> io::Result<()> {
        while let Some(m) = self.read_message(false)? {
            if m.typ == 20 || m.typ == 17 {
                let v = command_values(&m);
                if v.first().and_then(Amf::as_str) == Some("onStatus") {
                    let code = v.get(3).and_then(|o| o.get("code")).and_then(Amf::as_str).unwrap_or("");
                    log::info!("stream: server status {code}");
                    if code.contains("Error") || code.contains("Failed") || code == "NetStream.Publish.BadName" {
                        return Err(io::Error::other(describe_status(&v)));
                    }
                }
            }
        }
        Ok(())
    }

    // -- chunk stream -------------------------------------------------------------------------

    fn send(&mut self, csid: u32, ts: u32, typ: u8, sid: u32, payload: &[u8]) -> io::Result<()> {
        let out = &mut self.out;
        out.clear();
        let ext = ts >= 0xFF_FFFF;
        let basic = |out: &mut Vec<u8>, fmt: u8| {
            if csid < 64 {
                out.push((fmt << 6) | csid as u8);
            } else if csid < 320 {
                out.push(fmt << 6);
                out.push((csid - 64) as u8);
            } else {
                out.push((fmt << 6) | 1);
                let c = csid - 64;
                out.push(c as u8);
                out.push((c >> 8) as u8);
            }
        };
        basic(out, 0);
        let t = if ext { 0xFF_FFFF } else { ts };
        out.extend_from_slice(&t.to_be_bytes()[1..]);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        out.push(typ);
        out.extend_from_slice(&sid.to_le_bytes());
        if ext {
            out.extend_from_slice(&ts.to_be_bytes());
        }
        let mut rest = payload;
        let first = rest.len().min(OUT_CHUNK);
        out.extend_from_slice(&rest[..first]);
        rest = &rest[first..];
        while !rest.is_empty() {
            basic(out, 3);
            if ext {
                out.extend_from_slice(&ts.to_be_bytes());
            }
            let n = rest.len().min(OUT_CHUNK);
            out.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
        }
        self.io.write_all(&self.out)?;
        self.io.flush()?;
        self.status.bytes_sent.fetch_add(self.out.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Next complete message. `block`: wait (up to the socket timeout) for data; otherwise
    /// return `None` when nothing complete has arrived.
    fn read_message(&mut self, block: bool) -> io::Result<Option<Message>> {
        loop {
            if let Some(m) = self.parse_chunk()? {
                if let Some(m) = self.handle_control(m)? {
                    return Ok(Some(m));
                }
                continue;
            }
            let mut buf = [0u8; 16384];
            match self.io.read(&mut buf) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "server closed the connection")),
                Ok(n) => {
                    self.inbuf.extend_from_slice(&buf[..n]);
                    self.bytes_in += n as u64;
                    if self.bytes_in - self.last_ack >= self.window / 2 {
                        self.last_ack = self.bytes_in;
                        let seq = (self.bytes_in as u32).to_be_bytes();
                        self.send(2, 0, 3, 0, &seq)?;
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
            if !block && self.inbuf.is_empty() {
                return Ok(None);
            }
        }
    }

    /// Protocol control messages are answered here; everything else is returned.
    fn handle_control(&mut self, m: Message) -> io::Result<Option<Message>> {
        let p = &m.payload;
        let be32 = |p: &[u8]| p.get(..4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        match m.typ {
            1 => {
                if let Some(n) = be32(p) {
                    self.in_chunk = (n & 0x7FFF_FFFF).max(1) as usize;
                }
                Ok(None)
            }
            2 => {
                if let Some(c) = be32(p) {
                    if let Some(st) = self.in_states.get_mut(&c) {
                        st.partial.clear();
                    }
                }
                Ok(None)
            }
            3 => Ok(None),
            4 => {
                // Ping request → ping response.
                if p.len() >= 6 && u16::from_be_bytes([p[0], p[1]]) == 6 {
                    let mut r = vec![0, 7];
                    r.extend_from_slice(&p[2..6]);
                    self.send(2, 0, 4, 0, &r)?;
                }
                Ok(None)
            }
            5 => {
                if let Some(w) = be32(p) {
                    self.window = w.max(1) as u64;
                }
                Ok(None)
            }
            6 => {
                if let Some(w) = be32(p) {
                    self.send(2, 0, 5, 0, &w.to_be_bytes())?;
                }
                Ok(None)
            }
            _ => Ok(Some(m)),
        }
    }

    /// Consume one chunk from `inbuf` if it is complete; returns a message when one finishes.
    fn parse_chunk(&mut self) -> io::Result<Option<Message>> {
        loop {
            let b = &self.inbuf;
            if b.is_empty() {
                return Ok(None);
            }
            let fmt = b[0] >> 6;
            let (csid, mut p) = match b[0] & 0x3F {
                0 => {
                    if b.len() < 2 {
                        return Ok(None);
                    }
                    (64 + b[1] as u32, 2)
                }
                1 => {
                    if b.len() < 3 {
                        return Ok(None);
                    }
                    (64 + b[1] as u32 + 256 * b[2] as u32, 3)
                }
                c => (c as u32, 1),
            };
            let hlen = [11, 7, 3, 0][fmt as usize];
            if b.len() < p + hlen {
                return Ok(None);
            }
            let u24 = |s: &[u8]| ((s[0] as u32) << 16) | ((s[1] as u32) << 8) | s[2] as u32;
            let prev = self.in_states.get(&csid);
            let (mut ts, mut delta, mut len, mut typ, mut sid, mut ext) = match prev {
                Some(s) => (s.ts, s.delta, s.len, s.typ, s.sid, s.ext),
                None => (0, 0, 0, 0, 0, false),
            };
            let starting = prev.is_none_or(|s| s.partial.is_empty());
            let h = &b[p..p + hlen];
            let field = if fmt <= 2 { Some(u24(&h[0..3])) } else { None };
            if fmt <= 1 {
                len = u24(&h[3..6]) as usize;
                typ = h[6];
            }
            if fmt == 0 {
                sid = u32::from_le_bytes([h[7], h[8], h[9], h[10]]);
            }
            p += hlen;
            if let Some(f) = field {
                ext = f == 0xFF_FFFF;
            }
            let mut tsval = field.unwrap_or(0);
            if ext {
                if b.len() < p + 4 {
                    return Ok(None);
                }
                tsval = u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]);
                p += 4;
            }
            let have = prev.map_or(0, |s| if starting { 0 } else { s.partial.len() });
            let n = (len - have.min(len)).min(self.in_chunk);
            if b.len() < p + n {
                return Ok(None);
            }
            // Commit.
            match fmt {
                0 => ts = tsval,
                1 | 2 => {
                    delta = tsval;
                    if starting {
                        ts = ts.wrapping_add(delta);
                    }
                }
                _ => {
                    if starting {
                        ts = ts.wrapping_add(delta);
                    }
                }
            }
            let data = self.inbuf[p..p + n].to_vec();
            self.inbuf.drain(..p + n);
            let st = self.in_states.entry(csid).or_insert(InState { ts: 0, delta: 0, len: 0, typ: 0, sid: 0, ext: false, partial: Vec::new() });
            st.ts = ts;
            st.delta = delta;
            st.len = len;
            st.typ = typ;
            st.sid = sid;
            st.ext = ext;
            if starting {
                st.partial.clear();
            }
            st.partial.extend_from_slice(&data);
            if st.partial.len() >= len {
                let payload = std::mem::take(&mut st.partial);
                return Ok(Some(Message { typ, payload }));
            }
        }
    }
}

fn command_values(m: &Message) -> Vec<Amf> {
    // AMF3 command messages (17) carry a leading 0 then AMF0.
    let data = if m.typ == 17 && !m.payload.is_empty() { &m.payload[1..] } else { &m.payload[..] };
    decode_all(data)
}

fn describe_status(v: &[Amf]) -> String {
    for a in v {
        if let (Some(code), desc) = (a.get("code").and_then(Amf::as_str), a.get("description").and_then(Amf::as_str)) {
            return match desc {
                Some(d) if !d.is_empty() => format!("{code} ({d})"),
                _ => code.to_string(),
            };
        }
    }
    format!("{v:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("rtmp://a.rtmp.youtube.com/live2").unwrap();
        assert_eq!((u.tls, u.host.as_str(), u.port, u.app.as_str()), (false, "a.rtmp.youtube.com", 1935, "live2"));
        let u = parse_url("rtmps://live-api-s.facebook.com:443/rtmp/").unwrap();
        assert_eq!((u.tls, u.port, u.app.as_str()), (true, 443, "rtmp"));
        let u = parse_url("rtmp://127.0.0.1:1936/live/sub").unwrap();
        assert_eq!((u.port, u.app.as_str()), (1936, "live/sub"));
        assert!(parse_url("http://x").is_err());
    }
}
