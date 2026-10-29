mod spec;
mod store;
mod web;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use spec::Spec;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};
use store::GitStatus;

/// Collect secrets from the user in a local browser page and write them to a file, without the agent seeing them.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ask the user for values described by a JSON spec (file path, or `-` for stdin) and save them.
    Request {
        #[arg(default_value = "-")]
        spec: String,
        /// Seconds to wait for the user before giving up.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Don't open the browser; just print the URL (e.g. over SSH).
        #[arg(long)]
        no_open: bool,
        /// Return immediately with an id; poll it with `agentikeys wait <id>`.
        #[arg(long)]
        detach: bool,
        #[arg(long, hide = true)]
        receipt: Option<PathBuf>,
    },
    /// Wait for a detached request to finish and print its receipt.
    Wait {
        id: String,
        /// Seconds to wait before reporting it as still pending.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

/// What the agent gets back. Never contains secret values.
#[derive(Serialize, Deserialize, Default)]
struct Receipt {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gitignore: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Receipt {
    fn status(status: &str) -> Receipt {
        Receipt { status: status.into(), ..Default::default() }
    }

    fn error(e: anyhow::Error) -> Receipt {
        Receipt { error: Some(format!("{e:#}")), ..Receipt::status("error") }
    }

    fn exit_code(&self) -> ExitCode {
        ExitCode::from(match self.status.as_str() {
            "saved" => 0,
            "cancelled" => 2,
            "timed_out" => 3,
            "pending" => 4,
            _ => 1,
        })
    }
}

fn main() -> ExitCode {
    let receipt = match Cli::parse().command {
        Cmd::Request { spec, timeout, no_open, detach, receipt } => {
            let r = request(&spec, timeout, no_open, detach, receipt.as_deref()).unwrap_or_else(Receipt::error);
            if let Some(path) = &receipt {
                let _ = write_receipt(path, &r);
            }
            r
        }
        Cmd::Wait { id, timeout } => wait(&id, timeout).unwrap_or_else(Receipt::error),
    };
    println!("{}", serde_json::to_string(&receipt).expect("receipt serializes"));
    receipt.exit_code()
}

fn request(spec_arg: &str, timeout: u64, no_open: bool, detach: bool, receipt: Option<&Path>) -> Result<Receipt> {
    let text = if spec_arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("reading spec from stdin")?;
        s
    } else {
        fs::read_to_string(spec_arg).with_context(|| format!("reading spec {spec_arg}"))?
    };
    let spec = Spec::parse(&text)?;
    let cwd = std::env::current_dir()?;
    let plan = store::plan(&spec, &cwd)?;

    if detach {
        return spawn_detached(&text, timeout, no_open, &cwd);
    }

    let outcome = web::serve(&spec, &plan, Duration::from_secs(timeout), |url| {
        if !no_open && open_browser(url) {
            eprintln!("agentikeys: opened a page in your browser; waiting for you to fill it in.");
        } else {
            eprintln!("agentikeys: open this link to enter the values: {url}");
        }
        if let Some(path) = receipt {
            let _ = write_receipt(path, &Receipt { url: Some(url.into()), ..Receipt::status("pending") });
        }
    })?;

    let path = Some(plan.path.display().to_string());
    Ok(match outcome {
        web::Outcome::Saved(keys) => Receipt {
            path,
            format: Some(plan.format.as_str().into()),
            keys,
            gitignore: Some(
                match plan.git {
                    GitStatus::NotInRepo => "not_in_repo",
                    GitStatus::AlreadyIgnored => "already_ignored",
                    GitStatus::WillIgnore => "added",
                }
                .into(),
            ),
            ..Receipt::status("saved")
        },
        web::Outcome::Cancelled => Receipt { path, ..Receipt::status("cancelled") },
        web::Outcome::TimedOut => Receipt { path, ..Receipt::status("timed_out") },
    })
}

fn spawn_detached(spec_text: &str, timeout: u64, no_open: bool, cwd: &Path) -> Result<Receipt> {
    let dir = state_dir()?;
    let id = web::random_hex(8)?;
    let spec_path = dir.join(format!("{id}.spec.json"));
    let receipt_path = dir.join(format!("{id}.json"));
    fs::write(&spec_path, spec_text)?;

    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("request").arg(&spec_path).arg("--timeout").arg(timeout.to_string());
    cmd.arg("--receipt").arg(&receipt_path).current_dir(cwd);
    if no_open {
        cmd.arg("--no-open");
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // Own process group, so the harness ending this command doesn't kill the page.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn().context("starting background request")?;

    // Wait briefly for the URL so the agent can pass it on if the browser didn't open.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if let Some(r) = read_receipt(&receipt_path) {
            let _ = fs::remove_file(&spec_path);
            return Ok(Receipt { id: Some(id), ..r });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Receipt { id: Some(id), ..Receipt::status("pending") })
}

fn wait(id: &str, timeout: u64) -> Result<Receipt> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not a request id: {id}");
    }
    let path = state_dir()?.join(format!("{id}.json"));
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let done = Instant::now() >= deadline;
        match read_receipt(&path) {
            Some(r) if r.status != "pending" || done => return Ok(Receipt { id: Some(id.into()), ..r }),
            None if done => bail!("no request with id {id}"),
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn state_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("agentikeys");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn write_receipt(path: &Path, r: &Receipt) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string(r)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn read_receipt(path: &Path) -> Option<Receipt> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn open_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        Command::new("xdg-open")
    };
    cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().is_ok()
}
