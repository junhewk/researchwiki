use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use chrono::{Duration as ChronoDuration, Local, Timelike};
use egui_extras::{Column, TableBuilder};

use crate::{
    models::{
        job::{JobEventResponse, JobRunDetailResponse, JobRunResponse},
        knowledge_graph::{
            KGBackfillStatusResponse, KGFullBackfillStatus, KGStatsResponse,
            KGSynthesisCompileStatus,
        },
        settings::{SchedulerSettings, SchedulerStatusResponse, SettingsUpdate},
    },
    runtime::UiEvent,
    services::pipeline::{GATHER_SOURCE_IDS, source_label},
    ui::{style, toast::ToastKind},
};

use super::{MsgChannel, PanelCtx};

const HISTORY_LIMIT: u32 = 50;
const ACTIVE_STATUSES: &[&str] = &["queued", "running"];
const SCHEDULER_SOURCE_IDS: &[&str] = &["arxiv", "pmc", "pubmed"];
const JOB_POLL_INTERVAL: Duration = Duration::from_secs(2);
const OPS_POLL_INTERVAL: Duration = Duration::from_secs(5);

enum Msg {
    JobsLoaded(Vec<JobRunResponse>),
    JobDetail(JobRunDetailResponse),
    JobStarted(JobRunResponse),
    JobCancelled(JobRunResponse),
    OperationsLoaded(KgSnapshot),
    OperationNotice(String),
    SchedulerLoaded(SchedulerSettings, SchedulerStatusResponse),
    SchedulerArmed(
        SchedulerSettings,
        SchedulerStatusResponse,
        SchedulerTestState,
    ),
    SchedulerRestored(SchedulerSettings, SchedulerStatusResponse),
    Error(String),
}

struct KgSnapshot {
    stats: KGStatsResponse,
    backfill: KGBackfillStatusResponse,
    synthesis: KGSynthesisCompileStatus,
    full: KGFullBackfillStatus,
}

struct SchedulerTestState {
    source: String,
    backup: SchedulerSettings,
    known_run_ids: HashSet<String>,
    scheduled_for: String,
    detected_run_id: Option<String>,
    restore_requested: bool,
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,
    /// Workspace the current jobs/statuses belong to; a mismatch clears them
    /// and forces every poller to refetch, keeping the user's inputs.
    loaded_workspace: Option<i64>,

    days_back: i32,
    jobs: Vec<JobRunResponse>,
    selected_run_id: Option<String>,
    detail: Option<JobRunDetailResponse>,

    test_source: String,
    scheduler_test_source: String,
    kg_batch_size: i32,
    wiki_batch_size: i32,
    kg_stats: Option<KGStatsResponse>,
    kg_backfill_status: Option<KGBackfillStatusResponse>,
    kg_synthesis_status: Option<KGSynthesisCompileStatus>,
    kg_full_status: Option<KGFullBackfillStatus>,
    scheduler_settings: Option<SchedulerSettings>,
    scheduler_status: Option<SchedulerStatusResponse>,
    scheduler_test: Option<SchedulerTestState>,

    loading: bool,
    ops_loading: bool,
    scheduler_loading: bool,
    action_in_flight: bool,
    pending_ops_refresh: bool,
    last_refresh: Option<Instant>,
    last_ops_refresh: Option<Instant>,
    last_scheduler_refresh: Option<Instant>,
    /// Persistent error from the last failed operation. Successes go straight
    /// to the app toast stack instead.
    error: Option<String>,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.channel.is_none() {
            self.channel = Some(MsgChannel::default());
        }
        self.drain(ctx);
        if !self.initialized {
            self.initialized = true;
            self.test_source = "arxiv".to_string();
            self.scheduler_test_source = "arxiv".to_string();
            self.days_back = 2;
            self.kg_batch_size = 5;
            self.wiki_batch_size = 5;
        }
        if self.loaded_workspace != Some(ctx.active_workspace_id) {
            self.loaded_workspace = Some(ctx.active_workspace_id);
            self.jobs.clear();
            self.selected_run_id = None;
            self.detail = None;
            self.kg_stats = None;
            self.kg_backfill_status = None;
            self.kg_synthesis_status = None;
            self.kg_full_status = None;
            self.scheduler_settings = None;
            self.scheduler_status = None;
            self.last_refresh = None;
            self.last_ops_refresh = None;
            self.last_scheduler_refresh = None;
            self.refresh(ctx);
            self.refresh_operations(ctx);
            self.refresh_scheduler(ctx);
        }
        self.restore_scheduler_after_detection(ctx);
        if self.pending_ops_refresh && !self.ops_loading {
            self.pending_ops_refresh = false;
            self.refresh_operations(ctx);
        }

        style::panel_header_icon(ui, style::icon::DOWNLOAD_SIMPLE, ctx.t("Gather"), None);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                style::section_heading(ui, ctx.t("Run gather"));
                self.show_controls(ui, ctx);
                self.show_error(ui, ctx);
                style::section_break(ui);
                self.show_active_runs(ui, ctx);
                style::section_break(ui);
                self.show_history(ui, ctx);
                style::section_break(ui);
                self.show_kg_ops(ui, ctx);
                if dev_tools_enabled() {
                    style::section_break(ui);
                    egui::CollapsingHeader::new(ctx.t("Advanced diagnostics"))
                        .default_open(false)
                        .show(ui, |ui| {
                            self.show_pipeline_test(ui, ctx);
                            style::section_break(ui);
                            self.show_scheduler_check(ui, ctx);
                        });
                }
            });

        if self.detail.is_some() {
            self.show_detail_window(ui.ctx());
        }

        if self.should_poll() {
            ui.ctx().request_repaint_after(JOB_POLL_INTERVAL);
            let refresh_due = self
                .last_refresh
                .is_none_or(|last| last.elapsed() >= JOB_POLL_INTERVAL);
            if !self.loading && refresh_due {
                self.refresh(ctx);
            }

            let ops_due = self
                .last_ops_refresh
                .is_none_or(|last| last.elapsed() >= OPS_POLL_INTERVAL);
            if !self.ops_loading && ops_due {
                self.refresh_operations(ctx);
            }

            let scheduler_due = self
                .last_scheduler_refresh
                .is_none_or(|last| last.elapsed() >= OPS_POLL_INTERVAL);
            if !self.scheduler_loading && scheduler_due {
                self.refresh_scheduler(ctx);
            }
        }
    }

    fn drain(&mut self, ctx: &PanelCtx<'_>) {
        let mut drained = Vec::new();
        if let Some(channel) = self.channel.as_mut() {
            while let Ok(msg) = channel.rx.try_recv() {
                drained.push(msg);
            }
        }

        let toast = |kind: ToastKind, message: String| {
            let _ = ctx.ui_tx.send(UiEvent::Toast { kind, message });
        };

        for msg in drained {
            match msg {
                Msg::JobsLoaded(jobs) => {
                    let had_active_jobs = self.has_active_jobs();
                    self.jobs = jobs;
                    if had_active_jobs && !self.has_active_jobs() {
                        self.pending_ops_refresh = true;
                    }
                    if let Some(message) = self.detect_scheduler_run() {
                        toast(ToastKind::Success, message);
                    }
                    self.loading = false;
                }
                Msg::JobDetail(detail) => {
                    self.selected_run_id = Some(detail.run.run_id.clone());
                    self.detail = Some(detail);
                    self.loading = false;
                }
                Msg::JobStarted(run) => {
                    self.action_in_flight = false;
                    toast(
                        ToastKind::Success,
                        format!("Started {} gather ({})", label_for(&run.source), run.run_id),
                    );
                    upsert_job(&mut self.jobs, run);
                    self.last_refresh = None;
                }
                Msg::JobCancelled(run) => {
                    self.action_in_flight = false;
                    toast(ToastKind::Success, format!("Cancelled run {}", run.run_id));
                    upsert_job(&mut self.jobs, run);
                    self.last_refresh = None;
                }
                Msg::OperationsLoaded(snapshot) => {
                    self.kg_stats = Some(snapshot.stats);
                    self.kg_backfill_status = Some(snapshot.backfill);
                    self.kg_synthesis_status = Some(snapshot.synthesis);
                    self.kg_full_status = Some(snapshot.full);
                    self.ops_loading = false;
                }
                Msg::OperationNotice(message) => {
                    self.action_in_flight = false;
                    toast(ToastKind::Info, message);
                }
                Msg::SchedulerLoaded(settings, status) => {
                    self.scheduler_settings = Some(settings);
                    self.scheduler_status = Some(status);
                    self.scheduler_loading = false;
                }
                Msg::SchedulerArmed(settings, status, test) => {
                    self.scheduler_settings = Some(settings);
                    self.scheduler_status = Some(status);
                    self.scheduler_test = Some(test);
                    self.action_in_flight = false;
                    toast(
                        ToastKind::Info,
                        "Scheduler test armed for the next minute (local time)".to_string(),
                    );
                }
                Msg::SchedulerRestored(settings, status) => {
                    let message = self
                        .scheduler_test
                        .as_ref()
                        .and_then(|test| {
                            test.detected_run_id.as_ref().map(|run_id| {
                                format!(
                                    "Scheduler fired {} ({run_id}); settings restored",
                                    label_for(&test.source)
                                )
                            })
                        })
                        .unwrap_or_else(|| "Scheduler settings restored".to_string());
                    self.scheduler_settings = Some(settings);
                    self.scheduler_status = Some(status);
                    self.scheduler_test = None;
                    self.action_in_flight = false;
                    toast(ToastKind::Success, message);
                }
                Msg::Error(err) => {
                    self.loading = false;
                    self.ops_loading = false;
                    self.scheduler_loading = false;
                    self.action_in_flight = false;
                    self.error = Some(err);
                }
            }
        }
    }

    fn show_pipeline_test(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Pipeline smoke test"));
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Source"));
            source_combo(
                ui,
                "gather-pipeline-source-combo",
                &mut self.test_source,
                GATHER_SOURCE_IDS,
            );

            ui.label(ctx.t("Days back"));
            ui.add(
                egui::DragValue::new(&mut self.days_back)
                    .range(1..=30)
                    .speed(0.2),
            );

            let running_sources = self.running_sources();
            let busy = self.source_is_busy(&self.test_source, &running_sources);
            if ui
                .add_enabled(
                    !self.action_in_flight && !busy,
                    egui::Button::new(ctx.t("Run gather + KG smoke test")),
                )
                .clicked()
            {
                let source = self.test_source.clone();
                self.start_job(ctx, &source);
            }
        });

        ui.add_space(4.0);
        self.show_kg_stats(ui);
    }

    fn show_kg_ops(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Knowledge graph / wiki backfill"));

        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("KG batch"));
            style::help_icon(
                ui,
                ctx.t("Articles processed per backfill batch. Larger batches finish faster but use more LLM tokens per run."),
            );
            ui.add(
                egui::DragValue::new(&mut self.kg_batch_size)
                    .range(1..=100)
                    .speed(0.5),
            );
            ui.label(ctx.t("Wiki batch"));
            style::help_icon(
                ui,
                ctx.t("Entities synthesized per compile batch. Larger batches finish faster but use more LLM tokens per run."),
            );
            ui.add(
                egui::DragValue::new(&mut self.wiki_batch_size)
                    .range(1..=100)
                    .speed(0.5),
            );

            if ui
                .add_enabled(!self.ops_loading, egui::Button::new(ctx.t("Refresh KG")))
                .clicked()
            {
                self.refresh_operations(ctx);
            }

            let kg_busy = self
                .kg_backfill_status
                .as_ref()
                .is_some_and(|status| status.running);
            if ui
                .add_enabled(
                    !self.action_in_flight && !kg_busy,
                    egui::Button::new(ctx.t("Backfill KG batch")),
                )
                .clicked()
            {
                self.start_kg_backfill(ctx);
            }

            let synthesis_busy = self
                .kg_synthesis_status
                .as_ref()
                .is_some_and(|status| status.running);
            if ui
                .add_enabled(
                    !self.action_in_flight && !synthesis_busy,
                    egui::Button::new(ctx.t("Compile wiki syntheses")),
                )
                .clicked()
            {
                self.start_synthesis(ctx);
            }

            let full_busy = self
                .kg_full_status
                .as_ref()
                .is_some_and(|status| status.running);
            if ui
                .add_enabled(
                    !self.action_in_flight && !full_busy,
                    egui::Button::new(ctx.t("Run full KG + wiki backfill")),
                )
                .clicked()
            {
                self.start_full_backfill(ctx);
            }

            if ui
                .add_enabled(
                    !self.action_in_flight && full_busy,
                    egui::Button::new(ctx.t("Stop full backfill")),
                )
                .clicked()
            {
                self.stop_full_backfill(ctx);
            }

            if self.ops_loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });

        ui.add_space(4.0);
        self.show_kg_statuses(ui);
    }

    fn show_scheduler_check(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Scheduler check"));

        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Scheduled source"));
            source_combo(
                ui,
                "gather-scheduler-source-combo",
                &mut self.scheduler_test_source,
                SCHEDULER_SOURCE_IDS,
            );

            if ui
                .add_enabled(
                    !self.scheduler_loading,
                    egui::Button::new(ctx.t("Refresh scheduler")),
                )
                .clicked()
            {
                self.refresh_scheduler(ctx);
            }

            let scheduler_test_running = self.scheduler_test.is_some();
            if ui
                .add_enabled(
                    !self.action_in_flight && !scheduler_test_running,
                    egui::Button::new(ctx.t("Run scheduled source now")),
                )
                .clicked()
            {
                let source = self.scheduler_test_source.clone();
                self.start_job_with_days(ctx, &source, 2);
            }

            if ui
                .add_enabled(
                    !self.action_in_flight && !scheduler_test_running,
                    egui::Button::new(ctx.t("Arm next-minute scheduler test")),
                )
                .clicked()
            {
                self.arm_scheduler_test(ctx);
            }

            if let Some(test) = &self.scheduler_test {
                let can_restore = !self.action_in_flight && !test.restore_requested;
                if ui
                    .add_enabled(
                        can_restore,
                        egui::Button::new(ctx.t("Restore scheduler settings")),
                    )
                    .clicked()
                {
                    let backup = test.backup.clone();
                    self.restore_scheduler_settings(ctx, backup);
                }
            }

            if self.scheduler_loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });

        ui.add_space(4.0);
        self.show_scheduler_status(ui);
        self.show_scheduler_test(ui);
    }

    fn show_controls(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Days back"));
            style::help_icon(
                ui,
                ctx.t(
                    "Gather caps: each source returns ~50 candidates per query; PMC only looks back 30 days. A long lookback broadens coverage across sources rather than exhaustively.",
                ),
            );
            ui.add(
                egui::DragValue::new(&mut self.days_back)
                    .range(1..=30)
                    .speed(0.2),
            );

            if ui
                .add_enabled(!self.action_in_flight, egui::Button::new(ctx.t("Refresh")))
                .clicked()
            {
                self.refresh(ctx);
            }

            if self.loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let running_sources = self.running_sources();
            let all_busy = self.source_is_busy("all", &running_sources);
            if ui
                .add_enabled_ui(!self.action_in_flight && !all_busy, |ui| {
                    style::primary_button(ui, ctx.t("Run all sources"))
                })
                .inner
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

    fn show_error(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let Some(err) = self.error.clone() else {
            return;
        };
        match style::error_notice(ui, &err, Some(ctx.t("Refresh"))) {
            style::NoticeAction::Retry => {
                self.error = None;
                self.refresh(ctx);
                self.refresh_operations(ctx);
                self.refresh_scheduler(ctx);
            }
            style::NoticeAction::Dismiss => self.error = None,
            style::NoticeAction::None => {}
        }
    }

    fn show_kg_stats(&self, ui: &mut egui::Ui) {
        let Some(stats) = &self.kg_stats else {
            ui.label("KG stats unavailable.");
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Nodes: {}", stats.nodes));
            ui.label(format!("Edges: {}", stats.edges));
            if let Some(error) = &stats.error {
                ui.colored_label(egui::Color32::RED, error);
            } else {
                let types = stats
                    .entity_types
                    .iter()
                    .take(5)
                    .map(|(name, count)| format!("{name}: {count}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if !types.is_empty() {
                    ui.add(egui::Label::new(format!("Types: {types}")).truncate());
                }
            }
        });
    }

    fn show_kg_statuses(&self, ui: &mut egui::Ui) {
        egui::Grid::new("gather-kg-status-grid")
            .num_columns(2)
            .spacing([10.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Backfill");
                if let Some(status) = &self.kg_backfill_status {
                    ui.label(format!(
                        "{} | processed {}/{} | inserted {} | failed {}",
                        running_label(status.running),
                        status.processed,
                        status.total,
                        status.inserted,
                        status.failed
                    ));
                } else {
                    ui.label("unavailable");
                }
                ui.end_row();

                ui.strong("Wiki syntheses");
                if let Some(status) = &self.kg_synthesis_status {
                    ui.label(format!(
                        "{} | processed {}/{} | compiled {} | failed {}",
                        running_label(status.running),
                        status.processed,
                        status.total,
                        status.compiled,
                        status.failed
                    ));
                } else {
                    ui.label("unavailable");
                }
                ui.end_row();

                ui.strong("Full backfill");
                if let Some(status) = &self.kg_full_status {
                    let message = status.message.as_deref().unwrap_or("no message");
                    ui.add(
                        egui::Label::new(format!(
                            "{} | phase {} | KG processed {} inserted {} failed {} | Wiki processed {} compiled {} failed {} | {message}",
                            running_label(status.running),
                            status.phase,
                            status.kg_processed,
                            status.kg_inserted,
                            status.kg_failed,
                            status.wiki_processed,
                            status.wiki_compiled,
                            status.wiki_failed
                        ))
                        .truncate(),
                    );
                } else {
                    ui.label("unavailable");
                }
                ui.end_row();
            });
    }

    fn show_scheduler_status(&self, ui: &mut egui::Ui) {
        let Some(status) = &self.scheduler_status else {
            ui.label("Scheduler status unavailable.");
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Status: {}", status.status));
            if let Some(settings) = &self.scheduler_settings {
                ui.label(format!(
                    "arXiv {:02}:{:02} | PMC {:02}:{:02} | PubMed {:02}:{:02} local",
                    settings.arxiv_schedule_hour,
                    settings.arxiv_schedule_minute,
                    settings.pmc_schedule_hour,
                    settings.pmc_schedule_minute,
                    settings.pubmed_schedule_hour,
                    settings.pubmed_schedule_minute
                ));
            }
        });

        egui::Grid::new("gather-scheduler-status-grid")
            .num_columns(3)
            .spacing([10.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Job");
                ui.strong("ID");
                ui.strong("Next run");
                ui.end_row();

                for job in status.jobs.iter().take(5) {
                    ui.label(&job.name);
                    ui.label(&job.id);
                    ui.label(job.next_run.as_deref().unwrap_or("manual"));
                    ui.end_row();
                }
            });
    }

    fn show_scheduler_test(&self, ui: &mut egui::Ui) {
        let Some(test) = &self.scheduler_test else {
            return;
        };

        ui.add_space(4.0);
        if let Some(run_id) = &test.detected_run_id {
            ui.colored_label(
                egui::Color32::from_rgb(0, 130, 0),
                format!(
                    "Scheduler fired {} at {}; run {} detected. Restoring settings.",
                    label_for(&test.source),
                    test.scheduled_for,
                    run_id
                ),
            );
        } else {
            ui.label(format!(
                "Waiting for {} scheduler at {}. Keep the app running or minimized to tray.",
                label_for(&test.source),
                test.scheduled_for
            ));
        }
    }

    fn show_active_runs(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Active runs"));
        let active = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| is_active_status(&job.status))
            .map(|(idx, _)| idx)
            .collect::<Vec<_>>();

        if active.is_empty() {
            ui.label(ctx.t("No active gather jobs."));
            return;
        }

        let mut detail_request: Option<String> = None;
        let mut cancel_request: Option<String> = None;

        for idx in active {
            let Some(job) = self.jobs.get(idx) else {
                continue;
            };

            style::card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("{} ({})", label_for(&job.source), job.status));
                    ui.separator();
                    ui.label(format!(
                        "found {} | screened {} | relevant {}",
                        job.candidates_found, job.candidates_screened, job.candidates_relevant
                    ));
                    ui.separator();
                    ui.label(format!(
                        "fetched {} | evaluated {} | saved {} | skipped {} | errors {}",
                        job.candidates_fetched,
                        job.candidates_evaluated,
                        job.candidates_saved,
                        job.candidates_skipped,
                        job.errors
                    ));
                });

                ui.add_space(4.0);
                ui.add(egui::Label::new(format!("Run: {}", job.run_id)).wrap());
                ui.add(
                    egui::Label::new(format!(
                        "Step: {}",
                        job.current_step.as_deref().unwrap_or("queued")
                    ))
                    .wrap(),
                );
                if let Some(item) = &job.current_item {
                    ui.add(egui::Label::new(format!("Current: {item}")).wrap());
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(ctx.t("Details")).clicked() {
                        detail_request = Some(job.run_id.clone());
                    }
                    if ui
                        .add_enabled(!self.action_in_flight, egui::Button::new(ctx.t("Cancel")))
                        .clicked()
                    {
                        cancel_request = Some(job.run_id.clone());
                    }
                });
            });
            ui.add_space(6.0);
        }

        if let Some(run_id) = detail_request {
            self.load_detail(ctx, &run_id);
        }
        if let Some(run_id) = cancel_request {
            self.cancel_job(ctx, &run_id);
        }
    }

    fn show_history(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Run history"));
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
                    ui.strong(ctx.t("Requested"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Source"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Status"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Step"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Found"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Saved"));
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
                        if ui.small_button(ctx.t("Open")).clicked() {
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
        if egui_ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.detail = None;
            return;
        }
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
        let workspace_id = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let result = jobs.list_jobs(HISTORY_LIMIT, workspace_id).await;
            let _ = match result {
                Ok(items) => tx.send(Msg::JobsLoaded(items)),
                Err(err) => tx.send(Msg::Error(format!("Failed to load gather jobs: {err}"))),
            };
        });
    }

    fn start_job(&mut self, ctx: &PanelCtx<'_>, source: &str) {
        self.start_job_with_days(ctx, source, self.days_back);
    }

    fn start_job_with_days(&mut self, ctx: &PanelCtx<'_>, source: &str, days_back: i32) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let jobs = ctx.state.job_service.clone();
        let source = source.to_string();
        let workspace_id = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let result = jobs.enqueue_source(&source, days_back, workspace_id).await;
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

    fn refresh_operations(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.ops_loading = true;
        self.last_ops_refresh = Some(Instant::now());
        let tx = channel.tx.clone();
        let kg = ctx.state.knowledge_graph_service.clone();
        let kg_ws = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let result = load_kg_snapshot(&kg, kg_ws).await;
            let _ = match result {
                Ok(snapshot) => tx.send(Msg::OperationsLoaded(snapshot)),
                Err(err) => tx.send(Msg::Error(format!("Failed to load KG status: {err}"))),
            };
        });
    }

    fn refresh_scheduler(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.scheduler_loading = true;
        self.last_scheduler_refresh = Some(Instant::now());
        let tx = channel.tx.clone();
        let settings = ctx.state.settings_service.clone();
        let jobs = ctx.state.job_service.clone();
        ctx.handle.spawn(async move {
            let result = load_scheduler_snapshot(&settings, &jobs).await;
            let _ = match result {
                Ok((settings, status)) => tx.send(Msg::SchedulerLoaded(settings, status)),
                Err(err) => tx.send(Msg::Error(format!("Failed to load scheduler: {err}"))),
            };
        });
    }

    fn start_kg_backfill(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let kg = ctx.state.knowledge_graph_service.clone();
        let kg_ws = ctx.active_workspace_id;
        let batch_size = self.kg_batch_size.max(1) as u32;
        ctx.handle.spawn(async move {
            let result = kg.start_backfill(batch_size, 0).await;
            match result {
                Ok(response) => {
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(response.message.clone()));
                    let _ = tx.send(Msg::OperationNotice(response.message));
                    if let Ok(snapshot) = load_kg_snapshot(&kg, kg_ws).await {
                        let _ = tx.send(Msg::OperationsLoaded(snapshot));
                    }
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("KG backfill failed: {err}")));
                }
            }
        });
    }

    fn start_synthesis(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let kg = ctx.state.knowledge_graph_service.clone();
        let kg_ws = ctx.active_workspace_id;
        let batch_size = self.wiki_batch_size.max(1) as u32;
        ctx.handle.spawn(async move {
            let result = kg
                .start_synthesis_compilation(batch_size, false, None)
                .await;
            match result {
                Ok(response) => {
                    let message = format!(
                        "{} ({} eligible entities)",
                        response.message, response.total_entities
                    );
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(message.clone()));
                    let _ = tx.send(Msg::OperationNotice(message));
                    if let Ok(snapshot) = load_kg_snapshot(&kg, kg_ws).await {
                        let _ = tx.send(Msg::OperationsLoaded(snapshot));
                    }
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Wiki synthesis failed: {err}")));
                }
            }
        });
    }

    fn start_full_backfill(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let kg = ctx.state.knowledge_graph_service.clone();
        let kg_ws = ctx.active_workspace_id;
        let kg_batch_size = self.kg_batch_size.max(1) as u32;
        let wiki_batch_size = self.wiki_batch_size.max(1) as u32;
        ctx.handle.spawn(async move {
            let result = kg.start_full_backfill(kg_batch_size, wiki_batch_size).await;
            match result {
                Ok(response) => {
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(response.message.clone()));
                    let _ = tx.send(Msg::OperationNotice(response.message));
                    if let Ok(snapshot) = load_kg_snapshot(&kg, kg_ws).await {
                        let _ = tx.send(Msg::OperationsLoaded(snapshot));
                    }
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Full backfill failed: {err}")));
                }
            }
        });
    }

    fn stop_full_backfill(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let kg = ctx.state.knowledge_graph_service.clone();
        let kg_ws = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let result = kg.request_full_backfill_stop();
            match result {
                Ok(_) => {
                    let message = "Full backfill stop requested".to_string();
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(message.clone()));
                    let _ = tx.send(Msg::OperationNotice(message));
                    if let Ok(snapshot) = load_kg_snapshot(&kg, kg_ws).await {
                        let _ = tx.send(Msg::OperationsLoaded(snapshot));
                    }
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Stop full backfill failed: {err}")));
                }
            }
        });
    }

    fn arm_scheduler_test(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let settings = ctx.state.settings_service.clone();
        let jobs = ctx.state.job_service.clone();
        let source = self.scheduler_test_source.clone();
        let known_run_ids = self
            .jobs
            .iter()
            .map(|job| job.run_id.clone())
            .collect::<HashSet<_>>();

        ctx.handle.spawn(async move {
            let result = async {
                let current = settings.get_settings().await?.scheduler;
                let backup = current.clone();
                let mut next_settings = current;
                let (hour, minute, scheduled_for) = next_local_minute();
                next_settings.enabled = true;
                set_schedule_for_source(&mut next_settings, &source, hour, minute);
                settings
                    .update_settings(SettingsUpdate {
                        scheduler: Some(next_settings.clone()),
                        ui_language: None,
                    })
                    .await?;
                let status = jobs.scheduler_status(&next_settings);
                Ok::<_, crate::error::AppError>((
                    next_settings,
                    status,
                    SchedulerTestState {
                        source,
                        backup,
                        known_run_ids,
                        scheduled_for,
                        detected_run_id: None,
                        restore_requested: false,
                    },
                ))
            }
            .await;

            match result {
                Ok((settings, status, test)) => {
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(format!(
                        "Scheduler test armed for {} at {}",
                        label_for(&test.source),
                        test.scheduled_for
                    )));
                    let _ = tx.send(Msg::SchedulerArmed(settings, status, test));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Scheduler test failed: {err}")));
                }
            }
        });
    }

    fn restore_scheduler_settings(&mut self, ctx: &PanelCtx<'_>, scheduler: SchedulerSettings) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.action_in_flight = true;
        let tx = channel.tx.clone();
        let ui_tx = ctx.ui_tx.clone();
        let settings = ctx.state.settings_service.clone();
        let jobs = ctx.state.job_service.clone();
        ctx.handle.spawn(async move {
            let result = async {
                settings
                    .update_settings(SettingsUpdate {
                        scheduler: Some(scheduler.clone()),
                        ui_language: None,
                    })
                    .await?;
                let status = jobs.scheduler_status(&scheduler);
                Ok::<_, crate::error::AppError>((scheduler, status))
            }
            .await;

            match result {
                Ok((settings, status)) => {
                    let _ = ui_tx.send(crate::runtime::UiEvent::Status(
                        "Scheduler settings restored".to_string(),
                    ));
                    let _ = tx.send(Msg::SchedulerRestored(settings, status));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Scheduler restore failed: {err}")));
                }
            }
        });
    }

    fn restore_scheduler_after_detection(&mut self, ctx: &PanelCtx<'_>) {
        let Some(test) = self.scheduler_test.as_mut() else {
            return;
        };
        if test.detected_run_id.is_none() || test.restore_requested {
            return;
        }

        test.restore_requested = true;
        let backup = test.backup.clone();
        self.restore_scheduler_settings(ctx, backup);
    }

    fn has_active_jobs(&self) -> bool {
        self.jobs
            .iter()
            .any(|job| is_active_status(job.status.as_str()))
    }

    fn has_active_ops(&self) -> bool {
        self.kg_backfill_status
            .as_ref()
            .is_some_and(|status| status.running)
            || self
                .kg_synthesis_status
                .as_ref()
                .is_some_and(|status| status.running)
            || self
                .kg_full_status
                .as_ref()
                .is_some_and(|status| status.running)
    }

    fn should_poll(&self) -> bool {
        self.action_in_flight
            || self.has_active_jobs()
            || self.has_active_ops()
            || self.scheduler_test.is_some()
    }

    /// Returns a user-facing message when the armed scheduler test's run shows
    /// up in the job list.
    fn detect_scheduler_run(&mut self) -> Option<String> {
        let test = self.scheduler_test.as_mut()?;
        if test.detected_run_id.is_some() {
            return None;
        }

        let run_id = self
            .jobs
            .iter()
            .find(|job| job.source == test.source && !test.known_run_ids.contains(&job.run_id))
            .map(|job| job.run_id.clone())?;

        test.detected_run_id = Some(run_id.clone());
        Some(format!(
            "Scheduler fired {} ({run_id})",
            label_for(&test.source)
        ))
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

/// Developer diagnostics (pipeline smoke test, scheduler arm-test) are hidden in
/// release builds. Enable them in a release build with `RESEARCHWIKI_DEV=1`.
fn dev_tools_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var("RESEARCHWIKI_DEV").is_ok()
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

fn source_combo(ui: &mut egui::Ui, id: &str, value: &mut String, sources: &[&str]) {
    if value.is_empty() {
        if let Some(source) = sources.first() {
            *value = (*source).to_string();
        }
    }

    egui::ComboBox::new(id, "")
        .selected_text(label_for(value))
        .show_ui(ui, |ui| {
            for &source in sources {
                ui.selectable_value(value, source.to_string(), label_for(source));
            }
        });
}

fn running_label(running: bool) -> &'static str {
    if running { "running" } else { "idle" }
}

async fn load_kg_snapshot(
    kg: &crate::services::knowledge_graph::KnowledgeGraphService,
    workspace_id: i64,
) -> Result<KgSnapshot, crate::error::AppError> {
    let stats = kg.get_stats(workspace_id).await?;
    let backfill = kg.get_backfill_status()?;
    let synthesis = kg.get_synthesis_compile_status()?;
    let full = kg.get_full_backfill_status()?;

    Ok(KgSnapshot {
        stats,
        backfill,
        synthesis,
        full,
    })
}

async fn load_scheduler_snapshot(
    settings: &crate::services::settings::SettingsService,
    jobs: &crate::services::jobs::JobService,
) -> Result<(SchedulerSettings, SchedulerStatusResponse), crate::error::AppError> {
    let settings = settings.get_settings().await?.scheduler;
    let status = jobs.scheduler_status(&settings);
    Ok((settings, status))
}

fn next_local_minute() -> (u8, u8, String) {
    let next = Local::now() + ChronoDuration::minutes(1);
    (
        next.hour() as u8,
        next.minute() as u8,
        next.format("%Y-%m-%d %H:%M %Z").to_string(),
    )
}

fn set_schedule_for_source(settings: &mut SchedulerSettings, source: &str, hour: u8, minute: u8) {
    match source {
        "arxiv" => {
            settings.arxiv_schedule_hour = hour;
            settings.arxiv_schedule_minute = minute;
        }
        "pmc" => {
            settings.pmc_schedule_hour = hour;
            settings.pmc_schedule_minute = minute;
        }
        "pubmed" => {
            settings.pubmed_schedule_hour = hour;
            settings.pubmed_schedule_minute = minute;
        }
        _ => {}
    }
}
