// vlat - A colorful, pretty and over-engineered yet easy to use ping monitoring utility.
// Copyright (C) 2026  Jason D. Rivard <code@jrivard.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, TcpStream, UdpSocket},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, sync::{mpsc, watch}, time};
use tokio::net::TcpStream as TokioTcpStream;
use crate::cli::{Args, PingMode};
use crate::types::{ProbeResult, ProbeStarted};

#[derive(Clone, Debug)]
pub enum TlsVerify {
    Skip,
    System,
    Cert(String),
}

#[derive(Clone, Debug)]
pub enum TlsVersion { Any, V12, V13 }

#[derive(Clone, Debug)]
pub struct TlsConfig {
    pub verify:  TlsVerify,
    pub version: TlsVersion,
}

// ── ICMP duplicate detection ─────────────────────────────────────────────────

/// Tracks ICMP echo-reply (identifier, sequence) pairs observed via a parallel
/// raw socket.  When the same (ident, seq) arrives more than once, a duplicate
/// is detected.  The tracker is only functional on Unix where raw ICMP sockets
/// are available; on other platforms `check_dup` always returns false.
/// (identifier, sequence) → (times seen, last seen).
type IcmpSeenMap = HashMap<(u16, u16), (u32, Instant)>;

pub struct IcmpDupTracker {
    seen: Arc<Mutex<IcmpSeenMap>>,
}

impl IcmpDupTracker {
    pub fn new() -> Self {
        Self { seen: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Spawn background raw-socket listeners.  Safe to call multiple times but
    /// should be called exactly once after creation.
    pub fn spawn_listeners(self: &Arc<Self>) {
        #[cfg(unix)]
        {
            let seen4 = self.seen.clone();
            tokio::spawn(async move { icmp4_dup_listener(seen4).await; });
            let seen6 = self.seen.clone();
            tokio::spawn(async move { icmp6_dup_listener(seen6).await; });
        }
    }

    /// Returns true if (ident, seq) was seen more than once since it was first
    /// recorded - i.e., a duplicate reply arrived.
    pub fn check_dup(&self, ident: u16, seq: u16) -> bool {
        self.seen.lock().unwrap()
            .get(&(ident, seq))
            .map(|(count, _)| *count > 1)
            .unwrap_or(false)
    }
}

#[cfg(unix)]
fn record_icmp_seen(seen: &Mutex<IcmpSeenMap>, ident: u16, seq: u16) {
    let mut map = seen.lock().unwrap();
    let entry = map.entry((ident, seq)).or_insert((0, Instant::now()));
    entry.0 += 1;
    entry.1  = Instant::now();
    // Prune when map grows large (30-second TTL on each entry).
    if map.len() > 4096 {
        let cutoff = Instant::now() - Duration::from_secs(30);
        map.retain(|_, (_, t)| *t > cutoff);
    }
}

/// Raw ICMPv4 socket listener: counts every echo-reply by (identifier, sequence).
#[cfg(unix)]
async fn icmp4_dup_listener(seen: Arc<Mutex<IcmpSeenMap>>) {
    use std::mem::MaybeUninit;
    use socket2::{Domain, Protocol, Socket, Type};
    use tokio::io::unix::AsyncFd;

    let sock = match Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::from(1 /* IPPROTO_ICMP */))) {
        Ok(s) => s,
        Err(_) => return,
    };
    if sock.set_nonblocking(true).is_err() { return; }
    let fd = match AsyncFd::new(sock) { Ok(f) => f, Err(_) => return };

    let mut buf = vec![MaybeUninit::<u8>::uninit(); 65536];
    loop {
        let mut guard = match fd.readable().await { Ok(g) => g, Err(_) => break };
        let n = match guard.try_io(|inner| inner.get_ref().recv(&mut buf)) {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        // Safety: kernel initialised the first n bytes via recv.
        let data = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
        if n < 28 { continue; }
        let ihl = ((data[0] & 0x0f) as usize) * 4;
        if n < ihl + 8 { continue; }
        let icmp = &data[ihl..];
        if icmp[0] != 0 { continue; } // ICMP Echo Reply = type 0
        let ident = u16::from_be_bytes([icmp[4], icmp[5]]);
        let seq   = u16::from_be_bytes([icmp[6], icmp[7]]);
        record_icmp_seen(&seen, ident, seq);
    }
}

/// Raw ICMPv6 socket listener: counts every echo-reply by (identifier, sequence).
#[cfg(unix)]
async fn icmp6_dup_listener(seen: Arc<Mutex<IcmpSeenMap>>) {
    use std::mem::MaybeUninit;
    use socket2::{Domain, Protocol, Socket, Type};
    use tokio::io::unix::AsyncFd;

    // ICMPv6 raw sockets on Linux do not include the IPv6 header in recvmsg.
    let sock = match Socket::new(Domain::IPV6, Type::RAW, Some(Protocol::from(58 /* IPPROTO_ICMPV6 */))) {
        Ok(s) => s,
        Err(_) => return,
    };
    if sock.set_nonblocking(true).is_err() { return; }
    let fd = match AsyncFd::new(sock) { Ok(f) => f, Err(_) => return };

    let mut buf = vec![MaybeUninit::<u8>::uninit(); 65536];
    loop {
        let mut guard = match fd.readable().await { Ok(g) => g, Err(_) => break };
        let n = match guard.try_io(|inner| inner.get_ref().recv(&mut buf)) {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let data = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
        if n < 8 { continue; }
        if data[0] != 129 { continue; } // ICMPv6 Echo Reply = type 129
        let ident = u16::from_be_bytes([data[4], data[5]]);
        let seq   = u16::from_be_bytes([data[6], data[7]]);
        record_icmp_seen(&seen, ident, seq);
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Returns `(v4_client, v6_client, effective_mode)`.
/// When `bind_ip` is set only the matching address-family client is created.
/// When it is unset both are attempted; if both fail the mode falls back to UDP.
pub fn setup_ping_mode(args: &Args, bind_ip: Option<std::net::IpAddr>) -> (Option<Client>, Option<Client>, PingMode) {
    match args.mode.as_ref().unwrap_or(&PingMode::Icmp) {
        PingMode::Icmp => {
            if let Some(ip) = bind_ip {
                let kind     = if ip.is_ipv6() { ICMP::V6 } else { ICMP::V4 };
                let sock_addr = socket2::SockAddr::from(std::net::SocketAddr::new(ip, 0));
                let config   = Config { kind, bind: Some(sock_addr), ..Config::default() };
                match Client::new(&config) {
                    Ok(c) if ip.is_ipv6() => (None, Some(c), PingMode::Icmp),
                    Ok(c)                 => (Some(c), None, PingMode::Icmp),
                    Err(e)                => {
                        crate::logfile::write(&format!("probe: ICMP unavailable with bind address {}: {}, falling back to UDP", ip, e));
                        (None, None, PingMode::Udp)
                    }
                }
            } else {
                let v4 = Client::new(&Config { kind: ICMP::V4, ..Config::default() }).ok();
                let v6 = Client::new(&Config { kind: ICMP::V6, ..Config::default() }).ok();
                if v4.is_none() && v6.is_none() {
                    crate::logfile::write("probe: ICMP unavailable (requires root or CAP_NET_RAW), falling back to UDP");
                    (None, None, PingMode::Udp)
                } else {
                    (v4, v6, PingMode::Icmp)
                }
            }
        }
        PingMode::Udp   => (None, None, PingMode::Udp),
        PingMode::Tcp   => (None, None, PingMode::Tcp),
        PingMode::Http  => (None, None, PingMode::Http),
        PingMode::Https => (None, None, PingMode::Https),
        PingMode::Dns   => (None, None, PingMode::Dns),
        PingMode::Tls   => (None, None, PingMode::Tls),
        PingMode::Ntp   => (None, None, PingMode::Ntp),
        PingMode::Ssh   => (None, None, PingMode::Ssh),
        PingMode::Smtp  => (None, None, PingMode::Smtp),
        PingMode::Smtps => (None, None, PingMode::Smtps),
        PingMode::Exec  => (None, None, PingMode::Exec),
        PingMode::Quic  => (None, None, PingMode::Quic),
    }
}

/// Everything needed to run one target's probe loop.
pub struct ProbeTask {
    pub task_id:     usize,
    pub ip_shared:   Arc<Mutex<std::net::IpAddr>>,
    pub interval:    u64,
    pub timeout:     f64,
    pub mode:        PingMode,
    pub port:        u16,
    pub icmp4:       Option<Arc<Client>>,
    pub icmp6:       Option<Arc<Client>>,
    pub bind_ip:     Option<std::net::IpAddr>,
    pub hostname:    Option<Arc<String>>,
    pub dns_query:   Arc<String>,
    pub tls_config:  Arc<TlsConfig>,
    pub http_path:   Arc<String>,
    pub exec_cmd:    Arc<String>,
    pub tx:          mpsc::UnboundedSender<ProbeResult>,
    pub start_tx:    mpsc::UnboundedSender<ProbeStarted>,
    pub cancel:      watch::Receiver<bool>,
    pub start_delay: Duration,
    pub dup_tracker: Option<Arc<IcmpDupTracker>>,
}

pub fn spawn_probe_task(task: ProbeTask) {
    let ProbeTask {
        task_id, ip_shared, interval, timeout, mode, port,
        icmp4: icmp4_arc, icmp6: icmp6_arc, bind_ip, hostname, dns_query,
        tls_config, http_path, exec_cmd, tx, start_tx, mut cancel, start_delay, dup_tracker,
    } = task;
    let mode_tag: u8 = match mode {
        PingMode::Icmp  => 0,
        PingMode::Udp   => 1,
        PingMode::Tcp   => 2,
        PingMode::Http  => 3,
        PingMode::Https => 4,
        PingMode::Dns   => 5,
        PingMode::Tls   => 6,
        PingMode::Ntp   => 7,
        PingMode::Ssh   => 8,
        PingMode::Smtp  => 9,
        PingMode::Smtps => 10,
        PingMode::Exec  => 11,
        PingMode::Quic  => 12,
    };

    tokio::spawn(async move {
        let first_tick = time::Instant::now() + start_delay;
        let mut ticker = time::interval_at(first_tick, Duration::from_millis(interval));
        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        let mut seq: usize = 0;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = cancel.changed() => { break; }
            }
            if *cancel.borrow() { break; }

            let current_seq = seq;
            seq = seq.wrapping_add(1);

            // Notify main loop that a probe slot is opening on the timeline
            if start_tx.send(ProbeStarted { task_id, seq: current_seq }).is_err() { break; }

            // Spawn a concurrent sub-task so the ticker is never blocked
            let ip              = *ip_shared.lock().unwrap();
            let tx_c            = tx.clone();
            let icmp4_c         = icmp4_arc.clone();
            let icmp6_c         = icmp6_arc.clone();
            let hostname_c      = hostname.clone();
            let dns_query_c     = dns_query.clone();
            let tls_config_c    = tls_config.clone();
            let http_path_c     = http_path.clone();
            let exec_cmd_c      = exec_cmd.clone();
            let dup_tracker_c   = dup_tracker.clone();
            let mut cancel_c    = cancel.clone();

            tokio::spawn(async move {
                let probe_timeout = Duration::from_secs_f64(
                    timeout.min(crate::constants::MAX_PROBE_TIMEOUT_SECS)
                );
                // probe_fut returns (outcome, dup_detected, bytes_sent, bytes_received)
                let probe_fut = async {
                    match mode_tag {
                        0 => {
                            let seq16   = (current_seq & 0xFFFF) as u16;
                            let ident16 = task_id as u16;
                            let tracker = dup_tracker_c.as_deref();
                            match icmp_probe(ip, seq16, ident16, &icmp4_c, &icmp6_c, probe_timeout, tracker).await {
                                Ok((rtt, dup)) => (Ok(rtt), dup, 0u64, 0u64),
                                Err(())        => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        1 => {
                            let addr = SocketAddr::new(ip, port);
                            let r = tokio::task::spawn_blocking(move || udp_probe(addr, probe_timeout, bind_ip))
                                .await.ok().and_then(|r| r.ok());
                            match r {
                                Some((rtt, bytes_rx)) => (Ok(rtt), false, 4u64, bytes_rx),
                                None                  => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        2 => {
                            let addr = SocketAddr::new(ip, port);
                            let r = tokio::task::spawn_blocking(move || tcp_probe(addr, probe_timeout, bind_ip))
                                .await.ok().and_then(|r| r.ok()).ok_or(());
                            (r, false, 0u64, 0u64)
                        }
                        3 => {
                            let addr = SocketAddr::new(ip, port);
                            match http_probe(addr, hostname_c, http_path_c, probe_timeout).await {
                                Ok((rtt, tx, rx)) => (Ok(rtt), false, tx, rx),
                                Err(())           => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        4 => {
                            let addr = SocketAddr::new(ip, port);
                            match https_probe(addr, hostname_c, tls_config_c, http_path_c, probe_timeout).await {
                                Ok((rtt, tx, rx)) => (Ok(rtt), false, tx, rx),
                                Err(())           => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        5 => {
                            let addr = SocketAddr::new(ip, port);
                            match dns_probe(addr, dns_query_c, probe_timeout).await {
                                Ok((rtt, tx, rx)) => (Ok(rtt), false, tx, rx),
                                Err(())           => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        6 => {
                            let addr = SocketAddr::new(ip, port);
                            let r = tls_probe(addr, hostname_c, tls_config_c, probe_timeout).await;
                            (r, false, 0u64, 0u64)
                        }
                        7 => {
                            let addr = SocketAddr::new(ip, port);
                            match ntp_probe(addr, probe_timeout).await {
                                Ok(rtt) => (Ok(rtt), false, 48u64, 48u64),
                                Err(()) => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        8 => {
                            let addr = SocketAddr::new(ip, port);
                            match ssh_probe(addr, probe_timeout).await {
                                Ok((rtt, bytes_rx)) => (Ok(rtt), false, 0u64, bytes_rx),
                                Err(())             => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        9 => {
                            let addr = SocketAddr::new(ip, port);
                            match smtp_probe(addr, probe_timeout).await {
                                Ok((rtt, bytes_rx)) => (Ok(rtt), false, 0u64, bytes_rx),
                                Err(())             => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        10 => {
                            let addr = SocketAddr::new(ip, port);
                            match smtps_probe(addr, hostname_c, tls_config_c, probe_timeout).await {
                                Ok((rtt, bytes_rx)) => (Ok(rtt), false, 0u64, bytes_rx),
                                Err(())             => (Err(()), false, 0u64, 0u64),
                            }
                        }
                        12 => {
                            let addr = SocketAddr::new(ip, port);
                            let r = quic_probe(addr, hostname_c, probe_timeout).await;
                            (r, false, 0u64, 0u64)
                        }
                        _ => {
                            let r = exec_probe(exec_cmd_c, ip, hostname_c, port, probe_timeout).await;
                            (r, false, 0u64, 0u64)
                        }
                    }
                };
                tokio::select! {
                    (outcome, dup, bytes_sent, bytes_received) = probe_fut => {
                        let _ = tx_c.send(ProbeResult { task_id, seq: current_seq, outcome, dup, bytes_sent, bytes_received });
                    }
                    _ = cancel_c.changed() => {}
                }
            });
        }
    });
}

/// Synthetic stand-in for `spawn_probe_task`: no sockets, no DNS - just the
/// shared `crate::demo` model deciding each probe's outcome on the same
/// interval/cancel/channel contract real probe tasks use, so nothing
/// downstream of the channels needs to know or care that the data isn't
/// real. Each task gets its own RNG (seeded from `task_id`) so targets
/// decorrelate instead of spiking/dropping in lockstep.
pub fn spawn_demo_task(
    task_id:     usize,
    profile:     crate::demo::Profile,
    interval:    u64,
    tx:          mpsc::UnboundedSender<ProbeResult>,
    start_tx:    mpsc::UnboundedSender<ProbeStarted>,
    mut cancel:  watch::Receiver<bool>,
    start_delay: Duration,
) {
    tokio::spawn(async move {
        let mut rng            = crate::demo::Rng::seeded_with(task_id as u64);
        let mut down_remaining = 0u32;

        let first_tick = time::Instant::now() + start_delay;
        let mut ticker = time::interval_at(first_tick, Duration::from_millis(interval));
        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        let mut seq: usize = 0;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = cancel.changed() => { break; }
            }
            if *cancel.borrow() { break; }

            let current_seq = seq;
            seq = seq.wrapping_add(1);

            if start_tx.send(ProbeStarted { task_id, seq: current_seq }).is_err() { break; }

            let outcome = crate::demo::decide(&profile, &mut rng, &mut down_remaining);
            let msg = ProbeResult {
                task_id, seq: current_seq, outcome, dup: false,
                bytes_sent: 0, bytes_received: 0,
            };
            if tx.send(msg).is_err() { break; }
        }
    });
}

pub async fn icmp_probe(
    ip:          std::net::IpAddr,
    seq:         u16,
    ident:       u16,
    v4:          &Option<std::sync::Arc<Client>>,
    v6:          &Option<std::sync::Arc<Client>>,
    timeout:     Duration,
    dup_tracker: Option<&IcmpDupTracker>,
) -> Result<(f64, bool), ()> {
    let client = if ip.is_ipv6() { v6.as_ref() } else { v4.as_ref() }.ok_or(())?;
    let mut pinger = client.pinger(ip, PingIdentifier(ident)).await;
    pinger.timeout(timeout);
    match pinger.ping(PingSequence(seq), &[]).await {
        Ok((_, dur)) => {
            let rtt = dur.as_secs_f64() * 1000.0;
            let dup = if let Some(tracker) = dup_tracker {
                time::sleep(Duration::from_millis(20)).await;
                tracker.check_dup(ident, seq)
            } else {
                false
            };
            crate::logfile::write(&format!(
                "icmp: → {} seq={} ident={} → rtt={:.2}ms{}",
                ip, seq, ident, rtt, if dup { " DUP" } else { "" }
            ));
            Ok((rtt, dup))
        }
        Err(e) => {
            crate::logfile::write(&format!("icmp: → {} seq={} ident={} → {}", ip, seq, ident, e));
            Err(())
        }
    }
}

pub fn udp_probe(addr: SocketAddr, timeout: Duration, bind: Option<std::net::IpAddr>) -> Result<(f64, u64), ()> {
    let bind_addr: SocketAddr = match bind {
        Some(ip) => SocketAddr::new(ip, 0),
        None => if addr.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() },
    };
    let sock = UdpSocket::bind(bind_addr).map_err(|_| ())?;
    sock.set_read_timeout(Some(timeout)).map_err(|_| ())?;
    sock.connect(addr).map_err(|_| ())?;
    let t0 = Instant::now();
    sock.send(b"vlat").map_err(|_| ())?;
    let mut buf = [0u8; 64];
    match sock.recv(&mut buf) {
        Ok(n) => {
            let rtt = t0.elapsed().as_secs_f64() * 1000.0;
            crate::logfile::write(&format!(
                "udp: → {} sent=\"vlat\" (4 bytes) → received={} bytes rtt={:.2}ms",
                addr, n, rtt
            ));
            Ok((rtt, n as u64))
        }
        Err(ref e) if is_port_unreach(e) => {
            let rtt = t0.elapsed().as_secs_f64() * 1000.0;
            crate::logfile::write(&format!(
                "udp: → {} sent=\"vlat\" (4 bytes) → ICMP-port-unreachable rtt={:.2}ms",
                addr, rtt
            ));
            Ok((rtt, 0))
        }
        _ => Err(()),
    }
}

pub fn tcp_probe(addr: SocketAddr, timeout: Duration, bind: Option<std::net::IpAddr>) -> Result<f64, ()> {
    if let Some(ip) = bind {
        use socket2::{Domain, Protocol, Socket, Type};
        let domain   = if ip.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
        let sock     = Socket::new(domain, Type::STREAM, Some(Protocol::TCP)).map_err(|_| ())?;
        let bind_sa  = socket2::SockAddr::from(SocketAddr::new(ip, 0));
        sock.bind(&bind_sa).map_err(|_| ())?;
        sock.set_nonblocking(false).map_err(|_| ())?;
        let dest_sa  = socket2::SockAddr::from(addr);
        let t0       = Instant::now();
        // connect_timeout via blocking connect with SO_SNDTIMEO
        sock.set_write_timeout(Some(timeout)).map_err(|_| ())?;
        return match sock.connect(&dest_sa) {
            Ok(_) => {
                let rtt = t0.elapsed().as_secs_f64() * 1000.0;
                crate::logfile::write(&format!("tcp: → {} → connected rtt={:.2}ms", addr, rtt));
                Ok(rtt)
            }
            Err(ref e) if is_conn_refused(e) => {
                crate::logfile::write(&format!("tcp: → {} → refused (drop)", addr));
                Err(())
            }
            _ => Err(()),
        };
    }
    let t0 = Instant::now();
    match TcpStream::connect_timeout(&addr, timeout) {
        Ok(_) => {
            let rtt = t0.elapsed().as_secs_f64() * 1000.0;
            crate::logfile::write(&format!("tcp: → {} → connected rtt={:.2}ms", addr, rtt));
            Ok(rtt)
        }
        Err(ref e) if is_conn_refused(e) => {
            crate::logfile::write(&format!("tcp: → {} → refused (drop)", addr));
            Err(())
        }
        _ => Err(()),
    }
}

fn build_sni(hostname: &Option<Arc<String>>, addr: SocketAddr) -> Result<rustls::pki_types::ServerName<'static>, ()> {
    use rustls::pki_types::ServerName;
    if let Some(ref h) = hostname {
        ServerName::try_from(h.as_str().to_owned()).map_err(|_| ())
    } else {
        Ok(ServerName::IpAddress(addr.ip().into()))
    }
}

fn build_http_request(path: &str, host: &str) -> String {
    let safe_path = path.replace(['\r', '\n'], "");
    let safe_host = host.replace(['\r', '\n'], "");
    format!("GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n", safe_path, safe_host)
}

fn log_http_exchange(label: &str, request: &str, response: &[u8]) {
    let req_display = request.replace("\r\n", "\n");
    let resp_display = String::from_utf8_lossy(response);
    crate::logfile::write(&format!(
        "{} request:\n{}\n{} response ({} bytes):\n{}",
        label, req_display.trim_end(),
        label, response.len(), resp_display.trim_end()
    ));
}

fn tls_version_str(conn: &rustls::ClientConnection) -> &'static str {
    match conn.protocol_version() {
        Some(rustls::ProtocolVersion::TLSv1_2) => "TLS 1.2",
        Some(rustls::ProtocolVersion::TLSv1_3) => "TLS 1.3",
        _ => "TLS unknown",
    }
}

fn parse_dns_response(buf: &[u8]) -> (u8, u16, Vec<std::net::IpAddr>) {
    if buf.len() < 12 { return (0xFF, 0, vec![]); }
    let rcode   = buf[3] & 0x0F;
    let ancount = u16::from_be_bytes([buf[6], buf[7]]);
    let mut pos = 12;
    loop {
        if pos >= buf.len() { return (rcode, ancount, vec![]); }
        let len = buf[pos] as usize;
        if len == 0 { pos += 1; break; }
        if len & 0xC0 == 0xC0 { pos += 2; break; }
        pos += 1 + len;
    }
    pos += 4; // QTYPE + QCLASS
    let mut ips = Vec::new();
    for _ in 0..ancount {
        if pos >= buf.len() { break; }
        if buf[pos] & 0xC0 == 0xC0 {
            pos += 2;
        } else {
            loop {
                if pos >= buf.len() { return (rcode, ancount, ips); }
                let len = buf[pos] as usize;
                if len == 0 { pos += 1; break; }
                if len & 0xC0 == 0xC0 { pos += 2; break; }
                pos += 1 + len;
            }
        }
        if pos + 10 > buf.len() { break; }
        let rrtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen  = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > buf.len() { break; }
        match (rrtype, rdlen) {
            (1, 4) => {
                ips.push(std::net::IpAddr::V4(
                    std::net::Ipv4Addr::new(buf[pos], buf[pos+1], buf[pos+2], buf[pos+3])
                ));
            }
            (28, 16) => {
                let mut o = [0u8; 16];
                o.copy_from_slice(&buf[pos..pos + 16]);
                ips.push(std::net::IpAddr::V6(std::net::Ipv6Addr::from(o)));
            }
            _ => {}
        }
        pos += rdlen;
    }
    (rcode, ancount, ips)
}

fn rcode_name(rcode: u8) -> &'static str {
    match rcode {
        0 => "NOERROR", 1 => "FORMERR", 2 => "SERVFAIL",
        3 => "NXDOMAIN", 4 => "NOTIMP",  5 => "REFUSED",
        _ => "UNKNOWN",
    }
}

fn parse_ntp_fields(buf: &[u8]) -> String {
    let li      = (buf[0] >> 6) & 0x03;
    let vn      = (buf[0] >> 3) & 0x07;
    let stratum = buf[1];
    let ref_id  = &buf[12..16];
    let ref_id_str = if stratum <= 1 {
        String::from_utf8_lossy(ref_id).trim_end_matches('\0').to_string()
    } else {
        format!("{}.{}.{}.{}", ref_id[0], ref_id[1], ref_id[2], ref_id[3])
    };
    let tx_secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
    let unix_ts = tx_secs.saturating_sub(2208988800);
    format!("NTPv{} LI={} stratum={} refid={} tx_unix={}", vn, li, stratum, ref_id_str, unix_ts)
}

/// HTTP probe: TCP connect + send GET <path> HTTP/1.0 + validate "HTTP/" prefix in response.
/// RTT is measured from connection start to first response bytes.
pub async fn http_probe(
    addr:     SocketAddr,
    hostname: Option<Arc<String>>,
    path:     Arc<String>,
    timeout:  Duration,
) -> Result<(f64, u64, u64), ()> {
    let t0 = tokio::time::Instant::now();
    let connect_fut = TokioTcpStream::connect(addr);
    let mut stream = tokio::time::timeout(timeout, connect_fut).await
        .map_err(|_| ())?.map_err(|_| ())?;

    let host = hostname.as_deref().map(|s| s.as_str()).unwrap_or("");
    let request = build_http_request(&path, host);
    let bytes_tx = request.len() as u64;
    tokio::time::timeout(timeout, stream.write_all(request.as_bytes()))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let mut buf = [0u8; 1024];
    let n = tokio::time::timeout(timeout, stream.read(&mut buf))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    log_http_exchange("http", &request, &buf[..n]);

    if n < 5 || &buf[..5] != b"HTTP/" {
        return Err(());
    }
    Ok((t0.elapsed().as_secs_f64() * 1000.0, bytes_tx, n as u64))
}

/// Build a TlsConnector from a TlsConfig (shared by https and tls probes).
fn build_tls_connector(config: &TlsConfig) -> Result<tokio_rustls::TlsConnector, ()> {
    let versions: &[&rustls::SupportedProtocolVersion] = match config.version {
        TlsVersion::Any => rustls::DEFAULT_VERSIONS,
        TlsVersion::V12 => &[&rustls::version::TLS12],
        TlsVersion::V13 => &[&rustls::version::TLS13],
    };
    let tls_cfg = match &config.verify {
        TlsVerify::Skip => {
            rustls::ClientConfig::builder_with_provider(
                Arc::new(rustls::crypto::ring::default_provider()),
            )
            .with_protocol_versions(versions).map_err(|_| ())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerifier))
            .with_no_client_auth()
        }
        TlsVerify::System => {
            let mut root_store = rustls::RootCertStore::empty();
            let loaded = rustls_native_certs::load_native_certs();
            for cert in loaded.certs {
                let _ = root_store.add(cert);
            }
            if root_store.is_empty() {
                crate::logfile::write("tls: system certificate store is empty, TLS verification will likely fail");
            }
            rustls::ClientConfig::builder_with_provider(
                Arc::new(rustls::crypto::ring::default_provider()),
            )
            .with_protocol_versions(versions).map_err(|_| ())?
            .with_root_certificates(root_store)
            .with_no_client_auth()
        }
        TlsVerify::Cert(path) => {
            let pem = std::fs::read(path).map_err(|e| {
                crate::logfile::write(&format!("tls: cannot read certificate file '{}': {}", path, e));
            })?;
            let mut cursor = std::io::Cursor::new(pem);
            let certs: Vec<_> = rustls_pemfile::certs(&mut cursor)
                .filter_map(|r| r.ok())
                .collect();
            if certs.is_empty() {
                crate::logfile::write(&format!("tls: no valid certificates found in '{}'", path));
            }
            let mut root_store = rustls::RootCertStore::empty();
            for cert in certs {
                let _ = root_store.add(cert);
            }
            rustls::ClientConfig::builder_with_provider(
                Arc::new(rustls::crypto::ring::default_provider()),
            )
            .with_protocol_versions(versions).map_err(|_| ())?
            .with_root_certificates(root_store)
            .with_no_client_auth()
        }
    };
    Ok(tokio_rustls::TlsConnector::from(Arc::new(tls_cfg)))
}

/// HTTPS probe: TLS connect + send GET <path> HTTP/1.0 + validate response.
/// TLS behaviour (version, cert verification) is controlled by `tls_config`.
pub async fn https_probe(
    addr:       SocketAddr,
    hostname:   Option<Arc<String>>,
    tls_config: Arc<TlsConfig>,
    path:       Arc<String>,
    timeout:    Duration,
) -> Result<(f64, u64, u64), ()> {
    let t0 = tokio::time::Instant::now();

    let connector = build_tls_connector(&tls_config)?;
    let sni = build_sni(&hostname, addr)?;

    let tcp = tokio::time::timeout(timeout, TokioTcpStream::connect(addr))
        .await.map_err(|_| ())?.map_err(|_| ())?;
    let mut stream = tokio::time::timeout(timeout, connector.connect(sni, tcp))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let tls_ver = tls_version_str(stream.get_ref().1);

    let host = hostname.as_deref().map(|s| s.as_str()).unwrap_or("");
    let request = build_http_request(&path, host);
    let bytes_tx = request.len() as u64;
    tokio::time::timeout(timeout, stream.write_all(request.as_bytes()))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let mut buf = [0u8; 1024];
    let n = tokio::time::timeout(timeout, stream.read(&mut buf))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    crate::logfile::write(&format!("https: handshake addr={} {}", addr, tls_ver));
    log_http_exchange("https", &request, &buf[..n]);

    if n < 5 || &buf[..5] != b"HTTP/" {
        return Err(());
    }
    Ok((t0.elapsed().as_secs_f64() * 1000.0, bytes_tx, n as u64))
}

/// A rustls ServerCertVerifier that accepts any certificate without validation.
#[derive(Debug)]
struct SkipVerifier;

impl rustls::client::danger::ServerCertVerifier for SkipVerifier {
    fn verify_server_cert(
        &self,
        _end_entity:    &rustls::pki_types::CertificateDer,
        _intermediates: &[rustls::pki_types::CertificateDer],
        _server_name:   &rustls::pki_types::ServerName,
        _ocsp:          &[u8],
        _now:           rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert:    &rustls::pki_types::CertificateDer,
        _dss:     &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert:    &rustls::pki_types::CertificateDer,
        _dss:     &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

/// DNS A-record probe over UDP.
/// Sends a minimal well-formed DNS query and measures RTT to first response byte.
pub async fn dns_probe(
    addr:    std::net::SocketAddr,
    query:   Arc<String>,
    timeout: Duration,
) -> Result<(f64, u64, u64), ()> {
    use tokio::net::UdpSocket;

    let bind: std::net::SocketAddr = if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let sock = UdpSocket::bind(bind).await.map_err(|_| ())?;
    sock.connect(addr).await.map_err(|_| ())?;

    let txid: u16 = rand::random();
    let pkt = build_dns_query(&query, txid);
    let bytes_tx = pkt.len() as u64;
    crate::logfile::write(&format!(
        "dns: query name={} type=A txid={:#06x} {} bytes",
        query, txid, pkt.len()
    ));
    let t0 = tokio::time::Instant::now();
    tokio::time::timeout(timeout, sock.send(&pkt)).await.map_err(|_| ())?.map_err(|_| ())?;

    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf)).await.map_err(|_| ())?.map_err(|_| ())?;

    let (rcode, ancount, ips) = parse_dns_response(&buf[..n]);
    let ip_list = if ips.is_empty() {
        "none".to_string()
    } else {
        ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>().join(", ")
    };
    crate::logfile::write(&format!(
        "dns: response txid={:#06x} {} bytes rcode={} ({}) answers={} addrs=[{}]",
        txid, n, rcode_name(rcode), rcode, ancount, ip_list
    ));

    // Minimal validation: response bit set (QR=1 in byte 2 bit 7), and matches our txid.
    if n < 4 || buf[0] != pkt[0] || buf[1] != pkt[1] || (buf[2] & 0x80) == 0 {
        return Err(());
    }
    Ok((t0.elapsed().as_secs_f64() * 1000.0, bytes_tx, n as u64))
}

/// TLS probe: TCP connect + TLS handshake only. RTT = time to established secure channel.
/// Cert verification is controlled by `config.verify`; TLS version by `config.version`.
pub async fn tls_probe(
    addr:     SocketAddr,
    hostname: Option<Arc<String>>,
    config:   Arc<TlsConfig>,
    timeout:  Duration,
) -> Result<f64, ()> {
    let t0 = tokio::time::Instant::now();
    let connector = build_tls_connector(&config)?;
    let sni = build_sni(&hostname, addr)?;

    let tcp = tokio::time::timeout(timeout, TokioTcpStream::connect(addr))
        .await.map_err(|_| ())?.map_err(|_| ())?;
    let stream = tokio::time::timeout(timeout, connector.connect(sni, tcp))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let tls_ver  = tls_version_str(stream.get_ref().1);
    let sni_name = hostname.as_deref().map(|s| s.as_str()).unwrap_or("<ip>");
    crate::logfile::write(&format!(
        "tls: handshake addr={} sni={} {} → ok",
        addr, sni_name, tls_ver
    ));

    Ok(t0.elapsed().as_secs_f64() * 1000.0)
}

/// NTP probe over UDP (port 123).
/// Sends a minimal NTPv4 client request and validates the server response.
/// RTT is measured from sending the request to receiving the first response byte.
pub async fn ntp_probe(addr: SocketAddr, timeout: Duration) -> Result<f64, ()> {
    use tokio::net::UdpSocket;

    let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let sock = UdpSocket::bind(bind).await.map_err(|_| ())?;
    sock.connect(addr).await.map_err(|_| ())?;

    // NTPv4 client request: LI=0, VN=4, Mode=3 (client) → 0x23; rest zeros.
    let mut pkt = [0u8; 48];
    pkt[0] = 0x23;
    crate::logfile::write(&format!("ntp: request → {} NTPv4 LI=0 Mode=3(client) 48 bytes", addr));

    let t0 = tokio::time::Instant::now();
    tokio::time::timeout(timeout, sock.send(&pkt)).await.map_err(|_| ())?.map_err(|_| ())?;

    let mut buf = [0u8; 48];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf)).await.map_err(|_| ())?.map_err(|_| ())?;

    // Validate: response must be ≥48 bytes, version 1–4, mode 4 (server reply).
    if n < 48 {
        crate::logfile::write(&format!("ntp: response {} bytes (too short, expected 48)", n));
        return Err(());
    }
    let version = (buf[0] >> 3) & 0x07;
    let mode    = buf[0] & 0x07;
    crate::logfile::write(&format!("ntp: response {} bytes {}", n, parse_ntp_fields(&buf)));
    if !(1..=4).contains(&version) || mode != 4 {
        return Err(());
    }
    Ok(t0.elapsed().as_secs_f64() * 1000.0)
}

/// Build a minimal DNS query packet for an A record.
fn build_dns_query(name: &str, txid: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(512);
    pkt.extend_from_slice(&txid.to_be_bytes());
    // Flags: standard query, recursion desired.
    pkt.extend_from_slice(&[0x01, 0x00]);
    // QDCOUNT=1, ANCOUNT=0, NSCOUNT=0, ARCOUNT=0
    pkt.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    // QNAME: encode each label as length + bytes, terminated by 0x00.
    for label in name.split('.') {
        let b = label.as_bytes();
        pkt.push(b.len() as u8);
        pkt.extend_from_slice(b);
    }
    pkt.push(0x00);
    // QTYPE=A (1), QCLASS=IN (1)
    pkt.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
    pkt
}

/// SSH banner probe: TCP connect + read the SSH identification string.
/// Validates the response starts with "SSH-". RTT is measured from connection
/// start to the moment the banner is received.
/// Generic TCP banner probe: connects to a port and validates the initial response prefix.
pub async fn tcp_banner_probe(
    addr:    SocketAddr,
    timeout: Duration,
    prefix:  &[u8],
    label:   &str,
) -> Result<(f64, u64), ()> {
    let t0 = tokio::time::Instant::now();
    let mut stream = tokio::time::timeout(timeout, TokioTcpStream::connect(addr))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(timeout, stream.read(&mut buf))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let banner = String::from_utf8_lossy(&buf[..n]);
    crate::logfile::write(&format!(
        "{}: banner addr={} {} bytes:\n{}",
        label, addr, n, banner.trim_end()
    ));

    if n < prefix.len() || &buf[..prefix.len()] != prefix {
        return Err(());
    }
    Ok((t0.elapsed().as_secs_f64() * 1000.0, n as u64))
}

/// Generic TLS banner probe: performs TLS handshake and validates the initial response prefix.
pub async fn tls_banner_probe(
    addr:       SocketAddr,
    hostname:   Option<Arc<String>>,
    tls_config: Arc<TlsConfig>,
    timeout:    Duration,
    prefix:     &[u8],
    label:      &str,
) -> Result<(f64, u64), ()> {
    let t0 = tokio::time::Instant::now();
    let connector = build_tls_connector(&tls_config)?;
    let sni = build_sni(&hostname, addr)?;

    let tcp = tokio::time::timeout(timeout, TokioTcpStream::connect(addr))
        .await.map_err(|_| ())?.map_err(|_| ())?;
    let mut stream = tokio::time::timeout(timeout, connector.connect(sni, tcp))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let tls_ver  = tls_version_str(stream.get_ref().1);
    let sni_name = hostname.as_deref().map(|s| s.as_str()).unwrap_or("<ip>");
    crate::logfile::write(&format!(
        "{}: handshake addr={} sni={} {}",
        label, addr, sni_name, tls_ver
    ));

    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(timeout, stream.read(&mut buf))
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let banner = String::from_utf8_lossy(&buf[..n]);
    crate::logfile::write(&format!(
        "{}: banner addr={} {} bytes:\n{}",
        label, addr, n, banner.trim_end()
    ));

    if n < prefix.len() || &buf[..prefix.len()] != prefix {
        return Err(());
    }
    Ok((t0.elapsed().as_secs_f64() * 1000.0, n as u64))
}

pub async fn ssh_probe(addr: SocketAddr, timeout: Duration) -> Result<(f64, u64), ()> {
    tcp_banner_probe(addr, timeout, b"SSH-", "ssh").await
}

pub async fn smtp_probe(addr: SocketAddr, timeout: Duration) -> Result<(f64, u64), ()> {
    tcp_banner_probe(addr, timeout, b"220", "smtp").await
}

pub async fn smtps_probe(
    addr:       SocketAddr,
    hostname:   Option<Arc<String>>,
    tls_config: Arc<TlsConfig>,
    timeout:    Duration,
) -> Result<(f64, u64), ()> {
    tls_banner_probe(addr, hostname, tls_config, timeout, b"220", "smtps").await
}

/// Exec probe: run a shell command and measure its wall-clock duration.
/// Exit code 0 → Hit (success), any other exit code → Drop (failure).
/// stdout/stderr are suppressed to prevent TUI corruption.
/// VLAT_HOST, VLAT_IP, and VLAT_PORT are set in the child's environment.
/// kill_on_drop(true) ensures the child is killed if the probe times out.
pub async fn exec_probe(
    cmd:      Arc<String>,
    ip:       std::net::IpAddr,
    hostname: Option<Arc<String>>,
    port:     u16,
    timeout:  Duration,
) -> Result<f64, ()> {
    use tokio::process::Command;
    use std::process::Stdio;

    if cmd.is_empty() {
        return Err(());
    }

    let host_str = hostname.as_deref().map(|s| s.as_str()).unwrap_or("").to_string();
    let ip_str   = ip.to_string();
    let port_str = port.to_string();

    let t0 = tokio::time::Instant::now();
    let mut child = Command::new("sh")
        .args(["-c", cmd.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("VLAT_HOST", &host_str)
        .env("VLAT_IP",   &ip_str)
        .env("VLAT_PORT", &port_str)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            crate::logfile::write(&format!("exec [{}]: spawn failed: {}", cmd, e));
        })?;

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    async fn read_pipe(handle: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
        let Some(mut h) = handle else { return String::new() };
        let mut buf = vec![0u8; 2048];
        let n = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::io::AsyncReadExt::read(&mut h, &mut buf),
        ).await.ok().and_then(|r| r.ok()).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).trim().to_string()
    }

    match tokio::time::timeout(timeout, child.wait()).await {
        Err(_) => {
            crate::logfile::write(&format!("exec [{}]: timed out after {:.1}s", cmd, timeout.as_secs_f64()));
            Err(())
        }
        Ok(Err(e)) => {
            crate::logfile::write(&format!("exec [{}]: wait error: {}", cmd, e));
            Err(())
        }
        Ok(Ok(status)) => {
            let rtt = t0.elapsed().as_secs_f64() * 1000.0;
            let stdout_text = read_pipe(stdout_handle).await;
            let stderr_text = read_pipe(stderr_handle).await;
            let code = status.code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string());
            let mut msg = format!("exec [{}]: exit {} rtt={:.1}ms", cmd, code, rtt);
            if !stdout_text.is_empty() { msg.push_str(&format!("\nstdout: {}", stdout_text)); }
            if !stderr_text.is_empty() { msg.push_str(&format!("\nstderr: {}", stderr_text)); }
            crate::logfile::write(&msg);
            if status.success() { Ok(rtt) } else { Err(()) }
        }
    }
}

/// QUIC handshake probe: sends a QUIC Initial packet over UDP and measures the
/// RTT to a completed connection (QUIC+TLS 1.3 handshake).  Certificate
/// validation is skipped by default (consistent with the tls/https probes).
pub async fn quic_probe(
    addr:     SocketAddr,
    hostname: Option<Arc<String>>,
    timeout:  Duration,
) -> Result<f64, ()> {
    let mut tls_cfg = rustls::ClientConfig::builder_with_provider(
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| ())?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(SkipVerifier))
    .with_no_client_auth();

    // QUIC servers require ALPN; advertise h3 (HTTP/3) which all QUIC stacks accept.
    tls_cfg.alpn_protocols = vec![b"h3".to_vec()];

    let quic_client_cfg = quinn::crypto::rustls::QuicClientConfig::try_from(tls_cfg)
        .map_err(|_| ())?;

    let local: SocketAddr = if addr.is_ipv6() {
        "[::]:0".parse().unwrap()
    } else {
        "0.0.0.0:0".parse().unwrap()
    };

    let mut endpoint = quinn::Endpoint::client(local).map_err(|_| ())?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_client_cfg)));

    let server_name = hostname
        .as_deref()
        .map(|s| s.as_str().to_owned())
        .unwrap_or_else(|| addr.ip().to_string());

    crate::logfile::write(&format!(
        "quic: connecting → {} server_name={} ALPN=h3",
        addr, server_name
    ));
    let t0 = tokio::time::Instant::now();
    let connecting = endpoint.connect(addr, &server_name).map_err(|_| ())?;
    let connection = tokio::time::timeout(timeout, connecting)
        .await.map_err(|_| ())?.map_err(|_| ())?;

    let rtt = t0.elapsed().as_secs_f64() * 1000.0;
    crate::logfile::write(&format!(
        "quic: handshake complete addr={} server_name={} rtt={:.2}ms",
        connection.remote_address(), server_name, rtt
    ));
    // Close gracefully then drop the endpoint; don't wait_idle() as it can stall.
    connection.close(0u32.into(), b"");
    drop(endpoint);
    Ok(rtt)
}

fn is_conn_refused(e: &io::Error) -> bool { e.kind() == io::ErrorKind::ConnectionRefused }
fn is_port_unreach(e: &io::Error) -> bool {
    #[cfg(windows)]
    { e.kind() == io::ErrorKind::ConnectionReset }
    #[cfg(not(windows))]
    { e.kind() == io::ErrorKind::ConnectionRefused }
}
