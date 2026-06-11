pub const BODY_TEXT_SIZE: f32 = 15.0;
pub const HELP_TEXT_SIZE: f32 = 14.0;
pub const SECTION_TEXT_SIZE: f32 = 16.0;
pub const GRAPH_LABEL_TEXT_SIZE: f32 = 13.0;

/// Corner radii (egui `CornerRadius` is `u8`).
pub const RADIUS_CARD: u8 = 8;
pub const RADIUS_CONTROL: u8 = 6;

/// Spacing scale shared by helpers and panels.
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_S: f32 = 8.0;
pub const SPACE_M: f32 = 12.0;
pub const SPACE_L: f32 = 16.0;

/// Light "Tailwind-ish" design tokens. One source of truth for every surface,
/// border, text, and accent color used by the component helpers and theme.
pub mod color {
    use egui::Color32;

    pub const SURFACE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    pub const SURFACE_ALT: Color32 = Color32::from_rgb(0xF8, 0xFA, 0xFC);
    pub const SURFACE_SUNKEN: Color32 = Color32::from_rgb(0xF1, 0xF5, 0xF9);
    pub const BORDER: Color32 = Color32::from_rgb(0xE5, 0xE7, 0xEB);
    pub const TEXT: Color32 = Color32::from_rgb(0x11, 0x18, 0x27);
    pub const MUTED: Color32 = Color32::from_rgb(0x6B, 0x72, 0x80);
    pub const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x46, 0xE5);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x63, 0x66, 0xF1);
    pub const ACCENT_WEAK: Color32 = Color32::from_rgb(0xEE, 0xF2, 0xFF);
    pub const SUCCESS: Color32 = Color32::from_rgb(0x16, 0xA3, 0x4A);
    pub const SUCCESS_WEAK: Color32 = Color32::from_rgb(0xDC, 0xFC, 0xE7);
    pub const DANGER: Color32 = Color32::from_rgb(0xDC, 0x26, 0x26);
    pub const DANGER_WEAK: Color32 = Color32::from_rgb(0xFE, 0xE2, 0xE2);
    pub const WARNING: Color32 = Color32::from_rgb(0xB4, 0x54, 0x09);
    pub const WARNING_WEAK: Color32 = Color32::from_rgb(0xFE, 0xF3, 0xC7);
    pub const ON_ACCENT: Color32 = Color32::WHITE;
    /// Pressed-control fill; one step darker than [`ACCENT_WEAK`] so press ≠ hover.
    pub const ACCENT_PRESSED: Color32 = Color32::from_rgb(0xE0, 0xE7, 0xFF);
}

/// Tone for [`badge`] and status pills.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Danger,
    Warning,
}

impl Tone {
    fn colors(self) -> (egui::Color32, egui::Color32) {
        match self {
            Tone::Neutral => (color::SURFACE_SUNKEN, color::MUTED),
            Tone::Accent => (color::ACCENT_WEAK, color::ACCENT),
            Tone::Success => (color::SUCCESS_WEAK, color::SUCCESS),
            Tone::Danger => (color::DANGER_WEAK, color::DANGER),
            Tone::Warning => (color::WARNING_WEAK, color::WARNING),
        }
    }
}

pub fn apply_app_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(BODY_TEXT_SIZE, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(21.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(14.0, egui::FontFamily::Monospace),
        );

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.indent = 18.0;
        style.spacing.window_margin = egui::Margin::same(12);

        style.visuals = light_visuals();
    });
}

/// The light/indigo theme. Built from `Visuals::light()` with surface, border,
/// accent, and rounded-corner overrides so the whole app reads as one modern UI.
fn light_visuals() -> egui::Visuals {
    use egui::{Color32, CornerRadius, Stroke};

    let radius = CornerRadius::same(RADIUS_CONTROL);
    let border = Stroke::new(1.0, color::BORDER);
    let text = Stroke::new(1.0, color::TEXT);

    let mut v = egui::Visuals::light();
    v.panel_fill = color::SURFACE;
    v.window_fill = color::SURFACE;
    v.extreme_bg_color = color::SURFACE; // text-edit background
    v.faint_bg_color = color::SURFACE_ALT; // striped rows
    v.code_bg_color = color::SURFACE_SUNKEN;
    v.hyperlink_color = color::ACCENT;
    v.selection.bg_fill = color::ACCENT_WEAK;
    v.selection.stroke = Stroke::new(1.0, color::ACCENT);
    v.window_corner_radius = CornerRadius::same(RADIUS_CARD);
    v.menu_corner_radius = CornerRadius::same(RADIUS_CARD);
    v.window_stroke = border;

    // Resting (non-interactive) surfaces and labels.
    v.widgets.noninteractive.bg_fill = color::SURFACE;
    v.widgets.noninteractive.weak_bg_fill = color::SURFACE;
    v.widgets.noninteractive.bg_stroke = border;
    v.widgets.noninteractive.fg_stroke = text;
    v.widgets.noninteractive.corner_radius = radius;

    // Idle controls (buttons, inputs).
    v.widgets.inactive.bg_fill = color::SURFACE_ALT;
    v.widgets.inactive.weak_bg_fill = color::SURFACE_ALT;
    v.widgets.inactive.bg_stroke = border;
    v.widgets.inactive.fg_stroke = text;
    v.widgets.inactive.corner_radius = radius;

    // Hover: soft accent wash with a lighter outline.
    v.widgets.hovered.bg_fill = color::ACCENT_WEAK;
    v.widgets.hovered.weak_bg_fill = color::ACCENT_WEAK;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, color::ACCENT_HOVER);
    v.widgets.hovered.fg_stroke = text;
    v.widgets.hovered.corner_radius = radius;

    // Pressed / active: one step darker than hover so the press registers.
    v.widgets.active.bg_fill = color::ACCENT_PRESSED;
    v.widgets.active.weak_bg_fill = color::ACCENT_PRESSED;
    v.widgets.active.bg_stroke = Stroke::new(1.0, color::ACCENT);
    v.widgets.active.fg_stroke = text;
    v.widgets.active.corner_radius = radius;

    v.widgets.open.bg_fill = color::SURFACE_ALT;
    v.widgets.open.bg_stroke = border;
    v.widgets.open.fg_stroke = text;
    v.widgets.open.corner_radius = radius;

    // Subtle card-like window shadow.
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 2],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(20),
    };
    v.popup_shadow = v.window_shadow;

    v
}

/// Phosphor (regular) glyph constants, re-exported so panels use
/// `style::icon::GEAR` without importing the crate directly.
pub mod icon {
    pub use egui_phosphor::regular::*;
}

pub fn panel_header(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>) {
    ui.heading(egui::RichText::new(title).color(color::TEXT));
    if let Some(subtitle) = subtitle {
        muted_label(ui, subtitle);
    }
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(8.0);
}

/// `panel_header` with a leading Phosphor icon glyph.
pub fn panel_header_icon(ui: &mut egui::Ui, glyph: &str, title: &str, subtitle: Option<&str>) {
    panel_header(ui, &format!("{glyph}  {title}"), subtitle);
}

pub fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(title)
            .strong()
            .size(SECTION_TEXT_SIZE)
            .color(color::TEXT),
    );
    ui.add_space(2.0);
}

pub fn section_break(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(10.0);
}

pub fn muted_label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text.into())
                .size(HELP_TEXT_SIZE)
                .color(color::MUTED),
        )
        .wrap(),
    )
}

pub fn body_label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(egui::RichText::new(text.into()).size(BODY_TEXT_SIZE)).wrap())
}

pub fn status_notice(ui: &mut egui::Ui, success: bool, text: &str) -> egui::Response {
    let tone = if success { Tone::Success } else { Tone::Danger };
    badge(ui, text, tone)
}

/// A surface container: filled, bordered, rounded panel for grouping content.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::group(ui.style())
        .fill(color::SURFACE_ALT)
        .stroke(egui::Stroke::new(1.0, color::BORDER))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::same(12))
        .show(ui, add)
        .inner
}

/// Filled accent button for the primary action in a row.
pub fn primary_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    button_filled(ui, text, color::ACCENT, color::ON_ACCENT)
}

/// Outlined neutral button for secondary actions.
pub fn secondary_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text.into()).color(color::TEXT))
            .fill(color::SURFACE)
            .stroke(egui::Stroke::new(1.0, color::BORDER))
            .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL)),
    )
}

/// Frameless text-only button (toolbars, low-emphasis actions).
pub fn ghost_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text.into()).color(color::ACCENT))
            .frame(false)
            .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL)),
    )
}

/// Filled danger button for destructive actions.
pub fn danger_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    button_filled(ui, text, color::DANGER, color::ON_ACCENT)
}

fn button_filled(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    fill: egui::Color32,
    fg: egui::Color32,
) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text.into()).color(fg))
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL)),
    )
}

/// A small rounded pill for statuses, tiers, and counts.
pub fn badge(ui: &mut egui::Ui, text: &str, tone: Tone) -> egui::Response {
    let (bg, fg) = tone.colors();
    egui::Frame::new()
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(HELP_TEXT_SIZE)
                    .color(fg)
                    .strong(),
            )
        })
        .inner
}

/// A labeled input row: bold label, the widget, and an optional muted hint.
/// Returns whatever `add` returns (typically the widget `Response`).
pub fn field<R>(
    ui: &mut egui::Ui,
    label: &str,
    hint: Option<&str>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.label(egui::RichText::new(label).strong().color(color::TEXT));
    let result = add(ui);
    if let Some(hint) = hint {
        muted_label(ui, hint);
    }
    ui.add_space(6.0);
    result
}

/// Centered empty-state block: large muted icon, strong title, muted
/// description, and an optional primary action. Returns the action button's
/// `Response` when `action` is provided so callers can react to clicks.
pub fn empty_state(
    ui: &mut egui::Ui,
    glyph: &str,
    title: &str,
    description: &str,
    action: Option<&str>,
) -> Option<egui::Response> {
    let mut action_response = None;
    ui.add_space(SPACE_L * 2.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(glyph).size(40.0).color(color::MUTED));
        ui.add_space(SPACE_S);
        ui.label(
            egui::RichText::new(title)
                .size(SECTION_TEXT_SIZE)
                .strong()
                .color(color::TEXT),
        );
        ui.add_space(SPACE_XS);
        ui.add(
            egui::Label::new(
                egui::RichText::new(description)
                    .size(HELP_TEXT_SIZE)
                    .color(color::MUTED),
            )
            .wrap(),
        );
        if let Some(action) = action {
            ui.add_space(SPACE_M);
            action_response = Some(primary_button(ui, action));
        }
    });
    ui.add_space(SPACE_L * 2.0);
    action_response
}

/// Small muted "?" glyph that reveals a tooltip on hover. Place beside labels
/// for advanced or ambiguous fields.
pub fn help_icon(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Label::new(
            egui::RichText::new(icon::QUESTION)
                .size(HELP_TEXT_SIZE)
                .color(color::MUTED),
        )
        .sense(egui::Sense::hover()),
    )
    .on_hover_text(text)
}

/// What the user chose on an [`error_notice`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoticeAction {
    None,
    Retry,
    Dismiss,
}

/// Persistent error card: danger-toned, wrapped message, optional retry
/// button, and a dismiss button. Stays visible until the caller clears it.
pub fn error_notice(ui: &mut egui::Ui, message: &str, retry_label: Option<&str>) -> NoticeAction {
    let mut action = NoticeAction::None;
    egui::Frame::new()
        .fill(color::DANGER_WEAK)
        .stroke(egui::Stroke::new(1.0, color::DANGER))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(icon::WARNING_CIRCLE)
                        .size(SECTION_TEXT_SIZE)
                        .color(color::DANGER),
                );
                let reserved = if retry_label.is_some() { 110.0 } else { 40.0 };
                ui.add_sized(
                    [(ui.available_width() - reserved).max(80.0), 0.0],
                    egui::Label::new(
                        egui::RichText::new(message)
                            .size(HELP_TEXT_SIZE)
                            .color(color::DANGER),
                    )
                    .wrap(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(icon::X)
                                    .size(HELP_TEXT_SIZE)
                                    .color(color::DANGER),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        action = NoticeAction::Dismiss;
                    }
                    if let Some(label) = retry_label {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(label)
                                        .size(HELP_TEXT_SIZE)
                                        .color(color::DANGER)
                                        .strong(),
                                )
                                .fill(color::SURFACE)
                                .stroke(egui::Stroke::new(1.0, color::DANGER))
                                .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL)),
                            )
                            .clicked()
                        {
                            action = NoticeAction::Retry;
                        }
                    }
                });
            });
        });
    action
}

/// Inline spinner with a muted label, for any in-flight async operation.
pub fn loading_indicator(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        muted_label(ui, label);
    });
}

/// Single-line secret input with a reveal toggle. `revealed` lives in the
/// caller's struct so the toggle persists across frames.
pub fn secret_edit(
    ui: &mut egui::Ui,
    value: &mut String,
    revealed: &mut bool,
    hint: &str,
) -> egui::Response {
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .password(!*revealed)
                .hint_text(hint)
                .desired_width(ui.available_width() - 36.0),
        );
        let glyph = if *revealed {
            icon::EYE_SLASH
        } else {
            icon::EYE
        };
        if ui
            .add(egui::Button::new(egui::RichText::new(glyph).color(color::MUTED)).frame(false))
            .clicked()
        {
            *revealed = !*revealed;
        }
        response
    })
    .inner
}

/// Top-navigation tab: icon + label, accent text and a 2px underline when
/// selected, soft accent fill on hover.
pub fn nav_tab(ui: &mut egui::Ui, selected: bool, glyph: &str, label: &str) -> egui::Response {
    let fg = if selected { color::ACCENT } else { color::MUTED };
    let text = egui::RichText::new(format!("{glyph}  {label}"))
        .size(HELP_TEXT_SIZE)
        .color(fg);
    let text = if selected { text.strong() } else { text };
    let response = ui.add(
        egui::Button::new(text)
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL)),
    );
    if response.hovered() && !selected {
        ui.painter().rect_filled(
            response.rect,
            egui::CornerRadius::same(RADIUS_CONTROL),
            color::ACCENT_WEAK.gamma_multiply(0.6),
        );
    }
    if selected {
        let rect = response.rect;
        let underline = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - 2.0),
            egui::pos2(rect.right() - 4.0, rect.bottom()),
        );
        ui.painter()
            .rect_filled(underline, egui::CornerRadius::same(1), color::ACCENT);
    }
    response
}

/// Bold muted small text for table/grid headers.
pub fn table_header(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(HELP_TEXT_SIZE)
            .strong()
            .color(color::MUTED),
    );
}

/// A dashboard metric card: large value over a muted label.
pub fn metric_tile(ui: &mut egui::Ui, label: &str, value: &str) {
    egui::Frame::group(ui.style())
        .fill(color::SURFACE_ALT)
        .stroke(egui::Stroke::new(1.0, color::BORDER))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(value)
                        .size(24.0)
                        .strong()
                        .color(color::TEXT),
                );
                ui.label(
                    egui::RichText::new(label)
                        .size(HELP_TEXT_SIZE)
                        .color(color::MUTED),
                );
            });
        });
}
