use std::collections::HashMap;

use crate::models::knowledge_graph::{
    KGEntityResponse, KGGraphDataQuery, KGGraphDataResponse, KGQueryRequest, KGQueryResponse,
    KGSearchEntity, KGStatsResponse,
};

use super::{MsgChannel, PanelCtx};

const GRAPH_PADDING: f32 = 28.0;
const MAX_LABELS: usize = 24;

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
    nodes: Vec<KgNodeView>,
    edges: Vec<KgEdgeView>,
    graph_loading: bool,
    selected_entity: Option<String>,

    // Stats header + selected-entity detail.
    stats: Option<KGStatsResponse>,
    detail: Option<KGEntityResponse>,

    error: Option<String>,
}

#[derive(Clone, Debug)]
struct KgNodeView {
    name: String,
    entity_type: String,
    degree: i64,
    mention_count: i64,
    pos: egui::Pos2,
}

#[derive(Clone, Debug)]
struct KgEdgeView {
    source: usize,
    target: usize,
    weight: f64,
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
            ui.add(
                egui::DragValue::new(&mut self.limit)
                    .range(10..=1000)
                    .speed(2.0),
            );
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
        if self.nodes.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    "No graph data. Adjust the filters and click \"Load graph\", or populate \
                     the knowledge graph from the Gather tab (Run gather + KG smoke test).",
                );
            });
            return;
        }

        let desired = ui.available_size().max(egui::vec2(360.0, 360.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

        let graph_rect = rect.shrink(GRAPH_PADDING);
        let scale = graph_rect.width().min(graph_rect.height()) * 0.5;
        let center = graph_rect.center();
        let to_screen = |pos: egui::Pos2| center + egui::vec2(pos.x * scale, pos.y * scale);

        for edge in &self.edges {
            let Some(source) = self.nodes.get(edge.source) else {
                continue;
            };
            let Some(target) = self.nodes.get(edge.target) else {
                continue;
            };
            let stroke_width = (edge.weight as f32).sqrt().clamp(0.7, 2.2);
            painter.line_segment(
                [to_screen(source.pos), to_screen(target.pos)],
                egui::Stroke::new(stroke_width, egui::Color32::from_gray(190)),
            );
        }

        let hovered = response
            .hover_pos()
            .and_then(|cursor| self.node_at(cursor, &to_screen));
        if response.clicked() {
            if let Some(idx) = hovered {
                if let Some(name) = self.nodes.get(idx).map(|node| node.name.clone()) {
                    self.load_entity(ctx, &name);
                }
            }
        }

        let max_degree = self.nodes.iter().map(|node| node.degree).max().unwrap_or(1);
        for (idx, node) in self.nodes.iter().enumerate() {
            let screen = to_screen(node.pos);
            let selected = self.selected_entity.as_deref() == Some(node.name.as_str());
            let hovered = hovered == Some(idx);
            let radius = node_radius(node, max_degree);
            let color = entity_color(&node.entity_type);
            painter.circle_filled(screen, radius, color);
            painter.circle_stroke(
                screen,
                radius,
                egui::Stroke::new(
                    if selected || hovered { 2.0 } else { 1.0 },
                    if selected || hovered {
                        egui::Color32::BLACK
                    } else {
                        egui::Color32::from_gray(80)
                    },
                ),
            );

            if selected || hovered || idx < MAX_LABELS {
                painter.text(
                    screen + egui::vec2(radius + 3.0, -radius - 2.0),
                    egui::Align2::LEFT_TOP,
                    &node.name,
                    egui::FontId::proportional(11.0),
                    ui.visuals().text_color(),
                );
            }
        }

        if let Some(idx) = hovered.and_then(|idx| self.nodes.get(idx).map(|_| idx)) {
            let node = &self.nodes[idx];
            response.clone().on_hover_text(format!(
                "{}\n{} | degree {} | mentions {}",
                node.name, node.entity_type, node.degree, node.mention_count
            ));
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
        let mut nodes = Vec::new();
        let mut idx_by_id: HashMap<String, usize> = HashMap::new();

        for node in &resp.nodes {
            let name = node
                .properties
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| node.id.clone());
            let entity_type = node
                .properties
                .get("entity_type")
                .and_then(|v| v.as_str())
                .unwrap_or("UNKNOWN")
                .to_string();
            let degree = node
                .properties
                .get("degree")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let mention_count = node
                .properties
                .get("mention_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let idx = nodes.len();
            idx_by_id.insert(node.id.clone(), idx);
            nodes.push(KgNodeView {
                name,
                entity_type,
                degree,
                mention_count,
                pos: egui::pos2(0.0, 0.0),
            });
        }
        layout_nodes(&mut nodes);

        let mut edges = Vec::new();
        for edge in &resp.edges {
            if let (Some(&s), Some(&t)) = (idx_by_id.get(&edge.source), idx_by_id.get(&edge.target))
            {
                let weight = edge
                    .properties
                    .get("weight")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);
                edges.push(KgEdgeView {
                    source: s,
                    target: t,
                    weight,
                });
            }
        }

        self.nodes = nodes;
        self.edges = edges;
        self.selected_entity = None;
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
        self.selected_entity = Some(name.to_string());
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

    fn node_at(
        &self,
        cursor: egui::Pos2,
        to_screen: &impl Fn(egui::Pos2) -> egui::Pos2,
    ) -> Option<usize> {
        let max_degree = self.nodes.iter().map(|node| node.degree).max().unwrap_or(1);
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, node)| {
                let screen = to_screen(node.pos);
                let radius = node_radius(node, max_degree) + 4.0;
                let dist = screen.distance(cursor);
                (dist <= radius).then_some((idx, dist))
            })
            .min_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(idx, _)| idx)
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

fn layout_nodes(nodes: &mut [KgNodeView]) {
    let count = nodes.len();
    if count == 0 {
        return;
    }
    if count == 1 {
        nodes[0].pos = egui::pos2(0.0, 0.0);
        return;
    }

    let max_ring = (count as f32).sqrt().ceil().max(2.0);
    for (idx, node) in nodes.iter_mut().enumerate() {
        if idx == 0 {
            node.pos = egui::pos2(0.0, 0.0);
            continue;
        }
        let ring = ((idx as f32).sqrt().floor() + 1.0).min(max_ring);
        let radius = (ring / max_ring).clamp(0.18, 0.96);
        let angle = idx as f32 * 2.399_963_1;
        node.pos = egui::pos2(radius * angle.cos(), radius * angle.sin());
    }
}

fn node_radius(node: &KgNodeView, max_degree: i64) -> f32 {
    let degree = node.degree.max(0) as f32;
    let max_degree = max_degree.max(1) as f32;
    4.5 + 8.0 * (degree / max_degree).sqrt()
}

fn entity_color(entity_type: &str) -> egui::Color32 {
    match entity_type.to_ascii_uppercase().as_str() {
        "CONCEPT" => egui::Color32::from_rgb(95, 146, 220),
        "TECHNOLOGY" => egui::Color32::from_rgb(80, 170, 132),
        "METHODOLOGY" => egui::Color32::from_rgb(202, 143, 61),
        "DATASET" => egui::Color32::from_rgb(165, 124, 205),
        "ORGANIZATION" => egui::Color32::from_rgb(210, 118, 112),
        "REGULATION" => egui::Color32::from_rgb(118, 156, 92),
        "MEDICAL_CONDITION" => egui::Color32::from_rgb(188, 111, 153),
        _ => egui::Color32::from_rgb(130, 150, 165),
    }
}
