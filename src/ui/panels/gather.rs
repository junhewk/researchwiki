use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use egui_extras::{Column, TableBuilder};

use crate::{
    models::job::{JobEventResponse, JobRunDetailResponse, JobRunResponse},
    services::pipeline::{GATHER_SOURCE_IDS, source_label},
};

use super::{MsgChannel, PanelCtx};

const HISTORY_LIMIT: u32 = 50;
const ACTIVE_STATUSES: &[&str] = &["queued", "running"];

enum Msg {
    JobsLoaded(Vec<JobRunResponse>),
    JobDetail(JobRunDetailResponse),
    JobStarted(JobRunResponse),
    JobCancelled(JobRunResponse),
    Error(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,

    days_back: i32,
    jobs: Vec<JobRunResponse>,
    selected_run_id: Option<String>,
    detail: Option<JobRunDetailResponse>,

    loading: bool,
    action_in_flight: bool,
    last_refresh: Option<Instant>,
    notice: Option<(NoticeKind, String)>,
}

#[derive(Clone, Copy)]
enum NoticeKind {
    Success,
    Error,
}

impl Default for NoticeKind {
    fn default() -> Self {
        Self::Success
    }
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.channel.is_none() {
            self.channel = Some(MsgChannel::default());
        }
        self.drain();
        if !self.initialized {
            self.initialized = true;
            self.days_back = 2;
            self.refresh(ctx);
        }

        ui.heading("Gather");
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.show_controls(ui, ctx);
                ui.add_space(8.0);
                self.show_notice(ui);
                ui.separator();
                self.show_active_runs(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                self.show_history(ui, ctx);
            });

        if self.detail.is_some() {
            self.show_detail_window(ui.ctx());
        }

        if self.has_active_jobs() {
            ui.ctx().request_repaint_after(Duration::from_secs(2));
            let refresh_due = self
                .last_refresh
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(2));
            if !self.loading && refresh_due {
                self.refresh(ctx);
            }
        }
    }

    fn drain(&mut self) {
        let mut drained = Vec::new();
        if let Some(channel) = self.channel.as_mut() {
            while let Ok(msg) = channel.rx.try_recv() {
                drained.push(msg);
            }
        }

        for msg in drained {
            match msg {
                Msg::JobsLoaded(jobs) => {
                    self.jobs = jobs;
                    self.loading = false;
                }
                Msg::JobDetail(detail) => {
                    self.selected_run_id = Some(detail.run.run_id.clone());
                    self.detail = Some(detail);
                    self.loading = false;
                }
                Msg::JobStarted(run) => {
                    self.action_in_flight = false;
                    self.notice = Some((
                        NoticeKind::Success,
                        format!("Started {} gather ({})", label_for(&run.source), run.run_id),
                    ));
                    upsert_job(&mut self.jobs, run);
                }
                Msg::JobCancelled(run) => {
                    self.action_in_flight = false;
                    self.notice =
                        Some((NoticeKind::Success, format!("Cancelled run {}", run.run_id)));
                    upsert_job(&mut self.jobs, run);
                }
                Msg::Error(err) => {
                    self.loading = false;
                    self.action_in_flight = false;
                    self.notice = Some((NoticeKind::Error, err));
                }
            }
        }
    }

    fn show_controls(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Days back");
            ui.add(
                egui::DragValue::new(&mut self.days_back)
                    .range(1..=30)
                    .speed(0.2),
            );

            if ui
                .add_enabled(!self.action_in_flight, egui::Button::new("Refresh"))
                .clicked()
            {
                self.refresh(ctx);
            }

            if self.loading {
                ui.spinner();
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let running_sources = self.running_sources();
            let all_busy = self.source_is_busy("all", &running_sources);
            if ui
                .add_enabled(
                    !self.action_in_flight && !all_busy,
                    egui::Button::new("Run all sources"),
                )
                .clicked()
            {
                self.start_job(ctx, "all");
            }

            for &source in GATHER_SOURCE_IDS {
                let busy = self.source_is_busy(source, &running_sources);
                let label = format!("Run {}", label_for(source));
                if ui
                    .add_enabled(!self.action_in_flight && !busy, egui::Button::new(label))
                    .clicked()
                {
                    self.start_job(ctx, source);
                }
            }
        });
    }

    fn show_notice(&self, ui: &mut egui::Ui) {
        let Some((kind, msg)) = &self.notice else {
            return;
        };
        let color = match kind {
            NoticeKind::Success => egui::Color32::from_rgb(0, 130, 0),
            NoticeKind::Error => egui::Color32::RED,
        };
        ui.colored_label(color, msg);
    }

    fn show_active_runs(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("Active runs");
        let active = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| is_active_status(&job.status))
            .map(|(idx, _)| idx)
            .collect::<Vec<_>>();

        if active.is_empty() {
            ui.label("No active gather jobs.");
            return;
        }

        let mut detail_request: Option<String> = None;
        let mut cancel_request: Option<String> = None;

        egui::Grid::new("gather-active-runs-grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for idx in active {
                    let Some(job) = self.jobs.get(idx) else {
                        continue;
                    };
                    ui.vertical(|ui| {
                        ui.strong(format!("{} ({})", label_for(&job.source), job.status));
                        ui.label(format!("Run: {}", job.run_id));
                        ui.label(format!(
                            "Step: {}",
                            job.current_step.as_deref().unwrap_or("queued")
                        ));
                        if let Some(item) = &job.current_item {
                            ui.add(egui::Label::new(format!("Current: {item}")).truncate());
                        }
                    });
                    ui.vertical(|ui| {
                        ui.label(format!(
                            "found {} | screened {} | relevant {}",
                            job.candidates_found, job.candidates_screened, job.candidates_relevant
                        ));
                        ui.label(format!(
                            "fetched {} | evaluated {} | saved {} | skipped {} | errors {}",
                            job.candidates_fetched,
                            job.candidates_evaluated,
                            job.candidates_saved,
                            job.candidates_skipped,
                            job.errors
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("Details").clicked() {
                                detail_request = Some(job.run_id.clone());
                            }
                            if ui
                                .add_enabled(!self.action_in_flight, egui::Button::new("Cancel"))
                                .clicked()
                            {
                                cancel_request = Some(job.run_id.clone());
                            }
                        });
                    });
                    ui.end_row();
                }
            });

        if let Some(run_id) = detail_request {
            self.load_detail(ctx, &run_id);
        }
        if let Some(run_id) = cancel_request {
            self.cancel_job(ctx, &run_id);
        }
    }

    fn show_history(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.heading("Run history");
        let text_height = egui::TextStyle::Body.resolve(ui.style()).size + 8.0;
        let rows = self.jobs.len();
        let max_scroll_height = ui.available_height().max(180.0);
        let mut detail_request: Option<String> = None;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(130.0).at_least(90.0))
            .column(Column::initial(85.0).at_least(70.0))
            .column(Column::initial(95.0).at_least(70.0))
            .column(Column::remainder().at_least(160.0))
            .column(Column::initial(92.0).at_least(70.0))
            .column(Column::initial(92.0).at_least(70.0))
            .column(Column::initial(70.0).at_least(60.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(max_scroll_height)
            .header(text_height, |mut header| {
                header.col(|ui| {
                    ui.strong("Requested");
                });
                header.col(|ui| {
                    ui.strong("Source");
                });
                header.col(|ui| {
                    ui.strong("Status");
                });
                header.col(|ui| {
                    ui.strong("Step");
                });
                header.col(|ui| {
                    ui.strong("Found");
                });
                header.col(|ui| {
                    ui.strong("Saved");
                });
                header.col(|ui| {
                    ui.strong("");
                });
            })
            .body(|body| {
                body.rows(text_height, rows, |mut row| {
                    let idx = row.index();
                    let Some(job) = self.jobs.get(idx) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.label(job.requested_at.as_deref().unwrap_or("—"));
                    });
                    row.col(|ui| {
                        ui.label(label_for(&job.source));
                    });
                    row.col(|ui| {
                        ui.label(&job.status);
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(job.current_step.as_deref().unwrap_or("—")).truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.label(job.candidates_found.to_string());
                    });
                    row.col(|ui| {
                        ui.label(job.candidates_saved.to_string());
                    });
                    row.col(|ui| {
                        if ui.small_button("Open").clicked() {
                            detail_request = Some(job.run_id.clone());
                        }
                    });
                });
            });

        if let Some(run_id) = detail_request {
            self.load_detail(ctx, &run_id);
        }
    }

    fn show_detail_window(&mut self, egui_ctx: &egui::Context) {
        let mut open = true;
        let mut clear_detail = false;

        let Some(detail) = self.detail.as_ref() else {
            return;
        };

        egui::Window::new("Gather run detail")
            .resizable(true)
            .default_width(680.0)
            .default_height(620.0)
            .open(&mut open)
            .show(egui_ctx, |ui| {
                let run = &detail.run;
                ui.heading(format!("{} gather", label_for(&run.source)));
                ui.label(format!("Run: {}", run.run_id));
                ui.label(format!("Status: {}", run.status));
                if let Some(error) = &run.error_message {
                    ui.colored_label(egui::Color32::RED, error);
                }

                ui.add_space(6.0);
                egui::Grid::new("gather-detail-counters-grid")
                    .num_columns(4)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        counter_cell(ui, "Found", run.candidates_found);
                        counter_cell(ui, "Screened", run.candidates_screened);
                        counter_cell(ui, "Relevant", run.candidates_relevant);
                        counter_cell(ui, "Fetched", run.candidates_fetched);
                        ui.end_row();
                        counter_cell(ui, "Evaluated", run.candidates_evaluated);
                        counter_cell(ui, "Saved", run.candidates_saved);
                        counter_cell(ui, "Embedded", run.candidates_embedded);
                        counter_cell(ui, "Errors", run.errors);
                    });

                ui.add_space(8.0);
                ui.heading("Events");
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for event in &detail.events {
                            event_row(ui, event);
                        }
                    });
            });

        if !open {
            clear_detail = true;
        }
        if clear_detail {
            self.detail = None;
        }
    }

    fn refresh(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.loading = true;
        self.last_refresh = Some(Instant::now());
        let tx = channel.tx.clone();
        let jobs = ctx.state.job_service.clone();
        ctx.handle.spawn(async move {
            let result = jobs.list_jobs(HISTORY_LIMIT).await;
            let _ = match result {
                Ok(items) => tx.send(Msg::JobsLoaded(items)),
                Err(err) => tx.send(Msg::Error(format!("Failed to load gather jobs: {err}"))),
            };
        });
    }

    fn start_job(&mut self, ctx: &PanelCtx<'_>, source: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let jobs = ctx.state.job_service.clone();
        let source = source.to_string();
        let days_back = self.days_back;
        ctx.handle.spawn(async move {
            let result = jobs.enqueue_source(&source, days_back).await;
            match result {
                Ok(run) => {
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(format!(
                        "Started {} gather ({})",
                        label_for(&run.source),
                        run.run_id
                    )));
                    let _ = tx.send(Msg::JobStarted(run));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Start failed: {err}")));
                }
            }
        });
    }

    fn cancel_job(&mut self, ctx: &PanelCtx<'_>, run_id: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let jobs = ctx.state.job_service.clone();
        let run_id = run_id.to_string();
        ctx.handle.spawn(async move {
            let result = jobs.cancel_job(&run_id).await;
            match result {
                Ok(run) => {
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(format!(
                        "Cancelled gather run {}",
                        run.run_id
                    )));
                    let _ = tx.send(Msg::JobCancelled(run));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Cancel failed: {err}")));
                }
            }
        });
    }

    fn load_detail(&mut self, ctx: &PanelCtx<'_>, run_id: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.loading = true;
        let tx = channel.tx.clone();
        let jobs = ctx.state.job_service.clone();
        let run_id = run_id.to_string();
        ctx.handle.spawn(async move {
            let result = jobs.get_job(&run_id).await;
            let _ = match result {
                Ok(detail) => tx.send(Msg::JobDetail(detail)),
                Err(err) => tx.send(Msg::Error(format!("Failed to load run detail: {err}"))),
            };
        });
    }

    fn has_active_jobs(&self) -> bool {
        self.jobs
            .iter()
            .any(|job| is_active_status(job.status.as_str()))
    }

    fn running_sources(&self) -> HashSet<String> {
        self.jobs
            .iter()
            .filter(|job| is_active_status(job.status.as_str()))
            .map(|job| job.source.clone())
            .collect()
    }

    fn source_is_busy(&self, source: &str, running_sources: &HashSet<String>) -> bool {
        if source == "all" {
            !running_sources.is_empty()
        } else {
            running_sources.contains(source) || running_sources.contains("all")
        }
    }
}

fn upsert_job(jobs: &mut Vec<JobRunResponse>, run: JobRunResponse) {
    if let Some(existing) = jobs.iter_mut().find(|job| job.run_id == run.run_id) {
        *existing = run;
    } else {
        jobs.insert(0, run);
    }
}

fn is_active_status(status: &str) -> bool {
    ACTIVE_STATUSES.contains(&status)
}

fn label_for(source: &str) -> &str {
    if source == "all" {
        "All sources"
    } else {
        source_label(source).unwrap_or(source)
    }
}

fn counter_cell(ui: &mut egui::Ui, label: &str, value: i32) {
    ui.vertical(|ui| {
        ui.strong(value.to_string());
        ui.label(label);
    });
}

fn event_row(ui: &mut egui::Ui, event: &JobEventResponse) {
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong(&event.event_type);
            ui.label(&event.created_at);
        });
        if let Some(payload) = &event.payload_json {
            ui.add(egui::Label::new(egui::RichText::new(payload).monospace()).wrap());
        }
    });
}
