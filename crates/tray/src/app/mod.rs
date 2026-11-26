// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray process: the icon and menu on the main thread (the system's
//! event loop, through `tao`), the daemon on a thread of its own
//! ([`watch`]), which sends what the icon should show back to the loop.

mod notify;
mod watch;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use wdm_tray::{i18n, prefs};

/// What the loop is told to do.
#[derive(Debug)]
pub enum UserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    Tooltip(String),
    /// The UI's language changed: the menu follows.
    Language(String),
    /// Quit (from the menu, done) or the app quit WDM: end the process.
    Exit,
}

pub type Proxy = EventLoopProxy<UserEvent>;

/// The app's icon, for the tray.
fn icon() -> Option<tray_icon::Icon> {
    let bytes: &[u8] = include_bytes!("../../../../apps/desktop/src-tauri/icons/128x128.png");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    tray_icon::Icon::from_rgba(buf, info.width, info.height).ok()
}

/// Opens the main window: the native one (`wdm-prompt --main`), which a
/// running `wdm-prompt` shows itself.
fn show_app() {
    if let Err(err) = wdm_tray::launch::start_main() {
        eprintln!("wdm-tray: could not open the main window: {err}");
    }
}

/// Runs the tray until Quit. Never returns.
pub fn run(dir: PathBuf) -> ! {
    // Opened again: the browser may hand downloads to WDM again.
    watch::mark_quit(&dir, false);
    // A new run: the UI's answers "for this run" start over.
    let _ = std::fs::remove_file(dir.join(wdm_tray::THIS_RUN_FILE));
    let mut reporter = wdm_crash::init("tray", env!("CARGO_PKG_VERSION"), &dir);

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // A menu-bar extra, not an app in the Dock.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Tray(e));
        }));
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
    }

    let language = prefs::load().language;
    let text = i18n::strings(&language);
    let show = MenuItem::new(&text.show, true, None);
    let quit = MenuItem::new(&text.quit, true, None);
    let mut tray: Option<TrayIcon> = None;
    let mut quitting = false;
    // Two clicks in a row open one window.
    let mut last_show = Instant::now() - Duration::from_secs(10);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // The icon is made once the loop runs (macOS needs that).
            Event::NewEvents(StartCause::Init) => {
                let menu = Menu::new();
                let built = menu
                    .append_items(&[&show, &quit])
                    .map_err(|e| e.to_string())
                    .and_then(|()| {
                        let mut b = TrayIconBuilder::new()
                            .with_menu(Box::new(menu))
                            .with_menu_on_left_click(false)
                            .with_tooltip(i18n::tooltip(&language, 0, 0));
                        if let Some(icon) = icon() {
                            b = b.with_icon(icon);
                        }
                        b.build().map_err(|e| e.to_string())
                    });
                match built {
                    Ok(t) => tray = Some(t),
                    Err(err) => {
                        // No tray on this desktop (no status notifier): the
                        // app then quits when its window closes.
                        eprintln!("wdm-tray: no tray icon: {err}");
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                }
                watch::start(dir.clone(), proxy.clone(), language.clone());
            }
            Event::UserEvent(UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            })) => {
                if last_show.elapsed() > Duration::from_secs(2) {
                    last_show = Instant::now();
                    show_app();
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == *show.id() {
                    last_show = Instant::now();
                    show_app();
                } else if e.id == *quit.id() && !quitting {
                    quitting = true;
                    let dir = dir.clone();
                    let p = proxy.clone();
                    std::thread::spawn(move || {
                        watch::quit_everything(&dir);
                        let _ = p.send_event(UserEvent::Exit);
                    });
                }
            }
            Event::UserEvent(UserEvent::Tooltip(text)) => {
                if let Some(t) = &tray {
                    let _ = t.set_tooltip(Some(text));
                }
            }
            Event::UserEvent(UserEvent::Language(tag)) => {
                let text = i18n::strings(&tag);
                show.set_text(&text.show);
                quit.set_text(&text.quit);
            }
            Event::UserEvent(UserEvent::Exit) => {
                tray = None;
                // Sends what is pending (the loop ends the process).
                drop(reporter.take());
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    })
}
