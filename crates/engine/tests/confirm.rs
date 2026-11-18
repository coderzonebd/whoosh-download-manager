//! Downloads caught in a browser wait for the user (`download.confirm`),
//! and the active time a download took (`activeMs`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use wdm_engine::Engine;
use wdm_proto::service::Service;
use wdm_proto::{
    ConfirmParams, DownloadState, IdsParams, QueueConfig, QueueNameParams, RemoveParams,
};

const LONG: Duration = Duration::from_secs(600);

fn from_browser(url: &str) -> wdm_proto::AddParams {
    let mut p = add_params(url);
    p.from_browser = true;
    p
}

fn answer(id: &str, start: bool) -> ConfirmParams {
    ConfirmParams {
        id: id.into(),
        start,
        filename: None,
        directory: None,
        category: None,
        queue: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_browser_download_waits_learns_its_size_and_starts_when_confirmed() {
    let lab = Lab::new(config()).await;
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(content(2 * MIB, 1)).cfg(|c| c.filename = Some("server.zip".into())),
        ),
    );
    let r = lab.engine.add(from_browser(URL)).await.unwrap();
    assert!(r.awaiting_confirmation);
    assert!(!r.needs_app, "the daemon says that, not the engine");

    // The dialog gets the probe's size, resumability and name.
    let t0 = tokio::time::Instant::now();
    let d = loop {
        let d = detail(&lab.engine, &r.id).await;
        if d.summary.total_bytes.is_some() {
            break d;
        }
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(d.summary.state, DownloadState::Paused);
    assert!(d.summary.awaiting_confirmation);
    assert_eq!(d.summary.total_bytes, Some(2 * MIB));
    assert!(d.summary.resumable);
    assert_eq!(d.summary.filename, "server.zip");
    assert_eq!(d.summary.category.as_deref(), Some("compressed"));

    // Nothing is transferred while it waits.
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(src.served(), 0);
    let d = detail(&lab.engine, &r.id).await;
    assert_eq!(d.summary.state, DownloadState::Paused);

    let mut ok = answer(&r.id, true);
    ok.filename = Some("mine.zip".into());
    ok.category = Some("documents".into());
    lab.engine.confirm(ok).await.unwrap();
    let (d, _) = finish(&lab.engine, &r.id, LONG).await;
    assert!(!d.summary.awaiting_confirmation);
    assert_eq!(d.summary.filename, "mine.zip", "the user's name is kept");
    assert_eq!(d.summary.category.as_deref(), Some("documents"));
    assert!(lab.file("mine.zip").is_some());

    // Answered once is answered.
    let e = lab.engine.confirm(answer(&r.id, true)).await.unwrap_err();
    assert_eq!(e.code, wdm_proto::error_code::INVALID_STATE);
}

#[tokio::test(start_paused = true)]
async fn later_keeps_it_paused_or_waiting_with_a_stopped_queue_and_cancel_removes_it() {
    let lab = Lab::new(config()).await;
    for i in 0..3 {
        lab.serve(
            &format!("https://lab.test/w/{i}.bin"),
            Arc::new(FaultSource::new(content(MIB, 10 + i))),
        );
    }
    let later = lab
        .engine
        .add(from_browser("https://lab.test/w/0.bin"))
        .await
        .unwrap()
        .id;
    lab.engine.confirm(answer(&later, false)).await.unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let d = detail(&lab.engine, &later).await;
    assert_eq!(d.summary.state, DownloadState::Paused);
    assert!(!d.summary.awaiting_confirmation);
    assert_eq!(d.summary.downloaded_bytes, 0);
    // An ordinary paused download from here on.
    lab.engine
        .resume(IdsParams {
            ids: vec![later.clone()],
        })
        .await
        .unwrap();
    finish(&lab.engine, &later, LONG).await;

    // "Later" into a stopped named queue: it waits with that queue.
    lab.engine
        .create_queue(QueueConfig {
            name: "night".into(),
            max_active: None,
            schedule: None,
        })
        .await
        .unwrap();
    lab.engine
        .stop_queue(QueueNameParams {
            name: "night".into(),
        })
        .await
        .unwrap();
    let queued = lab
        .engine
        .add(from_browser("https://lab.test/w/1.bin"))
        .await
        .unwrap()
        .id;
    let mut a = answer(&queued, false);
    a.queue = Some("night".into());
    lab.engine.confirm(a).await.unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let d = detail(&lab.engine, &queued).await;
    assert_eq!(d.summary.state, DownloadState::Queued);
    assert_eq!(d.summary.queue.as_deref(), Some("night"));
    lab.engine
        .start_queue(QueueNameParams {
            name: "night".into(),
        })
        .await
        .unwrap();
    finish(&lab.engine, &queued, LONG).await;

    // Cancel is a plain remove.
    let cancel = lab
        .engine
        .add(from_browser("https://lab.test/w/2.bin"))
        .await
        .unwrap()
        .id;
    lab.engine
        .remove(RemoveParams {
            ids: vec![cancel.clone()],
            delete_files: true,
        })
        .await
        .unwrap();
    assert!(lab.engine.record(&cancel).is_none());
}

#[tokio::test(start_paused = true)]
async fn only_browser_downloads_wait_and_only_while_the_setting_is_on() {
    let lab = Lab::new(config()).await;
    lab.serve(URL, Arc::new(FaultSource::new(content(MIB, 3))));
    let r = lab.engine.add(add_params(URL)).await.unwrap();
    assert!(!r.awaiting_confirmation);
    finish(&lab.engine, &r.id, LONG).await;

    // Not started anyway: nothing to confirm.
    let mut p = from_browser(URL);
    p.start = false;
    let r = lab.engine.add(p).await.unwrap();
    assert!(!r.awaiting_confirmation);

    let mut s = lab.engine.settings().await.unwrap();
    assert!(s.confirm_browser_downloads, "on by default");
    s.confirm_browser_downloads = false;
    lab.engine.set_settings(s).await.unwrap();
    let r = lab.engine.add(from_browser(URL)).await.unwrap();
    assert!(!r.awaiting_confirmation);
    finish(&lab.engine, &r.id, LONG).await;
}

#[tokio::test(start_paused = true)]
async fn a_waiting_download_still_waits_after_a_restart_and_resume_answers_it() {
    let lab = Lab::new(config()).await;
    lab.serve(URL, Arc::new(FaultSource::new(content(MIB, 4))));
    let id = lab.engine.add(from_browser(URL)).await.unwrap().id;
    lab.engine.shutdown().await;

    let again = Engine::start(lab.deps(), config()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let d = detail(&again, &id).await;
    assert_eq!(d.summary.state, DownloadState::Paused);
    assert!(d.summary.awaiting_confirmation);
    // A client that knows nothing of confirmation just resumes it.
    again
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    let (d, _) = finish(&again, &id, LONG).await;
    assert!(!d.summary.awaiting_confirmation);
}

#[tokio::test(start_paused = true)]
async fn active_time_leaves_out_pauses_and_survives_a_restart() {
    let lab = Lab::new(config()).await;
    // 8 MiB at 1 MiB/s: about 8 s of transfer.
    lab.serve(
        URL,
        Arc::new(FaultSource::new(content(8 * MIB, 5)).cfg(|c| c.global_bps = Some(MIB))),
    );
    let id = lab.add(URL).await;
    assert_eq!(detail(&lab.engine, &id).await.summary.active_ms, Some(0));
    tokio::time::sleep(Duration::from_secs(3)).await;
    lab.engine
        .pause(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    let paused = detail(&lab.engine, &id).await.summary.active_ms.unwrap();
    assert!((2_500..=3_500).contains(&paused), "{paused}");
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(
        detail(&lab.engine, &id).await.summary.active_ms,
        Some(paused),
        "a pause does not count"
    );
    lab.engine.shutdown().await;

    let again = Engine::start(lab.deps(), config()).await.unwrap();
    assert_eq!(
        detail(&again, &id).await.summary.active_ms,
        Some(paused),
        "kept across a restart"
    );
    tokio::time::sleep(Duration::from_secs(60)).await;
    again
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    let (d, _) = finish(&again, &id, LONG).await;
    let total = d.summary.active_ms.unwrap();
    // The rest of the transfer (and a re-probe) is added; the 3 minutes
    // paused, stopped and waiting are not.
    assert!((paused + 4_000..=20_000).contains(&total), "{total}");
}
