use reqwest::Client;

use crate::{
    config::AppConfig,
    services::{
        articles::ArticleService, embedding::EmbeddingService, jobs::JobService,
        knowledge_graph::KnowledgeGraphService, library::LibraryService, llm::LlmService,
        newsletter::NewsletterService, prompts::PromptService, settings::SettingsService,
        traces::TraceService,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub article_service: std::sync::Arc<ArticleService>,
    pub job_service: std::sync::Arc<JobService>,
    pub knowledge_graph_service: std::sync::Arc<KnowledgeGraphService>,
    pub library_service: std::sync::Arc<LibraryService>,
    pub llm_service: std::sync::Arc<LlmService>,
    pub newsletter_service: std::sync::Arc<NewsletterService>,
    pub settings_service: std::sync::Arc<SettingsService>,
    pub prompt_service: std::sync::Arc<PromptService>,
    pub trace_service: std::sync::Arc<TraceService>,
    pub embedding_service: std::sync::Arc<EmbeddingService>,
    pub http_client: Client,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let article_service =
            std::sync::Arc::new(ArticleService::new(config.storage.database_path.clone()));
        let settings_service =
            std::sync::Arc::new(SettingsService::new(config.storage.settings_file.clone()));
        let prompt_service = std::sync::Arc::new(PromptService::new(
            config.storage.prompts_dir.clone(),
            config.storage.database_path.clone(),
        ));
        let trace_service =
            std::sync::Arc::new(TraceService::new(config.storage.database_path.clone()));
        let llm_service = std::sync::Arc::new(LlmService::new(
            prompt_service.clone(),
            trace_service.clone(),
            config.llm.clone(),
        ));
        let http_client = Client::builder()
            .user_agent("researchwiki/0.1")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client should build");
        let embedding_service = std::sync::Arc::new(EmbeddingService::new(
            http_client.clone(),
            config.embedding.clone(),
        ));
        let knowledge_graph_service = std::sync::Arc::new(KnowledgeGraphService::new(
            config.storage.database_path.clone(),
            config.storage.wiki_export_dir.clone(),
            llm_service.clone(),
            embedding_service.clone(),
        ));
        let library_service = std::sync::Arc::new(LibraryService::new(
            config.storage.database_path.clone(),
            embedding_service.clone(),
            llm_service.clone(),
        ));
        let job_service = std::sync::Arc::new(JobService::new(
            config.storage.database_path.clone(),
            llm_service.clone(),
            settings_service.clone(),
            http_client.clone(),
            library_service.clone(),
            knowledge_graph_service.clone(),
        ));
        let newsletter_service = std::sync::Arc::new(NewsletterService::new(
            article_service.clone(),
            llm_service.clone(),
        ));

        Self {
            config,
            article_service,
            job_service,
            knowledge_graph_service,
            library_service,
            llm_service,
            newsletter_service,
            settings_service,
            prompt_service,
            trace_service,
            embedding_service,
            http_client,
        }
    }
}
