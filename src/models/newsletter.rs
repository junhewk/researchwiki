use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct NewsletterRenderRequest {
    pub article_uids: Vec<String>,
    #[serde(default)]
    pub byline: String,
    #[serde(default)]
    pub outro: String,
    #[serde(default)]
    pub newsletter_title: String,
    #[serde(default)]
    pub rephrased_titles: BTreeMap<String, String>,
    #[serde(default)]
    pub highlights: String,
}

#[derive(Debug, Serialize)]
pub struct NewsletterPreviewResponse {
    pub markdown: String,
}

#[derive(Debug, Serialize)]
pub struct NewsletterExportResponse {
    pub markdown: String,
    pub article_count: usize,
    pub export_date: String,
}

#[derive(Debug, Deserialize)]
pub struct GenerateIntroductionRequest {
    pub core_article_uid: String,
    pub article_count: i32,
}

#[derive(Debug, Deserialize)]
pub struct GenerateTitlesRequest {
    pub article_uids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GenerateHighlightsRequest {
    pub article_uids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GenerateClosingRequest {
    #[serde(default)]
    pub context: String,
    pub article_count: i32,
}

#[derive(Debug, Deserialize)]
pub struct GenerateTitleRequest {
    pub core_article_uid: String,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub introduction: String,
}

#[derive(Debug, Serialize)]
pub struct GenerationResponse {
    pub content: String,
    pub prompt_name: String,
}

#[derive(Debug, Serialize)]
pub struct TitleRephraseItem {
    pub original: String,
    pub rephrased: String,
}

#[derive(Debug, Serialize)]
pub struct TitleRephraseResponse {
    pub titles: Vec<TitleRephraseItem>,
    pub prompt_name: String,
}

#[derive(Debug, Serialize)]
pub struct NewsletterTitleResponse {
    pub options: Vec<String>,
    pub selected: String,
    pub prompt_name: String,
}
