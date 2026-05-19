#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Quit,
}

#[cfg(target_os = "windows")]
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

            let tray = TrayIconBuilder::new()
                .with_tooltip("ResearchWiki - scheduler active")
                .with_menu(Box::new(menu.clone()))
                .with_menu_on_left_click(false)
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

    fn icon() -> Result<Icon, tray_icon::BadIcon> {
        const SIZE: u32 = 32;
        let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let border = !(4..=27).contains(&x) || !(4..=27).contains(&y);
                let diagonal = x.abs_diff(y) <= 2 || x + y >= 30 && x + y <= 34;
                let (r, g, b, a) = if border {
                    (26, 82, 118, 255)
                } else if diagonal {
                    (48, 132, 170, 255)
                } else {
                    (240, 248, 252, 255)
                };
                rgba.extend_from_slice(&[r, g, b, a]);
            }
        }
        Icon::from_rgba(rgba, SIZE, SIZE)
    }

    fn restore_window(window_handle: Option<isize>) {
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

    fn close_window(window_handle: Option<isize>) {
        let Some(hwnd) = window_handle else {
            return;
        };

        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW, WM_CLOSE};

            let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
            if IsWindow(hwnd) != 0 {
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
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
