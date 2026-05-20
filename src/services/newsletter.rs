use std::{collections::BTreeMap, sync::Arc};

use chrono::Local;
use serde_json::Value;

use crate::{
    error::AppError,
    models::{
        article::ArticleResponse,
        newsletter::{
            GenerateClosingRequest, GenerateHighlightsRequest, GenerateIntroductionRequest,
            GenerateTitleRequest, GenerateTitlesRequest, GenerationResponse,
            NewsletterExportResponse, NewsletterPreviewResponse, NewsletterRenderRequest,
            NewsletterTitleResponse, TitleRephraseItem, TitleRephraseResponse,
        },
    },
    services::{
        articles::ArticleService,
        llm::{LlmOutputMode, LlmService},
    },
};

#[derive(Clone)]
pub struct NewsletterService {
    article_service: Arc<ArticleService>,
    llm_service: Arc<LlmService>,
}

impl NewsletterService {
    pub fn new(article_service: Arc<ArticleService>, llm_service: Arc<LlmService>) -> Self {
        Self {
            article_service,
            llm_service,
        }
    }

    pub async fn get_newsletter_articles(
        &self,
        days: u32,
        limit: u32,
    ) -> Result<Vec<ArticleResponse>, AppError> {
        self.article_service
            .get_top_articles(days, limit, None)
            .await
    }

    pub async fn preview_newsletter(
        &self,
        request: NewsletterRenderRequest,
    ) -> Result<NewsletterPreviewResponse, AppError> {
        let articles = self
            .article_service
            .get_articles_by_uids(&request.article_uids)
            .await?;
        Ok(NewsletterPreviewResponse {
            markdown: generate_newsletter_markdown(
                &articles,
                &request.byline,
                &request.outro,
                &request.newsletter_title,
                &request.rephrased_titles,
                &request.highlights,
            ),
        })
    }

    pub async fn export_newsletter(
        &self,
        request: NewsletterRenderRequest,
    ) -> Result<NewsletterExportResponse, AppError> {
        let articles = self
            .article_service
            .get_articles_by_uids(&request.article_uids)
            .await?;
        Ok(NewsletterExportResponse {
            markdown: generate_newsletter_markdown(
                &articles,
                &request.byline,
                &request.outro,
                &request.newsletter_title,
                &request.rephrased_titles,
                &request.highlights,
            ),
            article_count: articles.len(),
            export_date: Local::now().date_naive().to_string(),
        })
    }

    pub async fn generate_introduction(
        &self,
        request: GenerateIntroductionRequest,
    ) -> Result<GenerationResponse, AppError> {
        let article = self
            .article_service
            .get_article(&request.core_article_uid)
            .await?;

        let mut variables = BTreeMap::new();
        variables.insert(
            "core_title".to_string(),
            article.title.clone().unwrap_or_default(),
        );
        variables.insert(
            "core_author".to_string(),
            article
                .first_author
                .clone()
                .unwrap_or_else(|| "Unknown".to_string()),
        );
        variables.insert(
            "core_journal".to_string(),
            article
                .journal
                .clone()
                .unwrap_or_else(|| "Unknown".to_string()),
        );
        variables.insert(
            "core_summary".to_string(),
            article
                .byline_summary
                .clone()
                .or_else(|| article.title.clone())
                .unwrap_or_default(),
        );
        variables.insert(
            "core_key_argument".to_string(),
            article.key_argument.clone().unwrap_or_default(),
        );
        variables.insert(
            "core_why_it_matters".to_string(),
            article.why_it_matters.clone().unwrap_or_default(),
        );
        variables.insert(
            "article_count".to_string(),
            request.article_count.to_string(),
        );

        let result = self
            .llm_service
            .execute_prompt(
                "newsletter_introduction",
                variables,
                Some(&request.core_article_uid),
                LlmOutputMode::Text,
            )
            .await?;

        Ok(GenerationResponse {
            content: result.raw_text,
            prompt_name: "newsletter_introduction".to_string(),
        })
    }

    pub async fn generate_titles(
        &self,
        request: GenerateTitlesRequest,
    ) -> Result<TitleRephraseResponse, AppError> {
        let articles = self
            .article_service
            .get_articles_by_uids(&request.article_uids)
            .await?;
        if articles.is_empty() {
            return Err(AppError::NotFound("No articles found".to_string()));
        }

        let titles_list = articles
            .iter()
            .enumerate()
            .map(|(index, article)| {
                format!(
                    "{}. {}",
                    index + 1,
                    article.title.clone().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut variables = BTreeMap::new();
        variables.insert("titles_list".to_string(), titles_list);

        let result = self
            .llm_service
            .execute_prompt(
                "newsletter_title_rephrase",
                variables,
                None,
                LlmOutputMode::Json,
            )
            .await?;

        let titles = parse_title_rephrases(result.json_output)
            .or_else(|| parse_title_rephrases_from_text(&result.raw_text))
            .unwrap_or_default();

        Ok(TitleRephraseResponse {
            titles,
            prompt_name: "newsletter_title_rephrase".to_string(),
        })
    }

    pub async fn generate_highlights(
        &self,
        request: GenerateHighlightsRequest,
    ) -> Result<GenerationResponse, AppError> {
        let articles = self
            .article_service
            .get_articles_by_uids(&request.article_uids)
            .await?;
        if articles.is_empty() {
            return Err(AppError::NotFound("No articles found".to_string()));
        }

        let summaries = articles
            .iter()
            .enumerate()
            .map(|(index, article)| {
                format!(
                    "\n{}. {}\n   - Summary: {}\n   - Key finding: {}\n   - Why it matters: {}\n",
                    index + 1,
                    article.title.clone().unwrap_or_default(),
                    article
                        .byline_summary
                        .clone()
                        .unwrap_or_else(|| "N/A".to_string()),
                    article
                        .main_findings
                        .clone()
                        .unwrap_or_else(|| "N/A".to_string()),
                    article
                        .why_it_matters
                        .clone()
                        .unwrap_or_else(|| "N/A".to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join("");

        let mut variables = BTreeMap::new();
        variables.insert("articles_summaries".to_string(), summaries);

        let result = self
            .llm_service
            .execute_prompt(
                "newsletter_highlights",
                variables,
                None,
                LlmOutputMode::Text,
            )
            .await?;

        Ok(GenerationResponse {
            content: result.raw_text,
            prompt_name: "newsletter_highlights".to_string(),
        })
    }

    pub async fn generate_closing(
        &self,
        request: GenerateClosingRequest,
    ) -> Result<GenerationResponse, AppError> {
        let mut variables = BTreeMap::new();
        variables.insert("context".to_string(), request.context);
        variables.insert(
            "article_count".to_string(),
            request.article_count.to_string(),
        );

        let result = self
            .llm_service
            .execute_prompt("newsletter_closing", variables, None, LlmOutputMode::Text)
            .await?;

        Ok(GenerationResponse {
            content: result.raw_text,
            prompt_name: "newsletter_closing".to_string(),
        })
    }

    pub async fn generate_title(
        &self,
        request: GenerateTitleRequest,
    ) -> Result<NewsletterTitleResponse, AppError> {
        let article = self
            .article_service
            .get_article(&request.core_article_uid)
            .await?;

        let mut variables = BTreeMap::new();
        variables.insert(
            "core_topic".to_string(),
            article
                .primary_issue
                .clone()
                .or_else(|| article.ai_tech.clone())
                .unwrap_or_else(|| "healthcare AI ethics".to_string()),
        );
        variables.insert(
            "core_title".to_string(),
            article.title.clone().unwrap_or_default(),
        );
        variables.insert(
            "themes".to_string(),
            if request.themes.is_empty() {
                "healthcare AI ethics".to_string()
            } else {
                request.themes.join(", ")
            },
        );
        variables.insert(
            "introduction".to_string(),
            if request.introduction.trim().is_empty() {
                "(No introduction provided)".to_string()
            } else {
                request.introduction.clone()
            },
        );

        let result = self
            .llm_service
            .execute_prompt(
                "newsletter_title",
                variables,
                Some(&request.core_article_uid),
                LlmOutputMode::Json,
            )
            .await?;

        let parsed = parse_newsletter_title(result.json_output)
            .or_else(|| parse_newsletter_title_from_text(&result.raw_text));

        match parsed {
            Some((options, selected)) => Ok(NewsletterTitleResponse {
                options,
                selected,
                prompt_name: "newsletter_title".to_string(),
            }),
            None => Ok(NewsletterTitleResponse {
                options: vec![result.raw_text.clone()],
                selected: result.raw_text,
                prompt_name: "newsletter_title".to_string(),
            }),
        }
    }
}

fn generate_newsletter_markdown(
    articles: &[ArticleResponse],
    byline: &str,
    outro: &str,
    newsletter_title: &str,
    rephrased_titles: &BTreeMap<String, String>,
    highlights: &str,
) -> String {
    if articles.is_empty() {
        return "# 이번주 뉴스레터\n\n선정된 기사가 없습니다.".to_string();
    }

    let article_sections = articles
        .iter()
        .enumerate()
        .map(|(index, article)| format_article_section(article, index, rephrased_titles))
        .collect::<Vec<_>>()
        .join("\n\n");

    let references = articles
        .iter()
        .enumerate()
        .map(|(index, article)| format_reference(article, index))
        .collect::<Vec<_>>()
        .join("\n");

    let title_section = if newsletter_title.trim().is_empty() {
        String::new()
    } else {
        format!("# {newsletter_title}\n\n")
    };

    let highlights_section = if highlights.trim().is_empty() {
        "(하이라이트 없음)".to_string()
    } else {
        highlights.to_string()
    };

    format!(
        "{title_section}## 들어가며\n\n{}\n\n## 이번주 주목할 만한 소식\n\n{article_sections}\n\n## 이번주 소식, 하이라이트\n\n{highlights_section}\n\n---\n\n{}\n\n<sub>위 요약은 AI로 자동 수집, 요약 후 LLM-as-a-Judge를 통해 평가지표 기반 상위 {}개 논문·기사를 선정한 것입니다(사용 모델: local qwen3.6-27b-q8 via llama-server).</sub>\n\n## Reference\n\n{references}\n",
        if byline.trim().is_empty() {
            "byline"
        } else {
            byline
        },
        if outro.trim().is_empty() {
            "나가는 말"
        } else {
            outro
        },
        articles.len(),
    )
}

fn format_article_section(
    article: &ArticleResponse,
    index: usize,
    rephrased_titles: &BTreeMap<String, String>,
) -> String {
    let original_title = article
        .title
        .clone()
        .unwrap_or_else(|| "Untitled".to_string());
    let title = rephrased_titles
        .get(&original_title)
        .cloned()
        .unwrap_or_else(|| original_title.clone());

    format!(
        "### {title}\n\n> From {}: {}[^{}]\n\n#### 어떤 내용이야?\n\n{}\n\n#### 왜 읽어야 해?\n\n{}",
        article
            .journal
            .clone()
            .unwrap_or_else(|| "Unknown".to_string()),
        original_title,
        index + 1,
        article
            .byline_summary
            .clone()
            .unwrap_or_else(|| "요약 없음".to_string()),
        article
            .why_it_matters
            .clone()
            .unwrap_or_else(|| "설명 없음".to_string()),
    )
}

fn format_reference(article: &ArticleResponse, index: usize) -> String {
    let author = article
        .first_author
        .clone()
        .unwrap_or_else(|| "Unknown".to_string());
    let title = article
        .title
        .clone()
        .unwrap_or_else(|| "Untitled".to_string());
    let journal = article
        .journal
        .clone()
        .unwrap_or_else(|| "Unknown".to_string());
    let url = article.url.clone().unwrap_or_else(|| "#".to_string());

    format!(
        "[^{}]:{}. {}. *{}*. [{}]({})",
        index + 1,
        author,
        title,
        journal,
        url,
        url
    )
}

fn parse_title_rephrases(json_output: Option<Value>) -> Option<Vec<TitleRephraseItem>> {
    let items = json_output?
        .as_array()?
        .iter()
        .filter_map(|item| {
            Some(TitleRephraseItem {
                original: item.get("original")?.as_str()?.to_string(),
                rephrased: item.get("rephrased")?.as_str()?.to_string(),
            })
        })
        .collect::<Vec<_>>();

    Some(items)
}

fn parse_title_rephrases_from_text(text: &str) -> Option<Vec<TitleRephraseItem>> {
    let value: Value = serde_json::from_str(text).ok()?;
    parse_title_rephrases(Some(value))
}

fn parse_newsletter_title(json_output: Option<Value>) -> Option<(Vec<String>, String)> {
    let value = json_output?;
    let selected = value.get("selected")?.as_str()?.to_string();
    let options = value
        .get("options")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();

    Some((options, selected))
}

fn parse_newsletter_title_from_text(text: &str) -> Option<(Vec<String>, String)> {
    let value: Value = serde_json::from_str(text).ok()?;
    parse_newsletter_title(Some(value))
}
