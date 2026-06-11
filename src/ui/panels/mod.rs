pub mod articles;
pub mod dashboard;
pub mod gap_bridge;
pub mod gather;
pub mod knowledge_graph;
pub mod prompts;
pub mod settings;
pub mod traces;
pub mod wiki;
pub mod workspace;

use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::mpsc};

use crate::{models::settings::UiLanguage, runtime::UiEvent, state::AppState, ui::i18n};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Workspace,
    #[default]
    Dashboard,
    Articles,
    Gather,
    KnowledgeGraph,
    Wiki,
    GapBridge,
    Prompts,
    Settings,
    Traces,
}

impl Tab {
    /// Day-to-day reading flow: overview → gather → results.
    pub const MAIN: [Tab; 5] = [
        Tab::Dashboard,
        Tab::Gather,
        Tab::Articles,
        Tab::KnowledgeGraph,
        Tab::Wiki,
    ];

    /// Configuration and power-user tools.
    pub const CONFIG: [Tab; 5] = [
        Tab::Workspace,
        Tab::GapBridge,
        Tab::Prompts,
        Tab::Traces,
        Tab::Settings,
    ];

    /// `MAIN` then `CONFIG`, in display order. Index n binds to Ctrl+(n+1),
    /// with Ctrl+0 for the tenth tab.
    pub const ALL: [Tab; 10] = [
        Tab::Dashboard,
        Tab::Gather,
        Tab::Articles,
        Tab::KnowledgeGraph,
        Tab::Wiki,
        Tab::Workspace,
        Tab::GapBridge,
        Tab::Prompts,
        Tab::Traces,
        Tab::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Workspace => "Input Set",
            Tab::Dashboard => "Dashboard",
            Tab::Articles => "Articles",
            Tab::Gather => "Gather",
            Tab::KnowledgeGraph => "Knowledge Graph",
            Tab::Wiki => "Wiki",
            Tab::GapBridge => "Gap Bridge",
            Tab::Prompts => "Prompts",
            Tab::Settings => "Settings",
            Tab::Traces => "Traces",
        }
    }

    pub fn label_for(self, language: UiLanguage) -> &'static str {
        i18n::t(language, self.label())
    }

    /// Phosphor glyph shown beside the tab label in the top nav.
    pub fn icon(self) -> &'static str {
        use crate::ui::style::icon;
        match self {
            Tab::Dashboard => icon::GAUGE,
            Tab::Workspace => icon::SLIDERS_HORIZONTAL,
            Tab::Gather => icon::DOWNLOAD_SIMPLE,
            Tab::Articles => icon::ARTICLE,
            Tab::KnowledgeGraph => icon::GRAPH,
            Tab::Wiki => icon::BOOK_OPEN,
            Tab::GapBridge => icon::BRIDGE,
            Tab::Prompts => icon::CHAT_TEXT,
            Tab::Traces => icon::RECEIPT,
            Tab::Settings => icon::GEAR,
        }
    }
}

/// Per-frame context handed to each panel. `ui_tx` is for app-wide status
/// events; panel-local results travel on the panel's own channel.
pub struct PanelCtx<'a> {
    pub state: &'a AppState,
    pub handle: &'a Handle,
    pub ui_tx: &'a mpsc::UnboundedSender<UiEvent>,
    /// The workspace currently selected in the top-bar switcher. Panels scope
    /// their queries to this id.
    pub active_workspace_id: i64,
    pub language: UiLanguage,
}

impl PanelCtx<'_> {
    pub fn t(&self, text: &'static str) -> &'static str {
        i18n::t(self.language, text)
    }
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
    pub workspace: workspace::Panel,
    pub dashboard: dashboard::Panel,
    pub articles: articles::Panel,
    pub gather: gather::Panel,
    pub knowledge_graph: knowledge_graph::Panel,
    pub wiki: wiki::Panel,
    pub gap_bridge: gap_bridge::Panel,
    pub prompts: prompts::Panel,
    pub settings: settings::Panel,
    pub traces: traces::Panel,
}

impl Panels {
    pub fn show(&mut self, tab: Tab, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        match tab {
            Tab::Workspace => self.workspace.show(ui, ctx),
            Tab::Dashboard => self.dashboard.show(ui, ctx),
            Tab::Articles => self.articles.show(ui, ctx),
            Tab::Gather => self.gather.show(ui, ctx),
            Tab::KnowledgeGraph => self.knowledge_graph.show(ui, ctx),
            Tab::Wiki => self.wiki.show(ui, ctx),
            Tab::GapBridge => self.gap_bridge.show(ui, ctx),
            Tab::Prompts => self.prompts.show(ui, ctx),
            Tab::Settings => self.settings.show(ui, ctx),
            Tab::Traces => self.traces.show(ui, ctx),
        }
    }
}
