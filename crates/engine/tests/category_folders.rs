//! Sorting downloads into category folders (`Settings::organize_by_category`):
//! `Programs`, `Compressed`, `Images`, `Video`, `Music` and `Documents` inside
//! the default directory, decided by the final file name, never overriding a
//! folder the user picked.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use wdm_proto::service::Service;
use wdm_proto::{ConfirmParams, DownloadState};

const LONG: Duration = Duration::from_secs(600);

fn sorted() -> wdm_engine::EngineConfig {
    let mut c = config();
    c.default_settings.organize_by_category = Some(true);
    c
}

fn folder(name: &str) -> String {
    Path::new(DIR).join(name).to_string_lossy().into_owned()
}

#[tokio::test(start_paused = true)]
async fn files_go_to_their_category_folder() {
    let lab = Lab::new(sorted()).await;
    for (url, dir, name) in [
        (
            "https://lab.test/f/setup.exe",
            Some("Programs"),
            "setup.exe",
        ),
        (
            "https://lab.test/f/backup.zip",
            Some("Compressed"),
            "backup.zip",
        ),
        ("https://lab.test/f/photo.jpg", Some("Images"), "photo.jpg"),
        ("https://lab.test/f/clip.mp4", Some("Video"), "clip.mp4"),
        ("https://lab.test/f/song.mp3", Some("Music"), "song.mp3"),
        (
            "https://lab.test/f/paper.pdf",
            Some("Documents"),
            "paper.pdf",
        ),
        // "Other" stays in the download directory itself.
        ("https://lab.test/f/data.bin", None, "data.bin"),
    ] {
        let data = content(KIB, 3);
        lab.serve(url, Arc::new(FaultSource::new(data.clone())));
        let id = lab.add(url).await;
        let (d, _) = finish(&lab.engine, &id, LONG).await;
        let want = dir.map_or_else(|| DIR.to_owned(), folder);
        assert_eq!(d.summary.directory, want, "{name}");
        let file = lab.storage.finished(&Path::new(&want).join(name));
        assert_same(&file.expect(name), &data);
    }
}

#[tokio::test(start_paused = true)]
async fn the_servers_name_decides_the_folder() {
    // Added as `download.php` (other), the server names it a video: it is
    // written in Video from the first byte.
    let lab = Lab::new(sorted()).await;
    let u = "https://lab.test/download.php?file=x";
    let data = content(KIB, 5);
    lab.serve(
        u,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.filename = Some("Episode 5.mp4".into()))),
    );
    let id = lab.add(u).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.filename, "Episode 5.mp4");
    assert_eq!(d.summary.directory, folder("Video"));
    let file = lab
        .storage
        .finished(&Path::new(&folder("Video")).join("Episode 5.mp4"));
    assert_same(&file.expect("in Video"), &data);
}

#[tokio::test(start_paused = true)]
async fn a_folder_or_category_the_user_chose_is_kept() {
    let lab = Lab::new(sorted()).await;
    let u = "https://lab.test/f/setup.exe";
    lab.serve(u, Arc::new(FaultSource::new(content(KIB, 1))));

    let mine = folder("mine");
    let mut p = add_params(u);
    p.directory = Some(mine.clone());
    let id = lab.engine.add(p).await.unwrap().id;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.directory, mine);

    // A user category has no folder: the download directory.
    let mut p = add_params(u);
    p.category = Some("Work".into());
    let id = lab.engine.add(p).await.unwrap().id;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.directory, DIR);

    // A built-in category given by the user picks that category's folder.
    let mut p = add_params(u);
    p.category = Some("documents".into());
    let id = lab.engine.add(p).await.unwrap().id;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.directory, folder("Documents"));
}

#[tokio::test(start_paused = true)]
async fn nothing_is_sorted_until_the_user_chooses() {
    // Absent (the default) and off both keep everything in the directory.
    for organize in [None, Some(false)] {
        let mut c = config();
        c.default_settings.organize_by_category = organize;
        let lab = Lab::new(c).await;
        let u = "https://lab.test/f/setup.exe";
        lab.serve(u, Arc::new(FaultSource::new(content(KIB, 1))));
        let id = lab.add(u).await;
        let (d, _) = finish(&lab.engine, &id, LONG).await;
        assert_eq!(d.summary.directory, DIR, "{organize:?}");
        assert!(lab.file("setup.exe").is_some());
    }
}

#[tokio::test(start_paused = true)]
async fn a_browser_download_shows_and_keeps_its_category_folder() {
    let lab = Lab::new(sorted()).await;
    let u = "https://lab.test/get?id=1";
    lab.serve(
        u,
        Arc::new(FaultSource::new(content(KIB, 2)).cfg(|c| c.filename = Some("tool.msi".into()))),
    );
    let mut p = add_params(u);
    p.from_browser = true;
    let id = lab.engine.add(p).await.unwrap().id;
    // The confirmation dialog shows the folder for the server's name.
    let t0 = tokio::time::Instant::now();
    let d = loop {
        let d = detail(&lab.engine, &id).await;
        if d.summary.filename == "tool.msi" {
            break d;
        }
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(d.summary.state, DownloadState::Paused);
    assert_eq!(d.summary.directory, folder("Programs"));

    // Renamed to a .zip in the dialog, without touching the folder: Compressed.
    lab.engine
        .confirm(ConfirmParams {
            id: id.clone(),
            start: true,
            filename: Some("tool.zip".into()),
            directory: None,
            category: None,
            queue: None,
        })
        .await
        .unwrap();
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.directory, folder("Compressed"));
    assert!(
        lab.storage
            .finished(&Path::new(&folder("Compressed")).join("tool.zip"))
            .is_some()
    );
}
