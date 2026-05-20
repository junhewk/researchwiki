use super::{MsgChannel, PanelCtx};
use crate::models::workspace::Workspace;

enum Msg {
    Loaded(Box<Workspace>),
    Generated(String),
    Status(String),
    Error(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    loaded_id: Option<i64>,
    primary_question: String,
    gap_note: String,
    refined_question: String,
    status: Option<String>,
    busy: bool,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let channel = self.channel.get_or_insert_with(MsgChannel::default);
        while let Ok(msg) = channel.rx.try_recv() {
            match msg {
                Msg::Loaded(ws) => {
                    let ws = *ws;
                    self.primary_question = ws.primary_question;
                    self.gap_note = ws.gap_note;
                    self.refined_question = ws.refined_question;
                    self.loaded_id = Some(ws.id);
                    self.busy = false;
                }
                Msg::Generated(refined) => {
                    self.refined_question = refined;
                    self.status =
                        Some("Gap finder produced a refined question (saved).".to_string());
                    self.busy = false;
                }
                Msg::Status(text) => {
                    self.status = Some(text);
                    self.busy = false;
                }
                Msg::Error(err) => {
                    self.status = Some(format!("Error: {err}"));
                    self.busy = false;
                }
            }
        }

        let active = ctx.active_workspace_id;
        if self.loaded_id != Some(active) && !self.busy {
            self.load(ctx, active);
        }

        ui.heading("Gap Bridge");
        ui.label("From the broad primary question to the refined, next research question.");
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.label(egui::RichText::new("Broad question").strong());
                    ui.add_space(4.0);
                    ui.label(if self.primary_question.is_empty() {
                        "(set the primary question in the Input Set tab)"
                    } else {
                        &self.primary_question
                    });
                });
                cols[1].group(|ui| {
                    ui.label(egui::RichText::new("Identified gap").strong());
                    ui.add_space(4.0);
                    ui.label(if self.gap_note.is_empty() {
                        "(add a gap note in the Input Set tab)"
                    } else {
                        &self.gap_note
                    });
                });
            });

            ui.add_space(12.0);
            ui.label(egui::RichText::new("Refined / next research question").strong());
            ui.add(
                egui::TextEdit::multiline(&mut self.refined_question)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("the focused, answerable trial question that bridges the gap"),
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Save refined question"))
                    .clicked()
                {
                    self.save(ctx, active);
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Run gap finder (LLM)"))
                    .clicked()
                {
                    self.run_gap_finder(ctx, active);
                }
                if self.busy {
                    ui.spinner();
                }
            });

            if let Some(status) = &self.status {
                ui.add_space(8.0);
                ui.label(status);
            }

            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(
                    "\"Run gap finder\" analyzes this workspace's knowledge graph (isolated and \
                     under-connected concepts) and asks the LLM to draft the refined question from \
                     your primary question + gap note. You can edit and re-save it.",
                )
                .weak()
                .italics(),
            );
        });
    }

    fn load(&mut self, ctx: &PanelCtx<'_>, id: i64) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.loaded_id = Some(id);
        let tx = channel.tx.clone();
        let svc = ctx.state.workspace_service.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.get(id).await {
                Ok(ws) => tx.send(Msg::Loaded(Box::new(ws))),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn run_gap_finder(&mut self, ctx: &PanelCtx<'_>, id: i64) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.status = Some("Running gap finder…".to_string());
        let tx = channel.tx.clone();
        let kg = ctx.state.knowledge_graph_service.clone();
        let ws_svc = ctx.state.workspace_service.clone();
        let primary_question = self.primary_question.clone();
        let gap_note = self.gap_note.clone();
        ctx.handle.spawn(async move {
            let result: Result<String, crate::error::AppError> = async {
                let refined = kg
                    .generate_gap_bridge(id, primary_question, gap_note)
                    .await?;
                ws_svc.set_refined_question(id, refined.clone()).await?;
                Ok(refined)
            }
            .await;
            let _ = match result {
                Ok(refined) => tx.send(Msg::Generated(refined)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn save(&mut self, ctx: &PanelCtx<'_>, id: i64) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.status = Some("Saving…".to_string());
        let tx = channel.tx.clone();
        let svc = ctx.state.workspace_service.clone();
        let text = self.refined_question.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.set_refined_question(id, text).await {
                Ok(()) => tx.send(Msg::Status("Saved.".to_string())),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }
}
