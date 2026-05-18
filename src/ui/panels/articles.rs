use egui_extras::{Column, TableBuilder};

use crate::models::article::{ArticleListQuery, ArticleListResponse, ArticleResponse};

use super::{MsgChannel, PanelCtx};

enum Msg {
    Page(ArticleListResponse),
    Error(String),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SortCol {
    #[default]
    None,
    Date,
    Title,
    Category,
    Score,
    Tier,
}

#[derive(Clone, Default)]
struct Filters {
    search: String,
    category: String,
    tier: String,
    min_score: String,
    max_score: String,
    date_from: String,
    date_to: String,
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,

    items: Vec<ArticleResponse>,
    total: i64,
    page: u32,
    page_size: u32,
    pages: u32,

    filters: Filters,
    sort_col: SortCol,
    sort_asc: bool,

    selected_uid: Option<String>,
    selected_index: Option<usize>,
    detail_open: bool,

    loading: bool,
    error: Option<String>,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.channel.is_none() {
            self.channel = Some(MsgChannel::default());
        }
        self.drain();
        if !self.initialized {
            self.initialized = true;
            self.page = 1;
            self.page_size = 100;
            self.fetch(ctx);
        }

        ui.heading("Articles");
        ui.separator();

        self.show_filters(ui, ctx);
        ui.add_space(4.0);

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }

        self.show_table(ui);
        ui.add_space(4.0);
        self.show_pagination(ui, ctx);

        if self.detail_open {
            self.show_detail_window(ui.ctx());
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
                Msg::Page(resp) => {
                    self.items = resp.items;
                    self.total = resp.total;
                    self.page = resp.page;
                    self.page_size = resp.page_size;
                    self.pages = resp.pages;
                    self.apply_local_sort();
                    self.loading = false;
                    self.error = None;
                    self.selected_index = self.selected_uid
                        .as_ref()
                        .and_then(|uid| self.items.iter().position(|a| &a.uid == uid));
                }
                Msg::Error(err) => {
                    self.loading = false;
                    self.error = Some(format!("Load failed: {err}"));
                }
            }
        }
    }

    fn show_filters(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        egui::CollapsingHeader::new("Filters")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("articles-filters-grid")
                    .num_columns(4)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Search");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.search)
                                .hint_text("title or argument")
                                .desired_width(220.0),
                        );
                        ui.label("Category");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.category)
                                .desired_width(140.0),
                        );
                        ui.end_row();

                        ui.label("Date from");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.date_from)
                                .hint_text("YYYY-MM-DD")
                                .desired_width(120.0),
                        );
                        ui.label("Date to");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.date_to)
                                .hint_text("YYYY-MM-DD")
                                .desired_width(120.0),
                        );
                        ui.end_row();

                        ui.label("Min score");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.min_score)
                                .desired_width(60.0),
                        );
                        ui.label("Max score");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.max_score)
                                .desired_width(60.0),
                        );
                        ui.end_row();

                        ui.label("Tier");
                        egui::ComboBox::new("articles-tier-combo", "")
                            .selected_text(if self.filters.tier.is_empty() {
                                "(any)"
                            } else {
                                self.filters.tier.as_str()
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.filters.tier, String::new(), "(any)");
                                ui.selectable_value(&mut self.filters.tier, "Tier1".into(), "Tier1");
                                ui.selectable_value(&mut self.filters.tier, "Tier2".into(), "Tier2");
                                ui.selectable_value(&mut self.filters.tier, "Tier3".into(), "Tier3");
                            });
                        ui.end_row();
                    });

                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        self.page = 1;
                        self.fetch(ctx);
                    }
                    if ui.button("Reset").clicked() {
                        self.filters = Filters::default();
                        self.page = 1;
                        self.fetch(ctx);
                    }
                    if self.loading {
                        ui.spinner();
                    }
                });
            });
    }

    fn show_table(&mut self, ui: &mut egui::Ui) {
        let text_height = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;

        let available_height = ui.available_height() - 80.0;
        let mut sort_request: Option<SortCol> = None;
        let mut selected_uid_click: Option<(String, usize)> = None;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(95.0).at_least(80.0))
            .column(Column::remainder().at_least(200.0))
            .column(Column::initial(120.0).at_least(80.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(80.0).at_least(60.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(available_height.max(120.0))
            .sense(egui::Sense::click())
            .header(text_height + 4.0, |mut header| {
                header.col(|ui| sort_request = sort_request.or(self.header_label(ui, "Date", SortCol::Date)));
                header.col(|ui| sort_request = sort_request.or(self.header_label(ui, "Title", SortCol::Title)));
                header.col(|ui| sort_request = sort_request.or(self.header_label(ui, "Category", SortCol::Category)));
                header.col(|ui| sort_request = sort_request.or(self.header_label(ui, "Score", SortCol::Score)));
                header.col(|ui| sort_request = sort_request.or(self.header_label(ui, "Tier", SortCol::Tier)));
            })
            .body(|body| {
                let total = self.items.len();
                body.rows(text_height, total, |mut row| {
                    let idx = row.index();
                    let Some(article) = self.items.get(idx) else { return };
                    let selected = self.selected_index == Some(idx);
                    row.set_selected(selected);

                    let cells = [
                        row.col(|ui| { ui.label(article.reg_date.as_deref().unwrap_or("—")); }).1,
                        row.col(|ui| { ui.add(egui::Label::new(article.title.as_deref().unwrap_or("(untitled)")).truncate()); }).1,
                        row.col(|ui| { ui.label(article.category.as_deref().unwrap_or("—")); }).1,
                        row.col(|ui| { ui.label(article.total_score.map(|s| s.to_string()).unwrap_or_else(|| "—".into())); }).1,
                        row.col(|ui| { ui.label(article.priority.as_deref().unwrap_or("—")); }).1,
                    ];
                    if cells.iter().any(|r| r.clicked()) {
                        selected_uid_click = Some((article.uid.clone(), idx));
                    }
                });
            });

        if let Some((uid, idx)) = selected_uid_click {
            self.selected_uid = Some(uid);
            self.selected_index = Some(idx);
            self.detail_open = true;
        }

        if let Some(target) = sort_request {
            self.toggle_sort(target);
        }
    }

    fn header_label(&self, ui: &mut egui::Ui, label: &str, col: SortCol) -> Option<SortCol> {
        let arrow = if self.sort_col == col {
            if self.sort_asc { " ▲" } else { " ▼" }
        } else {
            ""
        };
        let text = format!("{label}{arrow}");
        if ui
            .add(egui::Button::new(egui::RichText::new(text).strong()).frame(false))
            .clicked()
        {
            Some(col)
        } else {
            None
        }
    }

    fn show_pagination(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} rows ({} of {})",
                self.items.len(),
                self.page,
                self.pages.max(1)
            ));
            ui.add_space(8.0);
            ui.label(format!("Total: {}", self.total));
            ui.add_space(16.0);

            let mut new_page = self.page;
            if ui
                .add_enabled(self.page > 1, egui::Button::new("◀ Prev"))
                .clicked()
            {
                new_page = self.page - 1;
            }
            if ui
                .add_enabled(self.page < self.pages.max(1), egui::Button::new("Next ▶"))
                .clicked()
            {
                new_page = self.page + 1;
            }
            if new_page != self.page {
                self.page = new_page;
                self.fetch(ctx);
            }

            ui.add_space(16.0);
            ui.label("Page size");
            let mut ps = self.page_size;
            egui::ComboBox::new("articles-ps-combo", "")
                .selected_text(ps.to_string())
                .show_ui(ui, |ui| {
                    for option in [25u32, 50, 100] {
                        ui.selectable_value(&mut ps, option, option.to_string());
                    }
                });
            if ps != self.page_size {
                self.page_size = ps;
                self.page = 1;
                self.fetch(ctx);
            }
        });
    }

    fn show_detail_window(&mut self, egui_ctx: &egui::Context) {
        let Some(idx) = self.selected_index else {
            self.detail_open = false;
            return;
        };
        let Some(article) = self.items.get(idx).cloned() else {
            self.detail_open = false;
            return;
        };

        let mut open = true;
        egui::Window::new("Article detail")
            .resizable(true)
            .default_width(540.0)
            .default_height(620.0)
            .vscroll(false)
            .open(&mut open)
            .show(egui_ctx, |ui| {
                ui.heading(article.title.as_deref().unwrap_or("(untitled)"));
                ui.label(format!("UID: {}", article.uid));
                if let Some(url) = &article.url {
                    ui.hyperlink_to(url, url);
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("article-detail-grid")
                            .num_columns(2)
                            .spacing([8.0, 4.0])
                            .show(ui, |ui| {
                                detail_kv(ui, "Authors", article.authors.as_deref());
                                detail_kv(ui, "Journal", article.journal.as_deref());
                                detail_kv(ui, "Pub date", article.pub_date.as_deref());
                                detail_kv(ui, "Reg date", article.reg_date.as_deref());
                                detail_kv(ui, "Category", article.category.as_deref());
                                detail_kv(ui, "Tier", article.priority.as_deref());
                                detail_kv(ui, "Total score", article.total_score.map(|v| v.to_string()).as_deref());
                                detail_kv(ui, "Scholarly rigor", article.scholarly_rigor.map(|v| v.to_string()).as_deref());
                                detail_kv(ui, "Novelty", article.novelty.map(|v| v.to_string()).as_deref());
                                detail_kv(ui, "Relevance", article.relevance_score.map(|v| v.to_string()).as_deref());
                                detail_kv(ui, "Practical impact", article.practical_impact.map(|v| v.to_string()).as_deref());
                                detail_kv(ui, "Interdisciplinary", article.interdisciplinary.map(|v| v.to_string()).as_deref());
                                detail_kv(ui, "Critical concerns", article.critical_concerns.map(|v| v.to_string()).as_deref());
                            });
                        ui.add_space(8.0);
                        detail_block(ui, "Byline summary", article.byline_summary.as_deref());
                        detail_block(ui, "Why it matters", article.why_it_matters.as_deref());
                        detail_block(ui, "Key argument", article.key_argument.as_deref());
                        detail_block(ui, "Main findings", article.main_findings.as_deref());
                        detail_block(ui, "Normative claims", article.normative_claims.as_deref());
                        detail_block(ui, "Limitations", article.limitations.as_deref());
                        detail_block(ui, "AI tech", article.ai_tech.as_deref());
                        detail_block(ui, "Clinical domain", article.clinical_domain.as_deref());
                        detail_block(ui, "Ethics framework", article.ethics_framework.as_deref());
                        detail_block(ui, "Primary issue", article.primary_issue.as_deref());
                        detail_block(ui, "Stakeholders", article.key_stakeholders.as_deref());
                        detail_block(ui, "Secondary issues", article.secondary_issues.as_deref());
                    });
            });

        if !open {
            self.detail_open = false;
        }
    }

    fn toggle_sort(&mut self, col: SortCol) {
        if self.sort_col == col {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_col = col;
            self.sort_asc = false;
        }
        self.apply_local_sort();
    }

    fn apply_local_sort(&mut self) {
        if self.sort_col == SortCol::None {
            return;
        }
        let asc = self.sort_asc;
        self.items.sort_by(|a, b| {
            let ord = match self.sort_col {
                SortCol::None => std::cmp::Ordering::Equal,
                SortCol::Date => a.reg_date.cmp(&b.reg_date),
                SortCol::Title => a.title.cmp(&b.title),
                SortCol::Category => a.category.cmp(&b.category),
                SortCol::Score => a.total_score.cmp(&b.total_score),
                SortCol::Tier => a.priority.cmp(&b.priority),
            };
            if asc { ord } else { ord.reverse() }
        });
    }

    fn fetch(&mut self, ctx: &PanelCtx<'_>) {
        self.loading = true;
        let Some(channel) = self.channel.as_ref() else { return };
        let tx = channel.tx.clone();
        let svc = ctx.state.article_service.clone();
        let query = build_query(&self.filters, self.page, self.page_size);
        ctx.handle.spawn(async move {
            let result = svc.list_articles(query).await;
            let _ = match result {
                Ok(resp) => tx.send(Msg::Page(resp)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }
}

fn build_query(filters: &Filters, page: u32, page_size: u32) -> ArticleListQuery {
    ArticleListQuery {
        page,
        page_size,
        search: opt(&filters.search),
        category: opt(&filters.category),
        tier: opt(&filters.tier),
        date_from: opt(&filters.date_from),
        date_to: opt(&filters.date_to),
        min_score: filters.min_score.trim().parse().ok(),
        max_score: filters.max_score.trim().parse().ok(),
    }
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn detail_kv(ui: &mut egui::Ui, label: &str, value: Option<&str>) {
    ui.label(egui::RichText::new(label).strong());
    ui.label(value.unwrap_or("—"));
    ui.end_row();
}

fn detail_block(ui: &mut egui::Ui, label: &str, value: Option<&str>) {
    let Some(value) = value.filter(|s| !s.is_empty()) else { return };
    ui.add_space(4.0);
    ui.label(egui::RichText::new(label).strong());
    ui.label(value);
}
