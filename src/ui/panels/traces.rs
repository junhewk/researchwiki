use super::PanelCtx;
use crate::ui::style;

#[derive(Default)]
pub struct Panel;

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        style::panel_header(ui, "Traces", None);
        style::body_label(ui, "Paginated LLM trace table with detail view - TODO");
    }
}
