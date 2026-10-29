//! One-shot page that collects the values, behind a random token. `transport` decides where it listens.

use crate::seal::{Opener, Sealed};
use crate::spec::Spec;
use crate::save::{Pick, Places, SavedTo};
use crate::transport::{Exposure, TUNNEL_NOTE, Via};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};

const FORM: &str = include_str!("form.html");
const MAX_BODY: u64 = 64 * 1024;

/// How the request ended, with every place values were saved to (a cancel can follow a partial save).
pub enum Outcome {
    /// Field names that were saved.
    Saved(Vec<String>, Vec<SavedTo>),
    Cancelled(Vec<SavedTo>),
    TimedOut(Vec<SavedTo>),
}

#[derive(Deserialize)]
struct Submission {
    values: BTreeMap<String, String>,
    places: Vec<Pick>,
}

/// Request body: plain values, or values sealed to this request's key.
#[derive(Deserialize)]
#[serde(untagged)]
enum Body {
    Sealed { sealed: Sealed },
    Plain(Submission),
}

pub fn serve(
    spec: &Spec,
    places: &Places,
    server: Server,
    exposure: &Exposure,
    timeout: Duration,
    on_ready: impl FnOnce(&str),
) -> Result<Outcome> {
    let token = random_hex(32)?;
    let nonce = random_hex(16)?;
    let opener = if exposure.encrypt { Some(Opener::new()?) } else { None };
    let page = render(spec, places, exposure, &nonce);
    let mut saved: Vec<SavedTo> = Vec::new();
    let base = format!("/{token}");

    let mut url = format!("{}{base}", exposure.public_origin);
    if let Some(o) = &opener {
        url.push_str(&format!("#k={}", o.public_key()));
    }
    on_ready(&url);

    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(Outcome::TimedOut(saved));
        }
        let Some(mut req) = server.recv_timeout(left)? else { continue };

        // Host check defeats DNS rebinding; the token keeps other local pages from finding the form.
        if !header(&req, "Host").is_some_and(|h| exposure.hosts.contains(&h)) {
            respond(req, 403, "text/plain", "forbidden".into(), None);
            continue;
        }
        let url = req.url().to_string();
        match (req.method(), url.as_str()) {
            (Method::Get, u) if u == base => respond(req, 200, "text/html; charset=utf-8", page.clone(), Some(&nonce)),
            (Method::Post, u) if u == format!("{base}/submit") || u == format!("{base}/cancel") => {
                if !header(&req, "Origin").is_some_and(|o| exposure.origins.contains(&o))
                    || !header(&req, "Content-Type").is_some_and(|c| c.starts_with("application/json"))
                {
                    respond(req, 403, "text/plain", "forbidden".into(), None);
                    continue;
                }
                if u.ends_with("/cancel") {
                    respond_json(req, 200, json!({"ok": true}));
                    return Ok(Outcome::Cancelled(saved));
                }
                let submission = read_json::<Body>(&mut req)
                    .and_then(|body| unseal(body, opener.as_ref(), token.as_bytes()))
                    .and_then(|s| collect(spec, s));
                let (values, picks) = match submission {
                    Ok(v) => v,
                    Err(e) => {
                        respond_json(req, 400, json!({"error": e.to_string()}));
                        continue;
                    }
                };
                let outcome = match places.save(spec, &picks, &values) {
                    Ok(o) => o,
                    Err(e) => {
                        respond_json(req, 400, json!({"error": format!("{e:#}")}));
                        continue;
                    }
                };
                let done: Vec<&str> = picks
                    .iter()
                    .filter(|p| outcome.saved.iter().any(|s| s.kind == p.id))
                    .map(|p| p.id.as_str())
                    .collect();
                let done = json!(done);
                saved.extend(outcome.saved);
                if outcome.failed.is_empty() {
                    let names: Vec<String> = values.into_keys().collect();
                    respond_json(req, 200, json!({"saved": names, "done": done}));
                    return Ok(Outcome::Saved(names, saved));
                }
                // Keep going so the user can retry or untick the place that failed.
                let error = format!("Couldn't save to {}", outcome.failed.join("; "));
                respond_json(req, 500, json!({"error": error, "done": done}));
            }
            _ => respond(req, 404, "text/plain", "not found".into(), None),
        }
    }
}

fn unseal(body: Body, opener: Option<&Opener>, aad: &[u8]) -> Result<Submission> {
    match (body, opener) {
        (Body::Sealed { sealed }, Some(o)) => Ok(serde_json::from_slice(&o.open(&sealed, aad)?)?),
        (Body::Plain(s), None) => Ok(s),
        (Body::Plain(_), Some(_)) => Err(anyhow!("values must be encrypted on this link; reload the page and try again")),
        (Body::Sealed { .. }, None) => Err(anyhow!("unexpected encrypted submission")),
    }
}

/// Checks the submission against the spec; returns field name -> trimmed value (skipping empty optionals), and the picks.
fn collect(spec: &Spec, s: Submission) -> Result<(BTreeMap<String, String>, Vec<Pick>)> {
    let mut out = BTreeMap::new();
    for name in s.values.keys() {
        if !spec.fields.iter().any(|f| &f.name == name) {
            return Err(anyhow!("unexpected field {name}"));
        }
    }
    for f in &spec.fields {
        let v = s.values.get(&f.name).map(|v| v.trim()).unwrap_or("");
        if v.is_empty() {
            if !f.optional {
                return Err(anyhow!("{} is required", f.label.as_deref().unwrap_or(&f.name)));
            }
            continue;
        }
        out.insert(f.name.clone(), v.to_string());
    }
    Ok((out, s.places))
}

fn render(spec: &Spec, places: &Places, exposure: &Exposure, nonce: &str) -> String {
    let data = json!({
        "title": spec.title,
        "reason": spec.reason,
        "places": places.for_page(spec, tilde),
        "encrypt": exposure.encrypt,
        "note": (exposure.via == Via::Tunnel).then_some(TUNNEL_NOTE),
        "fields": spec.fields.iter().map(|f| json!({
            "name": f.name, "label": f.label, "optional": f.optional,
            "placeholder": f.placeholder, "pattern": f.pattern, "help_url": f.help_url,
        })).collect::<Vec<_>>(),
    });
    // Escape so agent-supplied strings can't close the <script> block.
    let data = data
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    FORM.replace("__NONCE__", nonce).replace("__DATA__", &data)
}

fn tilde(path: &str) -> String {
    match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_string(),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T> {
    let mut body = String::new();
    req.as_reader().take(MAX_BODY).read_to_string(&mut body)?;
    Ok(serde_json::from_str(&body)?)
}

fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.to_string())
}

fn respond_json(req: Request, status: u16, body: serde_json::Value) {
    respond(req, status, "application/json", body.to_string(), None);
}

fn respond(req: Request, status: u16, content_type: &str, body: String, nonce: Option<&str>) {
    let csp = match nonce {
        Some(n) => format!(
            "default-src 'none'; script-src 'nonce-{n}'; style-src 'nonce-{n}'; connect-src 'self'; \
             form-action 'none'; base-uri 'none'; frame-ancestors 'none'"
        ),
        None => "default-src 'none'; frame-ancestors 'none'".to_string(),
    };
    let mut res = Response::from_string(body).with_status_code(status);
    for (k, v) in [
        ("Content-Type", content_type),
        ("Content-Security-Policy", csp.as_str()),
        ("Cache-Control", "no-store"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Robots-Tag", "noindex, nofollow"),
    ] {
        res.add_header(Header::from_bytes(k, v).expect("static header"));
    }
    let _ = req.respond(res);
}

pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("no randomness: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}
