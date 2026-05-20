use super::{MsgChannel, PanelCtx};
use crate::models::workspace::{Workspace, WorkspaceCreate, WorkspaceUpdate};

enum Msg {
    Loaded(Workspace),
    Saved(Workspace),
    Status(String),
    Error(String),
}

#[derive(Default)]
struct Form {
    name: String,
    primary_question: String,
    seed_concepts_text: String,
    gap_note: String,
    topic_descriptor: String,
    override_queries_text: String,
    lookback_days: i32,
    refined_question: String,
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    loaded_id: Option<i64>,
    form: Form,
    new_name: String,
    status: Option<String>,
    busy: bool,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let channel = self.channel.get_or_insert_with(MsgChannel::default);
        while let Ok(msg) = channel.rx.try_recv() {
            match msg {
                Msg::Loaded(ws) | Msg::Saved(ws) => {
                    self.form = form_from(&ws);
                    self.loaded_id = Some(ws.id);
                    self.busy = false;
                    if matches!(self.status.as_deref(), Some(s) if s.starts_with("Saving")) {
                        self.status = Some("Saved.".to_string());
                    }
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

        // Load the active workspace into the form when the selection changes.
        let active = ctx.active_workspace_id;
        if self.loaded_id != Some(active) && !self.busy {
            self.load(ctx, active);
        }

        ui.heading("Input Set");
        ui.label("Create or edit a research workspace. Seed concepts drive the gather queries; the gap note feeds the Gap Bridge.");
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("workspace_form")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.form.name);
                    ui.end_row();

                    ui.label("Primary question");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.primary_question)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Topic descriptor");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.topic_descriptor)
                            .hint_text("short natural-language topic, used by screening + prompt rewrite")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Seed concepts\n(one per line)");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.seed_concepts_text)
                            .desired_rows(6)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Gap note");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.gap_note)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Lookback (days)");
                    ui.add(egui::DragValue::new(&mut self.form.lookback_days).range(1..=3650));
                    ui.end_row();

                    ui.label("Override queries\n(optional, one per line)");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.override_queries_text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });

            if !self.form.refined_question.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Refined question (from Gap Bridge):").strong());
                ui.label(&self.form.refined_question);
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy, egui::Button::new("Save")).clicked() {
                    self.save(ctx, active);
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Run gather (all sources)"))
                    .clicked()
                {
                    self.run_gather(ctx, active);
                }
            });

            ui.add_space(14.0);
            ui.separator();
            ui.label(egui::RichText::new("Create a new workspace").strong());
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.new_name);
                if ui
                    .add_enabled(!self.busy && !self.new_name.trim().is_empty(), egui::Button::new("Create"))
                    .clicked()
                {
                    self.create(ctx);
                }
            });

            if let Some(status) = &self.status {
                ui.add_space(8.0);
                ui.label(status);
            }

            let caps = "Gather caps: each source returns ~50 candidates per query; PMC only looks back 30 days. A long lookback broadens coverage across sources rather than exhaustively.";
            ui.add_space(8.0);
            ui.label(egui::RichText::new(caps).weak().italics());
        });
    }

    fn load(&mut self, ctx: &PanelCtx<'_>, id: i64) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.loaded_id = Some(id); // optimistic: avoid re-spawning each frame
        let tx = channel.tx.clone();
        let svc = ctx.state.workspace_service.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.get(id).await {
                Ok(ws) => tx.send(Msg::Loaded(ws)),
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
        let update = WorkspaceUpdate {
            name: Some(self.form.name.clone()),
            primary_question: Some(self.form.primary_question.clone()),
            gap_note: Some(self.form.gap_note.clone()),
            refined_question: None,
            topic_descriptor: Some(self.form.topic_descriptor.clone()),
            seed_concepts: Some(lines(&self.form.seed_concepts_text)),
            override_queries: Some(lines(&self.form.override_queries_text)),
            lookback_days: Some(self.form.lookback_days.max(1)),
        };
        ctx.handle.spawn(async move {
            let _ = match svc.update(id, update).await {
                Ok(ws) => tx.send(Msg::Saved(ws)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn create(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.status = Some("Creating…".to_string());
        let tx = channel.tx.clone();
        let svc = ctx.state.workspace_service.clone();
        let request = WorkspaceCreate {
            name: self.new_name.trim().to_string(),
            primary_question: String::new(),
            gap_note: String::new(),
            topic_descriptor: String::new(),
            seed_concepts: Vec::new(),
            override_queries: Vec::new(),
            lookback_days: 180,
        };
        self.new_name.clear();
        ctx.handle.spawn(async move {
            match svc.create(request).await {
                Ok(ws) => {
                    let _ = tx.send(Msg::Status(format!(
                        "Created workspace '{}' (id {}). Select it in the top bar to make it active.",
                        ws.name, ws.id
                    )));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(err.to_string()));
                }
            }
        });
    }

    fn run_gather(&mut self, ctx: &PanelCtx<'_>, id: i64) {
        self.busy = true;
        self.status = Some("Starting gather…".to_string());
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let jobs = ctx.state.job_service.clone();
        let days = self.form.lookback_days.max(1);
        ctx.handle.spawn(async move {
            let _ = match jobs.enqueue_source("all", days, id).await {
                Ok(run) => tx.send(Msg::Status(format!(
                    "Gather started for all sources (run {}).",
                    run.run_id
                ))),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }
}

fn form_from(ws: &Workspace) -> Form {
    Form {
        name: ws.name.clone(),
        primary_question: ws.primary_question.clone(),
        seed_concepts_text: ws.seed_concepts.join("\n"),
        gap_note: ws.gap_note.clone(),
        topic_descriptor: ws.topic_descriptor.clone(),
        override_queries_text: ws.override_queries.join("\n"),
        lookback_days: ws.lookback_days,
        refined_question: ws.refined_question.clone(),
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}
