use std::collections::HashMap;

use crate::{
    models::knowledge_graph::{
        KGEntityResponse, KGGraphDataQuery, KGGraphDataResponse, KGQueryRequest, KGQueryResponse,
        KGSearchEntity, KGStatsResponse,
    },
    ui::style,
};

use crate::runtime::UiEvent;

use super::{MsgChannel, PanelCtx, Tab};

const GRAPH_PADDING: f32 = 28.0;
const MAX_LABELS: usize = 24;
const MIN_ZOOM: f32 = 0.35;
const MAX_ZOOM: f32 = 4.0;
const WHEEL_ZOOM_SPEED: f32 = 0.0015;
const EDGE_ALPHA: u8 = 72;
const EDGE_DIM_ALPHA: u8 = 34;
const EDGE_HIGHLIGHT_ALPHA: u8 = 168;

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
    /// Workspace the current graph was loaded for; a mismatch reloads while
    /// keeping search/filter inputs.
    loaded_workspace: Option<i64>,

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
    view_zoom: f32,
    view_pan: egui::Vec2,
    dragged_node: Option<usize>,

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
    home_pos: egui::Pos2,
    radius: f32,
    color: egui::Color32,
    label_priority: usize,
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
        }
        if self.loaded_workspace != Some(ctx.active_workspace_id) {
            self.loaded_workspace = Some(ctx.active_workspace_id);
            self.nodes.clear();
            self.edges.clear();
            self.results.clear();
            self.selected_entity = None;
            self.detail = None;
            self.stats = None;
            self.error = None;
            self.reset_view_transform();
            self.load_stats(ctx);
            self.load_graph(ctx);
        }

        style::panel_header_icon(ui, style::icon::GRAPH, ctx.t("Knowledge Graph"), None);

        self.show_controls(ui, ctx);
        ui.add_space(8.0);

        if let Some(err) = self.error.clone() {
            match style::error_notice(ui, &err, Some(ctx.t("Retry"))) {
                style::NoticeAction::Retry => {
                    self.error = None;
                    self.load_stats(ctx);
                    self.load_graph(ctx);
                }
                style::NoticeAction::Dismiss => self.error = None,
                style::NoticeAction::None => {}
            }
            ui.add_space(4.0);
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
        style::section_heading(ui, ctx.t("Entity search"));
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Search"));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("entity name")
                    .desired_width(220.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || ui.button(ctx.t("Search")).clicked() {
                self.run_search(ctx);
            }
            if self.searching {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
        });

        ui.add_space(8.0);
        style::section_heading(ui, ctx.t("Graph view"));
        ui.horizontal_wrapped(|ui| {
            ui.label(ctx.t("Graph nodes <="));
            ui.add(
                egui::DragValue::new(&mut self.limit)
                    .range(10..=1000)
                    .speed(2.0),
            );
            ui.label(ctx.t("Min degree"));
            ui.add(egui::DragValue::new(&mut self.min_degree).range(0..=20));
            ui.label(ctx.t("Type"));
            let mut type_options = self
                .stats
                .as_ref()
                .map(|stats| {
                    stats
                        .entity_types
                        .iter()
                        .map(|(kind, count)| (kind.clone(), *count))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !self.entity_type.is_empty()
                && !type_options
                    .iter()
                    .any(|(kind, _)| kind == &self.entity_type)
            {
                type_options.push((self.entity_type.clone(), 0));
            }
            let selected_type = if self.entity_type.is_empty() {
                ctx.t("All types").to_string()
            } else {
                entity_type_label(&self.entity_type)
            };
            egui::ComboBox::from_id_salt("kg-entity-type-filter")
                .selected_text(selected_type)
                .width(160.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.entity_type, String::new(), ctx.t("All types"));
                    for (kind, count) in type_options {
                        let label = if count > 0 {
                            format!("{} ({count})", entity_type_label(&kind))
                        } else {
                            entity_type_label(&kind)
                        };
                        ui.selectable_value(&mut self.entity_type, kind, label);
                    }
                });
            if ui.button(ctx.t("Load graph")).clicked() {
                self.load_graph(ctx);
            }
            if self.graph_loading {
                style::loading_indicator(ui, ctx.t("Loading…"));
            }
            ui.separator();
            ui.add(
                egui::Slider::new(&mut self.view_zoom, MIN_ZOOM..=MAX_ZOOM)
                    .logarithmic(true)
                    .text(ctx.t("Zoom")),
            );
            if ui.button(ctx.t("Reset view")).clicked() {
                self.reset_view();
            }
            if let Some(stats) = &self.stats {
                ui.separator();
                ui.label(format!("{} nodes · {} edges", stats.nodes, stats.edges));
            }
        });
    }

    fn show_graph(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if self.nodes.is_empty() {
            if !self.graph_loading
                && let Some(action) = style::empty_state(
                    ui,
                    style::icon::GRAPH,
                    ctx.t("No graph data"),
                    ctx.t(
                        "Adjust the filters and click \"Load graph\", or populate the knowledge \
                         graph by running a gather first.",
                    ),
                    Some(ctx.t("Open Gather")),
                )
                && action.clicked()
            {
                let _ = ctx.ui_tx.send(UiEvent::SwitchTab(Tab::Gather));
            }
            return;
        }

        let desired = ui.available_size().max(egui::vec2(360.0, 360.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

        let graph_rect = rect.shrink(GRAPH_PADDING);
        let base_scale = graph_rect.width().min(graph_rect.height()) * 0.5;
        self.view_zoom = self.view_zoom.clamp(MIN_ZOOM, MAX_ZOOM);

        if response.hovered() {
            let wheel_delta = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel_delta.abs() > f32::EPSILON {
                let cursor = response.hover_pos().unwrap_or_else(|| graph_rect.center());
                let center = graph_rect.center() + self.view_pan;
                let old_scale = base_scale * self.view_zoom;
                let graph_cursor = screen_to_graph(cursor, center, old_scale);
                let zoom_factor = (wheel_delta * WHEEL_ZOOM_SPEED).exp();
                self.view_zoom = (self.view_zoom * zoom_factor).clamp(MIN_ZOOM, MAX_ZOOM);
                let new_scale = base_scale * self.view_zoom;
                let new_center =
                    cursor - egui::vec2(graph_cursor.x * new_scale, graph_cursor.y * new_scale);
                self.view_pan = new_center - graph_rect.center();
            }
        }

        let mut scale = base_scale * self.view_zoom;
        let mut center = graph_rect.center() + self.view_pan;
        let hovered = response
            .hover_pos()
            .and_then(|cursor| self.node_at(cursor, center, scale));

        if response.drag_started_by(egui::PointerButton::Primary) {
            self.dragged_node = hovered;
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_delta();
            if let Some(idx) = self.dragged_node {
                if let Some(node) = self.nodes.get_mut(idx) {
                    node.pos += delta / scale;
                }
            } else {
                self.view_pan += delta;
                center = graph_rect.center() + self.view_pan;
            }
        }
        if response.drag_stopped() {
            self.dragged_node = None;
        }
        if response.double_clicked() && hovered.is_none() {
            self.reset_view();
            scale = base_scale * self.view_zoom;
            center = graph_rect.center() + self.view_pan;
        }

        let selected_idx = self
            .selected_entity
            .as_deref()
            .and_then(|name| self.nodes.iter().position(|node| node.name == name));
        let connected = selected_idx.map(|idx| connected_nodes(idx, &self.edges, self.nodes.len()));

        for edge in &self.edges {
            let Some(source) = self.nodes.get(edge.source) else {
                continue;
            };
            let Some(target) = self.nodes.get(edge.target) else {
                continue;
            };
            let stroke_width = (edge.weight as f32).sqrt().clamp(0.7, 2.2);
            let highlighted =
                selected_idx.is_some_and(|idx| edge.source == idx || edge.target == idx);
            painter.line_segment(
                [
                    graph_to_screen(source.pos, center, scale),
                    graph_to_screen(target.pos, center, scale),
                ],
                egui::Stroke::new(
                    if highlighted {
                        stroke_width + 0.8
                    } else {
                        stroke_width
                    },
                    if highlighted {
                        edge_color(65, 110, 185, EDGE_HIGHLIGHT_ALPHA)
                    } else if selected_idx.is_some() {
                        edge_color(100, 100, 100, EDGE_DIM_ALPHA)
                    } else {
                        edge_color(190, 190, 190, EDGE_ALPHA)
                    },
                ),
            );
        }

        if response.clicked() && !response.double_clicked() {
            if let Some(idx) = hovered {
                if let Some(name) = self.nodes.get(idx).map(|node| node.name.clone()) {
                    self.load_entity(ctx, &name);
                }
            }
        }

        for (idx, node) in self.nodes.iter().enumerate() {
            let screen = graph_to_screen(node.pos, center, scale);
            let selected = self.selected_entity.as_deref() == Some(node.name.as_str());
            let hovered = hovered == Some(idx);
            let radius = node.radius;
            let dimmed = connected
                .as_ref()
                .is_some_and(|connected| !connected[idx] && !hovered);
            let color = if dimmed {
                node.color.gamma_multiply(0.38)
            } else {
                node.color
            };
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

            if selected || hovered || node.label_priority < MAX_LABELS || self.view_zoom > 1.6 {
                painter.text(
                    screen + egui::vec2(radius + 3.0, -radius - 2.0),
                    egui::Align2::LEFT_TOP,
                    &node.name,
                    egui::FontId::proportional(style::GRAPH_LABEL_TEXT_SIZE),
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
        if hovered.is_some() || self.dragged_node.is_some() {
            response.on_hover_and_drag_cursor(egui::CursorIcon::Grab);
        } else if response.hovered() {
            response.on_hover_and_drag_cursor(egui::CursorIcon::Move);
        }
    }

    fn show_side(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !self.results.is_empty() {
                    ui.strong(format!(
                        "{} ({})",
                        ctx.t("Search results"),
                        self.results.len()
                    ));
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
                        style::body_label(ui, desc.as_str());
                    }
                    if !detail.neighbors.is_empty() {
                        ui.separator();
                        ui.strong(format!(
                            "{} ({})",
                            ctx.t("Neighbors"),
                            detail.neighbors.len()
                        ));
                        for neighbor in &detail.neighbors {
                            let label = format!(
                                "{} -> {} ({}, w{:.1})",
                                neighbor.relationship,
                                neighbor.entity,
                                neighbor.entity_type,
                                neighbor.weight,
                            );
                            if ui.selectable_label(false, label).clicked() {
                                navigate = Some(neighbor.entity.clone());
                            }
                            if let Some(evidence) = &neighbor.evidence_summary {
                                style::muted_label(ui, evidence.as_str());
                                ui.add_space(4.0);
                            }
                        }
                    }
                } else {
                    style::body_label(
                        ui,
                        ctx.t("Click a graph node or a search result to see entity details."),
                    );
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
            let pos = egui::pos2(
                node.properties
                    .get("x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32,
                node.properties
                    .get("y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32,
            );
            let radius = node
                .properties
                .get("radius")
                .and_then(|v| v.as_f64())
                .unwrap_or(8.0) as f32;
            let color = node
                .properties
                .get("color")
                .and_then(|v| v.as_str())
                .and_then(color_from_hex)
                .unwrap_or_else(|| entity_color(&entity_type));
            let idx = nodes.len();
            let label_priority = node
                .properties
                .get("label_priority")
                .and_then(|v| v.as_u64())
                .unwrap_or(idx as u64) as usize;
            idx_by_id.insert(node.id.clone(), idx);
            nodes.push(KgNodeView {
                name,
                entity_type,
                degree,
                mention_count,
                pos,
                home_pos: pos,
                radius,
                color,
                label_priority,
            });
        }

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
        self.reset_view_transform();
    }

    fn load_stats(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.knowledge_graph_service.clone();
        let workspace_id = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let _ = match svc.get_stats(workspace_id).await {
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
        let workspace_id = ctx.active_workspace_id;
        ctx.handle.spawn(async move {
            let _ = match svc.get_graph_data(query, workspace_id).await {
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

    fn node_at(&self, cursor: egui::Pos2, center: egui::Pos2, scale: f32) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, node)| {
                let screen = graph_to_screen(node.pos, center, scale);
                let radius = node.radius + 4.0;
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

    fn reset_view(&mut self) {
        self.reset_view_transform();
        for node in &mut self.nodes {
            node.pos = node.home_pos;
        }
    }

    fn reset_view_transform(&mut self) {
        self.view_zoom = 1.0;
        self.view_pan = egui::Vec2::ZERO;
        self.dragged_node = None;
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

fn entity_type_label(entity_type: &str) -> String {
    entity_type
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn graph_to_screen(pos: egui::Pos2, center: egui::Pos2, scale: f32) -> egui::Pos2 {
    center + egui::vec2(pos.x * scale, pos.y * scale)
}

fn screen_to_graph(pos: egui::Pos2, center: egui::Pos2, scale: f32) -> egui::Pos2 {
    let v = (pos - center) / scale;
    egui::pos2(v.x, v.y)
}

fn edge_color(r: u8, g: u8, b: u8, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

fn connected_nodes(selected: usize, edges: &[KgEdgeView], node_count: usize) -> Vec<bool> {
    let mut connected = vec![false; node_count];
    if selected < node_count {
        connected[selected] = true;
    }
    for edge in edges {
        if edge.source == selected && edge.target < node_count {
            connected[edge.target] = true;
        }
        if edge.target == selected && edge.source < node_count {
            connected[edge.source] = true;
        }
    }
    connected
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

fn color_from_hex(raw: &str) -> Option<egui::Color32> {
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}
