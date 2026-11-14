//! `whoosh watch`: a live table on a terminal; one JSON event per line
//! otherwise (or with `--json`).

use std::io::{IsTerminal, Write};
use std::time::Duration;

use serde_json::{Value, json};
use whoosh_proto::{DownloadSummary, ListParams, ProgressItem, StateEvent, Stats, event, method};

use crate::client::{Client, Event};
use crate::commands::Ctx;
use crate::daemon::Connected;
use crate::error::{CliError, Result, exit};
use crate::format;

pub async fn watch(ctx: &Ctx) -> Result<u8> {
    let Connected { client, .. } = ctx.connect().await?;
    if ctx.json || !std::io::stdout().is_terminal() {
        return stream(&client).await;
    }
    let mut rows: Vec<DownloadSummary> = client
        .call(method::DOWNLOAD_LIST, ListParams::default())
        .await?;
    let mut stats: Stats = client.call(method::STATS_GET, Value::Null).await?;

    let ctrl_c = crate::interrupt::interrupted();
    tokio::pin!(ctrl_c);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut dirty = true;
    let screen = Screen::enter();
    let result = loop {
        tokio::select! {
            ev = client.next_event() => match ev {
                Some(ev) => {
                    fold(&mut rows, &mut stats, ev);
                    dirty = true;
                }
                None => break Err(CliError::DaemonUnavailable("lost the connection to the daemon".into())),
            },
            _ = tick.tick() => {
                if dirty {
                    screen.render(&rows, &stats);
                    dirty = false;
                }
            }
            _ = &mut ctrl_c => break Ok(exit::OK),
        }
    };
    drop(screen);
    result
}

/// Non-interactive: every notification as one JSON line.
async fn stream(client: &Client) -> Result<u8> {
    let ctrl_c = crate::interrupt::interrupted();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            ev = client.next_event() => match ev {
                Some(Event { method, params }) => {
                    let mut out = std::io::stdout().lock();
                    let line = json!({ "method": method, "params": params });
                    if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                        // The reader went away (`whoosh watch | head`).
                        return Ok(exit::OK);
                    }
                }
                None => return Err(CliError::DaemonUnavailable("lost the connection to the daemon".into())),
            },
            _ = &mut ctrl_c => return Ok(exit::OK),
        }
    }
}

fn fold(rows: &mut Vec<DownloadSummary>, stats: &mut Stats, ev: Event) {
    match ev.method.as_str() {
        event::DOWNLOAD_ADDED => {
            if let Ok(s) = serde_json::from_value::<DownloadSummary>(ev.params)
                && !rows.iter().any(|r| r.id == s.id)
            {
                rows.insert(0, s);
            }
        }
        event::DOWNLOAD_UPDATED => {
            // The name, size or category changed after `added` (the server's
            // file name arrives with the first response): replace the row.
            if let Ok(s) = serde_json::from_value::<DownloadSummary>(ev.params) {
                match rows.iter_mut().find(|r| r.id == s.id) {
                    Some(r) => *r = s,
                    None => rows.insert(0, s),
                }
            }
        }
        event::DOWNLOAD_STATE => {
            if let Ok(st) = serde_json::from_value::<StateEvent>(ev.params)
                && let Some(r) = rows.iter_mut().find(|r| r.id == st.id)
            {
                r.state = st.state;
                r.error = st.error;
                if st.state != whoosh_proto::DownloadState::Downloading {
                    r.speed_bps = 0;
                    r.eta_secs = None;
                }
            }
        }
        event::DOWNLOAD_PROGRESS => {
            let items: Vec<ProgressItem> = ev
                .params
                .get("items")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            for item in items {
                if let Some(r) = rows.iter_mut().find(|r| r.id == item.id) {
                    r.downloaded_bytes = item.downloaded_bytes;
                    if item.total_bytes.is_some() {
                        r.total_bytes = item.total_bytes;
                    }
                    r.speed_bps = item.speed_bps;
                    r.eta_secs = item.eta_secs;
                    r.connections = item.connections;
                }
            }
        }
        event::DOWNLOAD_REMOVED => {
            let ids: Vec<String> = ev
                .params
                .get("ids")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            rows.retain(|r| !ids.contains(&r.id));
        }
        event::STATS => {
            if let Ok(s) = serde_json::from_value(ev.params) {
                *stats = s;
            }
        }
        _ => {}
    }
}

/// The terminal's alternate screen, restored on drop.
struct Screen;

impl Screen {
    fn enter() -> Self {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\x1b[?1049h\x1b[?25l");
        let _ = out.flush();
        Self
    }

    fn render(&self, rows: &[DownloadSummary], stats: &Stats) {
        let height = format::terminal_height().saturating_sub(4).max(1);
        let mut text = format!(
            "whoosh  {} active, {} queued, {}   (Ctrl-C to quit)\n\n",
            stats.active,
            stats.queued,
            format::speed(stats.download_bps)
        );
        if rows.is_empty() {
            text.push_str("no downloads\n");
        } else {
            text.push_str(&format::table(
                &rows[..rows.len().min(height)],
                format::terminal_width(),
            ));
            if rows.len() > height {
                text.push_str(&format!("… and {} more\n", rows.len() - height));
            }
        }
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\x1b[H\x1b[2J{text}");
        let _ = out.flush();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> DownloadSummary {
        serde_json::from_value(json!({
            "id": id, "url": "u", "filename": "f", "directory": "/d", "state": "queued",
            "downloadedBytes": 0, "speedBps": 0, "connections": 0, "resumable": true, "addedAt": 0
        }))
        .unwrap()
    }

    #[test]
    fn folds_events_into_the_table() {
        let mut rows = vec![row("a")];
        let mut stats = Stats::default();
        let ev = |m: &str, p: Value| Event {
            method: m.into(),
            params: p,
        };
        fold(
            &mut rows,
            &mut stats,
            ev(
                event::DOWNLOAD_ADDED,
                serde_json::to_value(row("b")).unwrap(),
            ),
        );
        assert_eq!(rows[0].id, "b");
        fold(
            &mut rows,
            &mut stats,
            ev(
                event::DOWNLOAD_STATE,
                json!({"id": "a", "state": "downloading"}),
            ),
        );
        fold(
            &mut rows,
            &mut stats,
            ev(
                event::DOWNLOAD_PROGRESS,
                json!({"items": [{"id": "a", "downloadedBytes": 7, "speedBps": 3, "connections": 2}]}),
            ),
        );
        assert_eq!(rows[1].downloaded_bytes, 7);
        fold(
            &mut rows,
            &mut stats,
            ev(
                event::STATS,
                json!({"downloadBps": 3, "uploadBps": 0, "active": 1, "queued": 0}),
            ),
        );
        assert_eq!(stats.active, 1);
        fold(
            &mut rows,
            &mut stats,
            ev(event::DOWNLOAD_REMOVED, json!({"ids": ["b"]})),
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn an_updated_event_replaces_the_add_time_name() {
        // Added under the URL's name; the server's name arrives later.
        let mut rows = vec![row("a")];
        let mut stats = Stats::default();
        let mut renamed = row("a");
        renamed.filename = "Episode 5.mp4".into();
        renamed.total_bytes = Some(42);
        fold(
            &mut rows,
            &mut stats,
            Event {
                method: event::DOWNLOAD_UPDATED.into(),
                params: serde_json::to_value(&renamed).unwrap(),
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].filename, "Episode 5.mp4");
        assert_eq!(rows[0].total_bytes, Some(42));
    }
}
