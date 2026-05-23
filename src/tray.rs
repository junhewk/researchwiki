#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Quit,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod platform {
    use std::sync::mpsc;

    use tray_icon::{
        Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };

    use super::TrayCommand;

    const OPEN_ID: &str = "researchwiki.tray.open";
    const QUIT_ID: &str = "researchwiki.tray.quit";

    pub struct TrayController {
        _tray: TrayIcon,
        _menu: Menu,
        rx: mpsc::Receiver<TrayCommand>,
    }

    impl TrayController {
        pub fn new(ctx: &egui::Context, window_handle: Option<isize>) -> anyhow::Result<Self> {
            let (tx, rx) = mpsc::channel();
            let menu = Menu::new();
            let open = MenuItem::with_id(OPEN_ID, "Open ResearchWiki", true, None);
            let separator = PredefinedMenuItem::separator();
            let quit = MenuItem::with_id(QUIT_ID, "Quit", true, None);
            menu.append_items(&[&open, &separator, &quit])?;

            // macOS menu-bar convention: a left click opens the menu. On Windows
            // we keep the menu on right-click and restore on left double-click.
            let menu_on_left_click = cfg!(target_os = "macos");
            let tray = TrayIconBuilder::new()
                .with_tooltip("ResearchWiki - scheduler active")
                .with_menu(Box::new(menu.clone()))
                .with_menu_on_left_click(menu_on_left_click)
                .with_icon(icon()?)
                .build()?;

            let repaint_ctx = ctx.clone();
            let menu_tx = tx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                match event.id.as_ref() {
                    OPEN_ID => {
                        restore_window(window_handle);
                        let _ = menu_tx.send(TrayCommand::Show);
                    }
                    QUIT_ID => {
                        close_window(window_handle);
                        let _ = menu_tx.send(TrayCommand::Quit);
                    }
                    _ => {}
                }
                repaint_ctx.request_repaint();
            }));

            let repaint_ctx = ctx.clone();
            let tray_tx = tx;
            TrayIconEvent::set_event_handler(Some(move |event| {
                if matches!(
                    event,
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                ) {
                    restore_window(window_handle);
                    let _ = tray_tx.send(TrayCommand::Show);
                    repaint_ctx.request_repaint();
                }
            }));

            Ok(Self {
                _tray: tray,
                _menu: menu,
                rx,
            })
        }

        pub fn drain_commands(&mut self) -> Vec<TrayCommand> {
            let mut commands = Vec::new();
            while let Ok(command) = self.rx.try_recv() {
                commands.push(command);
            }
            commands
        }
    }

    fn icon() -> anyhow::Result<Icon> {
        let image = image::load_from_memory(include_bytes!("../assets/tray-icon.png"))?.to_rgba8();
        let (width, height) = image.dimensions();
        Ok(Icon::from_rgba(image.into_raw(), width, height)?)
    }

    // On Windows we nudge the window directly via Win32 because a hidden window
    // may not process queued egui ViewportCommands until forced. On macOS the
    // window show/hide is driven entirely by app.rs's ViewportCommand path
    // (TrayCommand::Show → restore_from_tray), so these are no-ops there.
    fn restore_window(window_handle: Option<isize>) {
        #[cfg(target_os = "windows")]
        {
            let Some(hwnd) = window_handle else {
                return;
            };

            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    BringWindowToTop, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow,
                    ShowWindowAsync,
                };

                let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
                if IsWindow(hwnd) == 0 {
                    return;
                }

                ShowWindowAsync(hwnd, SW_RESTORE);
                ShowWindow(hwnd, SW_RESTORE);
                BringWindowToTop(hwnd);
                SetForegroundWindow(hwnd);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = window_handle;
        }
    }

    fn close_window(window_handle: Option<isize>) {
        #[cfg(target_os = "windows")]
        {
            let Some(hwnd) = window_handle else {
                return;
            };

            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    IsWindow, PostMessageW, WM_CLOSE,
                };

                let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
                if IsWindow(hwnd) != 0 {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = window_handle;
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::TrayCommand;

    pub struct TrayController;

    impl TrayController {
        pub fn new(_ctx: &egui::Context, _window_handle: Option<isize>) -> anyhow::Result<Self> {
            Ok(Self)
        }

        pub fn drain_commands(&mut self) -> Vec<TrayCommand> {
            Vec::new()
        }
    }
}

pub use platform::TrayController;
