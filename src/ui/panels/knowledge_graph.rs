use std::collections::HashMap;

use egui_graphs::{
    DefaultEdgeShape, DefaultNodeShape, FruchtermanReingold, FruchtermanReingoldState, Graph,
    GraphView, LayoutForceDirected, SettingsInteraction, SettingsNavigation,
};
use petgraph::stable_graph::{NodeIndex, StableGraph};

use crate::models::knowledge_graph::{
    KGEntityResponse, KGGraphDataQuery, KGGraphDataResponse, KGQueryRequest, KGQueryResponse,
    KGSearchEntity, KGStatsResponse,
};

use super::{MsgChannel, PanelCtx};

/// `GraphView` with all generics pinned: `String` node/edge payloads and a
/// force-directed (Fruchterman-Reingold) layout. The struct's default generics
/// are not applied during `new()` inference, so they must be named explicitly.
type KgGraphView<'a> = GraphView<
    'a,
    String,
    String,
    petgraph::Directed,
    u32,
    DefaultNodeShape,
    DefaultEdgeShape,
    FruchtermanReingoldState,
    LayoutForceDirected<FruchtermanReingold>,
>;

enum Msg {
    Search(KGQueryResponse),
    Graph(KGGraphDataResponse),
    Entity(Box<KGEntityResponse>),
    Stats(KGStatsResponse),
    Error(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    initialized: bool,

    // Entity search.
    search: String,
    results: Vec<KGSearchEntity>,
    searching: bool,

    // Graph view + filters.
    limit: u32,
    min_degree: u32,
    entity_type: String,
    graph: Option<Graph<String, String>>,
    node_names: HashMap<NodeIndex, String>,
    graph_loading: bool,
    last_selected: Option<NodeIndex>,

    // Stats header + selected-entity detail.
    stats: Option<KGStatsResponse>,
    detail: Option<KGEntityResponse>,

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
            self.limit = 200;
            self.min_degree = 0;
            self.load_stats(ctx);
            self.load_graph(ctx);
        }

        ui.heading("Knowledge Graph");
        ui.separator();

        self.show_controls(ui, ctx);
        ui.add_space(4.0);

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }

        egui::SidePanel::right("kg-side")
            .resizable(true)
            .default_width(320.0)
            .show_inside(ui, |ui| {
                self.show_side(ui, ctx);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.show_graph(ui, ctx);
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
                Msg::Search(resp) => {
                    self.results = resp.entities;
                    self.searching = false;
                    self.error = None;
                }
                Msg::Graph(resp) => {
                    self.build_graph(resp);
                    self.graph_loading = false;
                    self.error = None;
                }
                Msg::Entity(resp) => {
                    self.detail = Some(*resp);
                }
                Msg::Stats(resp) => {
                    self.stats = Some(resp);
                }
                Msg::Error(err) => {
                    self.searching = false;
                    self.graph_loading = false;
                    self.error = Some(err);
                }
            }
        }
    }

    fn show_controls(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Search");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("entity name")
                    .desired_width(220.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || ui.button("Search").clicked() {
                self.run_search(ctx);
            }
            if self.searching {
                ui.spinner();
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Graph nodes ≤");
            ui.add(egui::DragValue::new(&mut self.limit).range(10..=1000).speed(2.0));
            ui.label("Min degree");
            ui.add(egui::DragValue::new(&mut self.min_degree).range(0..=20));
            ui.label("Type");
            ui.add(
                egui::TextEdit::singleline(&mut self.entity_type)
                    .hint_text("(any)")
                    .desired_width(120.0),
            );
            if ui.button("Load graph").clicked() {
                self.load_graph(ctx);
            }
            if self.graph_loading {
                ui.spinner();
            }
            if let Some(stats) = &self.stats {
                ui.separator();
                ui.label(format!("{} nodes · {} edges", stats.nodes, stats.edges));
            }
        });
    }

    fn show_graph(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.graph.is_none() || self.node_names.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    "No graph data. Adjust the filters and click \"Load graph\", or populate \
                     the knowledge graph from the Gather tab (Run gather + KG smoke test).",
                );
            });
            return;
        }

        let selected = {
            let graph = self.graph.as_mut().unwrap();
            ui.add(
                &mut KgGraphView::new(graph)
                    .with_interactions(
                        &SettingsInteraction::default()
                            .with_dragging_enabled(true)
                            .with_node_selection_enabled(true),
                    )
                    .with_navigations(
                        &SettingsNavigation::default()
                            .with_fit_to_screen_enabled(false)
                            .with_zoom_and_pan_enabled(true),
                    ),
            );
            // Keep stepping the force-directed layout while the tab is visible.
            ui.ctx().request_repaint();
            graph.selected_nodes().first().copied()
        };

        if let Some(sel) = selected {
            if self.last_selected != Some(sel) {
                self.last_selected = Some(sel);
                if let Some(name) = self.node_names.get(&sel).cloned() {
                    self.load_entity(ctx, &name);
                }
            }
        }
    }

    fn show_side(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !self.results.is_empty() {
                    ui.strong(format!("Search results ({})", self.results.len()));
                    let mut clicked: Option<String> = None;
                    for entity in &self.results {
                        let label = format!("{} ({})", entity.name, entity.entity_type);
                        if ui.selectable_label(false, label).clicked() {
                            clicked = Some(entity.name.clone());
                        }
                    }
                    if let Some(name) = clicked {
                        self.load_entity(ctx, &name);
                    }
                    ui.separator();
                }

                let mut navigate: Option<String> = None;
                if let Some(detail) = &self.detail {
                    ui.heading(&detail.entity);
                    if let Some(t) = &detail.entity_type {
                        ui.label(format!("Type: {t}"));
                    }
                    if let Some(count) = detail.mention_count {
                        ui.label(format!("Mentions: {count}"));
                    }
                    if let Some(desc) = &detail.description {
                        ui.add_space(2.0);
                        ui.label(desc);
                    }
                    if !detail.neighbors.is_empty() {
                        ui.separator();
                        ui.strong(format!("Neighbors ({})", detail.neighbors.len()));
                        for neighbor in &detail.neighbors {
                            let label = format!(
                                "{} → {} ({}, w{:.1})",
                                neighbor.relationship,
                                neighbor.entity,
                                neighbor.entity_type,
                                neighbor.weight,
                            );
                            if ui.selectable_label(false, label).clicked() {
                                navigate = Some(neighbor.entity.clone());
                            }
                            if let Some(evidence) = &neighbor.evidence_summary {
                                ui.label(egui::RichText::new(evidence).weak().small());
                            }
                        }
                    }
                } else {
                    ui.label("Click a graph node or a search result to see entity details.");
                }
                if let Some(name) = navigate {
                    self.load_entity(ctx, &name);
                }
            });
    }

    fn build_graph(&mut self, resp: KGGraphDataResponse) {
        let mut stable: StableGraph<String, String> = StableGraph::new();
        let mut idx_by_id: HashMap<String, NodeIndex> = HashMap::new();
        let mut node_names: HashMap<NodeIndex, String> = HashMap::new();

        for node in &resp.nodes {
            let name = node
                .properties
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| node.id.clone());
            let idx = stable.add_node(name.clone());
            idx_by_id.insert(node.id.clone(), idx);
            node_names.insert(idx, name);
        }
        for edge in &resp.edges {
            if let (Some(&s), Some(&t)) =
                (idx_by_id.get(&edge.source), idx_by_id.get(&edge.target))
            {
                let rel = edge
                    .properties
                    .get("relationship")
                    .or_else(|| edge.properties.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                stable.add_edge(s, t, rel);
            }
        }

        // egui_graphs labels nodes by index by default; relabel to entity names.
        let mut graph: Graph<String, String> = Graph::from(&stable);
        for (&idx, name) in &node_names {
            if let Some(node) = graph.node_mut(idx) {
                node.set_label(name.clone());
            }
        }

        self.graph = Some(graph);
        self.node_names = node_names;
        self.last_selected = None;
    }

    fn load_stats(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.get_stats().await {
                Ok(resp) => tx.send(Msg::Stats(resp)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn load_graph(&mut self, ctx: &PanelCtx<'_>) {
        self.graph_loading = true;
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        let query = KGGraphDataQuery {
            limit: self.limit.max(1),
            min_degree: self.min_degree,
            entity_types: opt(&self.entity_type),
        };
        ctx.handle.spawn(async move {
            let _ = match svc.get_graph_data(query).await {
                Ok(resp) => tx.send(Msg::Graph(resp)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn run_search(&mut self, ctx: &PanelCtx<'_>) {
        let query = self.search.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.searching = true;
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        ctx.handle.spawn(async move {
            let request = KGQueryRequest {
                query,
                mode: "hybrid".to_string(),
            };
            let _ = match svc.query(request).await {
                Ok(resp) => tx.send(Msg::Search(resp)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn load_entity(&mut self, ctx: &PanelCtx<'_>, name: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        let name = name.to_string();
        ctx.handle.spawn(async move {
            let _ = match svc.get_entity(&name).await {
                Ok(resp) => tx.send(Msg::Entity(Box::new(resp))),
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
