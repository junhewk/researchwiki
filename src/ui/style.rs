pub const BODY_TEXT_SIZE: f32 = 15.0;
pub const HELP_TEXT_SIZE: f32 = 14.0;
pub const SECTION_TEXT_SIZE: f32 = 16.0;
pub const GRAPH_LABEL_TEXT_SIZE: f32 = 13.0;

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
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.indent = 18.0;
        style.spacing.window_margin = egui::Margin::same(12);
    });
}

pub fn panel_header(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>) {
    ui.heading(title);
    if let Some(subtitle) = subtitle {
        muted_label(ui, subtitle);
    }
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(8.0);
}

pub fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(title).strong().size(SECTION_TEXT_SIZE));
    ui.add_space(2.0);
}

pub fn section_break(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(10.0);
}

pub fn muted_label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(egui::RichText::new(text.into()).weak().size(HELP_TEXT_SIZE)).wrap())
}

pub fn body_label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(egui::RichText::new(text.into()).size(BODY_TEXT_SIZE)).wrap())
}

pub fn status_notice(ui: &mut egui::Ui, success: bool, text: &str) -> egui::Response {
    let color = if success {
        egui::Color32::from_rgb(0, 120, 65)
    } else {
        egui::Color32::from_rgb(185, 30, 30)
    };
    ui.colored_label(color, egui::RichText::new(text).size(BODY_TEXT_SIZE))
}
