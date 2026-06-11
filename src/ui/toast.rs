use std::time::{Duration, Instant};

use super::style::{self, color};

/// How long a toast stays before fading out.
const TTL_INFO: Duration = Duration::from_secs(4);
const TTL_ERROR: Duration = Duration::from_secs(10);
/// Fade-out window at the end of a toast's life.
const FADE: Duration = Duration::from_millis(300);
/// Maximum toasts rendered at once; older ones collapse into a "+N more" row.
const MAX_VISIBLE: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl ToastKind {
    fn tone(self) -> style::Tone {
        match self {
            ToastKind::Info => style::Tone::Accent,
            ToastKind::Success => style::Tone::Success,
            ToastKind::Error => style::Tone::Danger,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            ToastKind::Info => style::icon::INFO,
            ToastKind::Success => style::icon::CHECK_CIRCLE,
            ToastKind::Error => style::icon::WARNING_CIRCLE,
        }
    }
}

struct Toast {
    kind: ToastKind,
    message: String,
    created: Instant,
    ttl: Duration,
}

impl Toast {
    /// Remaining-life opacity: 1.0 for most of the TTL, ramping to 0 over the
    /// final [`FADE`] window. `None` once expired.
    fn opacity(&self) -> Option<f32> {
        let elapsed = self.created.elapsed();
        if elapsed >= self.ttl {
            return None;
        }
        let left = self.ttl - elapsed;
        if left >= FADE {
            Some(1.0)
        } else {
            Some(left.as_secs_f32() / FADE.as_secs_f32())
        }
    }
}

/// App-level toast stack, rendered top-right above all panels.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, message: impl Into<String>) {
        let message = message.into();
        if message.is_empty() {
            return;
        }
        let ttl = match kind {
            ToastKind::Error => TTL_ERROR,
            _ => TTL_INFO,
        };
        self.items.push(Toast {
            kind,
            message,
            created: Instant::now(),
            ttl,
        });
    }

    /// Render the stack and drop expired toasts. Call once per frame at the
    /// end of `update()` so toasts overlay every panel.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.items.retain(|t| t.opacity().is_some());
        if self.items.is_empty() {
            return;
        }

        egui::Area::new(egui::Id::new("toast_stack"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 48.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_max_width(340.0);
                let mut dismiss: Option<usize> = None;
                let hidden = self.items.len().saturating_sub(MAX_VISIBLE);
                // Newest on top.
                for (idx, toast) in self.items.iter().enumerate().skip(hidden).rev() {
                    let alpha = toast.opacity().unwrap_or(0.0);
                    ui.scope(|ui| {
                        ui.set_opacity(alpha);
                        if show_toast(ui, toast) {
                            dismiss = Some(idx);
                        }
                    });
                    ui.add_space(style::SPACE_XS);
                }
                if hidden > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        style::muted_label(ui, format!("+{hidden}"));
                    });
                }
                if let Some(idx) = dismiss {
                    self.items.remove(idx);
                }
            });

        // Keep animating fades / expirations even when the app is idle.
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

/// Render a single toast frame; returns true when its dismiss button was clicked.
fn show_toast(ui: &mut egui::Ui, toast: &Toast) -> bool {
    let (bg, fg) = toast.kind.tone().colors();
    let mut dismissed = false;
    egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(1.0, fg))
        .corner_radius(egui::CornerRadius::same(style::RADIUS_CARD))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: egui::Color32::from_black_alpha(25),
        })
        .show(ui, |ui| {
            ui.set_width(320.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(toast.kind.glyph())
                        .size(style::SECTION_TEXT_SIZE)
                        .color(fg),
                );
                ui.add_sized(
                    [ui.available_width() - 28.0, 0.0],
                    egui::Label::new(
                        egui::RichText::new(&toast.message)
                            .size(style::HELP_TEXT_SIZE)
                            .color(color::TEXT),
                    )
                    .wrap(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(style::icon::X)
                                    .size(style::HELP_TEXT_SIZE)
                                    .color(fg),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        dismissed = true;
                    }
                });
            });
        });
    dismissed
}
