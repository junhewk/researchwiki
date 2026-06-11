use egui_extras::{Column, TableBuilder};

use super::{MsgChannel, PanelCtx};
use crate::{
    models::trace::{TraceListQuery, TraceListResponse, TraceResponse, TraceSummary},
    ui::style,
};

const PAGE_SIZE: u32 = 20;

enum Msg {
    Loaded(TraceListResponse),
    Summary(Vec<TraceSummary>),
    Detail(Box<TraceResponse>),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SuccessFilter {
    #[default]
    All,
    Succeeded,
    Failed,
}

impl SuccessFilter {
    fn as_bool(self) -> Option<bool> {
        match self {
            Self::All => None,
            Self::Succeeded => Some(true),
            Self::Failed => Some(false),
        }
    }
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,
    loaded_workspace: Option<i64>,

    list: Option<TraceListResponse>,
    summary: Vec<TraceSummary>,
    detail: Option<TraceResponse>,

    page: u32,
    prompt_filter: String,
    model_filter: String,
    article_uid_filter: String,
    success_filter: SuccessFilter,

    loading: bool,
    error: Option<String>,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.channel.is_none() {
            self.channel = Some(MsgChannel::default());
        }
        self.drain();

        // First open, or whenever the active workspace changes (each workspace
        // has its own trace store), reload from scratch.
        if !self.initialized || self.loaded_workspace != Some(ctx.active_workspace_id) {
            self.initialized = true;
            self.loaded_workspace = Some(ctx.active_workspace_id);
            self.page = 1;
            self.refresh(ctx);
        }

        style::panel_header_icon(ui, style::icon::RECEIPT, ctx.t("Traces"), None);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.show_summary(ui, ctx);
                style::section_break(ui);
                self.show_filters(ui, ctx);
                if let Some(err) = &self.error {
                    ui.add_space(6.0);
                    style::status_notice(ui, false, err);
                }
                style::section_break(ui);
                self.show_table(ui, ctx);
            });

        if self.detail.is_some() {
            self.show_detail_window(ui.ctx(), ctx);
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
                Msg::Loaded(list) => {
                    self.list = Some(list);
                    self.loading = false;
                }
                Msg::Summary(summary) => self.summary = summary,
                Msg::Detail(trace) => self.detail = Some(*trace),
                Msg::Error(err) => {
                    self.error = Some(err);
                    self.loading = false;
                }
            }
        }
    }

    fn show_summary(&self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Usage by prompt"));
        if self.summary.is_empty() {
            style::muted_label(ui, ctx.t("No traces yet — run a gather to populate."));
            return;
        }

        let text_height = egui::TextStyle::Body.resolve(ui.style()).size + 8.0;
        TableBuilder::new(ui)
            .id_salt("traces-summary-table")
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(160.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(90.0).at_least(70.0))
            .column(Column::initial(90.0).at_least(70.0))
            .column(Column::initial(90.0).at_least(70.0))
            .header(text_height, |mut header| {
                header.col(|ui| {
                    ui.strong(ctx.t("Prompt"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("OK"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Failed"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Avg ms"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Tokens"));
                });
                header.col(|ui| {
                    ui.strong(ctx.t("Cost"));
                });
            })
            .body(|mut body| {
                for row_data in &self.summary {
                    body.row(text_height, |mut row| {
                        row.col(|ui| {
                            ui.add(egui::Label::new(&row_data.prompt_name).truncate());
                        });
                        row.col(|ui| {
                            ui.label(row_data.successful_executions.to_string());
                        });
                        row.col(|ui| {
                            ui.label(row_data.failed_executions.to_string());
                        });
                        row.col(|ui| {
                            ui.label(
                                row_data
                                    .avg_latency_ms
                                    .map(|v| format!("{v:.0}"))
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                row_data
                                    .total_tokens
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        });
                        row.col(|ui| {
                            ui.label(
                                row_data
                                    .total_cost_usd
                                    .map(|v| format!("${v:.4}"))
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        });
                    });
                }
            });
    }

    fn show_filters(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Filters"));
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Prompt"));
            let selected = if self.prompt_filter.is_empty() {
                ctx.t("All").to_string()
            } else {
                self.prompt_filter.clone()
            };
            egui::ComboBox::new("traces-prompt-filter", "")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut self.prompt_filter, String::new(), ctx.t("All"))
                        .changed();
                    for item in &self.summary {
                        changed |= ui
                            .selectable_value(
                                &mut self.prompt_filter,
                                item.prompt_name.clone(),
                                &item.prompt_name,
                            )
                            .changed();
                    }
                });

            ui.label(ctx.t("Result"));
            for (variant, label) in [
                (SuccessFilter::All, ctx.t("All")),
                (SuccessFilter::Succeeded, ctx.t("OK")),
                (SuccessFilter::Failed, ctx.t("Failed")),
            ] {
                changed |= ui
                    .selectable_value(&mut self.success_filter, variant, label)
                    .changed();
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Model"));
            let model = ui.add(
                egui::TextEdit::singleline(&mut self.model_filter)
                    .desired_width(160.0)
                    .hint_text(ctx.t("any")),
            );
            ui.label(ctx.t("Article UID"));
            let uid = ui.add(
                egui::TextEdit::singleline(&mut self.article_uid_filter)
                    .desired_width(160.0)
                    .hint_text(ctx.t("any")),
            );
            // Re-query when the user finishes editing a text filter.
            changed |= (model.lost_focus() || uid.lost_focus())
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            if ui.button(ctx.t("Apply")).clicked() {
                changed = true;
            }
            if ui
                .add_enabled(!self.loading, egui::Button::new(ctx.t("Refresh")))
                .clicked()
            {
                changed = true;
            }
            if self.loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });

        if changed {
            self.page = 1;
            self.refresh(ctx);
        }
    }

    fn show_table(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let Some(list) = self.list.as_ref() else {
            ui.label(ctx.t("Loading…"));
            return;
        };
        if list.items.is_empty() {
            style::muted_label(ui, ctx.t("No traces match these filters."));
            return;
        }

        // Pagination controls.
        let pages = list.pages.max(1);
        let page = list.page;
        let total = list.total;
        let mut go_prev = false;
        let mut go_next = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(page > 1, egui::Button::new("◀")).clicked() {
                go_prev = true;
            }
            ui.label(format!("{} {page} / {pages}", ctx.t("Page")));
            if ui
                .add_enabled(page < pages, egui::Button::new("▶"))
                .clicked()
            {
                go_next = true;
            }
            ui.separator();
            ui.label(format!("{total} {}", ctx.t("traces")));
        });
        ui.add_space(4.0);

        let text_height = egui::TextStyle::Body.resolve(ui.style()).size + 8.0;
        let rows = list.items.len();
        let max_scroll_height = ui.available_height().max(180.0);
        let mut detail_request: Option<i64> = None;

        TableBuilder::new(ui)
            .id_salt("traces-list-table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(150.0).at_least(120.0))
            .column(Column::remainder().at_least(140.0))
            .column(Column::initial(120.0).at_least(80.0))
            .column(Column::initial(70.0).at_least(55.0))
            .column(Column::initial(70.0).at_least(55.0))
            .column(Column::initial(80.0).at_least(60.0))
            .column(Column::initial(50.0).at_least(40.0))
            .column(Column::initial(60.0).at_least(50.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(max_scroll_height)
            .header(text_height, |mut header| {
                for label in [
                    ctx.t("When"),
                    ctx.t("Prompt"),
                    ctx.t("Model"),
                    ctx.t("Latency"),
                    ctx.t("Tokens"),
                    ctx.t("Cost"),
                    ctx.t("OK"),
                    "",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|body| {
                body.rows(text_height, rows, |mut row| {
                    let idx = row.index();
                    let Some(item) = list.items.get(idx) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(item.created_at.as_deref().unwrap_or("—")).truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&item.prompt_name).truncate());
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(item.model.as_deref().unwrap_or("—")).truncate());
                    });
                    row.col(|ui| {
                        ui.label(
                            item.latency_ms
                                .map(|v| format!("{v}"))
                                .unwrap_or_else(|| "—".to_string()),
                        );
                    });
                    row.col(|ui| {
                        ui.label(
                            item.tokens_total
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "—".to_string()),
                        );
                    });
                    row.col(|ui| {
                        ui.label(
                            item.cost_usd
                                .map(|v| format!("${v:.4}"))
                                .unwrap_or_else(|| "—".to_string()),
                        );
                    });
                    row.col(|ui| {
                        if item.success {
                            ui.colored_label(egui::Color32::from_rgb(0, 130, 0), "✔");
                        } else {
                            ui.colored_label(egui::Color32::RED, "✘");
                        }
                    });
                    row.col(|ui| {
                        if ui.small_button(ctx.t("Open")).clicked() {
                            detail_request = Some(item.id);
                        }
                    });
                });
            });

        if go_prev && self.page > 1 {
            self.page -= 1;
            self.refresh(ctx);
        }
        if go_next {
            self.page += 1;
            self.refresh(ctx);
        }
        if let Some(id) = detail_request {
            self.load_detail(ctx, id);
        }
    }

    fn show_detail_window(&mut self, egui_ctx: &egui::Context, ctx: &PanelCtx<'_>) {
        if egui_ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.detail = None;
            return;
        }
        let mut open = true;
        let Some(detail) = self.detail.as_ref() else {
            return;
        };

        egui::Window::new(ctx.t("Trace detail"))
            .resizable(true)
            .default_width(680.0)
            .default_height(620.0)
            .open(&mut open)
            .show(egui_ctx, |ui| {
                ui.heading(&detail.prompt_name);
                egui::Grid::new("trace-detail-meta")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(ctx.t("Model"));
                        ui.label(detail.model.as_deref().unwrap_or("—"));
                        ui.end_row();
                        ui.label(ctx.t("When"));
                        ui.label(detail.created_at.as_deref().unwrap_or("—"));
                        ui.end_row();
                        ui.label(ctx.t("Article UID"));
                        ui.label(detail.article_uid.as_deref().unwrap_or("—"));
                        ui.end_row();
                        ui.label(ctx.t("Tokens"));
                        ui.label(format!(
                            "{} in / {} out / {} total",
                            opt(detail.tokens_input),
                            opt(detail.tokens_output),
                            opt(detail.tokens_total),
                        ));
                        ui.end_row();
                        ui.label(ctx.t("Latency"));
                        ui.label(
                            detail
                                .latency_ms
                                .map(|v| format!("{v} ms"))
                                .unwrap_or_else(|| "—".to_string()),
                        );
                        ui.end_row();
                        ui.label(ctx.t("Cost"));
                        ui.label(
                            detail
                                .cost_usd
                                .map(|v| format!("${v:.4}"))
                                .unwrap_or_else(|| "—".to_string()),
                        );
                        ui.end_row();
                    });

                if !detail.success {
                    ui.add_space(6.0);
                    if let Some(err) = &detail.error_message {
                        ui.colored_label(egui::Color32::RED, err);
                    } else {
                        ui.colored_label(egui::Color32::RED, ctx.t("Failed (no error message)"));
                    }
                }

                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading(ctx.t("Input"));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(detail.input_text.as_deref().unwrap_or("—"))
                                    .monospace(),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.heading(ctx.t("Output"));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(detail.output_text.as_deref().unwrap_or("—"))
                                    .monospace(),
                            )
                            .wrap(),
                        );
                    });
            });

        if !open {
            self.detail = None;
        }
    }

    fn current_query(&self) -> TraceListQuery {
        TraceListQuery {
            page: self.page.max(1),
            page_size: PAGE_SIZE,
            prompt_name: non_empty(&self.prompt_filter),
            article_uid: non_empty(&self.article_uid_filter),
            model: non_empty(&self.model_filter),
            success: self.success_filter.as_bool(),
        }
    }

    fn refresh(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.loading = true;
        self.error = None;
        let tx = channel.tx.clone();
        let svc = ctx.state.trace_service.clone();
        let query = self.current_query();
        ctx.handle.spawn(async move {
            match svc.list_traces(query).await {
                Ok(list) => {
                    let _ = tx.send(Msg::Loaded(list));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Failed to load traces: {err}")));
                }
            }
            match svc.get_summary().await {
                Ok(summary) => {
                    let _ = tx.send(Msg::Summary(summary));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Failed to load trace summary: {err}")));
                }
            }
        });
    }

    fn load_detail(&mut self, ctx: &PanelCtx<'_>, trace_id: i64) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.trace_service.clone();
        ctx.handle.spawn(async move {
            match svc.get_trace(trace_id).await {
                Ok(trace) => {
                    let _ = tx.send(Msg::Detail(Box::new(trace)));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Error(format!("Failed to load trace: {err}")));
                }
            }
        });
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn opt(value: Option<i64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".to_string())
}
