//! `whoosh get URL`: add, wait, report. Meant to be driven like curl by
//! scripts and the benchmark harness: the saved path is the only thing on
//! stdout, the progress bar (terminals only) goes to stderr, and the exit
//! code says what happened.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use serde_json::Value;
use whoosh_proto::{
    AddResult, DownloadDetail, DownloadState, DownloadSummary, IdParams, IdsParams, ProgressItem,
    StateEvent, event, method,
};

use crate::client::{Client, Event};
use crate::commands::{Ctx, add_params, request_context};
use crate::daemon::Connected;
use crate::error::{CliError, Result, exit};
use crate::{DownloadOpts, format};

/// How often to ask the daemon directly, in case an event went missing.
const POLL: Duration = Duration::from_secs(2);

pub async fn get(ctx: &Ctx, url: String, opts: &DownloadOpts) -> Result<u8> {
    let params = add_params(url, opts, request_context(opts)?, true)?;
    let Connected { client, .. } = ctx.connect().await?;
    let started = Instant::now();
    let id = client
        .call::<_, AddResult>(method::DOWNLOAD_ADD, &params)
        .await?
        .id;
    let mut bar = Bar::new(!ctx.json && std::io::stderr().is_terminal());
    let mut summary = fetch(&client, &id).await?.ok_or_else(|| removed(&id))?;

    let ctrl_c = crate::interrupt::interrupted();
    tokio::pin!(ctrl_c);
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + POLL, POLL);
    let outcome = loop {
        bar.draw(&summary);
        match summary.state {
            DownloadState::Completed => break Ok(()),
            DownloadState::Failed | DownloadState::NeedsRefresh => break Err(failure(&summary)),
            _ => {}
        }
        tokio::select! {
            ev = client.next_event() => match ev {
                Some(ev) => {
                    if apply(&mut summary, ev) == Applied::Removed {
                        break Err(removed(&id));
                    }
                }
                None => {
                    bar.clear();
                    return Err(CliError::DaemonUnavailable(format!(
                        "lost the connection to the daemon; the download may continue there \
                         (check with `whoosh info {id}`)"
                    )));
                }
            },
            _ = poll.tick() => match fetch(&client, &id).await? {
                Some(now) => summary = now,
                None => break Err(removed(&id)),
            },
            _ = &mut ctrl_c => {
                bar.clear();
                let pause = client.call_value(
                    method::DOWNLOAD_PAUSE,
                    serde_json::to_value(IdsParams { ids: vec![id.clone()] }).unwrap_or_default(),
                );
                let paused = matches!(tokio::time::timeout(Duration::from_secs(2), pause).await, Ok(Ok(_)));
                if paused {
                    eprintln!("whoosh: interrupted; {id} is paused (continue with `whoosh resume {id}`)");
                } else {
                    eprintln!("whoosh: interrupted; {id} may still be downloading (see `whoosh info {id}`)");
                }
                return Err(CliError::Interrupted);
            }
        }
    };
    bar.clear();
    outcome?;

    // The final name can differ from the one at add time (server-provided
    // names, de-duplication), so ask once more.
    let done = fetch(&client, &id).await?.unwrap_or(summary);
    let path = format::path_of(&done);
    let elapsed = started.elapsed();
    if ctx.json {
        let mut v = serde_json::to_value(&done).unwrap_or(Value::Null);
        v["path"] = Value::String(path);
        v["elapsedMs"] = Value::from(elapsed.as_millis() as u64);
        ctx.print_json(&v);
    } else {
        println!("{path}");
        if bar.enabled {
            let secs = elapsed.as_secs_f64().max(0.001);
            eprintln!(
                "whoosh: {} in {:.1}s ({})",
                format::bytes(done.downloaded_bytes),
                secs,
                format::speed((done.downloaded_bytes as f64 / secs) as u64)
            );
        }
    }
    client.close().await;
    Ok(exit::OK)
}

async fn fetch(client: &Client, id: &str) -> Result<Option<DownloadSummary>> {
    match client
        .call::<_, DownloadDetail>(method::DOWNLOAD_GET, IdParams { id: id.to_owned() })
        .await
    {
        Ok(d) => Ok(Some(d.summary)),
        Err(whoosh_client::ClientError::Rpc { code, .. })
            if code == i64::from(whoosh_proto::error_code::NOT_FOUND) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

fn removed(id: &str) -> CliError {
    CliError::Download {
        exit: exit::REMOVED,
        message: format!("{id} was removed before it finished"),
    }
}

fn failure(s: &DownloadSummary) -> CliError {
    let (code, message) = s
        .error
        .as_ref()
        .map(|e| (e.code.as_str(), e.message.as_str()))
        .unwrap_or(("", "no reason given"));
    let exit = match (s.state, code) {
        (DownloadState::NeedsRefresh, _) | (_, "link_expired") => exit::LINK_EXPIRED,
        (_, "checksum_mismatch") => exit::CHECKSUM_MISMATCH,
        _ => exit::DOWNLOAD_FAILED,
    };
    let what = if exit == exit::LINK_EXPIRED {
        "the link expired"
    } else {
        "download failed"
    };
    let code = if code.is_empty() {
        String::new()
    } else {
        format!(" [{code}]")
    };
    CliError::Download {
        exit,
        message: format!("{}: {what}: {message}{code}", s.filename),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Applied {
    Ignored,
    Updated,
    Removed,
}

/// Folds an event about this download into `s`.
fn apply(s: &mut DownloadSummary, ev: Event) -> Applied {
    match ev.method.as_str() {
        event::DOWNLOAD_PROGRESS => {
            let items: Vec<ProgressItem> = ev
                .params
                .get("items")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            match items.into_iter().find(|i| i.id == s.id) {
                Some(item) => {
                    s.downloaded_bytes = item.downloaded_bytes;
                    if item.total_bytes.is_some() {
                        s.total_bytes = item.total_bytes;
                    }
                    s.speed_bps = item.speed_bps;
                    s.eta_secs = item.eta_secs;
                    s.connections = item.connections;
                    Applied::Updated
                }
                None => Applied::Ignored,
            }
        }
        event::DOWNLOAD_STATE => match serde_json::from_value::<StateEvent>(ev.params) {
            Ok(st) if st.id == s.id => {
                s.state = st.state;
                s.error = st.error;
                Applied::Updated
            }
            _ => Applied::Ignored,
        },
        // `updated` carries the whole summary again (e.g. the server's file
        // name once the first response arrived).
        event::DOWNLOAD_ADDED | event::DOWNLOAD_UPDATED => {
            match serde_json::from_value::<DownloadSummary>(ev.params) {
                Ok(added) if added.id == s.id => {
                    *s = added;
                    Applied::Updated
                }
                _ => Applied::Ignored,
            }
        }
        event::DOWNLOAD_REMOVED => {
            let gone = ev
                .params
                .get("ids")
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.iter().any(|i| i == s.id.as_str()));
            if gone {
                Applied::Removed
            } else {
                Applied::Ignored
            }
        }
        _ => Applied::Ignored,
    }
}

/// A one-line progress bar on stderr, redrawn at most 10 times a second.
struct Bar {
    enabled: bool,
    last: Option<Instant>,
    /// The line on screen, if any.
    shown: Option<String>,
}

impl Bar {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last: None,
            shown: None,
        }
    }

    fn draw(&mut self, s: &DownloadSummary) {
        if !self.enabled {
            return;
        }
        if self
            .last
            .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        self.last = Some(Instant::now());
        let width = format::terminal_width();
        let status = match s.state {
            DownloadState::Downloading => {
                let eta = s
                    .eta_secs
                    .map(|e| format!("  ETA {}", format::duration(e)))
                    .unwrap_or_default();
                format!("{}{eta}", format::speed(s.speed_bps))
            }
            other => format::state(other).to_owned(),
        };
        let amount = match s.total_bytes {
            Some(t) => format!(
                "{:>4}  {}/{}",
                format::percent(s.downloaded_bytes, Some(t)),
                format::bytes(s.downloaded_bytes),
                format::bytes(t)
            ),
            None => format::bytes(s.downloaded_bytes),
        };
        let gauge = match s.total_bytes {
            Some(t) if t > 0 => {
                const W: usize = 20;
                let filled = ((s.downloaded_bytes as f64 / t as f64) * W as f64).round() as usize;
                format!(
                    "[{}{}] ",
                    "#".repeat(filled.min(W)),
                    "-".repeat(W - filled.min(W))
                )
            }
            _ => String::new(),
        };
        let tail = format!("{gauge}{amount}  {status}");
        let name_w = width.saturating_sub(tail.chars().count() + 3).max(8);
        let line = format!("{}  {tail}", format::fit(&s.filename, name_w));
        if self.shown.as_deref() == Some(line.as_str()) {
            return;
        }
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[2K{line}");
        let _ = err.flush();
        self.shown = Some(line);
    }

    fn clear(&mut self) {
        if self.enabled && self.shown.take().is_some() {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use whoosh_proto::ErrorInfo;

    fn summary() -> DownloadSummary {
        DownloadSummary {
            id: "dl_1".into(),
            url: "https://e/x".into(),
            filename: "x".into(),
            directory: "/d".into(),
            state: DownloadState::Queued,
            total_bytes: None,
            downloaded_bytes: 0,
            speed_bps: 0,
            eta_secs: None,
            connections: 0,
            resumable: true,
            category: None,
            queue: None,
            added_at: 0,
            completed_at: None,
            error: None,
            media: None,
        }
    }

    fn ev(method: &str, params: Value) -> Event {
        Event {
            method: method.into(),
            params,
        }
    }

    #[test]
    fn folds_events_for_this_download_only() {
        let mut s = summary();
        let progress = json!({"items": [
            {"id": "dl_2", "downloadedBytes": 9, "speedBps": 1, "connections": 1},
            {"id": "dl_1", "downloadedBytes": 5, "totalBytes": 10, "speedBps": 2, "etaSecs": 3, "connections": 4},
        ]});
        assert_eq!(
            apply(&mut s, ev(event::DOWNLOAD_PROGRESS, progress)),
            Applied::Updated
        );
        assert_eq!(
            (s.downloaded_bytes, s.total_bytes, s.connections),
            (5, Some(10), 4)
        );
        let other = json!({"id": "dl_2", "state": "failed"});
        assert_eq!(
            apply(&mut s, ev(event::DOWNLOAD_STATE, other)),
            Applied::Ignored
        );
        let failed = json!({"id": "dl_1", "state": "failed", "error": {"code": "checksum_mismatch", "message": "bad"}});
        assert_eq!(
            apply(&mut s, ev(event::DOWNLOAD_STATE, failed)),
            Applied::Updated
        );
        assert_eq!(failure(&s).exit_code(), exit::CHECKSUM_MISMATCH);
        assert_eq!(
            apply(
                &mut s,
                ev(event::DOWNLOAD_REMOVED, json!({"ids": ["dl_1"]}))
            ),
            Applied::Removed
        );
    }

    #[test]
    fn failure_exit_codes() {
        let mut s = summary();
        s.state = DownloadState::NeedsRefresh;
        assert_eq!(failure(&s).exit_code(), exit::LINK_EXPIRED);
        s.state = DownloadState::Failed;
        s.error = Some(ErrorInfo {
            code: "remote_changed".into(),
            message: "the file changed on the server".into(),
        });
        let e = failure(&s);
        assert_eq!(e.exit_code(), exit::DOWNLOAD_FAILED);
        assert!(e.to_string().contains("remote_changed"), "{e}");
        assert_eq!(removed("dl_1").exit_code(), exit::REMOVED);
    }
}
