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
use slint::{ComponentHandle as _, Model as _, PhysicalSize, Rgb8Pixel};
use tokio::sync::mpsc;
use wdm_proto::{event, method};

use crate::daemon::{Cmd, Msg, Tag};
use crate::{PromptWindow, ui};

const W: u32 = 520;
const H: u32 = 460;
const MW: u32 = 1120;
const MH: u32 = 700;

thread_local! {
    static NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    /// Every window made so far: the prompt window first, then each main window.
    static WINDOWS: std::cell::RefCell<Vec<Rc<MinimalSoftwareWindow>>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct TestPlatform;

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        WINDOWS.with(|ws| ws.borrow_mut().push(w.clone()));
        Ok(w)
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

    /// Draws the prompt window; it must not be blank. Saved as `<name>.png` when asked.
    fn shot(&self, name: &str) {
        self.draw(&self.window.clone(), W, H, name);
    }

    /// Draws the main window (the newest).
    fn main_shot(&self, name: &str) {
        self.main_shot_sized(name, MH);
    }

    fn main_shot_sized(&self, name: &str, height: u32) {
        let w = WINDOWS.with(|ws| ws.borrow().last().cloned()).unwrap();
        w.set_size(PhysicalSize::new(MW, height));
        self.draw(&w, MW, height, name);
        w.set_size(PhysicalSize::new(MW, MH));
    }

    fn draw(&self, window: &Rc<MinimalSoftwareWindow>, w: u32, h: u32, name: &str) {
        let mut buffer = vec![Rgb8Pixel::default(); (w * h) as usize];
        window.request_redraw();
        self.wait(Duration::from_millis(16));
        window.draw_if_needed(|r| {
            r.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            r.render(&mut buffer, w as usize);
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
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
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
    slint::platform::set_platform(Box::new(TestPlatform)).ok();
    let dir = std::env::temp_dir().join(format!("wdm-prompt-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lock = wdm_tray::locks::take(&dir, wdm_tray::locks::PROMPT).unwrap();
    let ui = PromptWindow::new().unwrap();
    let window = WINDOWS.with(|ws| ws.borrow()[0].clone());
    window.set_size(PhysicalSize::new(W, H));
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

    // ---------------------------------------------------------------- the main window
    h.settings("en", "light");
    let item = |id: &str, name: &str, state: &str, done: u64, total: Option<u64>, speed: u64| {
        json!({
            "id": id, "url": format!("https://cdn.example.org/{name}"), "filename": name,
            "directory": "/home/u/Downloads", "state": state, "totalBytes": total,
            "downloadedBytes": done, "speedBps": speed, "etaSecs": 130,
            "connections": if state == "downloading" { 8 } else { 0 }, "resumable": true,
            "addedAt": done % 97, "error": if state == "failed" { json!({"code": "http_404", "message": "The server says the file is not there (404)."}) } else { Value::Null },
            "activeMs": 61_000,
        })
    };
    h.msg(Msg::List(
        [
            item(
                "u",
                "ubuntu-26.10-desktop-amd64.iso",
                "downloading",
                2_147_483_648,
                Some(6_120_000_000),
                12_582_912,
            ),
            item(
                "v",
                "A walk through Dhaka at night.mp4",
                "downloading",
                88_000_000,
                Some(160_000_000),
                3_145_728,
            ),
            item(
                "r",
                "report-2026-final.pdf",
                "completed",
                3_500_000,
                Some(3_500_000),
                0,
            ),
            item(
                "z",
                "photos-backup.zip",
                "paused",
                410_000_000,
                Some(1_200_000_000),
                0,
            ),
            item("s", "setup-tool-v4.exe", "failed", 0, None, 0),
            item("q", "song.mp3", "queued", 0, Some(8_000_000), 0),
        ]
        .into_iter()
        .map(summary)
        .collect(),
    ));
    ui::with(ui::Controller::show_main);
    let main = ui::with_ret(|c| c.main_window())
        .flatten()
        .expect("the main window");
    assert_eq!(main.get_rows().row_count(), 6);
    h.main_shot("main");

    // Sorted by name, then a click selects and shows the details.
    main.invoke_sort("name".into());
    let names: Vec<String> = main.get_rows().iter().map(|r| r.name.to_string()).collect();
    assert_eq!(names[0], "A walk through Dhaka at night.mp4");
    main.invoke_row_pressed(0, false, false, false);
    assert!(main.get_detail());
    assert!(main.get_can_pause() && !main.get_can_resume());
    // Shift adds the run down to the fourth row.
    main.invoke_row_pressed(3, false, true, false);
    assert_eq!(main.get_rows().iter().filter(|r| r.selected).count(), 4);
    h.main_shot("main-selected");
    main.invoke_action("pause".into());
    let calls = h.calls();
    assert_eq!(calls[0].0, method::DOWNLOAD_PAUSE);
    assert_eq!(
        calls[0].1["ids"],
        json!(["v"]),
        "only the ones that can pause"
    );

    // Remove asks first; the files stay unless ticked.
    main.invoke_action("remove".into());
    assert!(main.get_removing());
    h.main_shot("main-remove");
    main.invoke_remove_confirmed(true);
    let calls = h.calls();
    assert_eq!(calls[0].0, method::DOWNLOAD_REMOVE);
    assert_eq!(calls[0].1["deleteFiles"], json!(false));
    assert!(!main.get_removing());

    // A view and a search.
    main.invoke_nav_picked("group:completed".into());
    assert_eq!(main.get_rows().row_count(), 1);
    main.invoke_row_pressed(0, false, false, false);
    assert!(main.get_can_open());
    h.main_shot("main-completed");
    main.invoke_nav_picked("group:all".into());
    main.invoke_search("zip".into());
    assert_eq!(main.get_rows().row_count(), 1);
    main.invoke_search("nothing like this".into());
    assert_eq!(main.get_rows().row_count(), 0);
    assert_eq!(
        main.get_empty_title(),
        "Nothing matches “nothing like this”"
    );
    main.invoke_action("clear-search".into());
    assert_eq!(main.get_rows().row_count(), 6);

    // Progress updates a row in place, without reordering.
    h.event(event::DOWNLOAD_PROGRESS, json!({"items": [{"id": "v", "downloadedBytes": 120_000_000, "totalBytes": 160_000_000, "speedBps": 1, "connections": 8}]}));
    let v = main.get_rows().iter().find(|r| r.id == "v").unwrap();
    assert_eq!(v.percent, "75.0%");

    // Add opens the add dialog in the prompt window.
    main.invoke_action("add".into());
    assert_eq!(h.ui.get_screen(), 2);
    assert_eq!(h.ui.get_url(), "");
    h.press("cancel");
    assert_eq!(h.ui.get_screen(), 0);

    // ---------------------------------------------------------------- Settings
    main.invoke_nav_picked("page:settings".into());
    assert_eq!(main.get_page(), 1);
    let g = main.global::<crate::SettingsData>();
    assert_eq!(g.get_folder(), "/home/u/Downloads");
    assert_eq!(g.get_active(), "3");
    assert!(!g.get_dirty());
    h.main_shot("settings");
    // The whole page at once.
    h.main_shot_sized("settings-all", 1640);
    // A bad value shows its error and cannot be saved.
    g.set_active("0".into());
    g.invoke_changed();
    assert!(g.get_dirty() && !g.get_valid());
    assert_eq!(g.get_active_error(), "Enter a whole number from 1 to 64.");
    h.main_shot("settings-error");
    // Five at once, a 1.5 MB/s limit, dark: Save sends them, with the rest as it was.
    g.set_active("5".into());
    g.set_limit_on(true);
    g.set_limit("1.5".into());
    g.set_unit_index(1);
    g.set_theme_index(2);
    g.invoke_changed();
    assert!(g.get_valid());
    g.invoke_save();
    let calls = h.calls();
    assert_eq!(calls[0].0, method::SETTINGS_SET);
    assert_eq!(calls[0].1["maxActiveDownloads"], json!(5));
    assert_eq!(calls[0].1["speedLimitBps"], json!(1_572_864));
    assert_eq!(calls[0].1["theme"], json!("dark"));
    assert_eq!(calls[0].1["maxConnectionsPerDownload"], json!(16));
    let saved = calls[0].1.clone();
    h.msg(Msg::Reply(Tag::Settings, Ok(saved)));
    assert!(!g.get_dirty());
    assert_eq!(g.get_status_text(), "Settings saved");
    // Revert puts back what is saved.
    g.set_folder("/tmp/elsewhere".into());
    g.invoke_changed();
    assert!(g.get_dirty());
    g.invoke_revert();
    assert_eq!(g.get_folder(), "/home/u/Downloads");
    assert!(!g.get_dirty());
    h.main_shot("settings-dark");
    h.settings("bn", "light");
    h.main_shot("settings-bn");
    h.settings("en", "light");
    main.invoke_nav_picked("group:all".into());
    assert_eq!(main.get_page(), 0);

    // ---------------------------------------------------------------- History
    main.invoke_nav_picked("page:history".into());
    assert_eq!(main.get_page(), 2);
    let hd = main.global::<crate::HistoryData>();
    let shown = |hd: &crate::HistoryData| {
        hd.get_rows()
            .iter()
            .map(|r| r.id.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(hd.get_empty(), 0);
    let all = shown(&hd);
    assert_eq!(all.len(), 2, "the completed and the failed one: {all:?}");
    h.main_shot("history");
    hd.invoke_pick_filter(2);
    assert_eq!(shown(&hd), vec!["s".to_owned()]);
    assert_eq!(hd.get_clear_text(), "Clear these…");
    let failed = hd.get_rows().row_data(0).unwrap();
    assert!(failed.failed && failed.size.is_empty());
    hd.invoke_action("retry".into(), "s".into());
    let calls = h.calls();
    assert_eq!(calls[0].0, method::DOWNLOAD_RESUME);
    hd.invoke_pick_filter(0);
    hd.invoke_search("no such file".into());
    assert_eq!(hd.get_empty(), 2);
    assert_eq!(hd.get_empty_title(), "No entries match");
    main.invoke_key("\u{1b}".into(), false, false);
    assert_eq!(shown(&hd).len(), 2, "Escape clears the search");
    // Clearing several asks first; the files stay.
    hd.invoke_action("clear".into(), "".into());
    assert!(hd.get_clearing());
    assert_eq!(hd.get_clear_title(), "Clear 2 entries from history?");
    h.main_shot("history-clear");
    hd.invoke_action("clear-no".into(), "".into());
    assert!(!hd.get_clearing() && h.calls().is_empty());
    // "Show" opens the download in the list.
    let done = all[0].clone();
    hd.invoke_action("show".into(), done.clone().into());
    assert_eq!(main.get_page(), 0);
    assert!(main.get_detail());
    assert!(
        main.get_rows()
            .iter()
            .any(|r| r.id == done.as_str() && r.selected)
    );
    main.invoke_nav_picked("page:history".into());
    hd.invoke_action("remove".into(), "s".into());
    let calls = h.calls();
    assert_eq!(calls[0].0, method::DOWNLOAD_REMOVE);
    assert_eq!(calls[0].1, json!({"ids": ["s"], "deleteFiles": false}));
    assert_eq!(shown(&hd), vec![done.clone()]);
    assert_eq!(hd.get_status_text(), "Cleared 1 entry");
    h.settings("ar", "dark");
    h.main_shot("history-ar-dark");
    h.settings("en", "light");

    // ---------------------------------------------------------------- Connect your browser
    use wdm_nmhost::connect::{Listing, Row as BRow};
    use wdm_nmhost::register::Browser;
    fn fake_rows() -> Vec<BRow> {
        let r = |browser, in_use, host_registered, extension_found, store| BRow {
            browser,
            in_use,
            host_registered,
            extension_found,
            store,
        };
        vec![
            r(Browser::Chrome, true, true, true, Listing::ChromeWebStore),
            r(Browser::Firefox, true, false, false, Listing::Github),
            r(Browser::Edge, true, true, false, Listing::EdgeAddOns),
            r(Browser::Brave, false, false, false, Listing::ChromeWebStore),
            r(
                Browser::Vivaldi,
                false,
                false,
                false,
                Listing::ChromeWebStore,
            ),
        ]
    }
    fn fake_link(_: Browser, _: &std::path::Path) -> Result<(), String> {
        Err("no permission".into())
    }
    ui::with(|c| {
        c.set_browser_source(ui::BrowserSource {
            rows: fake_rows,
            link: fake_link,
        })
    });
    main.invoke_nav_picked("page:browsers".into());
    assert_eq!(main.get_page(), 3);
    let bd = main.global::<crate::BrowsersData>();
    assert!(!bd.get_loading());
    let items: Vec<_> = bd.get_items().iter().collect();
    assert_eq!(items.len(), 3);
    assert!(items[0].ready && items[0].action.is_empty());
    assert_eq!(items[1].action, "connect");
    assert_eq!(items[2].action, "store");
    assert_eq!(items[2].action_text, "Get it from Edge Add-ons");
    assert_eq!(
        bd.get_not_found_text(),
        "Not found on this computer: Brave and Vivaldi."
    );
    bd.invoke_action("connect".into(), "firefox".into());
    assert_eq!(
        bd.get_status_text(),
        "Could not link Firefox: no permission"
    );
    assert!(bd.get_status_error() && !bd.get_busy());
    h.main_shot("browsers");
    h.settings("bn", "dark");
    h.main_shot("browsers-bn-dark");
    h.settings("en", "light");
    bd.invoke_action("done".into(), "".into());
    assert_eq!(main.get_page(), 0);

    // Bangla in the dark, Arabic right to left.
    h.settings("bn", "dark");
    main.invoke_row_pressed(0, false, false, false);
    h.main_shot("main-bn-dark");
    h.settings("ar", "light");
    h.main_shot("main-ar");
    h.settings("en", "light");

    // ---------------------------------------------------------------- nothing left: it closes
    h.event(event::DOWNLOAD_REMOVED, json!({"ids": ["m"]}));
    assert_eq!(h.ui.get_screen(), 0);
    assert!(h.followed().is_empty());
    h.wait(Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(&h.dir);
}
