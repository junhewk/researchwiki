use reqwest::Client;
use sha2::{Digest, Sha256};
use tokio::{process::Command, sync::Semaphore, time::timeout};
use tracing::{debug, info, warn};

use crate::{
    error::AppError,
    services::{pdf_text, pipeline::ArticleCandidate},
};

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const NCBI_EFETCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi";
const MARKITDOWN_COMMAND_ENV: &str = "MARKITDOWN_COMMAND";
const MARKITDOWN_TIMEOUT: Duration = Duration::from_secs(120);
/// Anything smaller cannot be a real article PDF.
const MIN_PDF_BYTES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentType {
    Pdf,
    Html,
    Xml,
    AbstractOnly,
    /// A PDF was downloaded and stored on disk, but text extraction failed.
    /// The file is kept so extraction can be retried later.
    PdfStored,
}

impl ContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Html => "html",
            Self::Xml => "xml",
            Self::AbstractOnly => "abstract_only",
            Self::PdfStored => "pdf_stored",
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
    /// Where the downloaded PDF was persisted, when one was acquired. Kept
    /// even if extraction failed so the article can be re-extracted later.
    pub pdf_path: Option<PathBuf>,
    pub pdf_sha256: Option<String>,
    pub pdf_bytes: Option<i64>,
    pub pdf_source_url: Option<String>,
    pub pdf_fetch_method: Option<String>,
    pub text_extraction_status: Option<String>,
    pub text_extraction_error: Option<String>,
}

struct PdfAcquisition {
    bytes: Vec<u8>,
    method: String,
    source_url: String,
}

struct StoredPdf {
    path: PathBuf,
    sha256: String,
    bytes: i64,
}

#[derive(Clone)]
pub struct ContentFetcher {
    client: Client,
    /// Contact email for Unpaywall OA lookups. `None` disables that strategy so
    /// we never send a placeholder address.
    contact_email: Option<String>,
    /// Directory where acquired PDFs are persisted, keyed by article uid.
    pdf_dir: PathBuf,
}

impl ContentFetcher {
    pub fn new(client: Client, contact_email: Option<String>, pdf_dir: PathBuf) -> Self {
        Self {
            client,
            contact_email: contact_email
                .map(|email| email.trim().to_string())
                .filter(|email| !email.is_empty()),
            pdf_dir,
        }
    }

    /// Two-phase fetch. Phase A walks every PDF acquisition strategy until one
    /// yields a valid PDF, persists it, then extracts text. Phase B falls back
    /// to text-only strategies (PMC XML, PubMed abstract, candidate summary).
    /// A stored-but-unextractable PDF is still reported so the path lands in
    /// the database for later re-extraction.
    pub async fn fetch(&self, candidate: &ArticleCandidate) -> Option<FetchedContent> {
        let mut stored_pdf: Option<PathBuf> = None;
        let mut stored_pdf_sha256: Option<String> = None;
        let mut stored_pdf_bytes: Option<i64> = None;
        let mut stored_pdf_source_url: Option<String> = None;
        let mut stored_pdf_fetch_method: Option<String> = None;
        let mut stored_pdf_extraction_error: Option<String> = None;

        if let Some(pdf) = self.acquire_pdf(candidate).await {
            match self.store_pdf(candidate, &pdf.bytes).await {
                Ok(stored) => match extract_text_from_pdf_file(&stored.path).await {
                    Ok(Some(text)) => {
                        info!("fetched {} using {}", candidate.uid(), pdf.method);
                        return Some(FetchedContent {
                            content_type: ContentType::Pdf,
                            content: ContentData::Text(text),
                            fetch_method: pdf.method.clone(),
                            pdf_path: Some(stored.path),
                            pdf_sha256: Some(stored.sha256),
                            pdf_bytes: Some(stored.bytes),
                            pdf_source_url: Some(pdf.source_url),
                            pdf_fetch_method: Some(pdf.method),
                            text_extraction_status: Some("extracted".to_string()),
                            text_extraction_error: None,
                        });
                    }
                    Ok(None) => {
                        let message = "PDF extraction returned no text".to_string();
                        warn!(
                            "{} for {}; keeping stored PDF at {}",
                            message,
                            candidate.uid(),
                            stored.path.display()
                        );
                        stored_pdf_extraction_error = Some(message);
                        stored_pdf = Some(stored.path);
                        stored_pdf_sha256 = Some(stored.sha256);
                        stored_pdf_bytes = Some(stored.bytes);
                        stored_pdf_source_url = Some(pdf.source_url);
                        stored_pdf_fetch_method = Some(pdf.method);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        warn!(
                            "PDF extraction failed for {}: {message}; keeping stored PDF at {}",
                            candidate.uid(),
                            stored.path.display()
                        );
                        stored_pdf_extraction_error = Some(message);
                        stored_pdf = Some(stored.path);
                        stored_pdf_sha256 = Some(stored.sha256);
                        stored_pdf_bytes = Some(stored.bytes);
                        stored_pdf_source_url = Some(pdf.source_url);
                        stored_pdf_fetch_method = Some(pdf.method);
                    }
                },
                Err(error) => {
                    warn!("failed to persist PDF for {}: {error}", candidate.uid());
                }
            }
        }

        for (name, applicable) in [
            ("pmc_xml", candidate.source == "pmc"),
            (
                "pubmed_abstract",
                candidate.source == "pubmed" || candidate.source == "pmc",
            ),
            ("candidate_summary", has_candidate_summary(candidate)),
        ] {
            if !applicable {
                continue;
            }
            debug!("trying text strategy {name} for {}", candidate.uid());
            let result = match name {
                "pmc_xml" => self.fetch_pmc_xml(candidate).await,
                "pubmed_abstract" => self.fetch_pubmed_abstract(candidate).await,
                _ => self.fetch_candidate_summary(candidate).await,
            };
            match result {
                Ok(Some(mut content)) => {
                    info!("fetched {} using {}", candidate.uid(), content.fetch_method);
                    content.pdf_path = stored_pdf;
                    content.pdf_sha256 = stored_pdf_sha256;
                    content.pdf_bytes = stored_pdf_bytes;
                    content.pdf_source_url = stored_pdf_source_url;
                    content.pdf_fetch_method = stored_pdf_fetch_method;
                    content.text_extraction_status =
                        content.pdf_path.as_ref().map(|_| "failed".to_string());
                    content.text_extraction_error = stored_pdf_extraction_error;
                    return Some(content);
                }
                Ok(None) => continue,
                Err(error) => {
                    warn!("{name} error for {}: {error}", candidate.uid());
                    continue;
                }
            }
        }

        if let Some(path) = stored_pdf {
            return Some(FetchedContent {
                content_type: ContentType::PdfStored,
                content: ContentData::Text(String::new()),
                fetch_method: "pdf_stored_unextracted".to_string(),
                pdf_path: Some(path),
                pdf_sha256: stored_pdf_sha256,
                pdf_bytes: stored_pdf_bytes,
                pdf_source_url: stored_pdf_source_url,
                pdf_fetch_method: stored_pdf_fetch_method,
                text_extraction_status: Some("needs_reextract".to_string()),
                text_extraction_error: stored_pdf_extraction_error,
            });
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

    /// Re-runs the local PDF extractor, falling back to MarkItDown, over a
    /// previously stored PDF.
    pub async fn re_extract_stored_pdf(&self, path: &Path) -> Result<Option<String>, AppError> {
        extract_text_from_pdf_file(path).await
    }

    /// Walks the PDF acquisition strategies in order of reliability and cost;
    /// the first valid PDF (magic-byte checked) wins.
    async fn acquire_pdf(&self, candidate: &ArticleCandidate) -> Option<PdfAcquisition> {
        let unpaywall_ready = candidate.doi.is_some() && self.contact_email.is_some();
        let strategies: [(&str, bool); 5] = [
            ("arxiv_pdf", candidate.source == "arxiv"),
            ("unpaywall_oa", unpaywall_ready),
            ("publisher_pdf", candidate.doi.is_some()),
            ("doi_negotiation", candidate.doi.is_some()),
            (
                "landing_page_pdf",
                candidate.doi.is_some() || !candidate.url.trim().is_empty(),
            ),
        ];

        for (name, applicable) in strategies {
            if !applicable {
                continue;
            }
            debug!("trying PDF strategy {name} for {}", candidate.uid());
            let result = match name {
                "arxiv_pdf" => self.fetch_arxiv_pdf_bytes(candidate).await,
                "unpaywall_oa" => self.fetch_unpaywall_pdf_bytes(candidate).await,
                "publisher_pdf" => self.fetch_publisher_pdf_bytes(candidate).await,
                "doi_negotiation" => self.fetch_doi_negotiation_bytes(candidate).await,
                _ => self.fetch_landing_page_pdf_bytes(candidate).await,
            };
            match result {
                Ok(Some((bytes, source_url))) => {
                    return Some(PdfAcquisition {
                        bytes,
                        method: name.to_string(),
                        source_url,
                    });
                }
                Ok(None) => continue,
                Err(error) => {
                    warn!("{name} error for {}: {error}", candidate.uid());
                    continue;
                }
            }
        }

        None
    }

    async fn store_pdf(
        &self,
        candidate: &ArticleCandidate,
        bytes: &[u8],
    ) -> Result<StoredPdf, AppError> {
        tokio::fs::create_dir_all(&self.pdf_dir)
            .await
            .map_err(|error| {
                AppError::Internal(format!(
                    "failed to create PDF directory {}: {error}",
                    self.pdf_dir.display()
                ))
            })?;
        let sha256 = sha256_hex(bytes);
        let path = self.pdf_dir.join(pdf_filename(&candidate.uid(), &sha256));
        tokio::fs::write(&path, bytes).await.map_err(|error| {
            AppError::Internal(format!("failed to write PDF {}: {error}", path.display()))
        })?;
        Ok(StoredPdf {
            path,
            sha256,
            bytes: bytes.len() as i64,
        })
    }

    /// Downloads `url` and returns the body only when it is a real PDF.
    async fn download_pdf_bytes(
        &self,
        url: &str,
        accept_pdf: bool,
    ) -> Result<Option<Vec<u8>>, AppError> {
        let mut request = self.client.get(url);
        if accept_pdf {
            request = request.header("Accept", "application/pdf");
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("PDF fetch failed for {url}: {error}")))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await.map_err(|error| {
            AppError::Internal(format!("failed to read PDF response from {url}: {error}"))
        })?;
        if !is_pdf_bytes(&bytes) {
            return Ok(None);
        }
        Ok(Some(bytes.to_vec()))
    }

    async fn fetch_arxiv_pdf_bytes(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<(Vec<u8>, String)>, AppError> {
        let arxiv_id = candidate.source_id.replace("v", "");
        let pdf_url = format!("https://arxiv.org/pdf/{arxiv_id}.pdf");
        Ok(self
            .download_pdf_bytes(&pdf_url, false)
            .await?
            .map(|bytes| (bytes, pdf_url)))
    }

    /// Unpaywall's real purpose: given a DOI, resolve the best open-access PDF
    /// location and download it. Works for any DOI-bearing candidate.
    async fn fetch_unpaywall_pdf_bytes(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<(Vec<u8>, String)>, AppError> {
        let Some(doi) = candidate.doi.as_deref() else {
            return Ok(None);
        };
        let Some(email) = self.contact_email.as_deref() else {
            return Ok(None);
        };
        let url = format!("https://api.unpaywall.org/v2/{doi}");

        let response = self
            .client
            .get(&url)
            .query(&[("email", email)])
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("unpaywall lookup failed: {error}")))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|error| AppError::Internal(format!("unpaywall json failed: {error}")))?;

        let location = body.get("best_oa_location");
        let pdf_url = location.and_then(|loc| {
            loc.get("url_for_pdf")
                .and_then(serde_json::Value::as_str)
                .or_else(|| loc.get("url").and_then(serde_json::Value::as_str))
                .map(str::to_string)
        });
        let Some(pdf_url) = pdf_url else {
            return Ok(None);
        };

        Ok(self
            .download_pdf_bytes(&pdf_url, false)
            .await?
            .map(|bytes| (bytes, pdf_url)))
    }

    async fn fetch_publisher_pdf_bytes(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<(Vec<u8>, String)>, AppError> {
        let Some(doi) = candidate.doi.as_deref() else {
            return Ok(None);
        };

        for (pdf_url, publisher) in publisher_pdf_urls(doi) {
            match self.download_pdf_bytes(&pdf_url, false).await {
                Ok(Some(bytes)) => {
                    debug!(
                        "publisher heuristic {publisher} hit for {}",
                        candidate.uid()
                    );
                    return Ok(Some((bytes, pdf_url)));
                }
                Ok(None) => continue,
                Err(error) => {
                    debug!("publisher heuristic {publisher} failed: {error}");
                    continue;
                }
            }
        }

        Ok(None)
    }

    /// DOI content negotiation: some registrars serve the PDF directly when
    /// asked for `application/pdf`. Cheap to try before scraping.
    async fn fetch_doi_negotiation_bytes(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<(Vec<u8>, String)>, AppError> {
        let Some(doi) = candidate.doi.as_deref() else {
            return Ok(None);
        };
        let url = format!("https://doi.org/{doi}");
        Ok(self
            .download_pdf_bytes(&url, true)
            .await?
            .map(|bytes| (bytes, url)))
    }

    /// Generalist fallback: load the article landing page (DOI redirect or the
    /// candidate's own URL) and follow its `citation_pdf_url` meta tag, which
    /// most publishers emit for Google Scholar.
    async fn fetch_landing_page_pdf_bytes(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<(Vec<u8>, String)>, AppError> {
        let landing_url = candidate
            .doi
            .as_deref()
            .map(|doi| format!("https://doi.org/{doi}"))
            .unwrap_or_else(|| candidate.url.trim().to_string());
        if landing_url.is_empty() {
            return Ok(None);
        }

        let response = self
            .client
            .get(&landing_url)
            .send()
            .await
            .map_err(|error| {
                AppError::Internal(format!(
                    "landing page fetch failed for {landing_url}: {error}"
                ))
            })?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let final_url = response.url().clone();
        let bytes = response.bytes().await.map_err(|error| {
            AppError::Internal(format!("failed to read landing page response: {error}"))
        })?;
        // Some landing URLs serve the PDF directly.
        if is_pdf_bytes(&bytes) {
            return Ok(Some((bytes.to_vec(), final_url.to_string())));
        }

        let html = String::from_utf8_lossy(&bytes);
        let Some(pdf_url) = extract_citation_pdf_url(&html) else {
            return Ok(None);
        };
        let pdf_url = match final_url.join(&pdf_url) {
            Ok(resolved) => resolved.to_string(),
            Err(_) => pdf_url,
        };
        Ok(self
            .download_pdf_bytes(&pdf_url, false)
            .await?
            .map(|bytes| (bytes, pdf_url)))
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
            pdf_path: None,
            pdf_sha256: None,
            pdf_bytes: None,
            pdf_source_url: None,
            pdf_fetch_method: None,
            text_extraction_status: None,
            text_extraction_error: None,
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
            pdf_path: None,
            pdf_sha256: None,
            pdf_bytes: None,
            pdf_source_url: None,
            pdf_fetch_method: None,
            text_extraction_status: None,
            text_extraction_error: None,
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
            pdf_path: None,
            pdf_sha256: None,
            pdf_bytes: None,
            pdf_source_url: None,
            pdf_fetch_method: None,
            text_extraction_status: None,
            text_extraction_error: None,
        }))
    }
}

fn has_candidate_summary(candidate: &ArticleCandidate) -> bool {
    candidate
        .summary
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

fn is_pdf_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= MIN_PDF_BYTES && bytes.starts_with(b"%PDF")
}

/// Deterministic filename for a stored PDF: sanitized uid plus an FNV-1a hash
/// suffix so distinct uids that sanitize identically cannot collide.
fn pdf_filename(uid: &str, sha256: &str) -> String {
    let safe: String = uid
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let short_hash = sha256.get(..16).unwrap_or(sha256);
    format!("{safe}-{short_hash}.pdf")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Heuristic per-publisher PDF URLs derived from the DOI. DOI prefixes are
/// stable publisher identifiers; the legacy substring checks stay for DOIs
/// that embed the publisher name.
fn publisher_pdf_urls(doi: &str) -> Vec<(String, &'static str)> {
    let doi = doi.trim();
    let doi_lower = doi.to_lowercase();
    let mut urls = Vec::new();

    if doi_lower.contains("springer") || doi_lower.contains("s41") {
        urls.push((
            format!("https://link.springer.com/content/pdf/{doi}.pdf"),
            "springer",
        ));
    }
    if doi_lower.contains("nature") {
        let suffix = doi.split('/').next_back().unwrap_or(doi);
        urls.push((
            format!("https://www.nature.com/articles/{suffix}.pdf"),
            "nature",
        ));
    }
    if doi_lower.contains("biomedcentral") || doi_lower.contains("bmc") {
        urls.push((
            format!("https://bmcmedethics.biomedcentral.com/track/pdf/{doi}.pdf"),
            "bmc",
        ));
    }
    if doi_lower.contains("jmir") {
        urls.push((
            format!("https://www.jmir.org/article/download/{doi}/"),
            "jmir",
        ));
    }
    if doi_lower.contains("cambridge") {
        urls.push((
            format!(
                "https://www.cambridge.org/core/services/aop-cambridge-core/content/view/{doi}"
            ),
            "cambridge",
        ));
    }
    if doi_lower.starts_with("10.3389/") {
        urls.push((
            format!("https://www.frontiersin.org/articles/{doi}/pdf"),
            "frontiers",
        ));
    }
    if doi_lower.starts_with("10.1371/") {
        urls.push((
            format!("https://journals.plos.org/plosone/article/file?id={doi}&type=printable"),
            "plos",
        ));
    }
    if doi_lower.starts_with("10.7554/") {
        if let Some(article) = doi_lower
            .strip_prefix("10.7554/elife.")
            .map(|suffix| suffix.split('.').next().unwrap_or(suffix))
            .filter(|value| !value.is_empty())
        {
            urls.push((
                format!("https://elifesciences.org/articles/{article}/pdf"),
                "elife",
            ));
        }
    }

    urls
}

/// Pulls the `citation_pdf_url` (Highwire/Google Scholar) meta tag out of a
/// landing page.
fn extract_citation_pdf_url(html: &str) -> Option<String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let selector = Selector::parse(r#"meta[name="citation_pdf_url"]"#).ok()?;
    document
        .select(&selector)
        .filter_map(|element| element.value().attr("content"))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn extract_text_from_pdf_file(path: &Path) -> Result<Option<String>, AppError> {
    let path_for_local = path.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path_for_local).map_err(|error| {
            AppError::Internal(format!(
                "failed to read PDF {}: {error}",
                path_for_local.display()
            ))
        })?;
        pdf_text::extract_pdf_text(&bytes)
            .map(|text| text.trim().to_string())
            .map_err(|error| {
                AppError::Internal(format!("local PDF text extraction failed: {error}"))
            })
    })
    .await
    {
        Ok(Ok(text)) if !text.is_empty() => return Ok(Some(text)),
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            debug!(
                "local PDF extraction failed for {}: {error}",
                path.display()
            );
        }
        Err(error) => {
            debug!(
                "local PDF extraction task failed for {}: {error}",
                path.display()
            );
        }
    }

    let markdown = run_markitdown_commands(path).await?;
    Ok((!markdown.is_empty()).then_some(markdown))
}

async fn run_markitdown_commands(path: &Path) -> Result<String, AppError> {
    let mut commands = Vec::<(String, Vec<String>)>::new();
    if let Ok(command) = env::var(MARKITDOWN_COMMAND_ENV) {
        let command = command.trim();
        if !command.is_empty() {
            commands.push((command.to_string(), Vec::new()));
        }
    }
    commands.push(("markitdown".to_string(), Vec::new()));
    commands.push((
        "uvx".to_string(),
        vec![
            "--from".to_string(),
            "markitdown[pdf]".to_string(),
            "markitdown".to_string(),
        ],
    ));

    let mut errors = Vec::new();
    for (program, args) in commands {
        match run_markitdown_command(&program, &args, path).await {
            Ok(markdown) => return Ok(markdown),
            Err(error) => errors.push(error.to_string()),
        }
    }

    Err(AppError::Internal(format!(
        "all MarkItDown command attempts failed: {}",
        errors.join(" | ")
    )))
}

async fn run_markitdown_command(
    program: &str,
    args: &[String],
    path: &Path,
) -> Result<String, AppError> {
    let mut command = Command::new(program);
    command.args(args).arg(path);
    let output = timeout(MARKITDOWN_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            AppError::Internal(format!(
                "{program} timed out after {} seconds",
                MARKITDOWN_TIMEOUT.as_secs()
            ))
        })?;
    let output = output.map_err(|error| {
        AppError::Internal(format!(
            "failed to run MarkItDown command '{}{}': {error}",
            program,
            format_command_args(args)
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Internal(format!(
            "MarkItDown command '{}{}' exited with status {}: {}",
            program,
            format_command_args(args),
            output.status,
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn format_command_args(args: &[String]) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(" {}", args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_summary_detection_trims_whitespace() {
        let mut candidate = test_candidate();
        candidate.summary = Some("  ".to_string());
        assert!(!has_candidate_summary(&candidate));

        candidate.summary = Some("Useful abstract".to_string());
        assert!(has_candidate_summary(&candidate));
    }

    #[test]
    fn formats_markitdown_command_args() {
        assert_eq!(format_command_args(&[]), "");
        assert_eq!(
            format_command_args(&[
                "--from".to_string(),
                "markitdown[pdf]".to_string(),
                "markitdown".to_string()
            ]),
            " --from markitdown[pdf] markitdown"
        );
    }

    #[test]
    fn pdf_magic_byte_check() {
        assert!(is_pdf_bytes(b"%PDF-1.7 rest of file"));
        assert!(!is_pdf_bytes(b"%PDF"));
        assert!(!is_pdf_bytes(b"<html><body>Not a PDF</body></html>"));
    }

    #[test]
    fn pdf_filenames_are_sanitized_and_collision_resistant() {
        let hash_a = sha256_hex(b"first pdf");
        let hash_b = sha256_hex(b"second pdf");
        let a = pdf_filename("arxiv:2605.00001", &hash_a);
        let b = pdf_filename("arxiv_2605.00001", &hash_b);
        assert!(a.starts_with("arxiv_2605.00001-"));
        assert!(a.ends_with(".pdf"));
        assert_ne!(
            a, b,
            "sanitization collisions are disambiguated by content hash"
        );
        assert_eq!(
            a,
            pdf_filename("arxiv:2605.00001", &hash_a),
            "deterministic"
        );
    }

    #[test]
    fn extracts_citation_pdf_url_from_landing_page() {
        let html = r#"
            <html><head>
                <meta name="citation_title" content="A Paper">
                <meta name="citation_pdf_url" content="https://example.com/article.pdf">
            </head><body></body></html>
        "#;
        assert_eq!(
            extract_citation_pdf_url(html).as_deref(),
            Some("https://example.com/article.pdf")
        );
        assert_eq!(extract_citation_pdf_url("<html></html>"), None);
    }

    #[test]
    fn publisher_urls_cover_doi_prefixes() {
        let frontiers = publisher_pdf_urls("10.3389/fmed.2026.101126");
        assert_eq!(frontiers.len(), 1);
        assert_eq!(frontiers[0].1, "frontiers");

        let plos = publisher_pdf_urls("10.1371/journal.pone.0123456");
        assert_eq!(plos[0].1, "plos");

        let elife = publisher_pdf_urls("10.7554/eLife.12345");
        assert_eq!(elife[0].0, "https://elifesciences.org/articles/12345/pdf");

        assert!(publisher_pdf_urls("10.1000/unknown").is_empty());
    }

    fn test_candidate() -> ArticleCandidate {
        ArticleCandidate {
            source: "arxiv".to_string(),
            source_id: "2605.00001".to_string(),
            title: "Test title".to_string(),
            summary: None,
            first_author: "Unknown".to_string(),
            authors: None,
            pub_date: None,
            journal: Some("arXiv".to_string()),
            doi: None,
            url: "https://arxiv.org/pdf/2605.00001.pdf".to_string(),
        }
    }
}
