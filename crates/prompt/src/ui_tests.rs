// SPDX-License-Identifier: GPL-3.0-or-later
//! The prompt window driven like the daemon and the user would: daemon
//! messages go in, buttons are pressed, and the calls that come out are
//! checked. Each screen is drawn by Slint's software renderer, with no
//! display, and must not come out blank; with `WDM_PROMPT_SHOTS=<dir>`
//! the drawings are saved there as PNG files to look at.
//!
//! One test function: a Slint platform is installed once per process.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{PhysicalSize, Rgb8Pixel};
use tokio::sync::mpsc;
use wdm_proto::{event, method};

use crate::daemon::{Cmd, Msg, Tag};
use crate::{PromptWindow, ui};

const W: u32 = 520;
const H: u32 = 460;

thread_local! {
    static NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        NOW.with(Cell::get)
    }
}

struct Harness {
    window: Rc<MinimalSoftwareWindow>,
    ui: PromptWindow,
    cmds: mpsc::UnboundedReceiver<Cmd>,
    dir: PathBuf,
    shots: Option<PathBuf>,
}

impl Harness {
    fn msg(&self, msg: Msg) {
        ui::with(|c| c.on_msg(msg));
    }

    fn event(&self, method: &str, params: Value) {
        self.msg(Msg::Event(method.into(), params));
    }

    /// Lets time pass, so timers (the link check, closing) run.
    fn wait(&self, d: Duration) {
        NOW.with(|n| n.set(n.get() + d));
        slint::platform::update_timers_and_animations();
    }

    /// The calls sent since the last look.
    fn calls(&mut self) -> Vec<(String, Value, Option<Tag>)> {
        let mut out = Vec::new();
        while let Ok(cmd) = self.cmds.try_recv() {
            match cmd {
                Cmd::Call {
                    method,
                    params,
                    tag,
                } => out.push((method.to_owned(), params, Some(tag))),
                Cmd::AddPlaylist(list) => out.push(("playlist".into(), json!(list.len()), None)),
            }
        }
        out
    }

    fn press(&self, id: &str) {
        self.ui.invoke_button(id.into());
    }

    /// Draws the window; it must not be blank. Saved as `<name>.png` when asked.
    fn shot(&self, name: &str) {
        let mut buffer = vec![Rgb8Pixel::default(); (W * H) as usize];
        self.window.request_redraw();
        self.wait(Duration::from_millis(16));
        self.window.draw_if_needed(|r| {
            r.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            r.render(&mut buffer, W as usize);
        });
        let colours: std::collections::HashSet<(u8, u8, u8)> =
            buffer.iter().step_by(37).map(|p| (p.r, p.g, p.b)).collect();
        assert!(
            colours.len() > 8,
            "{name}: the window is blank ({} colours)",
            colours.len()
        );
        if let Some(dir) = &self.shots {
            let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), W, H);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let bytes: Vec<u8> = buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            enc.write_header()
                .unwrap()
                .write_image_data(&bytes)
                .unwrap();
        }
    }

    fn settings(&self, language: &str, theme: &str) {
        self.msg(Msg::Settings(
            serde_json::from_value(json!({
                "defaultDirectory": "/home/u/Downloads", "maxActiveDownloads": 3,
                "maxConnectionsPerDownload": 16, "language": language, "theme": theme,
                "confirmBrowserDownloads": true,
            }))
            .unwrap(),
        ));
    }

    fn followed(&self) -> Vec<String> {
        wdm_tray::followed::read(&self.dir)
            .into_iter()
            .map(|f| f.id)
            .collect()
    }
}

fn dl(id: &str, name: &str, state: &str, waits: bool) -> Value {
    json!({
        "id": id, "url": format!("https://cdn.example.org/files/{name}"), "filename": name,
        "directory": "/home/u/Downloads", "state": state, "totalBytes": 734_003_200u64,
        "downloadedBytes": if state == "downloading" { 321_000_000u64 } else { 0 },
        "speedBps": if state == "downloading" { 4_404_019u64 } else { 0 }, "etaSecs": 94,
        "connections": if state == "downloading" { 8 } else { 0 }, "resumable": true,
        "addedAt": 1, "awaitingConfirmation": waits, "category": "programs",
    })
}

fn summary(v: Value) -> wdm_proto::DownloadSummary {
    serde_json::from_value(v).unwrap()
}

fn setup() -> Harness {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).ok();
    window.set_size(PhysicalSize::new(W, H));
    let dir = std::env::temp_dir().join(format!("wdm-prompt-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lock = wdm_tray::locks::take(&dir, wdm_tray::locks::PROMPT).unwrap();
    let ui = PromptWindow::new().unwrap();
    let (tx, cmds) = mpsc::unbounded_channel();
    ui::Controller::install(&ui, dir.clone(), tx, lock);
    let shots = std::env::var_os("WDM_PROMPT_SHOTS").map(PathBuf::from);
    if let Some(d) = &shots {
        std::fs::create_dir_all(d).unwrap();
    }
    Harness {
        window,
        ui,
        cmds,
        dir,
        shots,
    }
}

#[test]
fn the_prompt_window_from_start_to_end() {
    let mut h = setup();
    h.settings("en", "light");
    h.msg(Msg::Queues(None));
    h.msg(Msg::Media(true));

    // ---------------------------------------------------------------- a browser download waits
    h.msg(Msg::List(vec![
        summary(dl("a", "ubuntu-26.10-desktop-amd64.iso", "paused", true)),
        summary(dl("b", "second.zip", "paused", true)),
    ]));
    assert_eq!(h.ui.get_screen(), 1);
    assert_eq!(h.ui.get_name(), "ubuntu-26.10-desktop-amd64.iso");
    assert_eq!(h.ui.get_position_text(), "1 of 2");
    h.shot("confirm");

    // An edited name travels; the rest stays as the daemon has it.
    h.ui.set_name("ubuntu.iso".into());
    h.ui.invoke_name_edited("ubuntu.iso".into());
    h.press("start");
    let calls = h.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, method::DOWNLOAD_CONFIRM);
    assert_eq!(
        calls[0].1,
        json!({"id": "a", "start": true, "filename": "ubuntu.iso"})
    );
    // Pressed twice while waiting: still one call.
    h.press("start");
    assert!(h.calls().is_empty());
    h.msg(Msg::Reply(
        Tag::Confirm {
            id: "a".into(),
            start: true,
        },
        Ok(Value::Null),
    ));
    assert_eq!(h.followed(), vec!["a".to_owned()]);

    // The second one: "Download later" by Escape (never loses it).
    assert_eq!(h.ui.get_name(), "second.zip");
    assert_eq!(h.ui.get_position_text(), "");
    h.ui.invoke_escape();
    let calls = h.calls();
    assert_eq!(calls[0].1, json!({"id": "b", "start": false}));
    h.msg(Msg::Reply(
        Tag::Confirm {
            id: "b".into(),
            start: false,
        },
        Ok(Value::Null),
    ));

    // ---------------------------------------------------------------- the started one's progress
    h.event(
        event::DOWNLOAD_UPDATED,
        dl("a", "ubuntu.iso", "downloading", false),
    );
    assert_eq!(h.ui.get_screen(), 3);
    assert!(h.ui.get_f_running());
    assert_eq!(h.ui.get_f_progress_text(), "43.7% · 306 MB of 700 MB");
    h.shot("progress");

    // Cancel takes two presses; the first only arms it.
    h.press("cancel");
    assert!(h.calls().is_empty());
    h.wait(Duration::from_secs(5));
    h.press("cancel");
    assert!(h.calls().is_empty(), "disarmed after a few seconds");
    h.press("pause");
    assert_eq!(h.calls()[0].1, json!({"ids": ["a"]}));

    // It ends while hidden: "Download complete" comes back.
    h.press("hide");
    assert_eq!(h.ui.get_screen(), 0);
    h.event(
        event::DOWNLOAD_STATE,
        json!({"id": "a", "state": "completed"}),
    );
    assert_eq!(h.ui.get_screen(), 3);
    assert!(!h.ui.get_f_running());
    assert_eq!(h.ui.get_heading(), "Download complete");
    h.shot("complete");
    h.press("close");
    assert!(h.followed().is_empty());

    // ---------------------------------------------------------------- a page the extension offered
    h.msg(Msg::Offer(
        serde_json::from_value(
            json!({"id": "of_1", "url": "https://video.example/watch?v=1", "offeredAt": 2}),
        )
        .unwrap(),
    ));
    assert_eq!(h.ui.get_screen(), 2);
    h.wait(Duration::from_millis(1));
    let calls = h.calls();
    assert_eq!(calls[0].0, method::DOWNLOAD_PROBE);
    let seq = match calls[0].2 {
        Some(Tag::Probe(seq)) => seq,
        ref other => panic!("{other:?}"),
    };
    assert_eq!(h.ui.get_url_status(), 1, "checking");
    h.msg(Msg::Reply(
        Tag::Probe(seq),
        Ok(json!({"resumable": false, "filename": "watch.html", "contentType": "text/html", "finalUrl": "https://video.example/watch?v=1"})),
    ));
    // A web page: is there video on it?
    let calls = h.calls();
    assert_eq!(calls[0].0, method::MEDIA_PROBE);
    let mseq = match calls[0].2 {
        Some(Tag::MediaProbe(seq)) => seq,
        ref other => panic!("{other:?}"),
    };
    h.shot("add-looking");
    let fmt = |id: &str, kind: &str, height: Option<u32>, size: u64| json!({"id": id, "kind": kind, "ext": "mp4", "height": height, "sizeBytes": size, "direct": true});
    h.msg(Msg::Reply(
        Tag::MediaProbe(mseq),
        Ok(json!({
            "url": "https://video.example/watch?v=1", "title": "A walk through Dhaka at night",
            "extractor": "generic", "durationSecs": 754, "uploader": "Coder Zone BD",
            "formats": [fmt("a", "audioOnly", None, 9_000_000), fmt("v720", "videoOnly", Some(720), 80_000_000), fmt("v1080", "videoOnly", Some(1080), 160_000_000)],
            "subtitles": [{"lang": "bn", "name": "বাংলা"}, {"lang": "en", "name": "English", "automatic": true}],
        })),
    ));
    assert!(h.ui.get_picking());
    assert_eq!(h.ui.get_stem(), "A walk through Dhaka at night");
    assert_eq!(h.ui.get_estimate_text(), "About 161 MB");
    h.shot("add-video");
    // Audio only, as MP3, with Bangla subtitles... then the 720p video after all.
    h.ui.invoke_mode_picked(true);
    h.ui.invoke_audio_format_picked(1);
    h.ui.invoke_sub_toggled(0, true);
    h.ui.invoke_mode_picked(false);
    h.ui.invoke_quality_picked(2);
    assert_eq!(h.ui.get_estimate_text(), "About 84.9 MB");
    h.press("submit");
    let calls = h.calls();
    assert_eq!(calls[0].0, method::MEDIA_ADD);
    assert_eq!(
        calls[0].1["choice"],
        json!({"kind": "best", "maxHeight": 720})
    );
    assert_eq!(calls[0].1["subtitles"], json!(["bn"]));
    assert!(
        calls[0].1.get("filename").is_none(),
        "the title is the default name"
    );
    assert!(
        calls[0].1.get("directory").is_none(),
        "the default folder is the daemon's choice"
    );
    h.msg(Msg::Reply(Tag::Add { start: true }, Ok(json!({"id": "m"}))));
    assert_eq!(h.followed(), vec!["m".to_owned()]);

    // ---------------------------------------------------------------- Bangla, dark
    h.settings("bn", "dark");
    h.event(event::DOWNLOAD_ADDED, dl("c", "বাংলা-বই.pdf", "paused", true));
    assert_eq!(h.ui.get_screen(), 1);
    assert_eq!(h.ui.get_heading(), "ব্রাউজার থেকে নতুন ডাউনলোড");
    h.shot("confirm-bn-dark");
    h.press("cancel");
    assert_eq!(h.calls()[0].1, json!({"ids": ["c"], "deleteFiles": true}));
    h.msg(Msg::Reply(Tag::Remove("c".into()), Ok(Value::Null)));

    // ---------------------------------------------------------------- Arabic, right to left
    h.settings("ar", "light");
    h.event(
        event::DOWNLOAD_ADDED,
        dl(
            "m",
            "A walk through Dhaka at night.mp4",
            "downloading",
            false,
        ),
    );
    assert_eq!(h.ui.get_screen(), 3);
    let trail: Vec<String> = slint::Model::iter(&h.ui.get_lead())
        .map(|b| b.id.to_string())
        .collect();
    assert_eq!(
        trail,
        vec!["hide".to_owned(), "pause".to_owned()],
        "the main button comes first"
    );
    h.shot("progress-ar");

    // ---------------------------------------------------------------- nothing left: it closes
    h.event(event::DOWNLOAD_REMOVED, json!({"ids": ["m"]}));
    assert_eq!(h.ui.get_screen(), 0);
    assert!(h.followed().is_empty());
    h.wait(Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(&h.dir);
}
