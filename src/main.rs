mod log;
mod phrase;
mod save;
mod seal;
mod spec;
mod store;
mod stores;
mod transport;
mod web;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use spec::Spec;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};
use transport::Via;

/// Collect secrets from the user in a browser page and write them to a file, without the agent seeing them.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ask the user for values described by a JSON spec (file path, or `-` for stdin). Prints the link and an id
    /// straight away; collect the result with `agentikeys wait <id>`.
    Request {
        #[arg(default_value = "-")]
        spec: String,
        /// How the page reaches the user. Defaults to AGENTIKEYS_VIA, then `agentikeys config via`, then auto.
        #[arg(long, value_enum)]
        via: Option<Via>,
        /// Seconds to wait for the user before giving up.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Don't open this machine's browser (local mode); just print the link.
        #[arg(long)]
        no_open: bool,
        /// Block until the user is done instead of returning straight away.
        #[arg(long)]
        foreground: bool,
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
    /// Show settings, or set how pages reach you by default (`agentikeys config via tailscale`).
    Config {
        #[command(subcommand)]
        set: Option<ConfigSet>,
    },
    /// Show recent requests: which keys, saved where, for which project. Never values.
    Log {
        /// How many of the most recent requests to show.
        #[arg(short, default_value_t = 20)]
        n: usize,
        /// Only requests that asked for this key (field name).
        #[arg(long)]
        key: Option<String>,
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Show your security phrase (only in an interactive terminal, never to an agent).
    Phrase {
        /// Replace it with a new one.
        #[arg(long)]
        reset: bool,
    },
}

#[derive(Subcommand)]
enum ConfigSet {
    /// Default transport: auto, local, tailscale or tunnel.
    Via {
        #[arg(value_enum)]
        via: Via,
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
    via: Option<Via>,
    /// Something to pass on to the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    keys: Vec<String>,
    /// Every place the user chose that the values were saved to.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    saved_to: Vec<save::SavedTo>,
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
        Cmd::Request { spec, via, timeout, no_open, foreground, receipt } => {
            let r = request(&spec, via, timeout, no_open, foreground, receipt.as_deref()).unwrap_or_else(Receipt::error);
            if let Some(path) = &receipt {
                let _ = write_receipt(path, &r);
            }
            if r.status == "pending" {
                println!("{}", serde_json::to_string(&r).expect("receipt serializes"));
                return ExitCode::SUCCESS;
            }
            r
        }
        Cmd::Wait { id, timeout } => wait(&id, timeout).unwrap_or_else(Receipt::error),
        Cmd::Config { set } => return plain(config(set)),
        Cmd::Phrase { reset } => return plain(show_phrase(reset)),
        Cmd::Log { n, key, json } => return plain(show_log(n, key.as_deref(), json)),
    };
    println!("{}", serde_json::to_string(&receipt).expect("receipt serializes"));
    receipt.exit_code()
}

/// Exit status for commands a person runs, which print text rather than a receipt.
fn plain(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("agentikeys: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn show_log(n: usize, key: Option<&str>, json: bool) -> Result<()> {
    let entries = log::read(n, key)?;
    if entries.is_empty() && !json {
        println!("No requests logged yet.");
    }
    for e in &entries {
        println!("{}", if json { serde_json::to_string(e)? } else { log::describe(e) });
    }
    Ok(())
}

fn show_phrase(reset: bool) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        bail!("the security phrase is only shown in an interactive terminal, so it never reaches an agent; run `agentikeys phrase` yourself");
    }
    let phrase = if reset { phrase::reset()? } else { phrase::get_or_create()?.0 };
    println!("Your agentikeys security phrase: {phrase}");
    println!("Every real agentikeys page shows it. Never enter keys on a page that doesn't.");
    Ok(())
}

fn request(
    spec_arg: &str,
    via: Option<Via>,
    timeout: u64,
    no_open: bool,
    foreground: bool,
    receipt: Option<&Path>,
) -> Result<Receipt> {
    let text = if spec_arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("reading spec from stdin")?;
        s
    } else {
        fs::read_to_string(spec_arg).with_context(|| format!("reading spec {spec_arg}"))?
    };
    let spec = Spec::parse(&text)?;
    let cwd = std::env::current_dir()?;
    // Checked here too, so a bad file fails straight away instead of in the background.
    let file = store::plan_file(&spec, &cwd)?;

    let via = match via {
        Some(v) => v,
        None => configured_via()?,
    }
    .resolve();

    if !foreground {
        return spawn_detached(&text, via, timeout, no_open, &cwd);
    }

    let detecting = std::thread::spawn(stores::detect);
    let (server, exposure) = transport::start(via)?;
    let offers = detecting.join().unwrap_or_default();
    let places = save::Places::new(&spec, file, offers)?;
    let (phrase, phrase_new) = phrase::get_or_create()?;
    let page = web::Page { spec: &spec, places: &places, phrase: &phrase, phrase_new };
    let outcome = web::serve(&page, server, &exposure, Duration::from_secs(timeout), |url| {
        let opened = via == Via::Local && !no_open && open_browser(url);
        eprintln!(
            "agentikeys: {} {url}",
            if opened { "opened this page in the browser on this machine:" } else { "open this link to enter the values:" }
        );
        if let Some(path) = receipt {
            let note = (via == Via::Tunnel).then(|| transport::TUNNEL_NOTE.to_string());
            let _ = write_receipt(path, &Receipt { url: Some(url.into()), via: Some(via), note, ..Receipt::status("pending") });
        }
    })?;
    if via == Via::Tunnel {
        // Let the final response finish its trip back through Cloudflare before the tunnel closes.
        std::thread::sleep(Duration::from_secs(3));
    }
    drop(exposure);

    let done = match outcome {
        web::Outcome::Saved(keys, saved_to) => Receipt { keys, saved_to, ..Receipt::status("saved") },
        web::Outcome::Cancelled(saved_to) => Receipt { saved_to, ..Receipt::status("cancelled") },
        web::Outcome::TimedOut(saved_to) => Receipt { saved_to, ..Receipt::status("timed_out") },
    };
    let entry = log::Entry {
        time: log::now(),
        id: receipt.and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()),
        title: spec.title.clone(),
        cwd: cwd.display().to_string(),
        keys: spec.fields.iter().map(|f| f.name.clone()).collect(),
        via: name(via).into(),
        status: done.status.clone(),
        saved_to: done.saved_to.clone(),
    };
    if let Err(e) = log::append(&entry) {
        eprintln!("agentikeys: couldn't write the log: {e:#}");
    }
    Ok(done)
}

fn spawn_detached(spec_text: &str, via: Via, timeout: u64, no_open: bool, cwd: &Path) -> Result<Receipt> {
    let dir = state_dir()?;
    let id = web::random_hex(8)?;
    let spec_path = dir.join(format!("{id}.spec.json"));
    let receipt_path = dir.join(format!("{id}.json"));
    fs::write(&spec_path, spec_text)?;

    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("request").arg(&spec_path).arg("--foreground").arg("--timeout").arg(timeout.to_string());
    cmd.arg("--via").arg(name(via));
    cmd.arg("--receipt").arg(&receipt_path).current_dir(cwd);
    if no_open {
        cmd.arg("--no-open");
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // Own process group, so the harness ending this command doesn't kill the page.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn().context("starting background request")?;

    // Wait for the link (a tunnel can take a while) so the agent can post it in chat.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
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

/// agentikeys' own folder: settings, the security phrase and the log.
pub fn config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").or_else(|| std::env::var_os("APPDATA")) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME isn't set")?).join(".config"),
    };
    Ok(base.join("agentikeys"))
}

fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

fn configured_via() -> Result<Via> {
    if let Ok(v) = std::env::var("AGENTIKEYS_VIA") {
        return Via::from_str(&v, true).map_err(|_| anyhow::anyhow!("AGENTIKEYS_VIA={v} isn't auto, local, tailscale or tunnel"));
    }
    let path = config_path()?;
    let Ok(text) = fs::read_to_string(&path) else { return Ok(Via::Auto) };
    let doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("reading {}", path.display()))?;
    match doc.get("via").and_then(|v| v.as_str()) {
        Some(v) => Via::from_str(v, true).map_err(|_| anyhow::anyhow!("{}: via = {v:?} isn't valid", path.display())),
        None => Ok(Via::Auto),
    }
}

fn config(set: Option<ConfigSet>) -> Result<()> {
    let path = config_path()?;
    match set {
        None => {
            let via = configured_via()?;
            println!("via = {} (resolves to {} here)", name(via), name(via.resolve()));
            println!("config file: {}", path.display());
        }
        Some(ConfigSet::Via { via }) => {
            let mut doc: toml_edit::DocumentMut = fs::read_to_string(&path).unwrap_or_default().parse()?;
            doc["via"] = toml_edit::value(name(via));
            fs::create_dir_all(path.parent().expect("has parent"))?;
            fs::write(&path, doc.to_string())?;
            println!("agentikeys will reach you via {} by default ({})", name(via), path.display());
        }
    }
    Ok(())
}

fn name(via: Via) -> &'static str {
    match via {
        Via::Auto => "auto",
        Via::Local => "local",
        Via::Tailscale => "tailscale",
        Via::Tunnel => "tunnel",
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
