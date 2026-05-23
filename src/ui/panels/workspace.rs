use super::{MsgChannel, PanelCtx};
use crate::{
    models::workspace::{Workspace, WorkspaceCreate, WorkspaceUpdate},
    ui::style,
};

enum Msg {
    Loaded(Workspace),
    Saved(Workspace),
    GatherStarted(Workspace, String),
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
    /// Auto-gather cadence in days; `None` = off.
    cadence_days: Option<i32>,
    /// When due, gather automatically vs. ask first.
    cadence_auto: bool,
    last_gathered_at: Option<String>,
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
                Msg::GatherStarted(ws, text) => {
                    self.form = form_from(&ws);
                    self.loaded_id = Some(ws.id);
                    self.status = Some(text);
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

        // Load the active workspace into the form when the selection changes.
        let active = ctx.active_workspace_id;
        if self.loaded_id != Some(active) && !self.busy {
            self.load(ctx, active);
        }

        style::panel_header_icon(
            ui,
            style::icon::SLIDERS_HORIZONTAL,
            ctx.t("Input Set"),
            Some(ctx.t(
                "Set up what ResearchWiki gathers and studies. These settings drive every gather and the wiki it builds.",
            )),
        );

        egui::ScrollArea::vertical().show(ui, |ui| {
            style::section_heading(ui, ctx.t("Research"));
            egui::Grid::new("workspace_research")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(ctx.t("Research name"));
                    ui.text_edit_singleline(&mut self.form.name);
                    ui.end_row();

                    ui.label(ctx.t("What question are you trying to answer?"));
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.primary_question)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label(ctx.t("Key topics & search terms\n(one per line)"));
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.seed_concepts_text)
                            .desired_rows(6)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label(ctx.t("Known gap / what's missing (optional)"));
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.gap_note)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });

            style::section_break(ui);
            egui::CollapsingHeader::new(ctx.t("Advanced settings"))
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("workspace_advanced")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(ctx.t("Days to look back"));
                            ui.add(
                                egui::DragValue::new(&mut self.form.lookback_days).range(1..=3650),
                            );
                            ui.end_row();

                            ui.label(ctx.t("Topic descriptor\n(natural-language topic)"));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.form.topic_descriptor)
                                    .hint_text(ctx.t("used by screening + prompt rewrite"))
                                    .desired_width(f32::INFINITY),
                            );
                            ui.end_row();

                            ui.label(ctx.t("Override search queries\n(optional, one per line)"));
                            ui.add(
                                egui::TextEdit::multiline(&mut self.form.override_queries_text)
                                    .desired_rows(3)
                                    .desired_width(f32::INFINITY),
                            );
                            ui.end_row();
                        });
                    style::muted_label(
                        ui,
                        ctx.t(
                            "Override queries replace your key topics when searching. Leave blank to use the topics above.",
                        ),
                    );
                });

            style::section_break(ui);
            self.show_schedule_section(ui, ctx);

            style::section_break(ui);
            style::section_heading(ui, ctx.t("Actions"));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled_ui(!self.busy, |ui| style::secondary_button(ui, ctx.t("Save")))
                    .inner
                    .clicked()
                {
                    self.save(ctx, active);
                }
                if ui
                    .add_enabled_ui(!self.busy, |ui| {
                        style::primary_button(ui, ctx.t("Save & start gathering"))
                    })
                    .inner
                    .clicked()
                {
                    self.run_gather(ctx, active);
                }
            });

            ui.add_space(6.0);
            style::muted_label(
                ui,
                ctx.t(
                    "Save stores this research set. The Gather tab and the daily scheduler both use it to build search queries and prompts — saving alone does not gather.",
                ),
            );
            style::muted_label(
                ui,
                ctx.t(
                    "Save & start gathering also runs one gather now across all sources, looking back the days set in Advanced settings.",
                ),
            );
            style::muted_label(
                ui,
                ctx.t("To gather automatically on a schedule, set daily times in Settings → Scheduler."),
            );

            if let Some(status) = &self.status {
                ui.add_space(8.0);
                ui.label(status);
            }

            if !self.form.refined_question.is_empty() {
                style::section_break(ui);
                ui.label(egui::RichText::new(ctx.t("Refined question from Gap Bridge")).strong());
                style::body_label(ui, self.form.refined_question.as_str());
            }

            style::section_break(ui);
            style::section_heading(ui, ctx.t("Create another research set"));
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.new_name);
                if ui
                    .add_enabled(
                        !self.busy && !self.new_name.trim().is_empty(),
                        egui::Button::new(ctx.t("Create")),
                    )
                    .clicked()
                {
                    self.create(ctx);
                }
            });
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
        let update = self.update_request();
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
        self.status = Some("Saving input set before gather…".to_string());
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.workspace_service.clone();
        let jobs = ctx.state.job_service.clone();
        let update = self.update_request();
        ctx.handle.spawn(async move {
            let result = async {
                let ws = svc.update(id, update).await?;
                let run = jobs
                    .enqueue_source("all", ws.lookback_days.max(1), id)
                    .await?;
                Ok::<_, crate::error::AppError>((ws, run.run_id))
            }
            .await;
            let _ = match result {
                Ok((ws, run_id)) => tx.send(Msg::GatherStarted(
                    ws,
                    format!("Saved input set, then started gather for all sources (run {run_id})."),
                )),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn show_schedule_section(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Gathering schedule"));
        style::muted_label(
            ui,
            ctx.t(
                "Optionally gather this research set on a cadence. Checked when you open the app and periodically while it's open (the app must be running or in the tray).",
            ),
        );
        ui.add_space(4.0);

        let mut enabled = self.form.cadence_days.is_some();
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut enabled, ctx.t("Auto-gather every"))
                .changed()
            {
                self.form.cadence_days = enabled
                    .then(|| self.form.cadence_days.unwrap_or(7).max(1))
                    .or(None);
            }
            let mut days = self.form.cadence_days.unwrap_or(7).max(1);
            if ui
                .add_enabled(enabled, egui::DragValue::new(&mut days).range(1..=3650))
                .changed()
                && enabled
            {
                self.form.cadence_days = Some(days.max(1));
            }
            ui.label(ctx.t("days"));
        });

        if enabled {
            ui.horizontal(|ui| {
                ui.label(ctx.t("When due:"));
                ui.radio_value(&mut self.form.cadence_auto, false, ctx.t("Ask me first"));
                ui.radio_value(
                    &mut self.form.cadence_auto,
                    true,
                    ctx.t("Gather automatically"),
                );
            });
            style::muted_label(
                ui,
                ctx.t("Auto-gather looks back far enough to cover the gap since the last run."),
            );
        }

        let last = self
            .form
            .last_gathered_at
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let last_text = match last {
            Some(value) => value.to_string(),
            None => ctx.t("never").to_string(),
        };
        ui.add_space(4.0);
        style::muted_label(ui, format!("{}: {last_text}", ctx.t("Last gathered")));
    }

    fn update_request(&self) -> WorkspaceUpdate {
        WorkspaceUpdate {
            name: Some(self.form.name.clone()),
            primary_question: Some(self.form.primary_question.clone()),
            gap_note: Some(self.form.gap_note.clone()),
            refined_question: None,
            topic_descriptor: Some(self.form.topic_descriptor.clone()),
            seed_concepts: Some(lines(&self.form.seed_concepts_text)),
            override_queries: Some(lines(&self.form.override_queries_text)),
            lookback_days: Some(self.form.lookback_days.max(1)),
            cadence_days: Some(self.form.cadence_days),
            cadence_auto: Some(self.form.cadence_auto),
        }
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
        cadence_days: ws.cadence_days,
        cadence_auto: ws.cadence_auto,
        last_gathered_at: ws.last_gathered_at.clone(),
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}
