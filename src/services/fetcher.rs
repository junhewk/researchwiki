use reqwest::Client;
use tokio::{process::Command, sync::Semaphore, time::timeout};
use tracing::{debug, info, warn};

use crate::{error::AppError, services::pipeline::ArticleCandidate};

use std::{env, sync::Arc, time::Duration};

const NCBI_EFETCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi";
const MARKITDOWN_COMMAND_ENV: &str = "MARKITDOWN_COMMAND";
const MARKITDOWN_TIMEOUT: Duration = Duration::from_secs(120);
const SCORING_ABSTRACT_CHARS: usize = 4_000;
const SCORING_SECTION_CHARS: usize = 2_500;
const SCORING_SECTION_LIMIT: usize = 4;
const SCORING_TEXT_MAX_CHARS: usize = 14_000;

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

type FetchStrategy = (&'static str, fn(&ArticleCandidate) -> bool);

impl ContentFetcher {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn fetch(&self, candidate: &ArticleCandidate) -> Option<FetchedContent> {
        let strategies: &[FetchStrategy] = &[
            ("arxiv_pdf", |c| c.source == "arxiv"),
            ("arxiv_abstract", |c| {
                c.source == "arxiv" && has_candidate_summary(c)
            }),
            ("pmc_xml", |c| c.source == "pmc"),
            ("unpaywall_oa", |c| c.doi.is_some()),
            ("publisher_transform", |c| can_publisher_transform(c)),
            ("pubmed_abstract", |c| {
                c.source == "pubmed" || c.source == "pmc"
            }),
            ("candidate_summary", has_candidate_summary),
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
            "arxiv_abstract" => self.fetch_arxiv_abstract(candidate).await,
            "arxiv_pdf" => self.fetch_arxiv_pdf(candidate).await,
            "pmc_xml" => self.fetch_pmc_xml(candidate).await,
            "unpaywall_oa" => self.fetch_unpaywall_oa(candidate).await,
            "publisher_transform" => self.fetch_publisher_transform(candidate).await,
            "pubmed_abstract" => self.fetch_pubmed_abstract(candidate).await,
            "candidate_summary" => self.fetch_candidate_summary(candidate).await,
            _ => Ok(None),
        }
    }

    async fn fetch_arxiv_abstract(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        self.fetch_candidate_summary_with_method(candidate, "arxiv_abstract")
            .await
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

        let markdown = match pdf_to_markdown_with_markitdown(&bytes).await {
            Ok(Some(markdown)) => markdown,
            Ok(None) => {
                warn!(
                    "MarkItDown returned no Markdown for {}; falling back to abstract",
                    candidate.uid()
                );
                return Ok(None);
            }
            Err(error) => {
                warn!(
                    "MarkItDown PDF conversion failed for {}: {error}; falling back to abstract",
                    candidate.uid()
                );
                return Ok(None);
            }
        };

        let Some(scoring_text) = build_arxiv_scoring_text(candidate, &markdown) else {
            warn!(
                "MarkItDown produced no scoring sections for {}; falling back to abstract",
                candidate.uid()
            );
            return Ok(None);
        };

        Ok(Some(FetchedContent {
            content_type: ContentType::Pdf,
            content: ContentData::Text(scoring_text),
            fetch_method: "arxiv_pdf_markitdown_scoring".to_string(),
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

    /// Unpaywall's real purpose: given a DOI, resolve the best open-access PDF
    /// location and download it. Works for any DOI-bearing candidate.
    async fn fetch_unpaywall_oa(
        &self,
        candidate: &ArticleCandidate,
    ) -> Result<Option<FetchedContent>, AppError> {
        let Some(doi) = candidate.doi.as_deref() else {
            return Ok(None);
        };
        let email =
            env::var("UNPAYWALL_EMAIL").unwrap_or_else(|_| "junhewk.kim@gmail.com".to_string());
        let url = format!("https://api.unpaywall.org/v2/{doi}");

        let response = self
            .client
            .get(&url)
            .query(&[("email", email.as_str())])
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

        let pdf_response = self
            .client
            .get(&pdf_url)
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("unpaywall PDF fetch failed: {error}")))?;
        if !pdf_response.status().is_success() {
            return Ok(None);
        }
        let bytes = pdf_response.bytes().await.map_err(|error| {
            AppError::Internal(format!("failed to read unpaywall PDF response: {error}"))
        })?;
        if bytes.len() < 10 || !bytes[..4].starts_with(b"%PDF") {
            return Ok(None);
        }

        Ok(Some(FetchedContent {
            content_type: ContentType::Pdf,
            content: ContentData::Binary(bytes.to_vec()),
            fetch_method: "unpaywall_oa".to_string(),
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
            let suffix = doi.split('/').next_back().unwrap_or(doi);
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
        self.fetch_candidate_summary_with_method(candidate, "candidate_summary")
            .await
    }

    async fn fetch_candidate_summary_with_method(
        &self,
        candidate: &ArticleCandidate,
        fetch_method: &str,
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
            fetch_method: fetch_method.to_string(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownSection {
    heading: String,
    body: String,
}

fn build_arxiv_scoring_text(candidate: &ArticleCandidate, markdown: &str) -> Option<String> {
    let sections = selected_scoring_sections(markdown);
    if sections.is_empty() {
        return None;
    }

    let mut text = String::new();
    text.push_str("# Title\n");
    text.push_str(candidate.title.trim());
    text.push_str("\n\n");

    if let Some(summary) = candidate
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        text.push_str("## Abstract (selection basis)\n");
        text.push_str(&truncate_chars(summary, SCORING_ABSTRACT_CHARS));
        text.push_str("\n\n");
    }

    text.push_str("## Selected PDF evidence for scoring\n");
    for section in sections.into_iter().take(SCORING_SECTION_LIMIT) {
        text.push_str("### ");
        text.push_str(section.heading.trim());
        text.push('\n');
        text.push_str(&truncate_chars(section.body.trim(), SCORING_SECTION_CHARS));
        text.push_str("\n\n");

        if text.chars().count() >= SCORING_TEXT_MAX_CHARS {
            break;
        }
    }

    Some(truncate_chars(&text, SCORING_TEXT_MAX_CHARS))
}

fn selected_scoring_sections(markdown: &str) -> Vec<MarkdownSection> {
    let mut sections = Vec::new();
    let mut current_heading = None::<String>;
    let mut current_body = String::new();

    for line in markdown.lines() {
        if let Some(heading) = parse_section_heading(line) {
            push_markdown_section(&mut sections, current_heading.take(), &mut current_body);
            current_heading = Some(heading);
            continue;
        }

        if current_heading.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    push_markdown_section(&mut sections, current_heading, &mut current_body);

    sections
        .into_iter()
        .filter(|section| is_scoring_heading(&section.heading) && !section.body.trim().is_empty())
        .collect()
}

fn push_markdown_section(
    sections: &mut Vec<MarkdownSection>,
    heading: Option<String>,
    body: &mut String,
) {
    if let Some(heading) = heading {
        let trimmed_body = body.trim();
        if !trimmed_body.is_empty() {
            sections.push(MarkdownSection {
                heading,
                body: trimmed_body.to_string(),
            });
        }
    }
    body.clear();
}

fn parse_section_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('#') {
        let heading = trimmed.trim_start_matches('#').trim();
        return (!heading.is_empty()).then(|| clean_heading(heading));
    }

    if !looks_like_plain_heading(trimmed) {
        return None;
    }

    let heading = clean_heading(trimmed);
    (is_common_paper_heading(&heading) || is_scoring_heading(&heading)).then_some(heading)
}

fn looks_like_plain_heading(line: &str) -> bool {
    let char_count = line.chars().count();
    if !(3..=80).contains(&char_count) {
        return false;
    }
    if line.ends_with('.') || line.ends_with(',') || line.contains("  ") {
        return false;
    }
    line.split_whitespace().count() <= 8
}

fn clean_heading(heading: &str) -> String {
    let without_number = heading
        .trim()
        .trim_start_matches(|character: char| {
            character.is_ascii_digit() || matches!(character, '.' | ')' | '(' | '-' | ':' | ' ')
        })
        .trim();
    let cleaned = without_number
        .trim_matches(|character: char| matches!(character, ':' | '.' | '-' | ' '))
        .trim();

    if cleaned.is_empty() {
        heading.trim().to_string()
    } else {
        cleaned.to_string()
    }
}

fn is_scoring_heading(heading: &str) -> bool {
    let heading = heading.to_lowercase();
    [
        "result",
        "finding",
        "discussion",
        "conclusion",
        "evaluation",
        "experiment",
        "analysis",
        "limitation",
        "implication",
    ]
    .iter()
    .any(|keyword| heading.contains(keyword))
}

fn is_common_paper_heading(heading: &str) -> bool {
    let heading = heading.to_lowercase();
    [
        "abstract",
        "introduction",
        "background",
        "related work",
        "methods",
        "method",
        "materials and methods",
        "study design",
        "results",
        "findings",
        "discussion",
        "conclusion",
        "references",
        "bibliography",
        "acknowledgments",
        "acknowledgements",
        "appendix",
        "supplement",
    ]
    .iter()
    .any(|keyword| heading == *keyword || heading.contains(keyword))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n[truncated]")
    } else {
        truncated
    }
}

async fn pdf_to_markdown_with_markitdown(bytes: &[u8]) -> Result<Option<String>, AppError> {
    let path = env::temp_dir().join(format!(
        "researchwiki-markitdown-{}.pdf",
        uuid::Uuid::new_v4()
    ));

    let result = async {
        tokio::fs::write(&path, bytes).await.map_err(|error| {
            AppError::Internal(format!(
                "failed to write temporary PDF for MarkItDown at {}: {error}",
                path.display()
            ))
        })?;

        let markdown = run_markitdown_commands(&path).await?;
        Ok((!markdown.is_empty()).then_some(markdown))
    }
    .await;

    if let Err(error) = tokio::fs::remove_file(&path).await {
        debug!(
            "failed to remove temporary MarkItDown PDF {}: {error}",
            path.display()
        );
    }

    result
}

async fn run_markitdown_commands(path: &std::path::Path) -> Result<String, AppError> {
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
    path: &std::path::Path,
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

fn can_publisher_transform(candidate: &ArticleCandidate) -> bool {
    candidate
        .doi
        .as_deref()
        .map(str::to_lowercase)
        .is_some_and(|doi| {
            ["springer", "nature", "biomedcentral", "jmir", "cambridge"]
                .iter()
                .any(|publisher| doi.contains(publisher))
        })
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
    fn arxiv_scoring_text_keeps_abstract_and_selected_sections() {
        let mut candidate = test_candidate();
        candidate.summary = Some("This is the abstract used for selection.".to_string());
        let markdown = r#"
# Paper title

## Methods
This method text should not be included.

## Results
The intervention improved the primary outcome.

## Discussion
The finding changes the interpretation of prior work.

## References
Reference text should not be included.
"#;

        let text = build_arxiv_scoring_text(&candidate, markdown).unwrap();

        assert!(text.contains("This is the abstract used for selection."));
        assert!(text.contains("## Selected PDF evidence for scoring"));
        assert!(text.contains("### Results"));
        assert!(text.contains("The intervention improved the primary outcome."));
        assert!(text.contains("### Discussion"));
        assert!(!text.contains("This method text should not be included."));
        assert!(!text.contains("Reference text should not be included."));
    }

    #[test]
    fn selected_scoring_sections_support_plain_headings() {
        let markdown = r#"
INTRODUCTION
Background text.

RESULTS
Observed effect size was large.

REFERENCES
Ignored reference.
"#;

        let sections = selected_scoring_sections(markdown);

        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, "RESULTS");
        assert_eq!(sections[0].body, "Observed effect size was large.");
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
