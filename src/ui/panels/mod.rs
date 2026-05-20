pub mod articles;
pub mod dashboard;
pub mod gather;
pub mod knowledge_graph;
pub mod prompts;
pub mod settings;
pub mod traces;
pub mod wiki;

use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::mpsc};

use crate::{runtime::UiEvent, state::AppState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    #[default]
    Dashboard,
    Articles,
    Gather,
    KnowledgeGraph,
    Wiki,
    Prompts,
    Settings,
    Traces,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Dashboard,
        Tab::Articles,
        Tab::Gather,
        Tab::KnowledgeGraph,
        Tab::Wiki,
        Tab::Prompts,
        Tab::Settings,
        Tab::Traces,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Articles => "Articles",
            Tab::Gather => "Gather",
            Tab::KnowledgeGraph => "Knowledge Graph",
            Tab::Wiki => "Wiki",
            Tab::Prompts => "Prompts",
            Tab::Settings => "Settings",
            Tab::Traces => "Traces",
        }
    }
}

/// Per-frame context handed to each panel. `ui_tx` is for app-wide status
/// events; panel-local results travel on the panel's own channel.
pub struct PanelCtx<'a> {
    pub state: &'a AppState,
    pub handle: &'a Handle,
    pub ui_tx: &'a mpsc::UnboundedSender<UiEvent>,
}

/// Paired sender/receiver each panel uses to receive results from its own
/// spawned background tasks. Defaults to a fresh unbounded mpsc.
pub struct MsgChannel<T> {
    pub tx: mpsc::UnboundedSender<T>,
    pub rx: mpsc::UnboundedReceiver<T>,
}

impl<T> Default for MsgChannel<T> {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

#[derive(Default)]
pub struct Panels {
    pub dashboard: dashboard::Panel,
    pub articles: articles::Panel,
    pub gather: gather::Panel,
    pub knowledge_graph: knowledge_graph::Panel,
    pub wiki: wiki::Panel,
    pub prompts: prompts::Panel,
    pub settings: settings::Panel,
    pub traces: traces::Panel,
}

impl Panels {
    pub fn show(&mut self, tab: Tab, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        match tab {
            Tab::Dashboard => self.dashboard.show(ui, ctx),
            Tab::Articles => self.articles.show(ui, ctx),
            Tab::Gather => self.gather.show(ui, ctx),
            Tab::KnowledgeGraph => self.knowledge_graph.show(ui, ctx),
            Tab::Wiki => self.wiki.show(ui, ctx),
            Tab::Prompts => self.prompts.show(ui, ctx),
            Tab::Settings => self.settings.show(ui, ctx),
            Tab::Traces => self.traces.show(ui, ctx),
        }
    }
}
