use std::time::Duration;

use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::{
    models::knowledge_graph::{
        KGEntitySynthesis, KGEntitySynthesisSummary, KGSynthesisCompileStatus,
        KGSynthesisListQuery, KGSynthesisListResponse,
    },
    ui::style,
};

use crate::runtime::UiEvent;

use super::{MsgChannel, PanelCtx, Tab};

enum Msg {
    List(KGSynthesisListResponse),
    Search(Vec<KGEntitySynthesisSummary>),
    Detail(Box<KGEntitySynthesis>),
    CompileStarted,
    Error(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,
    /// Workspace the current data was fetched for; a mismatch refetches while
    /// keeping search/filter inputs.
    loaded_workspace: Option<i64>,

    // List + search.
    search: String,
    list: Vec<KGEntitySynthesisSummary>,
    total: i64,
    stale_count: i64,
    is_search_results: bool,

    // Filters / pagination.
    entity_type: String,
    stale_only: bool,
    limit: u32,
    offset: u32,
    loading: bool,

    // Selected synthesis (rendered as markdown).
    selected: Option<KGEntitySynthesis>,
    md_cache: CommonMarkCache,

    // Background compilation.
    compile_busy: bool,
    compiling: bool,
    compile_status: Option<KGSynthesisCompileStatus>,

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
            self.limit = 50;
        }
        if self.loaded_workspace != Some(ctx.active_workspace_id) {
            self.loaded_workspace = Some(ctx.active_workspace_id);
            self.list.clear();
            self.total = 0;
            self.stale_count = 0;
            self.is_search_results = false;
            self.offset = 0;
            self.selected = None;
            self.error = None;
            self.fetch_list(ctx);
        }
        self.poll_compile(ui, ctx);

        style::panel_header_icon(ui, style::icon::BOOK_OPEN, ctx.t("Wiki"), None);

        self.show_controls(ui, ctx);
        ui.add_space(8.0);

        if let Some(err) = self.error.clone() {
            match style::error_notice(ui, &err, Some(ctx.t("Retry"))) {
                style::NoticeAction::Retry => {
                    self.error = None;
                    self.fetch_list(ctx);
                }
                style::NoticeAction::Dismiss => self.error = None,
                style::NoticeAction::None => {}
            }
            ui.add_space(4.0);
        }

        egui::SidePanel::left("wiki-list")
            .resizable(true)
            .default_width(320.0)
            .show_inside(ui, |ui| {
                self.show_list(ui, ctx);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.show_detail(ui, ctx);
        });
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
                Msg::List(resp) => {
                    self.list = resp.syntheses;
                    self.total = resp.total;
                    self.stale_count = resp.stale_count;
                    self.is_search_results = false;
                    self.loading = false;
                    self.error = None;
                }
                Msg::Search(items) => {
                    self.list = items;
                    self.is_search_results = true;
                    self.loading = false;
                    self.error = None;
                }
                Msg::Detail(syn) => {
                    self.selected = Some(*syn);
                }
                Msg::CompileStarted => {
                    // Start polling; the service set `running = true` before acking.
                    self.compiling = true;
                    self.error = None;
                }
                Msg::Error(err) => {
                    self.loading = false;
                    self.compile_busy = false;
                    self.compiling = false;
                    self.error = Some(err);
                }
            }
        }
    }

    /// While a compilation is running, poll its status each frame and refresh the
    /// list once it finishes.
    fn poll_compile(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if !self.compiling {
            return;
        }
        match ctx
            .state
            .knowledge_graph_service
            .get_synthesis_compile_status()
        {
            Ok(status) => {
                let running = status.running;
                self.compile_status = Some(status);
                if running {
                    ui.ctx().request_repaint_after(Duration::from_millis(700));
                } else {
                    self.compiling = false;
                    self.compile_busy = false;
                    self.fetch_list(ctx);
                }
            }
            Err(err) => {
                self.compiling = false;
                self.compile_busy = false;
                self.error = Some(err.to_string());
            }
        }
    }

    fn show_controls(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        style::section_heading(ui, ctx.t("Search syntheses"));
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Search"));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("synthesis text")
                    .desired_width(220.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || ui.button(ctx.t("Search")).clicked() {
                self.run_search(ctx);
            }
            if ui.button(ctx.t("Clear")).clicked() {
                self.search.clear();
                self.offset = 0;
                self.fetch_list(ctx);
            }
            if self.loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });

        ui.add_space(8.0);
        style::section_heading(ui, ctx.t("Filters and compilation"));
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Type"));
            ui.add(
                egui::TextEdit::singleline(&mut self.entity_type)
                    .hint_text("(any)")
                    .desired_width(120.0),
            );
            if ui
                .checkbox(&mut self.stale_only, ctx.t("Stale only"))
                .changed()
            {
                self.offset = 0;
                self.fetch_list(ctx);
            }
            if ui.button(ctx.t("Apply")).clicked() {
                self.offset = 0;
                self.fetch_list(ctx);
            }
            ui.separator();
            if ui
                .add_enabled(
                    !self.compile_busy,
                    egui::Button::new(ctx.t("Compile syntheses")),
                )
                .clicked()
            {
                self.start_compile(ctx);
            }
            if self.compiling {
                ui.spinner();
                if let Some(status) = &self.compile_status {
                    ui.label(format!(
                        "compiling {}/{} ({} ok, {} failed)",
                        status.processed, status.total, status.compiled, status.failed,
                    ));
                }
            }
        });

        style::muted_label(
            ui,
            format!("{} entities · {} stale", self.total, self.stale_count),
        );
    }

    fn show_list(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 36.0)
            .show(ui, |ui| {
                if self.list.is_empty() {
                    if !self.loading
                        && let Some(action) = style::empty_state(
                            ui,
                            style::icon::BOOK_OPEN,
                            ctx.t("No wiki articles yet"),
                            ctx.t(
                                "Populate the knowledge graph from the Gather tab, then compile \
                                 syntheses. Only entities cited by >=3 articles appear.",
                            ),
                            Some(ctx.t("Open Gather")),
                        )
                        && action.clicked()
                    {
                        let _ = ctx.ui_tx.send(UiEvent::SwitchTab(Tab::Gather));
                    }
                    return;
                }
                let mut clicked: Option<String> = None;
                for item in &self.list {
                    let marker = if item.stale { " ⟳" } else { "" };
                    let label = format!(
                        "{}{}\n{} · {} sources",
                        item.entity_name, marker, item.entity_type, item.source_article_count,
                    );
                    if ui.selectable_label(false, label).clicked() {
                        clicked = Some(item.entity_name.clone());
                    }
                    ui.separator();
                }
                if let Some(name) = clicked {
                    self.load_detail(ctx, &name);
                }
            });

        if !self.is_search_results && self.total > 0 {
            ui.separator();
            ui.horizontal(|ui| {
                let has_prev = self.offset >= self.limit;
                if ui
                    .add_enabled(has_prev, egui::Button::new("◀ Prev"))
                    .clicked()
                {
                    self.offset = self.offset.saturating_sub(self.limit);
                    self.fetch_list(ctx);
                }
                let has_next = i64::from(self.offset) + i64::from(self.limit) < self.total;
                if ui
                    .add_enabled(has_next, egui::Button::new("Next ▶"))
                    .clicked()
                {
                    self.offset += self.limit;
                    self.fetch_list(ctx);
                }
                let from = self.offset + 1;
                let to = (self.offset + self.limit).min(self.total as u32);
                ui.label(format!("{from}–{to} of {}", self.total));
            });
        }
    }

    fn show_detail(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let mut navigate: Option<String> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(syn) = &self.selected else {
                    style::body_label(ui, ctx.t("Select an entity to view its synthesis."));
                    return;
                };
                ui.heading(&syn.entity_name);
                style::muted_label(
                    ui,
                    format!(
                        "{} · {} sources{}",
                        syn.entity_type,
                        syn.source_article_count,
                        if syn.stale { " · stale" } else { "" },
                    ),
                );
                if let Some(compiled_at) = &syn.compiled_at {
                    ui.label(
                        egui::RichText::new(format!("Compiled: {compiled_at}"))
                            .weak()
                            .size(style::HELP_TEXT_SIZE),
                    );
                }
                ui.separator();
                if !syn.summary.is_empty() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&syn.summary)
                                .italics()
                                .size(style::BODY_TEXT_SIZE),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                }
                if syn.synthesis.is_empty() {
                    style::muted_label(ui, "(no synthesis compiled yet)");
                } else {
                    let (linked_markdown, link_targets) =
                        markdown_with_entity_links(&syn.synthesis);
                    self.md_cache.link_hooks_clear();
                    for (destination, _) in &link_targets {
                        self.md_cache.add_link_hook(destination.clone());
                    }
                    ui.scope(|ui| {
                        ui.style_mut().text_styles.insert(
                            egui::TextStyle::Body,
                            egui::FontId::new(
                                style::BODY_TEXT_SIZE,
                                egui::FontFamily::Proportional,
                            ),
                        );
                        ui.style_mut().text_styles.insert(
                            egui::TextStyle::Monospace,
                            egui::FontId::new(14.0, egui::FontFamily::Monospace),
                        );
                        CommonMarkViewer::new().show(ui, &mut self.md_cache, &linked_markdown);
                    });
                    for (destination, target) in link_targets {
                        if self.md_cache.get_link_hook(&destination) == Some(true) {
                            navigate = Some(target);
                        }
                    }
                }
                if !syn.key_aspects.is_empty() {
                    ui.add_space(8.0);
                    ui.strong(ctx.t("Key aspects"));
                    for aspect in &syn.key_aspects {
                        style::body_label(ui, format!("• {aspect}"));
                    }
                }
                if !syn.related_entities.is_empty() {
                    ui.add_space(8.0);
                    ui.strong(ctx.t("Related entities"));
                    for rel in &syn.related_entities {
                        style::body_label(
                            ui,
                            format!(
                                "• {} — {} ({})",
                                rel.relationship_type, rel.name, rel.entity_type,
                            ),
                        );
                        if ui.link(format!("Open {}", rel.name)).clicked() {
                            navigate = Some(rel.name.clone());
                        }
                    }
                }
            });
        if let Some(name) = navigate {
            self.load_detail(ctx, &name);
        }
    }

    fn fetch_list(&mut self, ctx: &PanelCtx<'_>) {
        self.loading = true;
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        let query = KGSynthesisListQuery {
            limit: self.limit.max(1),
            offset: self.offset,
            stale_only: self.stale_only,
            entity_type: opt(&self.entity_type),
        };
        let workspace_id = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let _ = match svc.list_syntheses(query, workspace_id).await {
                Ok(resp) => tx.send(Msg::List(resp)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn run_search(&mut self, ctx: &PanelCtx<'_>) {
        let query = self.search.trim().to_string();
        if query.is_empty() {
            self.fetch_list(ctx);
            return;
        }
        self.loading = true;
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.search_syntheses(&query, 50).await {
                Ok(items) => tx.send(Msg::Search(items)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn load_detail(&mut self, ctx: &PanelCtx<'_>, name: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        let name = name.to_string();
        ctx.handle.spawn(async move {
            let _ = match svc.get_entity_synthesis(&name).await {
                Ok(resp) => tx.send(Msg::Detail(Box::new(resp))),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn start_compile(&mut self, ctx: &PanelCtx<'_>) {
        self.compile_busy = true;
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.start_synthesis_compilation(20, false, None).await {
                Ok(_) => tx.send(Msg::CompileStarted),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
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

fn markdown_with_entity_links(markdown: &str) -> (String, Vec<(String, String)>) {
    let mut rendered = String::with_capacity(markdown.len());
    let mut targets = Vec::new();
    let mut rest = markdown;

    while let Some(start) = rest.find("[[") {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("]]") else {
            rendered.push_str(&rest[start..]);
            return (rendered, targets);
        };

        let raw = &after_start[..end];
        let (target, label) = raw
            .split_once('|')
            .map(|(target, label)| (target.trim(), label.trim()))
            .unwrap_or_else(|| {
                let target = raw.trim();
                (target, target)
            });

        if target.is_empty() {
            rendered.push_str("[[]]");
        } else {
            let destination = entity_link_destination(target);
            rendered.push_str(&format!(
                "[{}]({})",
                escape_markdown_link_label(label),
                destination
            ));
            targets.push((destination, target.to_string()));
        }

        rest = &after_start[end + 2..];
    }

    rendered.push_str(rest);
    (rendered, targets)
}

fn entity_link_destination(entity_name: &str) -> String {
    format!("rw-entity:{}", urlencoding::encode(entity_name))
}

fn escape_markdown_link_label(value: &str) -> String {
    value.replace('[', "\\[").replace(']', "\\]")
}
