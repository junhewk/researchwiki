use egui_plot::{Bar, BarChart, Plot};

use super::{MsgChannel, PanelCtx, Tab};
use crate::{
    models::article::{ArticleResponse, ArticleStats, DailyStatsResponse},
    runtime::UiEvent,
    ui::style,
};

const CHART_DAYS: u32 = 30;
const TOP_LIMIT: u32 = 8;

enum Msg {
    Loaded {
        stats: ArticleStats,
        daily: DailyStatsResponse,
        top: Vec<ArticleResponse>,
    },
    Error(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    loaded_id: Option<i64>,
    loading: bool,
    stats: Option<ArticleStats>,
    daily: Option<DailyStatsResponse>,
    top: Vec<ArticleResponse>,
    error: Option<String>,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let channel = self.channel.get_or_insert_with(MsgChannel::default);
        while let Ok(msg) = channel.rx.try_recv() {
            match msg {
                Msg::Loaded { stats, daily, top } => {
                    self.stats = Some(stats);
                    self.daily = Some(daily);
                    self.top = top;
                    self.error = None;
                    self.loading = false;
                }
                Msg::Error(err) => {
                    self.error = Some(err);
                    self.loading = false;
                }
            }
        }

        let active = ctx.active_workspace_id;
        if self.loaded_id != Some(active) && !self.loading {
            self.refresh(ctx, active);
        }

        style::panel_header_icon(ui, style::icon::GAUGE, ctx.t("Dashboard"), None);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.loading, egui::Button::new(ctx.t("Refresh")))
                .clicked()
            {
                self.refresh(ctx, active);
            }
            if self.loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });
        ui.add_space(8.0);

        if let Some(err) = self.error.clone() {
            match style::error_notice(ui, &err, Some(ctx.t("Retry"))) {
                style::NoticeAction::Retry => {
                    self.error = None;
                    self.refresh(ctx, active);
                }
                style::NoticeAction::Dismiss => self.error = None,
                style::NoticeAction::None => {}
            }
            ui.add_space(8.0);
        }

        // Friendly first-run state instead of a wall of zeros.
        let is_empty = self
            .stats
            .as_ref()
            .map(|s| s.total_articles == 0)
            .unwrap_or(true);
        if is_empty && !self.loading {
            if let Some(response) = style::empty_state(
                ui,
                style::icon::ROCKET_LAUNCH,
                ctx.t("Welcome to ResearchWiki"),
                ctx.t(
                    "No articles yet. Open Input Set to describe your research, then run a gather to start building your wiki.",
                ),
                Some(ctx.t("Open Input Set")),
            ) && response.clicked()
            {
                let _ = ctx.ui_tx.send(UiEvent::SwitchTab(Tab::Workspace));
            }
            return;
        }

        if let Some(stats) = &self.stats {
            ui.horizontal(|ui| {
                style::metric_tile(
                    ui,
                    ctx.t("Total articles"),
                    &stats.total_articles.to_string(),
                );
                style::metric_tile(ui, ctx.t("This week"), &stats.this_week.to_string());
                style::metric_tile(ui, ctx.t("Tier 1"), &stats.tier1_count.to_string());
                style::metric_tile(
                    ui,
                    ctx.t("Pending review"),
                    &stats.pending_review.to_string(),
                );
            });
        }

        ui.add_space(10.0);

        if let Some(daily) = &self.daily {
            ui.label(egui::RichText::new(ctx.t("Articles per day (last 30 days)")).strong());
            let bars: Vec<Bar> = daily
                .days
                .iter()
                .enumerate()
                .map(|(i, d)| Bar::new(i as f64, d.count as f64))
                .collect();
            let chart = BarChart::new("per_day", bars);
            Plot::new("daily_stats_plot")
                .height(180.0)
                .allow_zoom(false)
                .allow_drag(false)
                .allow_scroll(false)
                .show(ui, |plot_ui| plot_ui.bar_chart(chart));
        }

        ui.add_space(10.0);
        style::section_heading(ui, ctx.t("Top articles"));
        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.top.is_empty() {
                style::muted_label(ui, ctx.t("No scored articles yet for this workspace."));
            }
            for article in &self.top {
                ui.horizontal(|ui| {
                    let score = article
                        .total_score
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".to_string());
                    ui.label(egui::RichText::new(format!("[{score}]")).monospace());
                    ui.label(article.title.as_deref().unwrap_or("(untitled)"));
                });
            }
        });
    }

    fn refresh(&mut self, ctx: &PanelCtx<'_>, workspace_id: i64) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.loading = true;
        self.loaded_id = Some(workspace_id);
        let tx = channel.tx.clone();
        let svc = ctx.state.article_service.clone();
        let ws = Some(workspace_id);
        ctx.handle.spawn(async move {
            let stats = svc.get_stats(ws).await;
            let daily = svc.get_daily_stats(CHART_DAYS, ws).await;
            let top = svc.get_top_articles(CHART_DAYS, TOP_LIMIT, ws).await;
            let _ = match (stats, daily, top) {
                (Ok(stats), Ok(daily), Ok(top)) => tx.send(Msg::Loaded { stats, daily, top }),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    tx.send(Msg::Error(format!("Failed to load dashboard: {e}")))
                }
            };
        });
    }
}
