//! The control API under `/_lab/`.

use http::header::{self, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use whoosh_core::ByteRange;

use crate::handler::{Body, ReqCtx, json, origin, text};
use crate::scenario::Scenario;
use crate::stats::ScenarioStats;

#[derive(Debug, Serialize)]
struct FileInfo {
    name: String,
    size: u64,
    path: String,
    sha256_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_disposition: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScenarioInfo<'a> {
    name: &'a str,
    description: &'a str,
    files: Vec<FileInfo>,
    config: &'a Scenario,
}

#[derive(Debug, Serialize)]
struct Minted {
    scenario: String,
    file: String,
    path: String,
    url: String,
    expires: u64,
}

#[derive(Debug, Serialize)]
struct Digest {
    scenario: String,
    file: String,
    version: u64,
    start: u64,
    end: u64,
    sha256: String,
}

fn query<B>(req: &Request<B>) -> BTreeMap<String, String> {
    req.uri()
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.to_owned()),
            None => (p.to_owned(), String::new()),
        })
        .collect()
}

fn bad(msg: &str) -> Response<Body> {
    text(StatusCode::BAD_REQUEST, format!("{msg}\n"))
}

pub(crate) async fn handle<B>(req: Request<B>, rest: &str, ctx: &ReqCtx) -> Response<Body> {
    let shared = &ctx.shared;
    let q = query(&req);
    let scenario_param = || -> Result<_, Box<Response<Body>>> {
        let name = q
            .get("scenario")
            .ok_or_else(|| Box::new(bad("missing ?scenario=")))?;
        shared
            .scenario(name)
            .cloned()
            .ok_or_else(|| Box::new(text(StatusCode::NOT_FOUND, "unknown scenario\n")))
    };
    match (req.method(), rest) {
        (&Method::GET, "health") => text(StatusCode::OK, "ok\n"),
        (&Method::GET, "scenarios") => {
            let list: Vec<_> = shared
                .scenarios
                .iter()
                .map(|rt| ScenarioInfo {
                    name: &rt.sc.name,
                    description: &rt.sc.description,
                    files: rt
                        .file_order
                        .iter()
                        .map(|n| {
                            let f = &rt.files[n];
                            FileInfo {
                                name: n.clone(),
                                size: f.base.size,
                                path: rt.path(n),
                                sha256_path: format!("{}.sha256", rt.path(n)),
                                content_disposition: f
                                    .disposition
                                    .as_ref()
                                    .map(|d| String::from_utf8_lossy(d.as_bytes()).into_owned()),
                            }
                        })
                        .collect(),
                    config: &rt.sc,
                })
                .collect();
            json(StatusCode::OK, &list)
        }
        (&Method::GET, "stats") => {
            let all: BTreeMap<&str, ScenarioStats> = shared
                .scenarios
                .iter()
                .map(|rt| (rt.sc.name.as_str(), rt.stats.snapshot()))
                .collect();
            json(StatusCode::OK, &all)
        }
        (&Method::GET, r) if r.starts_with("stats/") => {
            match shared.scenario(&r["stats/".len()..]) {
                Some(rt) => json(StatusCode::OK, &rt.stats.snapshot()),
                None => text(StatusCode::NOT_FOUND, "unknown scenario\n"),
            }
        }
        (&Method::POST, "reset") => {
            if q.contains_key("scenario") {
                match scenario_param() {
                    Ok(rt) => rt.reset(),
                    Err(r) => return *r,
                }
            } else {
                for rt in &shared.scenarios {
                    rt.reset();
                }
            }
            text(StatusCode::OK, "reset\n")
        }
        (&Method::GET, "mint") => {
            let rt = match scenario_param() {
                Ok(rt) => rt,
                Err(r) => return *r,
            };
            let Some(file) = q.get("file").filter(|f| rt.file(f).is_some()) else {
                return bad("missing or unknown ?file=");
            };
            let ttl = match q.get("ttl").map(|t| t.parse::<u64>()) {
                None => None,
                Some(Ok(t)) => Some(Duration::from_secs(t)),
                Some(Err(_)) => return bad("bad ?ttl="),
            };
            let (path, expires) = rt.mint(file, ttl);
            json(
                StatusCode::OK,
                &Minted {
                    scenario: rt.sc.name.clone(),
                    file: file.clone(),
                    url: format!("{}{}", origin(&req, ctx), path),
                    path,
                    expires,
                },
            )
        }
        (&Method::GET, "login") => {
            let rt = match scenario_param() {
                Ok(rt) => rt,
                Err(r) => return *r,
            };
            let Some(cookie) = &rt.sc.access.cookie else {
                return bad("scenario needs no cookie");
            };
            let mut r = text(StatusCode::OK, "logged in\n");
            if let Ok(v) = HeaderValue::from_str(&format!("{cookie}; Path=/")) {
                r.headers_mut().insert(header::SET_COOKIE, v);
            }
            r
        }
        (&Method::GET, "digest") => {
            let rt = match scenario_param() {
                Ok(rt) => rt,
                Err(r) => return *r,
            };
            let Some((name, f)) = q
                .get("file")
                .and_then(|n| Some((n.clone(), rt.file(n)?.clone())))
            else {
                return bad("missing or unknown ?file=");
            };
            let num = |k: &str| q.get(k).map(|v| v.parse::<u64>());
            let (version, start, end) = match (num("version"), num("start"), num("end")) {
                (Some(Err(_)), _, _) | (_, Some(Err(_)), _) | (_, _, Some(Err(_))) => {
                    return bad("version/start/end must be integers");
                }
                (v, s, e) => (
                    v.and_then(Result::ok)
                        .unwrap_or_else(|| rt.current_version(&f)),
                    s.and_then(Result::ok).unwrap_or(0),
                    e.and_then(Result::ok)
                        .unwrap_or(f.base.size)
                        .min(f.base.size),
                ),
            };
            if start > end {
                return bad("start > end");
            }
            let vf = f.base.version(version);
            let shared = shared.clone();
            let sha256 = tokio::task::spawn_blocking(move || {
                if start == 0 && end == vf.size {
                    shared.file_digest(vf)
                } else {
                    vf.sha256_hex(ByteRange { start, end })
                }
            })
            .await
            .unwrap_or_default();
            json(
                StatusCode::OK,
                &Digest {
                    scenario: rt.sc.name.clone(),
                    file: name,
                    version,
                    start,
                    end,
                    sha256,
                },
            )
        }
        (&Method::GET, "cert.pem") => match &shared.cert_pem {
            Some(pem) => {
                let mut r = text(StatusCode::OK, pem.clone());
                r.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-pem-file"),
                );
                r
            }
            None => text(StatusCode::NOT_FOUND, "TLS is disabled\n"),
        },
        _ => text(StatusCode::NOT_FOUND, "unknown control endpoint\n"),
    }
}

/// `GET /`: a human-readable map of the server.
pub(crate) fn index(ctx: &ReqCtx) -> Response<Body> {
    let mut s = String::from(
        "whoosh-labserver\n\nControl API:\n  GET  /_lab/health\n  GET  /_lab/scenarios\n  GET  /_lab/stats[/<scenario>]\n  POST /_lab/reset[?scenario=]\n  GET  /_lab/mint?scenario=&file=[&ttl=]\n  GET  /_lab/login?scenario=\n  GET  /_lab/digest?scenario=&file=[&start=&end=&version=]\n  GET  /_lab/cert.pem\n\nScenarios (GET /s/<scenario>/<file>, /s/<scenario>/<file>.sha256):\n",
    );
    for rt in &ctx.shared.scenarios {
        let _ = writeln!(s, "  {:<18} {}", rt.sc.name, rt.sc.description);
    }
    text(StatusCode::OK, s)
}
