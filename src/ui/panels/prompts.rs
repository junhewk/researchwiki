use std::collections::BTreeMap;

use super::{MsgChannel, PanelCtx};
use crate::error::AppError;
use crate::models::prompt::{
    PromptCreate, PromptFileConfig, PromptResponse, PromptVersionResponse,
};
use crate::services::llm::LlmOutputMode;
use crate::ui::style;

enum Msg {
    List(Vec<PromptResponse>),
    Versions(String, Vec<PromptVersionResponse>),
    Rewritten(String),
    Saved,
    Error(String),
}

#[derive(Default)]
pub struct Panel {
    channel: Option<MsgChannel<Msg>>,
    loaded: bool,
    /// Workspace the prompt list was loaded from; prompts live in the
    /// workspace DB, so a switch reloads and clears the editor.
    loaded_workspace: Option<i64>,
    prompts: Vec<PromptResponse>,
    selected: Option<String>,
    editor: String,
    versions: Vec<PromptVersionResponse>,
    status: Option<String>,
    busy: bool,
    reload_versions: bool,
}

impl Panel {
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let channel = self.channel.get_or_insert_with(MsgChannel::default);
        while let Ok(msg) = channel.rx.try_recv() {
            match msg {
                Msg::List(prompts) => {
                    self.prompts = prompts;
                    self.busy = false;
                    self.loaded = true;
                }
                Msg::Versions(name, versions) => {
                    if self.selected.as_deref() == Some(name.as_str()) {
                        self.versions = versions;
                    }
                    self.busy = false;
                }
                Msg::Rewritten(content) => {
                    self.editor = content;
                    self.status = Some(
                        "Rewritten for the active workspace's topic — review and Save.".to_string(),
                    );
                    self.busy = false;
                }
                Msg::Saved => {
                    self.status = Some("Saved (new version created).".to_string());
                    self.busy = false;
                    self.loaded = false; // triggers list reload after this loop
                    self.reload_versions = true;
                }
                Msg::Error(err) => {
                    self.status = Some(format!("Error: {err}"));
                    self.busy = false;
                }
            }
        }

        if self.loaded_workspace != Some(ctx.active_workspace_id) {
            self.loaded_workspace = Some(ctx.active_workspace_id);
            self.prompts.clear();
            self.selected = None;
            self.editor.clear();
            self.versions.clear();
            self.status = None;
            self.loaded = false;
        }
        if !self.loaded && !self.busy {
            self.load_list(ctx);
        }
        if self.reload_versions {
            self.reload_versions = false;
            if let Some(name) = self.selected.clone() {
                self.load_versions(ctx, &name);
            }
        }

        style::panel_header_icon(
            ui,
            style::icon::CHAT_TEXT,
            ctx.t("Prompts"),
            Some(ctx.t("Edit prompt templates (YAML). Saving creates a new version.")),
        );

        let names: Vec<(String, i64)> = self
            .prompts
            .iter()
            .map(|p| (p.name.clone(), p.current_version))
            .collect();
        let mut select_request: Option<String> = None;

        ui.columns(2, |cols| {
            // Left: prompt list.
            cols[0].label(egui::RichText::new(ctx.t("Prompts")).strong());
            egui::ScrollArea::vertical()
                .id_salt("prompt_list")
                .max_height(420.0)
                .show(&mut cols[0], |ui| {
                    for (name, version) in &names {
                        let selected = self.selected.as_deref() == Some(name.as_str());
                        if ui
                            .selectable_label(selected, format!("{name}  (v{version})"))
                            .clicked()
                        {
                            select_request = Some(name.clone());
                        }
                    }
                });

            // Right: editor + version history.
            let editor_ui = &mut cols[1];
            if let Some(name) = self.selected.clone() {
                editor_ui.label(egui::RichText::new(format!("Editing: {name}")).strong());
                egui::ScrollArea::vertical()
                    .id_salt("prompt_editor")
                    .max_height(300.0)
                    .show(editor_ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.editor)
                                .code_editor()
                                .desired_rows(16)
                                .desired_width(f32::INFINITY),
                        );
                    });
                editor_ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Save new version"))
                        .clicked()
                    {
                        self.save(ctx, &name);
                    }
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Rewrite for topic (LLM)"))
                        .clicked()
                    {
                        self.rewrite(ctx, &name);
                    }
                    if self.busy {
                        style::loading_indicator(ui, ctx.t("Loading…"));
                    }
                });

                editor_ui.add_space(8.0);
                editor_ui.label(egui::RichText::new(ctx.t("Version history")).strong());
                let versions: Vec<(i64, String, String)> = self
                    .versions
                    .iter()
                    .map(|v| {
                        (
                            v.version,
                            v.description.clone().unwrap_or_default(),
                            v.created_at.format("%Y-%m-%d %H:%M").to_string(),
                        )
                    })
                    .collect();
                let mut load_version: Option<i64> = None;
                egui::ScrollArea::vertical()
                    .id_salt("prompt_versions")
                    .max_height(160.0)
                    .show(editor_ui, |ui| {
                        for (version, desc, created) in &versions {
                            ui.horizontal(|ui| {
                                if ui.small_button(format!("v{version}")).clicked() {
                                    load_version = Some(*version);
                                }
                                ui.label(format!("{created}  {desc}"));
                            });
                        }
                    });
                if let Some(version) = load_version {
                    if let Some(v) = self.versions.iter().find(|v| v.version == version) {
                        self.editor = v.content.clone();
                        self.status =
                            Some(format!("Loaded v{version} into editor (not yet saved)."));
                    }
                }
            } else {
                editor_ui.label(ctx.t("Select a prompt to edit."));
            }
        });

        if let Some(name) = select_request {
            self.select(ctx, &name);
        }

        if let Some(status) = &self.status {
            ui.add_space(8.0);
            ui.label(status);
        }
    }

    fn select(&mut self, ctx: &PanelCtx<'_>, name: &str) {
        self.selected = Some(name.to_string());
        if let Some(prompt) = self.prompts.iter().find(|p| p.name == name) {
            self.editor = prompt.content.clone();
        }
        self.versions.clear();
        self.load_versions(ctx, name);
    }

    fn load_list(&mut self, ctx: &PanelCtx<'_>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.loaded = true; // optimistic: avoid re-spawning each frame
        let tx = channel.tx.clone();
        let svc = ctx.state.prompt_service.clone();
        ctx.handle.spawn(async move {
            let _ = match svc.list_prompts().await {
                Ok(list) => tx.send(Msg::List(list)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn load_versions(&mut self, ctx: &PanelCtx<'_>, name: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let tx = channel.tx.clone();
        let svc = ctx.state.prompt_service.clone();
        let name = name.to_string();
        ctx.handle.spawn(async move {
            let _ = match svc.list_versions(&name).await {
                Ok(versions) => tx.send(Msg::Versions(name, versions)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn rewrite(&mut self, ctx: &PanelCtx<'_>, _name: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.status = Some("Rewriting for topic…".to_string());
        let tx = channel.tx.clone();
        let llm = ctx.state.llm_service.clone();
        let ws_svc = ctx.state.workspace_service.clone();
        let workspace_id = ctx.active_workspace_id;
        let original = self.editor.clone();
        ctx.handle.spawn(async move {
            let result: Result<String, AppError> = async {
                let ws = ws_svc.get(workspace_id).await?;
                let mut vars = BTreeMap::new();
                vars.insert("original_prompt".to_string(), original);
                vars.insert("topic_descriptor".to_string(), ws.topic_descriptor);
                vars.insert("primary_question".to_string(), ws.primary_question);
                vars.insert("seed_concepts".to_string(), ws.seed_concepts.join(", "));
                let resp = llm
                    .execute_prompt("prompt_rewriter", vars, None, LlmOutputMode::Text)
                    .await?;
                let content = strip_code_fences(resp.raw_text.trim());
                serde_yaml::from_str::<PromptFileConfig>(&content).map_err(|e| {
                    AppError::BadRequest(format!("rewritten prompt is not valid YAML: {e}"))
                })?;
                Ok(content)
            }
            .await;
            let _ = match result {
                Ok(content) => tx.send(Msg::Rewritten(content)),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }

    fn save(&mut self, ctx: &PanelCtx<'_>, name: &str) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        self.busy = true;
        self.status = Some("Saving…".to_string());
        let tx = channel.tx.clone();
        let svc = ctx.state.prompt_service.clone();
        let name = name.to_string();
        let request = PromptCreate {
            content: self.editor.clone(),
            description: Some("Edited in Prompts tab".to_string()),
        };
        ctx.handle.spawn(async move {
            let _ = match svc.update_prompt(&name, request).await {
                Ok(_) => tx.send(Msg::Saved),
                Err(err) => tx.send(Msg::Error(err.to_string())),
            };
        });
    }
}

/// Strips a ```yaml … ``` code fence the LLM may wrap the output in.
fn strip_code_fences(text: &str) -> String {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    // Drop the language tag on the first line, then the trailing fence.
    let body = rest.split_once('\n').map(|x| x.1).unwrap_or("");
    body.trim().trim_end_matches("```").trim_end().to_string()
}
