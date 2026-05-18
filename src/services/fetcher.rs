use reqwest::Client;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::{error::AppError, services::pipeline::ArticleCandidate};

use std::sync::Arc;

const NCBI_EFETCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentType {
    Pdf,
    Html,
    Xml,
    AbstractOnly,
}

impl ContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Html => "html",
            Self::Xml => "xml",
            Self::AbstractOnly => "abstract_only",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ContentData {
    Text(String),
    Binary(Vec<u8>),
}

impl ContentData {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Binary(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchedContent {
    pub content_type: ContentType,
    pub content: ContentData,
    pub fetch_method: String,
}

#[derive(Clone)]
pub struct ContentFetcher {
    client: Client,
}

impl ContentFetcher {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn fetch(&self, candidate: &ArticleCandidate) -> Option<FetchedContent> {
        let strategies: &[(&str, fn(&ArticleCandidate) -> bool)] = &[
            ("arxiv_pdf", |c| c.source == "arxiv"),
            ("pmc_xml", |c| c.source == "pmc"),
            ("publisher_transform", |c| can_publisher_transform(c)),
            ("pubmed_abstract", |c| {
                c.source == "pubmed" || c.source == "pmc"
            }),
            ("candidate_summary", |c| {
                c.summary
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty())
            }),
        ];

        for &(name, can_fetch) in strategies {
            if !can_fetch(candidate) {
                continue;
            }
            debug!("trying strategy {name} for {}", candidate.uid());
            match self.try_strategy(name, candidate).await {
                Ok(Some(content)) => {
                    info!("fetched {} using {}", candidate.uid(), content.fetch_method);
                    return Some(content);
                }
                Ok(None) => continue,
                Err(error) => {
                    warn!("{name} error for {}: {error}", candidate.uid());
                    continue;
                }
            }
        }

        warn!("all fetch strategies failed for {}", candidate.uid());
        None
    }

    pub async fn fetch_batch(
        &self,
        candidates: &[ArticleCandidate],
        concurrency: usize,
    ) -> Vec<Option<FetchedContent>> {
        let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
        let futures: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                let fetcher = self.clone();
                let semaphore = semaphore.clone();
                let candidate = candidate.clone();
                async move {
                    let Ok(_permit) = semaphore.acquire().await else {
                        return None;
                    };
                    fetcher.fetch(&candidate).await
                }
            })
            .collect();
        futures::future::join_all(futures).await
    }

    async fn try_strategy(
        &self,
        name: &str,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        match name {
            "arxiv_pdf" => self.fetch_arxiv_pdf(candidate).await,
            "pmc_xml" => self.fetch_pmc_xml(candidate).await,
            "publisher_transform" => self.fetch_publisher_transform(candidate).await,
            "pubmed_abstract" => self.fetch_pubmed_abstract(candidate).await,
            "candidate_summary" => self.fetch_candidate_summary(candidate).await,
            _ => Ok(None),
        }
    }

    async fn fetch_arxiv_pdf(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        let arxiv_id = candidate.source_id.replace("v", "");
        let pdf_url = format!("https://arxiv.org/pdf/{arxiv_id}.pdf");

        let response = self
            .client
            .get(&pdf_url)
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("arXiv PDF fetch failed: {error}")))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let bytes = response.bytes().await.map_err(|error| {
            AppError::Internal(format!("failed to read arXiv PDF response: {error}"))
        })?;

        if bytes.len() < 10 || !bytes[..4].starts_with(b"%PDF") {
            warn!("arXiv response not a PDF for {}", candidate.uid());
            return Ok(None);
        }

        Ok(Some(FetchedContent {
            content_type: ContentType::Pdf,
            content: ContentData::Binary(bytes.to_vec()),
            fetch_method: "arxiv_pdf".to_string(),
        }))
    }

    async fn fetch_pmc_xml(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        let pmc_id = candidate.source_id.trim_start_matches("PMC");

        let response = self
            .client
            .get(NCBI_EFETCH_URL)
            .query(&[("db", "pmc"), ("id", pmc_id), ("rettype", "xml")])
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("PMC XML fetch failed: {error}")))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let content = response.text().await.map_err(|error| {
            AppError::Internal(format!("failed to read PMC XML response: {error}"))
        })?;

        if !content.contains("<article") && !content.contains("<body") {
            warn!("PMC XML fetch returned no article content for PMC{pmc_id}");
            return Ok(None);
        }

        Ok(Some(FetchedContent {
            content_type: ContentType::Xml,
            content: ContentData::Text(content),
            fetch_method: "pmc_xml".to_string(),
        }))
    }

    async fn fetch_publisher_transform(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        let Some(doi) = candidate.doi.as_deref() else {
            return Ok(None);
        };
        let doi_lower = doi.to_lowercase();

        let (pdf_url, publisher) = if doi_lower.contains("springer") || doi_lower.contains("s41") {
            (
                format!("https://link.springer.com/content/pdf/{doi}.pdf"),
                "springer",
            )
        } else if doi_lower.contains("nature") {
            let suffix = doi.split('/').last().unwrap_or(doi);
            (
                format!("https://www.nature.com/articles/{suffix}.pdf"),
                "nature",
            )
        } else if doi_lower.contains("biomedcentral") || doi_lower.contains("bmc") {
            (
                format!("https://bmcmedethics.biomedcentral.com/track/pdf/{doi}.pdf"),
                "bmc",
            )
        } else if doi_lower.contains("jmir") {
            (
                format!("https://www.jmir.org/article/download/{doi}/"),
                "jmir",
            )
        } else if doi_lower.contains("cambridge") {
            (
                format!(
                    "https://www.cambridge.org/core/services/aop-cambridge-core/content/view/{doi}"
                ),
                "cambridge",
            )
        } else {
            return Ok(None);
        };

        let response =
            self.client.get(&pdf_url).send().await.map_err(|error| {
                AppError::Internal(format!("publisher PDF fetch failed: {error}"))
            })?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let bytes = response.bytes().await.map_err(|error| {
            AppError::Internal(format!("failed to read publisher PDF response: {error}"))
        })?;

        if bytes.len() < 10 || !bytes[..4].starts_with(b"%PDF") {
            return Ok(None);
        }

        Ok(Some(FetchedContent {
            content_type: ContentType::Pdf,
            content: ContentData::Binary(bytes.to_vec()),
            fetch_method: format!("{publisher}_transform"),
        }))
    }

    async fn fetch_pubmed_abstract(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        let pubmed_id = if candidate.source == "pubmed" {
            candidate.source_id.clone()
        } else {
            // For PMC articles, try to use the linked PubMed ID from summary
            candidate.source_id.trim_start_matches("PMC").to_string()
        };

        let db = if candidate.source == "pmc" {
            "pmc"
        } else {
            "pubmed"
        };

        let response = self
            .client
            .get(NCBI_EFETCH_URL)
            .query(&[
                ("db", db),
                ("id", pubmed_id.as_str()),
                ("rettype", "abstract"),
                ("retmode", "xml"),
            ])
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("PubMed efetch failed: {error}")))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let content = response.text().await.map_err(|error| {
            AppError::Internal(format!("failed to read PubMed response: {error}"))
        })?;

        Ok(Some(FetchedContent {
            content_type: ContentType::AbstractOnly,
            content: ContentData::Text(content),
            fetch_method: "pubmed_efetch".to_string(),
        }))
    }

    async fn fetch_candidate_summary(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        let Some(summary) = candidate
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };

        Ok(Some(FetchedContent {
            content_type: ContentType::AbstractOnly,
            content: ContentData::Text(summary.to_string()),
            fetch_method: "candidate_summary".to_string(),
        }))
    }
}

fn can_publisher_transform(candidate: &ArticleCandidate) -> bool {
    candidate
        .doi
        .as_deref()
        .map(str::to_lowercase)
        .map_or(false, |doi| {
            ["springer", "nature", "biomedcentral", "jmir", "cambridge"]
                .iter()
                .any(|publisher| doi.contains(publisher))
        })
}
