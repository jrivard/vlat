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

use std::{net::{IpAddr, Ipv4Addr, SocketAddr}, sync::Arc};
use tokio::net::lookup_host;
use hickory_resolver::{
    config::{ConnectionConfig, NameServerConfig, ProtocolConfig, ResolverConfig},
    net::runtime::TokioRuntimeProvider,
    Resolver, TokioResolver,
};
use crate::cli::{Args, PingMode, parse_interval};
use crate::types::ResolvedTarget;

pub type DnsResolver = Arc<TokioResolver>;

pub struct TargetOverrides {
    pub interval:         Option<u64>,
    pub timeout:          Option<f64>,
    pub resolve_interval: Option<u64>,
    pub label:            Option<String>,
    pub http_path:        Option<String>,
    pub exec_cmd:         Option<String>,
}

pub struct ParsedSpec<'a> {
    pub host:      &'a str,
    pub mode:      PingMode,
    pub port:      u16,
    pub overrides: TargetOverrides,
}

/// Parse a DNS server address spec (after the scheme is stripped) into a SocketAddr
/// and an optional SNI hostname (present when the spec is a hostname, not an IP literal).
async fn parse_server_addr(spec: &str, default_port: u16) -> Result<(SocketAddr, Option<String>), String> {
    // Bracketed IPv6: [addr] or [addr]:port
    if spec.starts_with('[') {
        let close = spec.find(']').ok_or_else(|| format!("unmatched '[' in '{}'", spec))?;
        let ip: IpAddr = spec[1..close].parse()
            .map_err(|_| format!("invalid IPv6 address in '{}'", spec))?;
        let port = match spec.get(close + 1..) {
            Some(s) if s.starts_with(':') => s[1..].parse::<u16>()
                .map_err(|_| format!("invalid port in '{}'", spec))?,
            _ => default_port,
        };
        return Ok((SocketAddr::new(ip, port), None));
    }
    // Plain IP (v4 or bare v6 literal) with no port
    if let Ok(ip) = spec.parse::<IpAddr>() {
        return Ok((SocketAddr::new(ip, default_port), None));
    }
    // IP:port (v4 with explicit port, or SocketAddr literal)
    if let Ok(addr) = spec.parse::<SocketAddr>() {
        return Ok((addr, None));
    }
    // Hostname, possibly with :port
    let (hostname, port) = match spec.rfind(':') {
        Some(pos) => match spec[pos + 1..].parse::<u16>() {
            Ok(p) => (&spec[..pos], p),
            Err(_) => (spec, default_port),
        },
        None => (spec, default_port),
    };
    let sni = hostname.to_string();
    let addr = lookup_host(format!("{}:{}", hostname, port)).await
        .map_err(|_| format!("cannot resolve DNS server hostname '{}'", hostname))?
        .next()
        .ok_or_else(|| format!("no address found for DNS server '{}'", hostname))?;
    Ok((addr, Some(sni)))
}

/// Build a hickory async resolver pointed at a single explicit DNS server.
/// `spec` format: `[scheme://]host[:port]`
/// Schemes: `udp` (default), `tcp`, `tls` (DoT), `https` (DoH), `quic` (DoQ).
pub async fn build_dns_resolver(spec: &str) -> Result<DnsResolver, Box<dyn std::error::Error + Send + Sync>> {
    let (scheme, host_port) = match spec.find("://") {
        Some(idx) => (&spec[..idx], &spec[idx + 3..]),
        None      => ("udp", spec),
    };
    let default_port = match scheme {
        "udp" | "tcp"   => 53u16,
        "tls" | "quic"  => 853u16,
        "https"         => 443u16,
        other => return Err(format!(
            "unknown DNS scheme '{}' - valid: udp://, tcp://, tls://, https://, quic://", other
        ).into()),
    };
    // Separate an optional URL path (e.g. /dns-query for https) from the host part.
    let (host_part, path) = match host_port.find('/') {
        Some(idx) => (&host_port[..idx], Some(Arc::<str>::from(&host_port[idx..]))),
        None      => (host_port, None),
    };
    let (addr, sni) = parse_server_addr(host_part, default_port).await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    // Encrypted transports need a TLS server name; fall back to the IP for literals.
    let server_name = || Arc::<str>::from(sni.clone().unwrap_or_else(|| addr.ip().to_string()));
    let protocol = match scheme {
        "udp"   => ProtocolConfig::Udp,
        "tcp"   => ProtocolConfig::Tcp,
        "tls"   => ProtocolConfig::Tls { server_name: server_name() },
        "quic"  => ProtocolConfig::Quic { server_name: server_name() },
        "https" => ConnectionConfig::https(server_name(), path).protocol,
        _       => unreachable!(),
    };
    let mut connection = ConnectionConfig::new(protocol);
    connection.port = addr.port();
    let ns = NameServerConfig::new(addr.ip(), true, vec![connection]);
    let config = ResolverConfig::from_parts(None, Vec::new(), vec![ns]);
    let resolver = Resolver::builder_with_config(config, TokioRuntimeProvider::default()).build()?;
    Ok(Arc::new(resolver))
}

/// Resolve a hostname to an IP, using the custom resolver if provided or the
/// system resolver otherwise.  IP literals are passed through without a lookup.
pub async fn resolve_ip(
    host: &str,
    prefer_v4: bool,
    prefer_v6: bool,
    resolver: Option<&DnsResolver>,
) -> Result<IpAddr, String> {
    let pick = |ip: IpAddr| -> bool {
        if prefer_v4 { ip.is_ipv4() } else if prefer_v6 { ip.is_ipv6() } else { true }
    };
    if let Some(r) = resolver {
        let lookup = r.lookup_ip(host).await
            .map_err(|e| format!("cannot resolve '{}': {}", host, e))?;
        lookup.iter().find(|&ip| pick(ip))
            .ok_or_else(|| format!("no matching IP for '{}'", host))
    } else {
        lookup_host(format!("{}:0", host)).await
            .map_err(|_| format!("cannot resolve '{}'", host))?
            .find(|a| pick(a.ip()))
            .map(|a| a.ip())
            .ok_or_else(|| format!("no matching IP for '{}'", host))
    }
}

/// Normalise a bare HTTP path: ensure it starts with `/`.
fn normalise_http_path(path: &str) -> String {
    if path.starts_with('/') { path.to_string() } else { format!("/{}", path) }
}

pub async fn parse_and_resolve(
    spec: &str,
    args: &Args,
    dns_resolver: Option<&DnsResolver>,
) -> Result<ResolvedTarget, Box<dyn std::error::Error>> {
    let parsed = parse_target_spec(spec, args)?;
    let host_part = parsed.host;
    let inline_mode = parsed.mode;
    let inline_port = parsed.port;

    // Exec targets don't need DNS - the hostname is just a label.
    if inline_mode == PingMode::Exec {
        let has_cmd = parsed.overrides.exec_cmd.is_some() || args.exec_cmd.is_some();
        if !has_cmd {
            return Err(format!(
                "exec target '{}' requires a command - use exec=<cmd> inline or --exec-cmd",
                host_part
            ).into());
        }
        let (label, label_is_custom) = match parsed.overrides.label.clone() {
            Some(custom) => (custom, true),
            None         => (host_part.to_string(), false),
        };
        return Ok(ResolvedTarget {
            ip:               "0.0.0.0".parse().unwrap(),
            label,
            label_is_custom,
            hostname:         Some(host_part.to_string()),
            mode:             inline_mode,
            port:             inline_port,
            interval:         parsed.overrides.interval,
            timeout:          parsed.overrides.timeout,
            resolve_interval: parsed.overrides.resolve_interval,
            http_path:        parsed.overrides.http_path,
            exec_cmd:         parsed.overrides.exec_cmd,
        });
    }

    let ip = resolve_ip(host_part, args.ipv4, args.ipv6, dns_resolver).await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let input_is_ip = host_part.parse::<std::net::IpAddr>().is_ok();
    let auto_label = if input_is_ip {
        host_part.to_string()
    } else {
        format!("{} ({})", host_part, ip)
    };
    let (label, label_is_custom) = match parsed.overrides.label {
        Some(custom) => (custom, true),
        None         => (auto_label, false),
    };
    let hostname = if input_is_ip { None } else { Some(host_part.to_string()) };
    Ok(ResolvedTarget {
        ip,
        label,
        label_is_custom,
        hostname,
        mode: inline_mode,
        port: inline_port,
        interval:         parsed.overrides.interval,
        timeout:          parsed.overrides.timeout,
        resolve_interval: parsed.overrides.resolve_interval,
        http_path:        parsed.overrides.http_path,
        exec_cmd:         parsed.overrides.exec_cmd,
    })
}

/// Safety ceiling on how many hosts a single CIDR block is allowed to enumerate,
/// checked before generating the list. This exists only to avoid materialising
/// millions of strings for something like a typo'd `/8`; the real limit on the
/// total target count (`constants::MAX_TARGETS`) is enforced separately once all
/// specs have been expanded.
const MAX_CIDR_EXPANSION: u64 = 65_536;

/// Expand `A.B.C.D/N` into every usable host address in the block (network and
/// broadcast excluded for prefixes shorter than /31). Returns `Ok(None)` when
/// `host` isn't CIDR-shaped (no error - it's just not a range spec).
fn expand_cidr(host: &str) -> Result<Option<Vec<String>>, String> {
    let (addr_part, prefix_part) = match host.split_once('/') {
        Some(parts) => parts,
        None => return Ok(None),
    };
    let base: Ipv4Addr = match addr_part.parse() {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };
    let prefix: u32 = prefix_part.parse()
        .map_err(|_| format!("invalid CIDR prefix '/{}' in '{}'", prefix_part, host))?;
    if prefix > 32 {
        return Err(format!("invalid CIDR prefix '/{}' in '{}' - must be 0-32", prefix, host));
    }
    let host_bits = 32 - prefix;
    let block_size: u64 = 1u64 << host_bits;
    let (start, end): (u64, u64) = if host_bits >= 2 {
        (1, block_size - 2) // skip network and broadcast addresses
    } else {
        (0, block_size - 1) // /31, /32 - no network/broadcast concept
    };
    if end - start + 1 > MAX_CIDR_EXPANSION {
        return Err(format!(
            "CIDR block '{}' expands to too many hosts - vlat allows at most {} targets total",
            host, crate::constants::MAX_TARGETS
        ));
    }
    let network = if host_bits == 32 { 0u32 } else { u32::from(base) & (!0u32 << host_bits) };
    let out = (start..=end)
        .map(|offset| Ipv4Addr::from(network.wrapping_add(offset as u32)).to_string())
        .collect();
    Ok(Some(out))
}

/// Expand `A.B.C.D-E` (nmap-style last-octet range) into each address from D to E
/// inclusive. Returns `Ok(None)` when `host` isn't shaped like an octet range.
fn expand_octet_range(host: &str) -> Result<Option<Vec<String>>, String> {
    let last_dot = match host.rfind('.') {
        Some(i) => i,
        None => return Ok(None),
    };
    let prefix = &host[..last_dot];
    let last = &host[last_dot + 1..];
    let octets: Vec<&str> = prefix.split('.').collect();
    if octets.len() != 3 || octets.iter().any(|o| o.is_empty() || !o.chars().all(|c| c.is_ascii_digit())) {
        return Ok(None);
    }
    let (start_str, end_str) = match last.split_once('-') {
        Some(parts) => parts,
        None => return Ok(None),
    };
    if start_str.is_empty() || end_str.is_empty()
        || !start_str.chars().all(|c| c.is_ascii_digit())
        || !end_str.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    // Shape matches - from here, any problem is a real error, not "not a range".
    for o in &octets {
        o.parse::<u8>().map_err(|_| format!("invalid octet '{}' in range '{}'", o, host))?;
    }
    let start: u16 = start_str.parse().map_err(|_| format!("invalid range start '{}' in '{}'", start_str, host))?;
    let end: u16 = end_str.parse().map_err(|_| format!("invalid range end '{}' in '{}'", end_str, host))?;
    if start > 255 || end > 255 {
        return Err(format!("octet range '{}' out of bounds (0-255) in '{}'", last, host));
    }
    if start > end {
        return Err(format!("range start {} is greater than end {} in '{}'", start, end, host));
    }
    Ok(Some((start..=end).map(|v| format!("{}.{}", prefix, v)).collect()))
}

/// Expand a single target spec into one or more concrete specs if its host portion
/// is written as an IPv4 octet range (`192.168.1.1-20`) or CIDR block (`192.168.1.0/24`).
/// Any `:mode:port` suffix and `,key=val` overrides are preserved verbatim on every
/// expanded entry. Specs that aren't range-shaped come back unchanged.
///
/// URI-form specs (`scheme://...`) are passed through untouched - `/` already means
/// "start of path" there, so CIDR notation would be ambiguous.
pub fn expand_target_spec(spec: &str) -> Result<Vec<String>, String> {
    if spec.contains("://") {
        return Ok(vec![spec.to_string()]);
    }
    let (base, kv_part) = match spec.find(',') {
        Some(idx) => (&spec[..idx], &spec[idx..]), // kv_part keeps its leading ','
        None => (spec, ""),
    };
    let host_end = base.find(':').unwrap_or(base.len());
    let host = &base[..host_end];
    let suffix = &base[host_end..]; // ":mode:port" or ""

    if let Some(hosts) = expand_cidr(host)? {
        return Ok(hosts.into_iter().map(|h| format!("{}{}{}", h, suffix, kv_part)).collect());
    }
    if let Some(hosts) = expand_octet_range(host)? {
        return Ok(hosts.into_iter().map(|h| format!("{}{}{}", h, suffix, kv_part)).collect());
    }
    Ok(vec![spec.to_string()])
}

/// Expand every spec in `specs` via [`expand_target_spec`], in order.
pub fn expand_target_specs(specs: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        out.extend(expand_target_spec(spec)?);
    }
    Ok(out)
}

pub fn parse_target_spec<'a>(spec: &'a str, args: &Args) -> Result<ParsedSpec<'a>, String> {
    // Split on first comma to separate base spec from key=val overrides
    let (base, kv_part) = if let Some(idx) = spec.find(',') {
        (&spec[..idx], &spec[idx + 1..])
    } else {
        (spec, "")
    };

    // Parse key=val overrides
    let mut ov_interval:         Option<u64>    = None;
    let mut ov_timeout:          Option<f64>    = None;
    let mut ov_resolve_interval: Option<u64>    = None;
    let mut ov_label:            Option<String> = None;
    let mut ov_http_path:        Option<String> = None;
    let mut ov_exec_cmd:         Option<String> = None;

    if !kv_part.is_empty() {
        for pair in kv_part.split(',') {
            let mut it = pair.splitn(2, '=');
            let key = it.next().unwrap_or("").trim();
            let val = it.next().unwrap_or("").trim();
            match key {
                "interval" | "i" => {
                    if let Ok(ms) = parse_interval(val) {
                        ov_interval = Some(ms);
                    }
                }
                "timeout" | "t" => {
                    if let Ok(v) = val.parse::<f64>() {
                        ov_timeout = Some(v);
                    }
                }
                "resolve" | "resolve-interval" => {
                    if let Ok(v) = val.parse::<u64>() {
                        ov_resolve_interval = Some(v);
                    }
                }
                "label" => {
                    if !val.is_empty() {
                        ov_label = Some(val.to_string());
                    }
                }
                "hpath" => {
                    if !val.is_empty() {
                        ov_http_path = Some(normalise_http_path(val));
                    }
                }
                "exec" => {
                    if !val.is_empty() {
                        ov_exec_cmd = Some(val.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    let (host, mode, port, uri_path) = parse_base_spec(base, args)?;
    if ov_http_path.is_none() {
        ov_http_path = uri_path;
    }

    Ok(ParsedSpec {
        host,
        mode,
        port,
        overrides: TargetOverrides {
            interval:         ov_interval,
            timeout:          ov_timeout,
            resolve_interval: ov_resolve_interval,
            label:            ov_label,
            http_path:        ov_http_path,
            exec_cmd:         ov_exec_cmd,
        },
    })
}

/// Map a URI scheme to its probe mode. Only schemes with a natural URI form are
/// accepted - `icmp` (no port/connection concept) and `exec` (a local command,
/// not a network target) have none.
fn scheme_to_mode(scheme: &str) -> Result<PingMode, String> {
    match scheme.to_lowercase().as_str() {
        "http"  => Ok(PingMode::Http),
        "https" => Ok(PingMode::Https),
        "tcp"   => Ok(PingMode::Tcp),
        "udp"   => Ok(PingMode::Udp),
        "dns"   => Ok(PingMode::Dns),
        "tls"   => Ok(PingMode::Tls),
        "ntp"   => Ok(PingMode::Ntp),
        "ssh"   => Ok(PingMode::Ssh),
        "smtp"  => Ok(PingMode::Smtp),
        "smtps" => Ok(PingMode::Smtps),
        "quic"  => Ok(PingMode::Quic),
        other   => Err(format!(
            "unknown or unsupported URI scheme '{}' - valid: http, https, tcp, udp, dns, tls, ntp, ssh, smtp, smtps, quic (icmp and exec have no URI form)",
            other
        )),
    }
}

/// Split the `host[:port][/path]` portion that follows `scheme://` into its parts.
/// IPv6 hosts must be bracketed, per URI syntax (RFC 3986) - `scheme://[::1]:443/path`.
fn split_uri_authority(remainder: &str) -> Result<(&str, Option<u16>, Option<&str>), String> {
    if remainder.starts_with('[') {
        let close = remainder.find(']').ok_or_else(|| format!("unmatched '[' in '{}'", remainder))?;
        let host = &remainder[..=close];
        let mut rest = &remainder[close + 1..];
        let mut port = None;
        if let Some(r) = rest.strip_prefix(':') {
            let (port_str, after) = match r.find('/') {
                Some(i) => (&r[..i], &r[i..]),
                None => (r, ""),
            };
            port = Some(port_str.parse::<u16>().map_err(|_| format!("invalid port '{}' in '{}'", port_str, remainder))?);
            rest = after;
        }
        let path = if rest.is_empty() { None } else { Some(rest) };
        return Ok((host, port, path));
    }
    let (host_port, path) = match remainder.find('/') {
        Some(i) => (&remainder[..i], Some(&remainder[i..])),
        None => (remainder, None),
    };
    if host_port.is_empty() {
        return Err(format!("missing host in URI 'scheme://{}'", remainder));
    }
    let (host, port) = match host_port.rfind(':') {
        Some(pos) => match host_port[pos + 1..].parse::<u16>() {
            Ok(p) => (&host_port[..pos], Some(p)),
            Err(_) => (host_port, None),
        },
        None => (host_port, None),
    };
    if host.is_empty() {
        return Err(format!("missing host in URI 'scheme://{}'", remainder));
    }
    Ok((host, port, path))
}

/// Parse a `scheme://host[:port][/path]` target spec. `path` is only accepted for
/// http/https (fed into the hpath override); any other scheme with a path is an error.
fn parse_uri_spec<'a>(scheme: &str, remainder: &'a str, args: &Args) -> Result<(&'a str, PingMode, u16, Option<String>), String> {
    let mode = scheme_to_mode(scheme)?;
    let (host, port_opt, path_opt) = split_uri_authority(remainder)?;
    let port = port_opt.unwrap_or_else(|| mode.default_port(args));
    let path = match path_opt {
        None => None,
        Some(p) if matches!(mode, PingMode::Http | PingMode::Https) => Some(normalise_http_path(p)),
        Some(p) => return Err(format!("path '{}' is not supported for {}:// targets", p, scheme)),
    };
    Ok((host, mode, port, path))
}

fn parse_base_spec<'a>(spec: &'a str, args: &Args) -> Result<(&'a str, PingMode, u16, Option<String>), String> {
    if let Some(idx) = spec.find("://") {
        let scheme = &spec[..idx];
        let remainder = &spec[idx + 3..];
        return parse_uri_spec(scheme, remainder, args);
    }
    if spec.starts_with('[') {
        let close = spec.find(']').unwrap_or(spec.len() - 1);
        let host  = &spec[..=close];
        let rest  = &spec[close + 1..];
        let (mode, port) = parse_mode_port(rest.trim_start_matches(':'), args)?;
        return Ok((host, mode, port, None));
    }
    let colon_count = spec.chars().filter(|&c| c == ':').count();
    if colon_count > 1 {
        // More than 2 colons is always an IPv6 address.
        // With exactly 2 colons, check whether the middle segment is a mode keyword
        // (e.g. "host:tcp:443") - if so, fall through to normal host:mode:port parsing.
        let second_is_mode = spec.split(':').nth(1)
            .map(|s| matches!(s.to_lowercase().as_str(), "tcp" | "udp" | "icmp" | "http" | "https" | "dns" | "tls" | "ntp" | "ssh" | "smtp" | "smtps" | "exec" | "quic"))
            .unwrap_or(false);
        if !second_is_mode {
            let mode = args.mode.clone().unwrap_or(PingMode::Icmp);
            let default_port = mode.default_port(args);
            return Ok((spec, mode, default_port, None));
        }
    }
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    let host = parts[0];
    let rest = if parts.len() > 1 { parts[1..].join(":") } else { String::new() };
    let (mode, port) = parse_mode_port(&rest, args)?;
    Ok((host, mode, port, None))
}

pub fn parse_mode_port(rest: &str, args: &Args) -> Result<(PingMode, u16), String> {
    let default_mode = args.mode.clone().unwrap_or(PingMode::Icmp);
    if rest.is_empty() {
        let port = default_mode.default_port(args);
        return Ok((default_mode, port));
    }
    let parts: Vec<&str> = rest.splitn(2, ':').collect();
    let mode_str = parts[0].to_lowercase();
    let mode = match mode_str.as_str() {
        "tcp"   => PingMode::Tcp,
        "udp"   => PingMode::Udp,
        "icmp"  => PingMode::Icmp,
        "http"  => PingMode::Http,
        "https" => PingMode::Https,
        "dns"   => PingMode::Dns,
        "tls"   => PingMode::Tls,
        "ntp"   => PingMode::Ntp,
        "ssh"   => PingMode::Ssh,
        "smtp"  => PingMode::Smtp,
        "smtps" => PingMode::Smtps,
        "exec"  => PingMode::Exec,
        "quic"  => PingMode::Quic,
        other   => {
            // If it looks like a port number, treat as default mode + explicit port.
            if let Ok(p) = other.parse::<u16>() {
                return Ok((default_mode, p));
            }
            return Err(format!("unknown probe mode '{}' - valid modes: icmp, udp, tcp, http, https, dns, tls, ntp, ssh, smtp, smtps, exec, quic", other));
        }
    };
    let port = parts.get(1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(mode.default_port(args));
    Ok((mode, port))
}

pub fn fire_resolve(
    indices:   Vec<usize>,
    host:      String,
    prefer_v4: bool,
    prefer_v6: bool,
    tx:        tokio::sync::mpsc::UnboundedSender<crate::types::ResolveResult>,
    dns_resolver: Option<DnsResolver>,
) {
    tokio::spawn(async move {
        let new_ip = resolve_ip(&host, prefer_v4, prefer_v6, dns_resolver.as_ref()).await.ok();
        let new_label = match new_ip {
            Some(ip) => format!("{} ({})", host, ip),
            None     => format!("{} (?)", host),
        };
        let _ = tx.send(crate::types::ResolveResult { indices, new_ip, new_label });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Args, PingMode};
    use clap::Parser;

    fn default_args() -> Args {
        Args::try_parse_from(["vlat", "dummy"]).unwrap()
    }

    // --- parse_mode_port ---

    #[test]
    fn mode_port_empty_defaults_to_icmp_and_udp_port() {
        let args = default_args();
        let (mode, port) = parse_mode_port("", &args).unwrap();
        assert_eq!(mode, PingMode::Icmp);
        assert_eq!(port, crate::constants::DEFAULT_UDP_PORT);
    }

    #[test]
    fn mode_port_tcp_uses_default_tcp_port() {
        let args = default_args();
        let (mode, port) = parse_mode_port("tcp", &args).unwrap();
        assert_eq!(mode, PingMode::Tcp);
        assert_eq!(port, crate::constants::DEFAULT_TCP_PORT);
    }

    #[test]
    fn mode_port_tcp_with_explicit_port() {
        let args = default_args();
        let (mode, port) = parse_mode_port("tcp:443", &args).unwrap();
        assert_eq!(mode, PingMode::Tcp);
        assert_eq!(port, 443);
    }

    #[test]
    fn mode_port_udp() {
        let args = default_args();
        let (mode, port) = parse_mode_port("udp:9000", &args).unwrap();
        assert_eq!(mode, PingMode::Udp);
        assert_eq!(port, 9000);
    }

    #[test]
    fn mode_port_http_defaults_to_80() {
        let args = default_args();
        let (mode, port) = parse_mode_port("http", &args).unwrap();
        assert_eq!(mode, PingMode::Http);
        assert_eq!(port, 80);
    }

    #[test]
    fn mode_port_https_defaults_to_443() {
        let args = default_args();
        let (mode, port) = parse_mode_port("https", &args).unwrap();
        assert_eq!(mode, PingMode::Https);
        assert_eq!(port, 443);
    }

    #[test]
    fn mode_port_https_with_custom_port() {
        let args = default_args();
        let (mode, port) = parse_mode_port("https:8443", &args).unwrap();
        assert_eq!(mode, PingMode::Https);
        assert_eq!(port, 8443);
    }

    #[test]
    fn mode_port_unknown_keyword_is_error() {
        let args = default_args();
        assert!(parse_mode_port("zzz", &args).is_err());
    }

    #[test]
    fn mode_port_case_insensitive() {
        let args = default_args();
        let (mode, port) = parse_mode_port("TCP:8080", &args).unwrap();
        assert_eq!(mode, PingMode::Tcp);
        assert_eq!(port, 8080);
    }

    // --- parse_target_spec ---

    #[test]
    fn target_spec_simple_host() {
        let args = default_args();
        let p = parse_target_spec("example.org", &args).unwrap();
        assert_eq!(p.host, "example.org");
        assert_eq!(p.mode, PingMode::Icmp);
        assert!(p.overrides.interval.is_none());
        assert!(p.overrides.timeout.is_none());
        assert!(p.overrides.resolve_interval.is_none());
    }

    #[test]
    fn target_spec_host_with_tcp_mode_and_port() {
        let args = default_args();
        let p = parse_target_spec("example.org:tcp:443", &args).unwrap();
        assert_eq!(p.host, "example.org");
        assert_eq!(p.mode, PingMode::Tcp);
        assert_eq!(p.port, 443);
    }

    #[test]
    fn target_spec_host_with_mode_only() {
        let args = default_args();
        let p = parse_target_spec("example.org:http", &args).unwrap();
        assert_eq!(p.mode, PingMode::Http);
        assert_eq!(p.port, 80);
    }

    #[test]
    fn target_spec_interval_override_ms() {
        let args = default_args();
        let p = parse_target_spec("example.org,interval=500ms", &args).unwrap();
        assert_eq!(p.host, "example.org");
        assert_eq!(p.overrides.interval, Some(500));
    }

    #[test]
    fn target_spec_interval_override_shorthand() {
        let args = default_args();
        let p = parse_target_spec("example.org,i=2s", &args).unwrap();
        assert_eq!(p.overrides.interval, Some(2000));
    }

    #[test]
    fn target_spec_timeout_override() {
        let args = default_args();
        let p = parse_target_spec("example.org,timeout=5", &args).unwrap();
        assert_eq!(p.overrides.timeout, Some(5.0));
    }

    #[test]
    fn target_spec_timeout_shorthand() {
        let args = default_args();
        let p = parse_target_spec("example.org,t=2.5", &args).unwrap();
        assert_eq!(p.overrides.timeout, Some(2.5));
    }

    #[test]
    fn target_spec_resolve_interval_override() {
        let args = default_args();
        let p = parse_target_spec("example.org,resolve=60", &args).unwrap();
        assert_eq!(p.overrides.resolve_interval, Some(60));
    }

    #[test]
    fn target_spec_multiple_overrides() {
        let args = default_args();
        let p = parse_target_spec("example.org:tcp:8080,interval=200ms,timeout=3", &args).unwrap();
        assert_eq!(p.mode, PingMode::Tcp);
        assert_eq!(p.port, 8080);
        assert_eq!(p.overrides.interval, Some(200));
        assert_eq!(p.overrides.timeout, Some(3.0));
    }

    #[test]
    fn target_spec_ipv6_bracketed_no_mode() {
        let args = default_args();
        let p = parse_target_spec("[::1]", &args).unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.mode, PingMode::Icmp);
    }

    #[test]
    fn target_spec_ipv6_bracketed_with_tcp() {
        let args = default_args();
        let p = parse_target_spec("[::1]:tcp:443", &args).unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.mode, PingMode::Tcp);
        assert_eq!(p.port, 443);
    }

    #[test]
    fn target_spec_raw_ipv6_address() {
        // Multiple colons, middle segment is not a mode keyword → treated as IPv6 literal
        let args = default_args();
        let p = parse_target_spec("2001:db8::1", &args).unwrap();
        assert_eq!(p.host, "2001:db8::1");
        assert_eq!(p.mode, PingMode::Icmp);
    }

    #[test]
    fn target_spec_host_with_mode_keyword_as_second_segment() {
        // "host:tcp:port" - second segment IS a mode keyword so it's not treated as IPv6
        let args = default_args();
        let p = parse_target_spec("192.168.1.1:tcp:22", &args).unwrap();
        assert_eq!(p.host, "192.168.1.1");
        assert_eq!(p.mode, PingMode::Tcp);
        assert_eq!(p.port, 22);
    }

    #[test]
    fn target_spec_unknown_override_key_is_ignored() {
        let args = default_args();
        let p = parse_target_spec("example.org,foo=bar", &args).unwrap();
        assert!(p.overrides.interval.is_none());
        assert!(p.overrides.timeout.is_none());
        assert!(p.overrides.resolve_interval.is_none());
    }

    // --- URI target specs ---

    #[test]
    fn uri_https_bare_host_defaults_to_443() {
        let args = default_args();
        let p = parse_target_spec("https://example.org", &args).unwrap();
        assert_eq!(p.host, "example.org");
        assert_eq!(p.mode, PingMode::Https);
        assert_eq!(p.port, 443);
        assert_eq!(p.overrides.http_path, None);
    }

    #[test]
    fn uri_http_bare_host_defaults_to_80() {
        let args = default_args();
        let p = parse_target_spec("http://example.org", &args).unwrap();
        assert_eq!(p.mode, PingMode::Http);
        assert_eq!(p.port, 80);
    }

    #[test]
    fn uri_https_with_path_sets_hpath_override() {
        let args = default_args();
        let p = parse_target_spec("https://example.org/health", &args).unwrap();
        assert_eq!(p.host, "example.org");
        assert_eq!(p.port, 443);
        assert_eq!(p.overrides.http_path, Some("/health".to_string()));
    }

    #[test]
    fn uri_https_with_port_and_path_and_query() {
        let args = default_args();
        let p = parse_target_spec("https://example.org:8443/status?check=1", &args).unwrap();
        assert_eq!(p.port, 8443);
        assert_eq!(p.overrides.http_path, Some("/status?check=1".to_string()));
    }

    #[test]
    fn uri_explicit_hpath_override_wins_over_uri_path() {
        let args = default_args();
        let p = parse_target_spec("https://example.org/from-uri,hpath=/from-kv", &args).unwrap();
        assert_eq!(p.overrides.http_path, Some("/from-kv".to_string()));
    }

    #[test]
    fn uri_tcp_with_port() {
        let args = default_args();
        let p = parse_target_spec("tcp://10.0.0.1:2222", &args).unwrap();
        assert_eq!(p.host, "10.0.0.1");
        assert_eq!(p.mode, PingMode::Tcp);
        assert_eq!(p.port, 2222);
    }

    #[test]
    fn uri_tcp_with_path_is_error() {
        let args = default_args();
        assert!(parse_target_spec("tcp://10.0.0.1/foo", &args).is_err());
    }

    #[test]
    fn uri_ssh_default_port() {
        let args = default_args();
        let p = parse_target_spec("ssh://build.example.org", &args).unwrap();
        assert_eq!(p.mode, PingMode::Ssh);
        assert_eq!(p.port, crate::constants::DEFAULT_SSH_PORT);
    }

    #[test]
    fn uri_dns_with_port() {
        let args = default_args();
        let p = parse_target_spec("dns://10.0.0.1:5353", &args).unwrap();
        assert_eq!(p.mode, PingMode::Dns);
        assert_eq!(p.port, 5353);
    }

    #[test]
    fn uri_bracketed_ipv6_with_port_and_path() {
        let args = default_args();
        let p = parse_target_spec("https://[::1]:8443/health", &args).unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, 8443);
        assert_eq!(p.overrides.http_path, Some("/health".to_string()));
    }

    #[test]
    fn uri_bracketed_ipv6_no_port_no_path() {
        let args = default_args();
        let p = parse_target_spec("tcp://[::1]", &args).unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.mode, PingMode::Tcp);
    }

    #[test]
    fn uri_unknown_scheme_is_error() {
        let args = default_args();
        assert!(parse_target_spec("ftp://example.org", &args).is_err());
    }

    #[test]
    fn uri_icmp_scheme_is_error() {
        let args = default_args();
        assert!(parse_target_spec("icmp://example.org", &args).is_err());
    }

    #[test]
    fn uri_exec_scheme_is_error() {
        let args = default_args();
        assert!(parse_target_spec("exec://example.org", &args).is_err());
    }

    #[test]
    fn uri_with_per_target_overrides_after_comma() {
        let args = default_args();
        let p = parse_target_spec("https://example.org,interval=500ms,label=web", &args).unwrap();
        assert_eq!(p.host, "example.org");
        assert_eq!(p.overrides.interval, Some(500));
        assert_eq!(p.overrides.label, Some("web".to_string()));
    }

    // --- expand_target_spec: octet ranges ---

    #[test]
    fn range_simple_octet_range() {
        let out = expand_target_spec("192.168.1.1-3").unwrap();
        assert_eq!(out, vec!["192.168.1.1", "192.168.1.2", "192.168.1.3"]);
    }

    #[test]
    fn range_octet_range_with_mode_port() {
        let out = expand_target_spec("192.168.1.1-3:tcp:443").unwrap();
        assert_eq!(out, vec!["192.168.1.1:tcp:443", "192.168.1.2:tcp:443", "192.168.1.3:tcp:443"]);
    }

    #[test]
    fn range_octet_range_with_overrides() {
        let out = expand_target_spec("192.168.1.1-2,label=cam").unwrap();
        assert_eq!(out, vec!["192.168.1.1,label=cam", "192.168.1.2,label=cam"]);
    }

    #[test]
    fn range_octet_range_single_value() {
        let out = expand_target_spec("192.168.1.5-5").unwrap();
        assert_eq!(out, vec!["192.168.1.5"]);
    }

    #[test]
    fn range_octet_range_start_after_end_is_error() {
        assert!(expand_target_spec("192.168.1.9-3").is_err());
    }

    #[test]
    fn range_octet_range_out_of_bounds_is_error() {
        assert!(expand_target_spec("192.168.1.250-300").is_err());
    }

    // --- expand_target_spec: CIDR ---

    #[test]
    fn range_cidr_slash_30() {
        // /30 = 4 addresses, network + broadcast excluded -> 2 usable hosts
        let out = expand_target_spec("192.168.1.0/30").unwrap();
        assert_eq!(out, vec!["192.168.1.1", "192.168.1.2"]);
    }

    #[test]
    fn range_cidr_slash_31_includes_both_addresses() {
        let out = expand_target_spec("192.168.1.0/31").unwrap();
        assert_eq!(out, vec!["192.168.1.0", "192.168.1.1"]);
    }

    #[test]
    fn range_cidr_with_mode_and_overrides() {
        let out = expand_target_spec("192.168.1.0/30:tcp:443,label=cam").unwrap();
        assert_eq!(out, vec!["192.168.1.1:tcp:443,label=cam", "192.168.1.2:tcp:443,label=cam"]);
    }

    #[test]
    fn range_cidr_bad_prefix_is_error() {
        assert!(expand_target_spec("192.168.1.0/33").is_err());
    }

    #[test]
    fn range_cidr_huge_block_is_error() {
        assert!(expand_target_spec("10.0.0.0/8").is_err());
    }

    // --- expand_target_spec: non-range specs pass through unchanged ---

    #[test]
    fn range_plain_hostname_unchanged() {
        let out = expand_target_spec("example.org:tcp:443").unwrap();
        assert_eq!(out, vec!["example.org:tcp:443"]);
    }

    #[test]
    fn range_plain_ip_unchanged() {
        let out = expand_target_spec("10.0.0.1,label=gateway").unwrap();
        assert_eq!(out, vec!["10.0.0.1,label=gateway"]);
    }

    #[test]
    fn range_ipv6_literal_unchanged() {
        let out = expand_target_spec("2001:db8::1").unwrap();
        assert_eq!(out, vec!["2001:db8::1"]);
    }

    #[test]
    fn range_bracketed_ipv6_unchanged() {
        let out = expand_target_spec("[::1]:tcp:443").unwrap();
        assert_eq!(out, vec!["[::1]:tcp:443"]);
    }

    #[test]
    fn range_uri_form_unchanged_even_with_slash() {
        let out = expand_target_spec("tcp://192.168.1.0/24").unwrap();
        assert_eq!(out, vec!["tcp://192.168.1.0/24"]);
    }

    #[test]
    fn range_hyphenated_hostname_unchanged() {
        let out = expand_target_spec("web-01.example.com").unwrap();
        assert_eq!(out, vec!["web-01.example.com"]);
    }

    #[test]
    fn range_expand_target_specs_multiple() {
        let out = expand_target_specs(&["192.168.1.1-2".to_string(), "example.org".to_string()]).unwrap();
        assert_eq!(out, vec!["192.168.1.1", "192.168.1.2", "example.org"]);
    }
}
