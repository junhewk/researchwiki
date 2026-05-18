pub mod articles;
pub mod dashboard;
pub mod gather;
pub mod knowledge_graph;
pub mod newsletter;
pub mod prompts;
pub mod settings;
pub mod traces;
pub mod wiki;

use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Dashboard,
    Articles,
    Gather,
    KnowledgeGraph,
    Wiki,
    Newsletter,
    Prompts,
    Settings,
    Traces,
}

impl Default for Tab {
    fn default() -> Self {
        Self::Dashboard
    }
}

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Dashboard,
        Tab::Articles,
        Tab::Gather,
        Tab::KnowledgeGraph,
        Tab::Wiki,
        Tab::Newsletter,
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
            Tab::Newsletter => "Newsletter",
            Tab::Prompts => "Prompts",
            Tab::Settings => "Settings",
            Tab::Traces => "Traces",
        }
    }
}

pub fn show(tab: Tab, ui: &mut egui::Ui, state: &AppState) {
    match tab {
        Tab::Dashboard => dashboard::show(ui, state),
        Tab::Articles => articles::show(ui, state),
        Tab::Gather => gather::show(ui, state),
        Tab::KnowledgeGraph => knowledge_graph::show(ui, state),
        Tab::Wiki => wiki::show(ui, state),
        Tab::Newsletter => newsletter::show(ui, state),
        Tab::Prompts => prompts::show(ui, state),
        Tab::Settings => settings::show(ui, state),
        Tab::Traces => traces::show(ui, state),
    }
}
