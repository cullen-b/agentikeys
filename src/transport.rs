//! How the form page reaches the user: this machine, the user's tailnet, or a Cloudflare quick tunnel.

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tiny_http::Server;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// Local browser when at the machine; Tailscale (or a tunnel) over SSH.
    Auto,
    /// Open the page in this machine's browser.
    Local,
    /// Serve the page on this machine's Tailscale address.
    Tailscale,
    /// Serve the page through a temporary Cloudflare quick tunnel (no account needed).
    Tunnel,
}

pub const TUNNEL_NOTE: &str = "This link goes through a temporary Cloudflare tunnel. Everything on the page and \
everything you send is encrypted between your browser and agentikeys, so the tunnel only carries encrypted data. \
For a direct, private connection, consider \
setting up Tailscale (https://tailscale.com/download) and running `agentikeys config via tailscale`. The tunnel \
keeps working if you'd rather not.";

pub struct Exposure {
    pub via: Via,
    /// Base URL to give the user, without path.
    pub public_origin: String,
    /// Host header values the server accepts.
    pub hosts: Vec<String>,
    /// Origin header values the server accepts on POST.
    pub origins: Vec<String>,
    /// Encrypt page data, replies and values in the browser. Off only for Tailscale: browsers won't run
    /// WebCrypto on a plain-http tailnet address, and WireGuard already encrypts it end to end.
    pub encrypt: bool,
    tunnel: Option<Child>,
}

impl Drop for Exposure {
    fn drop(&mut self) {
        if let Some(child) = &mut self.tunnel {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Via {
    pub fn resolve(self) -> Via {
        if self != Via::Auto {
            return self;
        }
        let over_ssh = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        match (over_ssh, tailscale_ip().is_ok()) {
            (false, _) => Via::Local,
            (true, true) => Via::Tailscale,
            (true, false) => Via::Tunnel,
        }
    }
}

/// Binds the server and makes it reachable for `via` (already resolved).
pub fn start(via: Via) -> Result<(Server, Exposure)> {
    let bind_ip = match via {
        Via::Tailscale => tailscale_ip()?,
        _ => "127.0.0.1".to_string(),
    };
    let server = Server::http(format!("{bind_ip}:0")).map_err(|e| anyhow!("starting server on {bind_ip}: {e}"))?;
    let port = server.server_addr().to_ip().ok_or_else(|| anyhow!("no local port"))?.port();
    let local = format!("{bind_ip}:{port}");

    let exposure = match via {
        Via::Auto | Via::Local => Exposure {
            via,
            public_origin: format!("http://{local}"),
            origins: vec![format!("http://{local}")],
            hosts: vec![local],
            encrypt: true,
            tunnel: None,
        },
        Via::Tailscale => {
            let mut hosts = vec![local.clone()];
            if let Some(name) = tailscale_dns_name() {
                hosts.push(format!("{name}:{port}"));
            }
            Exposure {
                via,
                public_origin: format!("http://{local}"),
                origins: hosts.iter().map(|h| format!("http://{h}")).collect(),
                hosts,
                encrypt: false,
                tunnel: None,
            }
        }
        Via::Tunnel => {
            let (child, host) = start_tunnel(port)?;
            Exposure {
                via,
                public_origin: format!("https://{host}"),
                origins: vec![format!("https://{host}")],
                hosts: vec![host, local],
                encrypt: true,
                tunnel: Some(child),
            }
        }
    };
    Ok((server, exposure))
}

fn tailscale_ip() -> Result<String> {
    let out = Command::new("tailscale")
        .args(["ip", "-4"])
        .stderr(Stdio::null())
        .output()
        .context("Tailscale isn't installed (https://tailscale.com/download); or use --via tunnel")?;
    let ip = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
    if !out.status.success() || ip.is_empty() {
        bail!("Tailscale isn't connected on this machine; start it, or use --via tunnel");
    }
    Ok(ip)
}

fn tailscale_dns_name() -> Option<String> {
    let out = Command::new("tailscale").args(["status", "--json"]).stderr(Stdio::null()).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let name = v["Self"]["DNSName"].as_str()?.trim_end_matches('.');
    (!name.is_empty()).then(|| name.to_string())
}

/// Starts `cloudflared` and returns it with the public hostname once the tunnel is connected.
fn start_tunnel(port: u16) -> Result<(Child, String)> {
    // An existing ~/.cloudflared/config.yaml breaks quick tunnels, so point cloudflared at an empty one.
    let empty_config = std::env::temp_dir().join("agentikeys-cloudflared.yml");
    std::fs::write(&empty_config, "{}\n")?;
    let mut child = Command::new("cloudflared")
        .args(["tunnel", "--no-autoupdate", "--config"])
        .arg(&empty_config)
        .args(["--url", &format!("http://127.0.0.1:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            anyhow!(
                "cloudflared isn't installed. Install it (macOS: `brew install cloudflared`, Windows: `winget install \
                 --id Cloudflare.cloudflared`, Linux: https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/downloads/), \
                 or use --via tailscale"
            )
        })?;

    // Keep draining stderr for the tunnel's lifetime so cloudflared never blocks on a full pipe.
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let mut host = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        let Ok(line) = rx.recv_timeout(left) else { break };
        match host {
            None => host = trycloudflare_host(&line),
            Some(h) if line.contains("Registered tunnel connection") => {
                wait_until_published(&h);
                return Ok((child, h));
            }
            Some(_) => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!("cloudflared didn't open a tunnel within 45 seconds; try again, or use --via tailscale")
}

/// New tunnel names take a moment to appear in DNS, and an early lookup gets "doesn't exist" cached for
/// a minute. Ask a public resolver directly (not the system cache) until the name resolves, up to 20s.
fn wait_until_published(host: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if resolves_publicly(host).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Minimal DNS A query to 1.1.1.1; true when it answers with at least one record.
fn resolves_publicly(host: &str) -> std::io::Result<bool> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
    sock.connect("1.1.1.1:53")?;
    let mut id = [0u8; 2];
    getrandom::fill(&mut id).map_err(std::io::Error::other)?;
    let mut query = vec![id[0], id[1], 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in host.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    sock.send(&query)?;
    let mut buf = [0u8; 512];
    let n = sock.recv(&mut buf)?;
    let answers = u16::from_be_bytes([buf[6], buf[7]]);
    Ok(n >= 12 && buf[..2] == id && buf[3] & 0x0f == 0 && answers > 0)
}

fn trycloudflare_host(line: &str) -> Option<String> {
    let start = line.find("https://")? + "https://".len();
    let host: String =
        line[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.').collect();
    host.ends_with(".trycloudflare.com").then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tunnel_host_in_log_line() {
        let line = "2026-10-29T21:00:00Z INF |  https://quiet-river-abc-123.trycloudflare.com                    |";
        assert_eq!(trycloudflare_host(line).as_deref(), Some("quiet-river-abc-123.trycloudflare.com"));
        assert_eq!(trycloudflare_host("see https://www.cloudflare.com/website-terms/"), None);
    }

    #[test]
    #[ignore = "needs network"]
    fn public_dns_check() {
        assert!(resolves_publicly("cloudflare.com").unwrap());
        assert!(!resolves_publicly("agentikeys-does-not-exist-zz9.trycloudflare.com").unwrap());
    }
}
