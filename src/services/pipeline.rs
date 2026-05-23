use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use quick_xml::{Reader, events::Event};
use reqwest::{Client, StatusCode, header};
use rusqlite::{Connection, params};
use serde_json::Value;
use tokio::{sync::Mutex, task};

use crate::models::workspace::WorkspaceResearchContext;

const ARXIV_API_URL: &str = "https://export.arxiv.org/api/query";
const ARXIV_OAI_URL: &str = "https://oaipmh.arxiv.org/oai";
const ARXIV_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(3);
const ARXIV_MAX_RETRY_DELAY: Duration = Duration::from_secs(15 * 60);
const NCBI_ESEARCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi";
const NCBI_ESUMMARY_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi";
const NCBI_ELINK_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/elink.fcgi";
const EUROPE_PMC_SEARCH_URL: &str = "https://www.ebi.ac.uk/europepmc/webservices/rest/search";
const RXIV_MAX_CANDIDATES: usize = 200;
const RXIV_WINDOW_DAYS: i64 = 30;
const RXIV_MAX_WINDOWS_PER_RUN: usize = 8;
const RXIV_MAX_PAGES_PER_WINDOW: usize = 8;
const MAX_WORKSPACE_SOURCE_QUERIES: usize = 8;
const OPENALEX_SEARCH_URL: &str = "https://api.openalex.org/works";
const CROSSREF_SEARCH_URL: &str = "https://api.crossref.org/works";
const UNPAYWALL_SEARCH_URL: &str = "https://api.unpaywall.org/v2/search";
const SEMANTIC_SCHOLAR_SEARCH_URL: &str = "https://api.semanticscholar.org/graph/v1/paper/search";
const CLINICAL_TRIALS_SEARCH_URL: &str = "https://clinicaltrials.gov/api/v2/studies";
const BASE_USER_AGENT: &str = concat!("researchwiki/", env!("CARGO_PKG_VERSION"));
const DEFAULT_SOURCE_QUERY_LIMIT: i32 = 50;

/// Polite-pool User-Agent. Includes a `mailto:` only when the user has provided
/// a contact email, so we never advertise an address we don't own.
fn polite_pool_ua(contact_email: Option<&str>) -> String {
    match contact_email {
        Some(email) if !email.trim().is_empty() => {
            format!("{BASE_USER_AGENT} (mailto:{})", email.trim())
        }
        _ => BASE_USER_AGENT.to_string(),
    }
}

const ARXIV_QUERIES: &[&str] = &[
    r#"(all:"artificial intelligence" OR all:"machine learning" OR all:"large language model") AND (all:clinical OR all:healthcare OR all:medical OR all:medicine) AND (all:ethics OR all:bias OR all:fairness OR all:privacy OR all:governance)"#,
    r#"all:"clinical decision support" AND (all:ethics OR all:bias OR all:fairness OR all:accountability)"#,
    r#"(all:"human in the loop" OR all:"human oversight") AND (all:healthcare OR all:clinical OR all:medical)"#,
    r#"(all:"AI governance" OR all:"algorithmic fairness") AND (all:healthcare OR all:clinical OR all:medical)"#,
];

const ARXIV_OAI_SETS: &[&str] = &[
    "cs:cs:AI",
    "cs:cs:LG",
    "cs:cs:CL",
    "cs:cs:CY",
    "cs:cs:HC",
    "stat:stat:ML",
];
const ARXIV_OAI_MAX_PAGES_PER_SET: usize = 4;

const PMC_QUERY: &str = r#"("artificial intelligence"[All Fields] OR "machine learning"[All Fields] OR "large language model"[All Fields] OR "clinical decision support"[All Fields]) AND (ethics[All Fields] OR bias[All Fields] OR fairness[All Fields] OR privacy[All Fields] OR governance[All Fields] OR accountability[All Fields]) AND open access[filter]"#;
const PUBMED_QUERY: &str = r#"(("Artificial Intelligence"[Mesh] OR "Machine Learning"[Mesh] OR "artificial intelligence"[Title/Abstract] OR "machine learning"[Title/Abstract] OR "large language model"[Title/Abstract] OR "clinical decision support"[Title/Abstract]) AND (ethics[Title/Abstract] OR bias[Title/Abstract] OR fairness[Title/Abstract] OR privacy[Title/Abstract] OR governance[Title/Abstract] OR accountability[Title/Abstract]) AND hasabstract[text])"#;

const EUROPE_PMC_QUERIES: &[&str] = &[
    r#"("artificial intelligence" OR "machine learning" OR "large language model" OR "clinical decision support") AND (ethics OR bias OR fairness OR privacy OR governance OR accountability) AND (clinical OR healthcare OR medicine OR medical) AND OPEN_ACCESS:Y"#,
    r#""clinical decision support" AND (ethics OR bias OR fairness OR accountability) AND OPEN_ACCESS:Y"#,
    r#""large language model" AND (healthcare OR clinical OR medical) AND (ethics OR privacy OR governance OR safety) AND OPEN_ACCESS:Y"#,
];

const RXIV_MEDICAL_AI_ETHICS_QUERIES: &[&str] = &[
    "artificial intelligence ethics",
    "machine learning bias",
    "clinical decision support",
    "large language model healthcare",
];

const SCHOLARLY_FREE_TEXT_QUERIES: &[&str] = &[
    "clinical artificial intelligence ethics",
    "medical machine learning bias fairness",
    "large language models healthcare ethics",
    "clinical decision support privacy governance",
];

const CLINICAL_TRIAL_QUERIES: &[&str] = &[
    "artificial intelligence ethics",
    "machine learning bias",
    "clinical decision support",
    "large language model",
];

pub const GATHER_SOURCE_IDS: &[&str] = &[
    "arxiv",
    "pmc",
    "pubmed",
    "europepmc",
    "medrxiv",
    "biorxiv",
    "openalex",
    "crossref",
    "semantic_scholar",
    "clinical_trials",
];

#[derive(Clone)]
pub struct PipelineService {
    database_path: Arc<std::path::PathBuf>,
    client: Client,
    arxiv_limiter: Arc<Mutex<ArxivRequestLimiter>>,
    /// Contact email for Unpaywall (None disables that source). The polite-pool
    /// User-Agent is baked into `client` and `polite_ua`.
    contact_email: Option<String>,
    polite_ua: String,
}

#[derive(Debug, Default)]
struct ArxivRequestLimiter {
    next_request_at: Option<Instant>,
}

impl ArxivRequestLimiter {
    async fn wait_for_turn(&mut self) {
        let Some(next_request_at) = self.next_request_at else {
            return;
        };

        let now = Instant::now();
        if next_request_at > now {
            tokio::time::sleep(next_request_at.duration_since(now)).await;
        }
    }

    fn mark_request_started(&mut self) {
        self.next_request_at = Some(Instant::now() + ARXIV_MIN_REQUEST_INTERVAL);
    }

    fn defer_for(&mut self, delay: Duration) {
        let next_request_at = Instant::now() + delay.max(ARXIV_MIN_REQUEST_INTERVAL);
        self.next_request_at = Some(match self.next_request_at {
            Some(existing) => existing.max(next_request_at),
            None => next_request_at,
        });
    }
}

#[derive(Debug, Clone)]
pub struct ArticleCandidate {
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub summary: Option<String>,
    pub first_author: String,
    pub authors: Option<String>,
    pub pub_date: Option<String>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub url: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SaveCounters {
    pub saved: i32,
    pub skipped: i32,
    pub errors: i32,
}

impl PipelineService {
    pub fn new(database_path: std::path::PathBuf, contact_email: Option<String>) -> Self {
        let polite_ua = polite_pool_ua(contact_email.as_deref());
        let client = Client::builder()
            .user_agent(&polite_ua)
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client should build");

        Self {
            database_path: Arc::new(database_path),
            client,
            arxiv_limiter: Arc::new(Mutex::new(ArxivRequestLimiter::default())),
            contact_email: contact_email
                .map(|email| email.trim().to_string())
                .filter(|email| !email.is_empty()),
            polite_ua,
        }
    }

    pub async fn list_source(
        &self,
        source: &str,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let candidates = match source {
            "arxiv" => self.list_arxiv(days_back, profile).await,
            "pmc" => self.list_pmc(days_back, profile).await,
            "pubmed" => self.list_pubmed(days_back, profile).await,
            "europepmc" => self.list_europe_pmc(days_back, profile).await,
            "medrxiv" => self.list_rxiv("medrxiv", days_back, profile).await,
            "biorxiv" => self.list_rxiv("biorxiv", days_back, profile).await,
            "openalex" => self.list_openalex(days_back, profile).await,
            "crossref" => self.list_crossref(days_back, profile).await,
            "unpaywall" => self.list_unpaywall(days_back, profile).await,
            "semantic_scholar" => self.list_semantic_scholar(days_back, profile).await,
            "clinical_trials" => self.list_clinical_trials(days_back, profile).await,
            _ => bail!("unsupported source: {source}"),
        }?;
        Ok(filter_candidates_by_workspace_terms(candidates, profile))
    }

    pub async fn check_duplicates_batch(
        &self,
        uids: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        if uids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let database_path = self.database_path.clone();
        let uids = uids.to_vec();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path)?;
            let placeholders = vec!["?"; uids.len()].join(", ");
            let sql = format!(
                "SELECT uid FROM haie_rev
                 WHERE uid IN ({placeholders})
                   AND scholarly_rigor IS NOT NULL
                   AND novelty IS NOT NULL
                   AND relevance_score IS NOT NULL
                   AND practical_impact IS NOT NULL
                   AND interdisciplinary IS NOT NULL
                   AND critical_concerns IS NOT NULL
                   AND total_score IS NOT NULL
                   AND priority IS NOT NULL"
            );
            let params: Vec<rusqlite::types::Value> =
                uids.into_iter().map(rusqlite::types::Value::Text).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<std::result::Result<std::collections::HashSet<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .await
        .context("duplicate check task failed")?
    }

    pub async fn save_candidates(
        &self,
        candidates: Vec<ArticleCandidate>,
        workspace_id: i64,
    ) -> Result<SaveCounters> {
        let database_path = self.database_path.clone();

        task::spawn_blocking(move || save_candidates_sync(&database_path, candidates, workspace_id))
            .await
            .context("candidate save task failed")?
    }

    pub async fn save_evaluated_candidate(
        &self,
        candidate: &ArticleCandidate,
        evaluation: &serde_json::Map<String, serde_json::Value>,
        workspace_id: i64,
    ) -> Result<SaveCounters> {
        let database_path = self.database_path.clone();
        let candidate = candidate.clone();
        let evaluation = evaluation.clone();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path).with_context(|| {
                format!("failed to open database at {}", database_path.display())
            })?;
            save_article_sync(&conn, &candidate, Some(&evaluation), workspace_id)
        })
        .await
        .context("evaluated candidate save task failed")?
    }

    async fn list_arxiv(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let today = Utc::now().date_naive();
        let start = today
            .checked_sub_days(Days::new(days_back.max(1) as u64))
            .unwrap_or(today);

        if !profile.query_terms().is_empty() {
            return self.list_arxiv_legacy_search(start, today, profile).await;
        }

        match self.list_arxiv_oai(start, today).await {
            Ok(candidates) => return Ok(candidates),
            Err(error) => {
                tracing::warn!(
                    "arXiv OAI-PMH harvest failed; falling back to legacy search API: {error}"
                );
            }
        }

        self.list_arxiv_legacy_search(start, today, profile).await
    }

    async fn list_arxiv_oai(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let mut merged = std::collections::BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();

        for (index, set) in ARXIV_OAI_SETS.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(ARXIV_MIN_REQUEST_INTERVAL).await;
            }

            match self.fetch_arxiv_oai_set(set, start, end).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("arXiv OAI-PMH set '{set}' failed: {error}");
                    errors.push(error);
                }
            }
        }

        finish_merged_source("arXiv OAI-PMH", merged, errors)
    }

    async fn fetch_arxiv_oai_set(
        &self,
        set: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let mut candidates = Vec::new();
        let mut resumption_token = None::<String>;

        for _ in 0..ARXIV_OAI_MAX_PAGES_PER_SET {
            let body = if let Some(token) = resumption_token.as_deref() {
                self.send_arxiv_oai_request(&[
                    ("verb", "ListRecords".to_string()),
                    ("resumptionToken", token.to_string()),
                ])
                .await?
            } else {
                self.send_arxiv_oai_request(&[
                    ("verb", "ListRecords".to_string()),
                    ("from", start.format("%Y-%m-%d").to_string()),
                    ("until", end.format("%Y-%m-%d").to_string()),
                    ("metadataPrefix", "arXiv".to_string()),
                    ("set", set.to_string()),
                ])
                .await?
            };

            let page = parse_arxiv_oai_records(&body)?;
            if page.no_records_match {
                return Ok(filter_arxiv_oai_candidates(candidates));
            }
            candidates.extend(page.candidates);

            let Some(token) = page.resumption_token else {
                return Ok(filter_arxiv_oai_candidates(candidates));
            };
            if token.trim().is_empty() {
                return Ok(filter_arxiv_oai_candidates(candidates));
            }
            resumption_token = Some(token);
        }

        if resumption_token.is_some() {
            tracing::warn!(
                set,
                pages = ARXIV_OAI_MAX_PAGES_PER_SET,
                "arXiv OAI-PMH page limit reached; using partial harvest"
            );
        }

        Ok(filter_arxiv_oai_candidates(candidates))
    }

    async fn list_arxiv_legacy_search(
        &self,
        start: NaiveDate,
        today: NaiveDate,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let mut merged = std::collections::BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = arxiv_queries(profile);
        for (index, query) in queries.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(ARXIV_MIN_REQUEST_INTERVAL).await;
            }
            match self.fetch_arxiv_query(query, start, today).await {
                Ok(candidates) => {
                    for candidate in candidates {
                        merged
                            .entry(candidate.source_id.clone())
                            .or_insert(candidate);
                    }
                }
                Err(error) => {
                    let rate_limited = error.to_string().contains("HTTP 429");
                    tracing::warn!("arXiv query '{query}' failed: {error}");
                    errors.push(error);
                    if rate_limited {
                        break;
                    }
                }
            }
        }

        if merged.is_empty() && !errors.is_empty() {
            bail!(
                "all arXiv queries failed; first error: {}",
                errors.remove(0)
            );
        }

        Ok(merged.into_values().collect())
    }

    async fn fetch_arxiv_query(
        &self,
        query: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let from_str = format!("{}0000", start.format("%Y%m%d"));
        let to_str = format!("{}0000", end.format("%Y%m%d"));
        let search_query = format!("{query} AND submittedDate:[{from_str} TO {to_str}]");

        const MAX_ATTEMPTS: usize = 1;
        const RETRY_DELAYS: [Duration; 3] = [
            Duration::from_secs(60),
            Duration::from_secs(180),
            Duration::from_secs(600),
        ];

        for attempt in 1..=MAX_ATTEMPTS {
            let response = self.send_arxiv_query_request(&search_query).await?;

            let status = response.status();
            let retry_after = parse_retry_after(response.headers().get(header::RETRY_AFTER));
            let body = response
                .text()
                .await
                .context("failed to read arXiv response body")?;

            if status.is_success() {
                return parse_arxiv_feed(&body);
            }

            let snippet = body.chars().take(240).collect::<String>();
            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if retryable && attempt < MAX_ATTEMPTS {
                let delay = if status == StatusCode::TOO_MANY_REQUESTS {
                    retry_after
                        .unwrap_or(RETRY_DELAYS[attempt - 1])
                        .max(RETRY_DELAYS[attempt - 1])
                } else {
                    RETRY_DELAYS[attempt - 1]
                }
                .min(ARXIV_MAX_RETRY_DELAY);
                self.defer_arxiv_requests(delay).await;
                tracing::warn!(
                    "arXiv query '{query}' returned HTTP {}; backing off for {} seconds",
                    status.as_u16(),
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            bail!(
                "arXiv query '{query}' returned HTTP {}: {}",
                status.as_u16(),
                snippet
            );
        }

        unreachable!("arXiv retry loop always returns or bails")
    }

    async fn send_arxiv_query_request(&self, search_query: &str) -> Result<reqwest::Response> {
        let mut limiter = self.arxiv_limiter.lock().await;
        limiter.wait_for_turn().await;
        limiter.mark_request_started();

        let params = vec![
            ("search_query", search_query.to_string()),
            ("start", "0".to_string()),
            ("sortBy", "submittedDate".to_string()),
            ("sortOrder", "descending".to_string()),
            (
                "max_results",
                DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 100).to_string(),
            ),
        ];
        let request = if search_query.len() > 1400 {
            self.client.post(ARXIV_API_URL).form(&params)
        } else {
            self.client.get(ARXIV_API_URL).query(&params)
        };

        request
            .send()
            .await
            .context("failed to request arXiv query")
    }

    async fn send_arxiv_oai_request(&self, params: &[(&str, String)]) -> Result<String> {
        let mut limiter = self.arxiv_limiter.lock().await;
        limiter.wait_for_turn().await;
        limiter.mark_request_started();

        let response = self
            .client
            .get(ARXIV_OAI_URL)
            .query(params)
            .send()
            .await
            .context("failed to request arXiv OAI-PMH")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read arXiv OAI-PMH response body")?;

        if status.is_success() {
            return Ok(body);
        }

        let snippet = body.chars().take(240).collect::<String>();
        bail!(
            "arXiv OAI-PMH returned HTTP {}: {}",
            status.as_u16(),
            snippet
        )
    }

    async fn defer_arxiv_requests(&self, delay: Duration) {
        let mut limiter = self.arxiv_limiter.lock().await;
        limiter.defer_for(delay);
    }

    async fn list_pmc(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let ids = self
            .get_ncbi_json(
                NCBI_ESEARCH_URL,
                vec![
                    ("db", "pmc".to_string()),
                    ("term", pmc_term(profile)),
                    ("reldate", days_back.clamp(1, 3650).to_string()),
                    ("retmax", "50".to_string()),
                    ("retmode", "json".to_string()),
                ],
                "PMC search",
            )
            .await?
            .get("esearchresult")
            .and_then(|value| value.get("idlist"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();

        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids_csv = ids.join(",");
        let summaries = self
            .get_ncbi_json(
                NCBI_ESUMMARY_URL,
                vec![
                    ("db", "pmc".to_string()),
                    ("id", ids_csv.clone()),
                    ("retmode", "json".to_string()),
                ],
                "PMC summary",
            )
            .await?;

        let links = self
            .get_ncbi_json(
                NCBI_ELINK_URL,
                vec![
                    ("dbfrom", "pmc".to_string()),
                    ("db", "pubmed".to_string()),
                    ("id", ids_csv),
                    ("retmode", "json".to_string()),
                ],
                "PMC elink",
            )
            .await?;

        let mut pmc_to_pubmed = std::collections::HashMap::<String, String>::new();
        for linkset in links
            .get("linksets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source_pmc_id = linkset
                .get("ids")
                .and_then(Value::as_array)
                .and_then(|ids| ids.first())
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);

            let pubmed_id = linkset
                .get("linksetdbs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find_map(|entry| {
                    if entry.get("dbto").and_then(Value::as_str) != Some("pubmed") {
                        return None;
                    }

                    entry
                        .get("links")
                        .and_then(Value::as_array)
                        .and_then(|links| links.first())
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                });

            if let (Some(pmc_id), Some(pubmed_id)) = (source_pmc_id, pubmed_id) {
                pmc_to_pubmed.insert(pmc_id, pubmed_id);
            }
        }

        let result = summaries.get("result").unwrap_or(&Value::Null);
        let mut candidates = Vec::new();
        for pmc_id in ids {
            let Some(summary) = result.get(&pmc_id) else {
                continue;
            };

            let Some(title) = summary.get("title").and_then(Value::as_str) else {
                continue;
            };
            if title.trim().is_empty() {
                continue;
            }

            let authors = extract_authors(summary.get("authors"));
            let doi = extract_doi(summary);
            let pubmed_id = pmc_to_pubmed.get(&pmc_id).cloned();
            let mut journal = summary
                .get("fulljournalname")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            if journal.is_none() {
                journal = Some("PMC".to_string());
            }

            let mut summary_text = None;
            if let Some(pubmed_id) = pubmed_id {
                summary_text = Some(format!("Linked PubMed ID: {pubmed_id}"));
            }

            candidates.push(ArticleCandidate {
                source: "pmc".to_string(),
                source_id: format!("PMC{pmc_id}"),
                title: title.trim().to_string(),
                summary: summary_text,
                first_author: authors.0.unwrap_or_else(|| "Unknown".to_string()),
                authors: authors.1,
                pub_date: parse_partial_date(summary.get("pubdate").and_then(Value::as_str)),
                journal,
                doi,
                url: format!("https://www.ncbi.nlm.nih.gov/pmc/articles/PMC{pmc_id}"),
            });
        }

        Ok(candidates)
    }

    async fn list_pubmed(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let today = Utc::now().date_naive();
        let max_date = today
            .checked_sub_days(Days::new(1))
            .unwrap_or(today)
            .format("%Y/%m/%d")
            .to_string();
        let min_date = today
            .checked_sub_days(Days::new(days_back.max(1) as u64))
            .unwrap_or(today)
            .format("%Y/%m/%d")
            .to_string();

        let ids = self
            .get_ncbi_json(
                NCBI_ESEARCH_URL,
                vec![
                    ("db", "pubmed".to_string()),
                    ("term", pubmed_term(profile)),
                    ("retmode", "json".to_string()),
                    ("sort", "pub_date".to_string()),
                    ("mindate", min_date),
                    ("maxdate", max_date),
                    ("retstart", "0".to_string()),
                    ("retmax", "50".to_string()),
                ],
                "PubMed search",
            )
            .await?
            .get("esearchresult")
            .and_then(|value| value.get("idlist"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();

        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let summaries = self
            .get_ncbi_json(
                NCBI_ESUMMARY_URL,
                vec![
                    ("db", "pubmed".to_string()),
                    ("id", ids.join(",")),
                    ("retmode", "json".to_string()),
                ],
                "PubMed summary",
            )
            .await?;

        let result = summaries.get("result").unwrap_or(&Value::Null);
        let mut candidates = Vec::new();
        for pubmed_id in ids {
            let Some(summary) = result.get(&pubmed_id) else {
                continue;
            };

            let Some(title) = summary.get("title").and_then(Value::as_str) else {
                continue;
            };
            if title.trim().is_empty() {
                continue;
            }

            let authors = extract_authors(summary.get("authors"));
            let mut doi = summary
                .get("elocationid")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| value.starts_with("doi:"))
                .map(|value| {
                    value
                        .trim_start_matches("doi:")
                        .trim_start_matches(' ')
                        .trim()
                        .to_string()
                });
            if doi.is_none() {
                doi = extract_doi(summary);
            }

            let journal = summary
                .get("fulljournalname")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| Some("PubMed".to_string()));

            candidates.push(ArticleCandidate {
                source: "pubmed".to_string(),
                source_id: pubmed_id.clone(),
                title: title.trim().to_string(),
                summary: None,
                first_author: authors.0.unwrap_or_else(|| "Unknown".to_string()),
                authors: authors.1,
                pub_date: parse_partial_date(summary.get("pubdate").and_then(Value::as_str)),
                journal,
                doi,
                url: format!("https://pubmed.ncbi.nlm.nih.gov/{pubmed_id}"),
            });
        }

        Ok(candidates)
    }

    async fn list_europe_pmc(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = europe_pmc_queries(profile);
        for (index, query) in queries.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_europe_pmc_query(query, start, today).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("Europe PMC query '{query}' failed: {error}");
                    errors.push(error);
                }
            }
        }

        finish_merged_source("Europe PMC", merged, errors)
    }

    async fn fetch_europe_pmc_query(
        &self,
        query: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let query = format!(
            "{query} AND FIRST_PDATE:[{} TO {}]",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );
        let params = vec![
            ("query", query),
            ("format", "json".to_string()),
            ("resultType", "core".to_string()),
            ("pageSize", DEFAULT_SOURCE_QUERY_LIMIT.to_string()),
        ];
        let body = self
            .get_json_with_retries(EUROPE_PMC_SEARCH_URL, &params, "Europe PMC", None)
            .await?;
        let results = body
            .get("resultList")
            .and_then(|value| value.get("result"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(results.iter().filter_map(europe_pmc_candidate).collect())
    }

    async fn list_rxiv(
        &self,
        server: &'static str,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);
        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let queries = free_text_queries(profile, RXIV_MEDICAL_AI_ETHICS_QUERIES);
        let categories = rxiv_categories(server, profile);

        for (window_start, window_end) in rxiv_date_windows(start, today)
            .into_iter()
            .take(rxiv_max_windows_per_run(server))
        {
            let collection = self
                .fetch_rxiv_collection(server, window_start, window_end, &categories)
                .await?;
            for query in &queries {
                merge_candidates(
                    &mut merged,
                    rxiv_candidates_from_collection(server, &collection, query),
                );
            }
            if merged.len() >= RXIV_MAX_CANDIDATES.min(DEFAULT_SOURCE_QUERY_LIMIT as usize) {
                break;
            }
        }

        Ok(merged.into_values().collect())
    }

    async fn fetch_rxiv_collection(
        &self,
        server: &'static str,
        start: NaiveDate,
        end: NaiveDate,
        categories: &[Option<&'static str>],
    ) -> Result<Vec<Value>> {
        let mut merged = BTreeMap::<String, Value>::new();
        let mut errors = Vec::new();
        let categories = if categories.is_empty() {
            vec![None]
        } else {
            categories.to_vec()
        };

        for category in categories {
            match self
                .fetch_rxiv_category_collection(server, start, end, category)
                .await
            {
                Ok(collection) => {
                    for entry in collection {
                        let key = entry
                            .get("doi")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(ToOwned::to_owned)
                            .unwrap_or_else(|| {
                                entry
                                    .get("title")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string()
                            });
                        merged.entry(key).or_insert(entry);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        server,
                        category = category.unwrap_or("all"),
                        "rxiv metadata page failed: {error}"
                    );
                    errors.push(error);
                }
            }
        }

        finish_json_collection("rxiv metadata", merged, errors)
    }

    async fn fetch_rxiv_category_collection(
        &self,
        server: &'static str,
        start: NaiveDate,
        end: NaiveDate,
        category: Option<&'static str>,
    ) -> Result<Vec<Value>> {
        let mut cursor = 0usize;
        let mut collection = Vec::new();

        for _ in 0..rxiv_max_pages_per_window(server) {
            let body = self
                .fetch_rxiv_page(server, start, end, cursor, category)
                .await?;
            let mut page = rxiv_collection_from_body(&body);
            let page_len = page.len();
            if page_len == 0 {
                break;
            }

            collection.append(&mut page);
            cursor += page_len;

            let total = rxiv_total_from_body(&body);
            if total.is_some_and(|total| cursor >= total) {
                break;
            }
        }

        Ok(collection)
    }

    async fn fetch_rxiv_page(
        &self,
        server: &'static str,
        start: NaiveDate,
        end: NaiveDate,
        cursor: usize,
        category: Option<&str>,
    ) -> Result<Value> {
        let url = format!(
            "https://api.biorxiv.org/details/{server}/{}/{}/{cursor}/json",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(category) = category {
            params.push(("category", category.to_string()));
        }
        self.get_json_with_retries(&url, &params, rxiv_label(server), None)
            .await
    }

    async fn list_openalex(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = free_text_queries(profile, SCHOLARLY_FREE_TEXT_QUERIES);
        for (index, query) in queries.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_openalex_query(query, start, today).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("OpenAlex query '{query}' failed: {error}");
                    errors.push(error);
                }
            }
        }

        finish_merged_source("OpenAlex", merged, errors)
    }

    async fn fetch_openalex_query(
        &self,
        query: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let filter = format!(
            "from_publication_date:{},to_publication_date:{}",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );
        let params = vec![
            ("search", query.to_string()),
            ("filter", format!("{filter},is_oa:true")),
            (
                "per-page",
                DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 200).to_string(),
            ),
        ];
        let body = self
            .get_json_with_retries(
                OPENALEX_SEARCH_URL,
                &params,
                "OpenAlex",
                Some(self.polite_ua.as_str()),
            )
            .await?;
        let results = body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(results
            .iter()
            .filter_map(|work| openalex_candidate(work).ok())
            .collect())
    }

    async fn list_crossref(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = free_text_queries(profile, SCHOLARLY_FREE_TEXT_QUERIES);
        for (index, query) in queries.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_crossref_query(query, start, today).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("Crossref query '{query}' failed: {error}");
                    errors.push(error);
                }
            }
        }

        finish_merged_source("Crossref", merged, errors)
    }

    async fn fetch_crossref_query(
        &self,
        query: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let filter = format!(
            "from-pub-date:{},until-pub-date:{},type:journal-article,has-abstract:true",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );
        let params = vec![
            ("query.title", query.to_string()),
            ("filter", filter),
            ("rows", DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 100).to_string()),
        ];
        let body = self
            .get_json_with_retries(
                CROSSREF_SEARCH_URL,
                &params,
                "Crossref",
                Some(self.polite_ua.as_str()),
            )
            .await?;
        let items = body
            .get("message")
            .and_then(|value| value.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(items.iter().filter_map(crossref_candidate).collect())
    }

    async fn list_unpaywall(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let Some(email) = self.contact_email.clone() else {
            tracing::warn!(
                "Skipping Unpaywall source: no contact email configured (set one in Settings)."
            );
            return Ok(Vec::new());
        };
        let (start, today) = date_window(days_back);
        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = free_text_queries(profile, SCHOLARLY_FREE_TEXT_QUERIES);
        for (index, query) in queries.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_unpaywall_query(query, &email).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("Unpaywall query '{query}' failed: {error}");
                    errors.push(error);
                }
            }
        }

        finish_merged_source("Unpaywall", merged, errors)
            .map(|candidates| filter_candidates_by_known_date(candidates, start, today))
    }

    async fn fetch_unpaywall_query(
        &self,
        query: &str,
        email: &str,
    ) -> Result<Vec<ArticleCandidate>> {
        let params = vec![
            ("query", query.to_string()),
            ("is_oa", "true".to_string()),
            ("email", email.to_string()),
        ];
        let body = self
            .get_json_with_retries(UNPAYWALL_SEARCH_URL, &params, "Unpaywall", None)
            .await?;
        let results = body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(results
            .iter()
            .take(DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 100) as usize)
            .filter_map(unpaywall_candidate)
            .collect())
    }

    async fn list_semantic_scholar(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = free_text_queries(profile, SCHOLARLY_FREE_TEXT_QUERIES);
        for (index, query) in queries.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_semantic_scholar_query(query, start, today).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    let rate_limited = error.to_string().contains("HTTP 429");
                    tracing::warn!("Semantic Scholar query '{query}' failed: {error}");
                    errors.push(error);
                    if rate_limited {
                        break;
                    }
                }
            }
        }

        finish_merged_source("Semantic Scholar", merged, errors)
    }

    async fn fetch_semantic_scholar_query(
        &self,
        query: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<ArticleCandidate>> {
        let params = vec![
            ("query", query.to_string()),
            (
                "limit",
                DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 100).to_string(),
            ),
            (
                "fields",
                "title,abstract,url,year,authors,journal,externalIds,publicationDate".to_string(),
            ),
            ("year", format!("{}-{}", start.year(), end.year())),
        ];
        let body = self
            .get_json_with_retry_limit(
                SEMANTIC_SCHOLAR_SEARCH_URL,
                &params,
                "Semantic Scholar",
                None,
                1,
            )
            .await?;
        let items = body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(items
            .iter()
            .filter_map(semantic_scholar_candidate)
            .collect())
    }

    async fn list_clinical_trials(
        &self,
        days_back: i32,
        profile: &WorkspaceResearchContext,
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);
        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        let queries = free_text_queries(profile, CLINICAL_TRIAL_QUERIES);
        for (index, query) in queries.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_clinical_trials_query(query).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("ClinicalTrials.gov query '{query}' failed: {error}");
                    errors.push(error);
                }
            }
        }

        finish_merged_source("ClinicalTrials.gov", merged, errors)
            .map(|candidates| filter_candidates_by_known_date(candidates, start, today))
    }

    async fn fetch_clinical_trials_query(&self, query: &str) -> Result<Vec<ArticleCandidate>> {
        let params = vec![
            ("query.term", query.to_string()),
            (
                "pageSize",
                DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 100).to_string(),
            ),
            ("format", "json".to_string()),
        ];
        let body = self
            .get_json_with_retries(
                CLINICAL_TRIALS_SEARCH_URL,
                &params,
                "ClinicalTrials.gov",
                None,
            )
            .await?;
        let studies = body
            .get("studies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(studies
            .iter()
            .filter_map(clinical_trials_candidate)
            .collect())
    }

    async fn get_json_with_retries(
        &self,
        url: &str,
        params: &[(&str, String)],
        label: &str,
        user_agent: Option<&str>,
    ) -> Result<Value> {
        self.get_json_with_retry_limit(url, params, label, user_agent, 4)
            .await
    }

    async fn get_json_with_retry_limit(
        &self,
        url: &str,
        params: &[(&str, String)],
        label: &str,
        user_agent: Option<&str>,
        max_attempts: usize,
    ) -> Result<Value> {
        let mut delay = Duration::from_secs(2);
        let max_attempts = max_attempts.max(1);

        for attempt in 1..=max_attempts {
            let mut request = self.client.get(url).query(params);
            if let Some(user_agent) = user_agent {
                request = request.header("User-Agent", user_agent);
            }

            let response = request
                .send()
                .await
                .with_context(|| format!("failed to request {label}"))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .with_context(|| format!("failed to read {label} response body"))?;

            if status.is_success() {
                return serde_json::from_str(&body)
                    .with_context(|| format!("failed to decode {label} response"));
            }

            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if retryable && attempt < max_attempts {
                tracing::warn!(
                    label,
                    attempt,
                    status = status.as_u16(),
                    "retrying scholarly source request"
                );
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
                continue;
            }

            let snippet = body.chars().take(240).collect::<String>();
            bail!("{label} returned HTTP {}: {}", status.as_u16(), snippet);
        }

        bail!("{label} exhausted retries")
    }

    async fn get_ncbi_json(
        &self,
        url: &str,
        params: Vec<(&'static str, String)>,
        label: &str,
    ) -> Result<Value> {
        const MAX_ATTEMPTS: usize = 4;
        let mut delay = std::time::Duration::from_millis(1100);

        for attempt in 1..=MAX_ATTEMPTS {
            let response = self
                .client
                .get(url)
                .query(&params)
                .send()
                .await
                .with_context(|| format!("failed to request {label}"))?;

            let status = response.status();
            let body = response
                .text()
                .await
                .with_context(|| format!("failed to read {label} response body"))?;

            if status.is_success() {
                return serde_json::from_str(&body)
                    .with_context(|| format!("failed to decode {label} response"));
            }

            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if retryable && attempt < MAX_ATTEMPTS {
                tracing::warn!(
                    label,
                    attempt,
                    status = status.as_u16(),
                    "retrying throttled NCBI request"
                );
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
                continue;
            }

            let snippet = body.chars().take(240).collect::<String>();
            bail!("{label} returned HTTP {}: {}", status.as_u16(), snippet);
        }

        bail!("{label} exhausted retries")
    }
}

/// Builds a free-text query list from the workspace profile, falling back to the
/// source's default queries when the workspace defines no concepts.
fn free_text_queries(profile: &WorkspaceResearchContext, fallback: &[&str]) -> Vec<String> {
    let concepts = source_query_terms(profile);
    if concepts.is_empty() {
        fallback.iter().map(|q| (*q).to_string()).collect()
    } else {
        concepts
    }
}

fn source_query_terms(profile: &WorkspaceResearchContext) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    profile
        .query_terms()
        .iter()
        .filter_map(|term| {
            let term = clean_text(term);
            if term.is_empty() || !seen.insert(term.to_ascii_lowercase()) {
                None
            } else {
                Some(term)
            }
        })
        .take(MAX_WORKSPACE_SOURCE_QUERIES)
        .collect()
}

fn quoted_or(concepts: &[String], suffix: &str) -> String {
    concepts
        .iter()
        .map(|c| format!("\"{c}\"{suffix}"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn pmc_term(profile: &WorkspaceResearchContext) -> String {
    let concepts = source_query_terms(profile);
    if concepts.is_empty() {
        return PMC_QUERY.to_string();
    }
    format!(
        "({}) AND open access[filter]",
        quoted_or(&concepts, "[All Fields]")
    )
}

fn pubmed_term(profile: &WorkspaceResearchContext) -> String {
    let concepts = source_query_terms(profile);
    if concepts.is_empty() {
        return PUBMED_QUERY.to_string();
    }
    format!(
        "(({}) AND hasabstract[text])",
        quoted_or(&concepts, "[Title/Abstract]")
    )
}

fn europe_pmc_queries(profile: &WorkspaceResearchContext) -> Vec<String> {
    let concepts = source_query_terms(profile);
    if concepts.is_empty() {
        return EUROPE_PMC_QUERIES
            .iter()
            .map(|q| (*q).to_string())
            .collect();
    }
    vec![format!("({}) AND OPEN_ACCESS:Y", quoted_or(&concepts, ""))]
}

fn arxiv_queries(profile: &WorkspaceResearchContext) -> Vec<String> {
    let concepts = source_query_terms(profile);
    if concepts.is_empty() {
        return ARXIV_QUERIES.iter().map(|q| (*q).to_string()).collect();
    }

    concepts
        .iter()
        .filter_map(|query| arxiv_query_from_workspace_query(query))
        .collect()
}

fn arxiv_query_from_workspace_query(query: &str) -> Option<String> {
    let mut clauses = Vec::new();
    let domains = ordered_focused_query_tokens(query)
        .into_iter()
        .filter(|token| WORKSPACE_DOMAIN_TOKENS.contains(&token.as_str()))
        .map(|token| format!("all:{token}"))
        .collect::<Vec<_>>();
    if !domains.is_empty() {
        clauses.push(format!("({})", domains.join(" OR ")));
    }

    let phrase_group = arxiv_interest_phrase_clauses(query);
    if !phrase_group.is_empty() {
        clauses.push(format!("({})", phrase_group.join(" OR ")));
    }

    if clauses.len() < 2 {
        for token in ordered_focused_query_tokens(query) {
            if WORKSPACE_DOMAIN_TOKENS.contains(&token.as_str()) {
                continue;
            }
            if arxiv_phrase_covers_token(query, &token) {
                continue;
            }
            let clause = format!("all:{token}");
            if !clauses.contains(&clause) {
                clauses.push(clause);
            }
            if clauses.len() >= 3 {
                break;
            }
        }
    }

    (!clauses.is_empty()).then(|| clauses.join(" AND "))
}

fn filter_candidates_by_workspace_terms(
    candidates: Vec<ArticleCandidate>,
    profile: &WorkspaceResearchContext,
) -> Vec<ArticleCandidate> {
    let query_groups = focused_query_token_groups(profile);
    if query_groups.is_empty() {
        return candidates;
    }

    candidates
        .into_iter()
        .filter(|candidate| candidate_matches_workspace_query(candidate, &query_groups))
        .collect()
}

struct QueryTokenGroup {
    tokens: Vec<String>,
    anchors: Vec<String>,
    domains: Vec<String>,
    tech: Vec<String>,
}

fn focused_query_token_groups(profile: &WorkspaceResearchContext) -> Vec<QueryTokenGroup> {
    profile
        .query_terms()
        .iter()
        .filter_map(|query| {
            let tokens = focused_query_tokens(query);
            if tokens.is_empty() {
                return None;
            }
            let anchors = tokens
                .iter()
                .filter(|token| !WORKSPACE_ANCHOR_STOPWORDS.contains(&token.as_str()))
                .cloned()
                .collect();
            let domains = tokens
                .iter()
                .filter(|token| WORKSPACE_DOMAIN_TOKENS.contains(&token.as_str()))
                .cloned()
                .collect();
            let tech = tokens
                .iter()
                .filter(|token| WORKSPACE_TECH_TOKENS.contains(&token.as_str()))
                .cloned()
                .collect();
            Some(QueryTokenGroup {
                tokens,
                anchors,
                domains,
                tech,
            })
        })
        .collect()
}

fn candidate_matches_workspace_query(
    candidate: &ArticleCandidate,
    query_groups: &[QueryTokenGroup],
) -> bool {
    let text = candidate_search_text(candidate);
    let title = normalized_search_text(&candidate.title);
    query_groups.iter().any(|group| {
        let matched = group
            .tokens
            .iter()
            .filter(|token| text.contains(token.as_str()))
            .count();
        let anchor_matched = group.anchors.is_empty()
            || group
                .anchors
                .iter()
                .any(|token| title.contains(token.as_str()));
        let domain_matched = group.domains.is_empty()
            || group
                .domains
                .iter()
                .any(|token| text.contains(token.as_str()));
        let tech_matched =
            group.tech.is_empty() || group.tech.iter().any(|token| text.contains(token.as_str()));
        matched >= group.tokens.len().min(2) && anchor_matched && domain_matched && tech_matched
    })
}

fn candidate_search_text(candidate: &ArticleCandidate) -> String {
    let mut text = format!("{} ", candidate.title);
    if let Some(summary) = candidate.summary.as_deref() {
        text.push_str(summary);
        text.push(' ');
    }
    if let Some(journal) = candidate.journal.as_deref() {
        text.push_str(journal);
    }
    normalized_search_text(&text)
}

fn focused_query_tokens(query: &str) -> Vec<String> {
    tokenize_search_text(query)
        .into_iter()
        .filter(|token| !WORKSPACE_QUERY_STOPWORDS.contains(&token.as_str()))
        .collect()
}

fn ordered_focused_query_tokens(query: &str) -> Vec<String> {
    let normalized = normalized_search_text(query);
    let mut seen = std::collections::BTreeSet::new();
    normalized
        .split_whitespace()
        .filter(|token| token.len() >= 3)
        .map(str::to_string)
        .filter(|token| !WORKSPACE_QUERY_STOPWORDS.contains(&token.as_str()))
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

fn arxiv_interest_phrase_clauses(query: &str) -> Vec<String> {
    let lower = query.to_ascii_lowercase();
    let mut clauses = Vec::new();
    let mut push = |clause: &str| {
        let clause = clause.to_string();
        if !clauses.contains(&clause) {
            clauses.push(clause);
        }
    };

    if lower.contains("large language model") || lower.contains("large language models") {
        push(r#"all:"large language model""#);
        push("all:llm");
    }
    if lower.contains("chatbot") || lower.contains("chat bot") || lower.contains("chat-bot") {
        push("all:chatbot");
    }
    if lower.contains("chatgpt") {
        push("all:chatgpt");
    }
    if lower.contains("conversational agent") || lower.contains("conversational agents") {
        push(r#"all:"conversational agent""#);
    }
    if lower.contains("virtual coach") || lower.contains("virtual coaching") {
        push(r#"all:"virtual coach""#);
    }
    if lower.contains("digital coach") || lower.contains("digital coaching") {
        push(r#"all:"digital coach""#);
    }
    if lower.contains("counseling") {
        push("all:counseling");
    }
    if lower.contains("counselling") {
        push("all:counselling");
    }

    clauses
}

fn arxiv_phrase_covers_token(query: &str, token: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    match token {
        "llm" => lower.contains("large language model") || lower.contains("large language models"),
        "chatbot" => {
            lower.contains("chatbot") || lower.contains("chat bot") || lower.contains("chat-bot")
        }
        "chatgpt" => lower.contains("chatgpt"),
        "conversational" | "agent" => {
            lower.contains("conversational agent") || lower.contains("conversational agents")
        }
        "virtual" | "coach" | "coaching" => {
            lower.contains("virtual coach")
                || lower.contains("virtual coaching")
                || lower.contains("digital coach")
                || lower.contains("digital coaching")
        }
        "counseling" => lower.contains("counseling"),
        "counselling" => lower.contains("counselling"),
        _ => false,
    }
}

fn tokenize_search_text(text: &str) -> Vec<String> {
    let normalized = normalized_search_text(text);
    let mut tokens = normalized
        .split_whitespace()
        .filter(|token| token.len() >= 3)
        .map(str::to_string)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn normalized_search_text(text: &str) -> String {
    let mut normalized = text.to_ascii_lowercase();
    for (from, to) in [
        ("large language model", "llm"),
        ("large-language-model", "llm"),
        ("chat bot", "chatbot"),
        ("chat-bot", "chatbot"),
        ("self-management", "self management"),
    ] {
        normalized = normalized.replace(from, to);
    }
    normalized
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
}

const WORKSPACE_QUERY_STOPWORDS: &[&str] = &[
    "adult",
    "adults",
    "clinical",
    "education",
    "intervention",
    "large",
    "language",
    "life",
    "management",
    "model",
    "outcome",
    "outcomes",
    "patient",
    "patients",
    "quality",
    "randomized",
    "research",
    "safety",
    "self",
    "study",
    "trial",
    "type",
    "usual",
];

const WORKSPACE_ANCHOR_STOPWORDS: &[&str] = &[
    "adherence",
    "agent",
    "cgm",
    "diabetes",
    "glycemic",
    "glucose",
    "hba1c",
    "monitoring",
];

const WORKSPACE_DOMAIN_TOKENS: &[&str] = &["diabetes", "glycemic", "glycaemic", "glucose", "hba1c"];

const WORKSPACE_TECH_TOKENS: &[&str] = &[
    "cgm",
    "chatbot",
    "chatgpt",
    "conversational",
    "digital",
    "llm",
    "virtual",
];

fn date_window(days_back: i32) -> (NaiveDate, NaiveDate) {
    let today = Utc::now().date_naive();
    let start = today
        .checked_sub_days(Days::new(days_back.max(1) as u64))
        .unwrap_or(today);
    (start, today)
}

fn rxiv_date_windows(start: NaiveDate, end: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut windows = Vec::new();
    let mut window_end = end;

    loop {
        let window_start = window_end
            .checked_sub_days(Days::new(RXIV_WINDOW_DAYS as u64 - 1))
            .unwrap_or(start)
            .max(start);
        windows.push((window_start, window_end));
        if window_start <= start {
            break;
        }
        window_end = window_start
            .checked_sub_days(Days::new(1))
            .unwrap_or(window_start);
    }

    windows
}

fn rxiv_categories(
    server: &'static str,
    profile: &WorkspaceResearchContext,
) -> Vec<Option<&'static str>> {
    let text = profile.query_terms().join(" ").to_ascii_lowercase();
    if server == "medrxiv"
        && ["diabetes", "glycemic", "glycaemic", "glucose", "hba1c"]
            .iter()
            .any(|needle| text.contains(needle))
    {
        return vec![Some("endocrinology"), Some("health_informatics")];
    }

    vec![None]
}

fn rxiv_max_windows_per_run(server: &str) -> usize {
    match server {
        "biorxiv" => 2,
        _ => RXIV_MAX_WINDOWS_PER_RUN,
    }
}

fn rxiv_max_pages_per_window(server: &str) -> usize {
    match server {
        "biorxiv" => 2,
        _ => RXIV_MAX_PAGES_PER_WINDOW,
    }
}

fn rxiv_collection_from_body(body: &Value) -> Vec<Value> {
    body.get("collection")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn rxiv_total_from_body(body: &Value) -> Option<usize> {
    body.get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("total"))
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .map(|value| value as usize)
}

fn filter_candidates_by_known_date(
    candidates: Vec<ArticleCandidate>,
    start: NaiveDate,
    end: NaiveDate,
) -> Vec<ArticleCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| {
            candidate
                .pub_date
                .as_deref()
                .and_then(parse_candidate_date)
                .is_none_or(|date| date >= start && date <= end)
        })
        .collect()
}

fn parse_candidate_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(value, "%Y/%m/%d").ok())
}

fn parse_retry_after(value: Option<&header::HeaderValue>) -> Option<Duration> {
    let value = value?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&Utc);
    let now = Utc::now();
    if retry_at <= now {
        return Some(ARXIV_MIN_REQUEST_INTERVAL);
    }

    retry_at.signed_duration_since(now).to_std().ok()
}

async fn pause_between_source_queries(index: usize) {
    if index > 0 {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn merge_candidates(
    merged: &mut BTreeMap<String, ArticleCandidate>,
    candidates: Vec<ArticleCandidate>,
) {
    for candidate in candidates {
        merged.entry(candidate.uid()).or_insert(candidate);
    }
}

fn finish_merged_source(
    label: &str,
    merged: BTreeMap<String, ArticleCandidate>,
    mut errors: Vec<anyhow::Error>,
) -> Result<Vec<ArticleCandidate>> {
    if merged.is_empty() && !errors.is_empty() {
        bail!(
            "all {label} queries failed; first error: {}",
            errors.remove(0)
        );
    }

    Ok(merged.into_values().collect())
}

fn finish_json_collection(
    label: &str,
    merged: BTreeMap<String, Value>,
    mut errors: Vec<anyhow::Error>,
) -> Result<Vec<Value>> {
    if merged.is_empty() && !errors.is_empty() {
        bail!(
            "all {label} queries failed; first error: {}",
            errors.remove(0)
        );
    }

    Ok(merged.into_values().collect())
}

pub fn is_gather_source(source: &str) -> bool {
    GATHER_SOURCE_IDS.contains(&source)
}

pub fn source_label(source: &str) -> Option<&'static str> {
    match source {
        "arxiv" => Some("arXiv"),
        "pmc" => Some("PMC"),
        "pubmed" => Some("PubMed"),
        "europepmc" => Some("Europe PMC"),
        "medrxiv" => Some("medRxiv"),
        "biorxiv" => Some("bioRxiv"),
        "openalex" => Some("OpenAlex"),
        "crossref" => Some("Crossref"),
        "unpaywall" => Some("Unpaywall"),
        "semantic_scholar" => Some("Semantic Scholar"),
        "clinical_trials" => Some("ClinicalTrials.gov"),
        _ => None,
    }
}

fn europe_pmc_candidate(entry: &Value) -> Option<ArticleCandidate> {
    let title = entry
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty())?;
    let source_id = entry
        .get("pmid")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            entry
                .get("pmcid")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })?;
    let authors = entry
        .get("authorString")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty());
    let first_author = authors
        .as_deref()
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "Unknown".to_string());
    let doi = entry
        .get("doi")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let url = entry
        .get("fullTextUrlList")
        .and_then(|value| value.get("fullTextUrl"))
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|first| first.get("url"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("https://europepmc.org/article/MED/{source_id}"));

    Some(ArticleCandidate {
        source: "europepmc".to_string(),
        source_id,
        title,
        summary: entry
            .get("abstractText")
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty()),
        first_author,
        authors,
        pub_date: entry
            .get("firstPublicationDate")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        journal: entry
            .get("journalTitle")
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty())
            .or_else(|| Some("Europe PMC".to_string())),
        doi,
        url,
    })
}

fn rxiv_candidates_from_collection(
    server: &str,
    collection: &[Value],
    query: &str,
) -> Vec<ArticleCandidate> {
    let needle = query.trim().to_lowercase();
    let mut candidates = Vec::new();

    for entry in collection {
        let Some(title_raw) = entry.get("title").and_then(Value::as_str) else {
            continue;
        };
        let abstract_text = entry.get("abstract").and_then(Value::as_str).unwrap_or("");
        let haystack = format!("{title_raw} {abstract_text}");
        if !needle.is_empty() && !text_matches_query(&haystack, query) {
            continue;
        }

        let Some(doi) = entry
            .get("doi")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let authors = entry
            .get("authors")
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty());
        let first_author = authors
            .as_deref()
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Unknown".to_string());

        candidates.push(ArticleCandidate {
            source: server.to_string(),
            source_id: doi.clone(),
            title: clean_text(title_raw),
            summary: (!abstract_text.trim().is_empty()).then(|| clean_text(abstract_text)),
            first_author,
            authors,
            pub_date: entry
                .get("date")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            journal: Some(rxiv_label(server).to_string()),
            doi: Some(doi.clone()),
            url: format!("https://www.{server}.org/content/{doi}"),
        });

        if candidates.len() >= RXIV_MAX_CANDIDATES.min(DEFAULT_SOURCE_QUERY_LIMIT as usize) {
            break;
        }
    }

    candidates
}

fn openalex_candidate(work: &Value) -> Result<ArticleCandidate> {
    let id_url = work.get("id").and_then(Value::as_str).unwrap_or("");
    let source_id = strip_openalex_id(id_url).to_string();
    if source_id.is_empty() {
        bail!("OpenAlex work did not include an id");
    }

    let title = work
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("OpenAlex work did not include a title"))?;
    let author_names = work
        .get("authorships")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|authorship| {
            authorship
                .get("author")
                .and_then(|value| value.get("display_name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    let first_author = author_names
        .first()
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());
    let authors = (!author_names.is_empty()).then(|| author_names.join(", "));
    let summary = work
        .get("abstract_inverted_index")
        .and_then(Value::as_object)
        .map(reconstruct_inverted_abstract)
        .filter(|value| !value.is_empty());
    let doi = work
        .get("doi")
        .and_then(Value::as_str)
        .map(strip_doi_url)
        .filter(|value| !value.is_empty());
    let url = work
        .get("primary_location")
        .and_then(|value| value.get("landing_page_url"))
        .and_then(Value::as_str)
        .or_else(|| {
            work.get("open_access")
                .and_then(|value| value.get("oa_url"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("https://openalex.org/{source_id}"));

    Ok(ArticleCandidate {
        source: "openalex".to_string(),
        source_id,
        title,
        summary,
        first_author,
        authors,
        pub_date: work
            .get("publication_date")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        journal: work
            .get("primary_location")
            .and_then(|value| value.get("source"))
            .and_then(|value| value.get("display_name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| Some("OpenAlex".to_string())),
        doi,
        url,
    })
}

fn crossref_candidate(work: &Value) -> Option<ArticleCandidate> {
    let doi = work
        .get("DOI")
        .and_then(Value::as_str)
        .map(strip_doi_url)
        .filter(|value| !value.is_empty())?;
    let title = first_string(work.get("title"))?;
    let authors = crossref_authors(work.get("author"));
    let first_author = authors
        .first()
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());

    Some(ArticleCandidate {
        source: "crossref".to_string(),
        source_id: doi.clone(),
        title,
        summary: first_string(work.get("abstract")).map(|value| clean_text(&value)),
        first_author,
        authors: (!authors.is_empty()).then(|| authors.join(", ")),
        pub_date: work
            .get("published-print")
            .or_else(|| work.get("published-online"))
            .or_else(|| work.get("issued"))
            .and_then(date_parts),
        journal: first_string(work.get("container-title")),
        doi: Some(doi.clone()),
        url: work
            .get("URL")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("https://doi.org/{doi}")),
    })
}

fn unpaywall_candidate(result: &Value) -> Option<ArticleCandidate> {
    let item = result.get("response").unwrap_or(result);
    let doi = item
        .get("doi")
        .and_then(Value::as_str)
        .map(strip_doi_url)
        .filter(|value| !value.is_empty())?;
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty())?;
    let authors = person_authors(item.get("z_authors"));
    let first_author = authors
        .first()
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());

    Some(ArticleCandidate {
        source: "unpaywall".to_string(),
        source_id: doi.clone(),
        title,
        summary: None,
        first_author,
        authors: (!authors.is_empty()).then(|| authors.join(", ")),
        pub_date: item
            .get("published_date")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        journal: item
            .get("journal_name")
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty()),
        doi: Some(doi.clone()),
        url: item
            .get("best_oa_location")
            .and_then(|value| value.get("url"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("https://doi.org/{doi}")),
    })
}

fn semantic_scholar_candidate(paper: &Value) -> Option<ArticleCandidate> {
    let paper_id = paper.get("paperId").and_then(Value::as_str)?.to_string();
    let title = paper
        .get("title")
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty())?;
    let authors = paper
        .get("authors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|author| author.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let doi = paper
        .get("externalIds")
        .and_then(|value| value.get("DOI"))
        .and_then(Value::as_str)
        .map(strip_doi_url)
        .filter(|value| !value.is_empty());

    Some(ArticleCandidate {
        source: "semantic_scholar".to_string(),
        source_id: doi.clone().unwrap_or(paper_id.clone()),
        title,
        summary: paper
            .get("abstract")
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty()),
        first_author: authors
            .first()
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string()),
        authors: (!authors.is_empty()).then(|| authors.join(", ")),
        pub_date: paper
            .get("publicationDate")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                paper
                    .get("year")
                    .and_then(Value::as_i64)
                    .map(|year| format!("{year}-01-01"))
            }),
        journal: paper
            .get("journal")
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty()),
        doi,
        url: paper
            .get("url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("https://www.semanticscholar.org/paper/{paper_id}")),
    })
}

fn clinical_trials_candidate(study: &Value) -> Option<ArticleCandidate> {
    let protocol = study.get("protocolSection")?;
    let identification = protocol.get("identificationModule")?;
    let status = protocol.get("statusModule");
    let description = protocol.get("descriptionModule");
    let sponsor = protocol.get("sponsorCollaboratorsModule");

    let nct_id = identification
        .get("nctId")
        .and_then(Value::as_str)?
        .to_string();
    let title = identification
        .get("briefTitle")
        .or_else(|| identification.get("officialTitle"))
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty())?;
    let lead_sponsor = sponsor
        .and_then(|value| value.get("leadSponsor"))
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .map(clean_text)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "ClinicalTrials.gov".to_string());

    Some(ArticleCandidate {
        source: "clinical_trials".to_string(),
        source_id: nct_id.clone(),
        title,
        summary: description
            .and_then(|value| value.get("briefSummary"))
            .and_then(Value::as_str)
            .map(clean_text)
            .filter(|value| !value.is_empty()),
        first_author: lead_sponsor.clone(),
        authors: Some(lead_sponsor),
        pub_date: status
            .and_then(|value| value.get("studyFirstPostDateStruct"))
            .and_then(|value| value.get("date"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        journal: Some("ClinicalTrials.gov".to_string()),
        doi: None,
        url: format!("https://clinicaltrials.gov/study/{nct_id}"),
    })
}

fn rxiv_label(server: &str) -> &'static str {
    match server {
        "medrxiv" => "medRxiv",
        "biorxiv" => "bioRxiv",
        _ => "Rxiv",
    }
}

fn first_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .or_else(|| value.and_then(Value::as_str))
        .map(clean_text)
        .filter(|value| !value.is_empty())
}

fn crossref_authors(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|author| {
            let given = author
                .get("given")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let family = author
                .get("family")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let full = clean_text(format!("{given} {family}").as_str());
            (!full.is_empty()).then_some(full)
        })
        .collect()
}

fn person_authors(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|author| {
            let given = author
                .get("given")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let family = author
                .get("family")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let full = clean_text(format!("{given} {family}").as_str());
            (!full.is_empty()).then_some(full)
        })
        .collect()
}

fn date_parts(value: &Value) -> Option<String> {
    let parts = value
        .get("date-parts")
        .and_then(Value::as_array)?
        .first()?
        .as_array()?;
    let year = parts.first()?.as_i64()?;
    let month = parts.get(1).and_then(Value::as_i64).unwrap_or(1);
    let day = parts.get(2).and_then(Value::as_i64).unwrap_or(1);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn strip_openalex_id(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url).trim()
}

fn strip_doi_url(doi: &str) -> String {
    doi.trim()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("doi:")
        .trim()
        .to_string()
}

fn reconstruct_inverted_abstract(index: &serde_json::Map<String, Value>) -> String {
    let mut positioned: Vec<(usize, &str)> = Vec::new();
    for (word, positions) in index {
        if let Some(items) = positions.as_array() {
            for position in items {
                if let Some(position) = position.as_u64() {
                    positioned.push((position as usize, word.as_str()));
                }
            }
        }
    }
    positioned.sort_by_key(|(position, _)| *position);
    positioned
        .into_iter()
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn text_matches_query(text: &str, query: &str) -> bool {
    let haystack = normalized_search_text(text);
    let tokens = focused_query_tokens(query);
    if tokens.is_empty() {
        return true;
    }

    let matched = tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count();
    let domain_tokens = tokens
        .iter()
        .filter(|token| WORKSPACE_DOMAIN_TOKENS.contains(&token.as_str()))
        .collect::<Vec<_>>();
    let domain_matched = domain_tokens.is_empty()
        || domain_tokens
            .iter()
            .any(|token| haystack.contains(token.as_str()));

    matched >= tokens.len().min(2) && domain_matched
}

fn clean_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_end_matches('.')
        .to_string()
}

fn save_candidates_sync(
    database_path: &std::path::Path,
    candidates: Vec<ArticleCandidate>,
    workspace_id: i64,
) -> Result<SaveCounters> {
    let mut conn = crate::db::open_connection(database_path)
        .with_context(|| format!("failed to open database at {}", database_path.display()))?;
    let mut counters = SaveCounters::default();

    // `transaction()` rolls back automatically if a candidate save fails before
    // we reach `commit()`.
    let tx = conn.transaction()?;

    for candidate in &candidates {
        let result = save_article_sync(&tx, candidate, None, workspace_id)?;
        counters.saved += result.saved;
        counters.skipped += result.skipped;
        counters.errors += result.errors;
    }

    tx.commit()?;

    Ok(counters)
}

fn save_article_sync(
    conn: &Connection,
    candidate: &ArticleCandidate,
    evaluation: Option<&serde_json::Map<String, serde_json::Value>>,
    workspace_id: i64,
) -> Result<SaveCounters> {
    let mut counters = SaveCounters::default();
    let reg_date = Utc::now().date_naive().format("%Y-%m-%d").to_string();
    let category = category_for_source(&candidate.source);

    let get_str = |key: &str| -> Option<String> {
        evaluation
            .and_then(|eval| eval.get(key))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let get_int = |key: &str| -> Option<i64> {
        evaluation
            .and_then(|eval| eval.get(key))
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
    };

    let title = get_str("title").unwrap_or_else(|| candidate.title.clone());
    let first_author = get_str("first_author").unwrap_or_else(|| candidate.first_author.clone());
    let pub_date = candidate.pub_date.clone().or_else(|| get_str("pub_date"));
    let journal = candidate.journal.clone().or_else(|| get_str("journal"));
    let candidate_uid = candidate.uid();

    if find_duplicate_article_uid(conn, candidate, &title, workspace_id, &candidate_uid)?.is_some()
    {
        counters.skipped += 1;
        return Ok(counters);
    }

    let why_it_matters = get_str("why_it_matters").or_else(|| {
        evaluation.is_none().then(|| {
            format!(
                "Imported from {category} metadata. Detailed Rust evaluation is not ported yet."
            )
        })
    });
    let byline_summary = get_str("byline_summary").or_else(|| {
        if evaluation.is_none() {
            candidate.summary.clone()
        } else {
            None
        }
    });

    let changed = conn.execute(
        "INSERT OR IGNORE INTO haie_rev (
            uid, url, category, reg_date, title, first_author, authors, pub_date, journal, doi,
            ai_tech, clinical_domain, ethics_framework, primary_issue, key_stakeholders,
            practical_impl, secondary_issues, key_argument, main_findings, normative_claims,
            limitations, theoretical_strengths, theoretical_weaknesses,
            empirical_strengths, empirical_weaknesses,
            byline_summary, why_it_matters,
            scholarly_rigor, novelty, relevance_score, practical_impact,
            interdisciplinary, critical_concerns, total_score, priority,
            full_text, content_type, workspace_id
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24, ?25,
            ?26, ?27,
            ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35,
            ?36, ?37, ?38
        )",
        params![
            candidate_uid,
            candidate.url,
            category,
            reg_date,
            title,
            first_author,
            candidate.authors,
            pub_date,
            journal,
            candidate.doi,
            get_str("ai_tech"),
            get_str("clinical_domain"),
            get_str("ethics_framework"),
            get_str("primary_issue"),
            get_str("key_stakeholders"),
            get_str("practical_impl"),
            get_str("secondary_issues"),
            get_str("key_argument"),
            get_str("main_findings"),
            get_str("normative_claims"),
            get_str("limitations"),
            get_str("theoretical_strengths"),
            get_str("theoretical_weaknesses"),
            get_str("empirical_strengths"),
            get_str("empirical_weaknesses"),
            byline_summary.clone(),
            why_it_matters.clone(),
            get_int("scholarly_rigor"),
            get_int("novelty"),
            get_int("relevance_score"),
            get_int("practical_impact"),
            get_int("interdisciplinary"),
            get_int("critical_concerns"),
            get_int("total_score"),
            get_str("priority"),
            candidate.summary,
            Some("abstract_only".to_string()),
            workspace_id,
        ],
    );

    match changed {
        Ok(0) if evaluation.is_some() => {
            counters.saved += update_existing_article_sync(
                conn,
                candidate,
                category,
                &title,
                &first_author,
                candidate.authors.as_deref(),
                pub_date.as_deref(),
                journal.as_deref(),
                candidate.doi.as_deref(),
                get_str("ai_tech").as_deref(),
                get_str("clinical_domain").as_deref(),
                get_str("ethics_framework").as_deref(),
                get_str("primary_issue").as_deref(),
                get_str("key_stakeholders").as_deref(),
                get_str("practical_impl").as_deref(),
                get_str("secondary_issues").as_deref(),
                get_str("key_argument").as_deref(),
                get_str("main_findings").as_deref(),
                get_str("normative_claims").as_deref(),
                get_str("limitations").as_deref(),
                get_str("theoretical_strengths").as_deref(),
                get_str("theoretical_weaknesses").as_deref(),
                get_str("empirical_strengths").as_deref(),
                get_str("empirical_weaknesses").as_deref(),
                byline_summary.as_deref(),
                why_it_matters.as_deref(),
                get_int("scholarly_rigor"),
                get_int("novelty"),
                get_int("relevance_score"),
                get_int("practical_impact"),
                get_int("interdisciplinary"),
                get_int("critical_concerns"),
                get_int("total_score"),
                get_str("priority").as_deref(),
                candidate.summary.as_deref(),
            )? as i32;
        }
        Ok(0) => counters.skipped += 1,
        Ok(_) => counters.saved += 1,
        Err(_) => counters.errors += 1,
    }

    Ok(counters)
}

fn find_duplicate_article_uid(
    conn: &Connection,
    candidate: &ArticleCandidate,
    title: &str,
    workspace_id: i64,
    candidate_uid: &str,
) -> Result<Option<String>> {
    if let Some(candidate_doi) = candidate
        .doi
        .as_deref()
        .map(normalized_duplicate_doi)
        .filter(|value| !value.is_empty())
    {
        let mut stmt = conn.prepare(
            "SELECT uid, doi FROM haie_rev
             WHERE workspace_id = ?1
               AND uid != ?2
               AND doi IS NOT NULL
               AND TRIM(doi) != ''",
        )?;
        let rows = stmt.query_map(params![workspace_id, candidate_uid], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (uid, doi) = row?;
            if normalized_duplicate_doi(&doi) == candidate_doi {
                return Ok(Some(uid));
            }
        }
    }

    let candidate_title = normalized_duplicate_title(title);
    if candidate_title.is_empty() {
        return Ok(None);
    }

    let mut stmt = conn.prepare(
        "SELECT uid, title FROM haie_rev
         WHERE workspace_id = ?1
           AND uid != ?2
           AND title IS NOT NULL
           AND TRIM(title) != ''",
    )?;
    let rows = stmt.query_map(params![workspace_id, candidate_uid], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (uid, existing_title) = row?;
        if normalized_duplicate_title(&existing_title) == candidate_title {
            return Ok(Some(uid));
        }
    }

    Ok(None)
}

fn normalized_duplicate_doi(doi: &str) -> String {
    strip_doi_url(doi).to_ascii_lowercase()
}

fn normalized_duplicate_title(title: &str) -> String {
    normalized_search_text(title)
        .split_whitespace()
        .filter(|token| *token != "preprint")
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(clippy::too_many_arguments)]
fn update_existing_article_sync(
    conn: &Connection,
    candidate: &ArticleCandidate,
    category: &str,
    title: &str,
    first_author: &str,
    authors: Option<&str>,
    pub_date: Option<&str>,
    journal: Option<&str>,
    doi: Option<&str>,
    ai_tech: Option<&str>,
    clinical_domain: Option<&str>,
    ethics_framework: Option<&str>,
    primary_issue: Option<&str>,
    key_stakeholders: Option<&str>,
    practical_impl: Option<&str>,
    secondary_issues: Option<&str>,
    key_argument: Option<&str>,
    main_findings: Option<&str>,
    normative_claims: Option<&str>,
    limitations: Option<&str>,
    theoretical_strengths: Option<&str>,
    theoretical_weaknesses: Option<&str>,
    empirical_strengths: Option<&str>,
    empirical_weaknesses: Option<&str>,
    byline_summary: Option<&str>,
    why_it_matters: Option<&str>,
    scholarly_rigor: Option<i64>,
    novelty: Option<i64>,
    relevance_score: Option<i64>,
    practical_impact: Option<i64>,
    interdisciplinary: Option<i64>,
    critical_concerns: Option<i64>,
    total_score: Option<i64>,
    priority: Option<&str>,
    full_text: Option<&str>,
) -> Result<usize> {
    conn.execute(
        "UPDATE haie_rev
         SET url = COALESCE(?2, url),
             category = COALESCE(?3, category),
             title = COALESCE(?4, title),
             first_author = COALESCE(?5, first_author),
             authors = COALESCE(?6, authors),
             pub_date = COALESCE(?7, pub_date),
             journal = COALESCE(?8, journal),
             doi = COALESCE(?9, doi),
             ai_tech = COALESCE(?10, ai_tech),
             clinical_domain = COALESCE(?11, clinical_domain),
             ethics_framework = COALESCE(?12, ethics_framework),
             primary_issue = COALESCE(?13, primary_issue),
             key_stakeholders = COALESCE(?14, key_stakeholders),
             practical_impl = COALESCE(?15, practical_impl),
             secondary_issues = COALESCE(?16, secondary_issues),
             key_argument = COALESCE(?17, key_argument),
             main_findings = COALESCE(?18, main_findings),
             normative_claims = COALESCE(?19, normative_claims),
             limitations = COALESCE(?20, limitations),
             theoretical_strengths = COALESCE(?21, theoretical_strengths),
             theoretical_weaknesses = COALESCE(?22, theoretical_weaknesses),
             empirical_strengths = COALESCE(?23, empirical_strengths),
             empirical_weaknesses = COALESCE(?24, empirical_weaknesses),
             byline_summary = COALESCE(?25, byline_summary),
             why_it_matters = COALESCE(?26, why_it_matters),
             scholarly_rigor = COALESCE(?27, scholarly_rigor),
             novelty = COALESCE(?28, novelty),
             relevance_score = COALESCE(?29, relevance_score),
             practical_impact = COALESCE(?30, practical_impact),
             interdisciplinary = COALESCE(?31, interdisciplinary),
             critical_concerns = COALESCE(?32, critical_concerns),
             total_score = COALESCE(?33, total_score),
             priority = COALESCE(?34, priority),
             full_text = COALESCE(?35, full_text),
             content_type = CASE WHEN ?35 IS NULL THEN content_type ELSE 'abstract_only' END,
             updated_at = datetime('now')
         WHERE uid = ?1",
        params![
            candidate.uid(),
            Some(candidate.url.as_str()),
            Some(category),
            Some(title),
            Some(first_author),
            authors,
            pub_date,
            journal,
            doi,
            ai_tech,
            clinical_domain,
            ethics_framework,
            primary_issue,
            key_stakeholders,
            practical_impl,
            secondary_issues,
            key_argument,
            main_findings,
            normative_claims,
            limitations,
            theoretical_strengths,
            theoretical_weaknesses,
            empirical_strengths,
            empirical_weaknesses,
            byline_summary,
            why_it_matters,
            scholarly_rigor,
            novelty,
            relevance_score,
            practical_impact,
            interdisciplinary,
            critical_concerns,
            total_score,
            priority,
            full_text,
        ],
    )
    .map_err(anyhow::Error::from)
}

fn parse_arxiv_oai_records(xml: &str) -> Result<ArxivOaiPage> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut page = ArxivOaiPage::default();
    let mut current = ArxivOaiEntry::default();
    let mut current_author = ArxivOaiAuthor::default();
    let mut current_text_tag: Option<Vec<u8>> = None;
    let mut in_record = false;
    let mut in_metadata = false;
    let mut in_arxiv = false;
    let mut in_author = false;
    let mut in_resumption_token = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                let tag = event.local_name().as_ref().to_vec();
                match tag.as_slice() {
                    b"record" => {
                        current = ArxivOaiEntry::default();
                        in_record = true;
                    }
                    b"metadata" if in_record => in_metadata = true,
                    b"arXiv" if in_metadata => in_arxiv = true,
                    b"author" if in_arxiv => {
                        current_author = ArxivOaiAuthor::default();
                        in_author = true;
                    }
                    b"resumptionToken" => {
                        in_resumption_token = true;
                        current_text_tag = Some(tag);
                    }
                    b"error" => {
                        for attr in event.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"code"
                                && attr.unescape_value()?.as_ref() == "noRecordsMatch"
                            {
                                page.no_records_match = true;
                            }
                        }
                    }
                    _ if in_arxiv => current_text_tag = Some(tag),
                    _ => {}
                }
            }
            Ok(Event::Empty(event)) => {
                if event.local_name().as_ref() == b"error" {
                    for attr in event.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"code"
                            && attr.unescape_value()?.as_ref() == "noRecordsMatch"
                        {
                            page.no_records_match = true;
                        }
                    }
                }
            }
            Ok(Event::Text(event)) => {
                let text = event
                    .decode()
                    .context("failed to decode arXiv OAI-PMH XML text")?
                    .into_owned();
                if in_resumption_token {
                    let token = text.trim();
                    if !token.is_empty() {
                        page.resumption_token = Some(token.to_string());
                    }
                } else if in_arxiv {
                    apply_arxiv_oai_text(
                        &mut current,
                        &mut current_author,
                        current_text_tag.as_deref(),
                        in_author,
                        text.as_str(),
                    );
                }
            }
            Ok(Event::CData(event)) => {
                if in_arxiv {
                    let text = event
                        .decode()
                        .context("failed to decode arXiv OAI-PMH XML cdata")?
                        .into_owned();
                    apply_arxiv_oai_text(
                        &mut current,
                        &mut current_author,
                        current_text_tag.as_deref(),
                        in_author,
                        text.as_str(),
                    );
                }
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"record" => {
                    in_record = false;
                    in_metadata = false;
                    in_arxiv = false;
                    in_author = false;
                    in_resumption_token = false;
                    current_text_tag = None;
                    if let Some(candidate) = current.clone().into_candidate() {
                        page.candidates.push(candidate);
                    }
                    current = ArxivOaiEntry::default();
                }
                b"metadata" => in_metadata = false,
                b"arXiv" => {
                    in_arxiv = false;
                    current_text_tag = None;
                }
                b"author" => {
                    in_author = false;
                    current_text_tag = None;
                    if let Some(author) = current_author.clone().into_author_name() {
                        current.authors.push(author);
                    }
                    current_author = ArxivOaiAuthor::default();
                }
                b"resumptionToken" => {
                    in_resumption_token = false;
                    current_text_tag = None;
                }
                _ => {
                    current_text_tag = None;
                }
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(anyhow!("failed to parse arXiv OAI-PMH feed: {error}")),
            _ => {}
        }

        buf.clear();
    }

    Ok(page)
}

fn apply_arxiv_oai_text(
    current: &mut ArxivOaiEntry,
    current_author: &mut ArxivOaiAuthor,
    tag: Option<&[u8]>,
    in_author: bool,
    text: &str,
) {
    let Some(tag) = tag else {
        return;
    };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    match tag {
        b"id" => current.id = Some(trimmed.to_string()),
        b"created" => current.created = Some(trimmed.to_string()),
        b"updated" => current.updated = Some(trimmed.to_string()),
        b"title" => current.title = Some(trimmed.to_string()),
        b"abstract" => current.summary = Some(trimmed.to_string()),
        b"categories" => current.categories = Some(trimmed.to_string()),
        b"doi" => current.doi = Some(trimmed.to_string()),
        b"keyname" if in_author => current_author.keyname = Some(trimmed.to_string()),
        b"forenames" if in_author => current_author.forenames = Some(trimmed.to_string()),
        _ => {}
    }
}

fn filter_arxiv_oai_candidates(candidates: Vec<ArticleCandidate>) -> Vec<ArticleCandidate> {
    candidates
        .into_iter()
        .filter(arxiv_oai_matches_research_scope)
        .collect()
}

fn arxiv_oai_matches_research_scope(candidate: &ArticleCandidate) -> bool {
    let text = format!(
        "{} {}",
        candidate.title,
        candidate.summary.as_deref().unwrap_or_default()
    )
    .to_lowercase();

    has_any_phrase(
        &text,
        &[
            "artificial intelligence",
            "machine learning",
            "large language model",
            "large language models",
            "llm",
            "clinical decision support",
            "algorithmic",
            "federated learning",
        ],
    ) && has_any_phrase(
        &text,
        &[
            "clinical",
            "healthcare",
            "health care",
            "medical",
            "medicine",
            "patient",
            "patients",
            "hospital",
            "biomedical",
            "biomedicine",
            "public health",
        ],
    ) && has_any_phrase(
        &text,
        &[
            "ethics",
            "ethical",
            "bias",
            "fairness",
            "privacy",
            "governance",
            "accountability",
            "safety",
            "oversight",
            "human-in-the-loop",
            "human in the loop",
        ],
    )
}

fn has_any_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| text.contains(phrase))
}

fn parse_arxiv_feed(xml: &str) -> Result<Vec<ArticleCandidate>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut candidates = Vec::new();
    let mut current = ArxivEntry::default();
    let mut current_text_tag: Option<Vec<u8>> = None;
    let mut in_entry = false;
    let mut in_author = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                let tag = event.local_name().as_ref().to_vec();
                if tag.as_slice() == b"entry" {
                    current = ArxivEntry::default();
                    in_entry = true;
                } else if in_entry && tag.as_slice() == b"author" {
                    in_author = true;
                } else if in_entry && tag.as_slice() == b"link" {
                    let mut title = None;
                    let mut href = None;
                    for attr in event.attributes().flatten() {
                        match attr.key.local_name().as_ref() {
                            b"title" => title = Some(attr.unescape_value()?.into_owned()),
                            b"href" => href = Some(attr.unescape_value()?.into_owned()),
                            _ => {}
                        }
                    }
                    if title.as_deref() == Some("pdf") {
                        current.pdf_url = href.map(|value| value.replace("http://", "https://"));
                    }
                } else if in_entry {
                    current_text_tag = Some(tag);
                }
            }
            Ok(Event::Text(event)) => {
                if !in_entry {
                    buf.clear();
                    continue;
                }
                let text = event
                    .decode()
                    .context("failed to decode arXiv XML text")?
                    .into_owned();
                apply_arxiv_text(
                    &mut current,
                    current_text_tag.as_deref(),
                    in_author,
                    text.as_str(),
                );
            }
            Ok(Event::CData(event)) => {
                if !in_entry {
                    buf.clear();
                    continue;
                }
                let text = event
                    .decode()
                    .context("failed to decode arXiv XML cdata")?
                    .into_owned();
                apply_arxiv_text(
                    &mut current,
                    current_text_tag.as_deref(),
                    in_author,
                    text.as_str(),
                );
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"entry" => {
                    in_entry = false;
                    in_author = false;
                    current_text_tag = None;
                    if let Some(candidate) = current.clone().into_candidate() {
                        candidates.push(candidate);
                    }
                    current = ArxivEntry::default();
                }
                b"author" => {
                    in_author = false;
                    current_text_tag = None;
                }
                _ => {
                    current_text_tag = None;
                }
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(anyhow!("failed to parse arXiv feed: {error}")),
            _ => {}
        }

        buf.clear();
    }

    Ok(candidates)
}

fn apply_arxiv_text(current: &mut ArxivEntry, tag: Option<&[u8]>, in_author: bool, text: &str) {
    let Some(tag) = tag else {
        return;
    };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    match tag {
        b"id" => current.id = Some(trimmed.to_string()),
        b"title" => current.title = Some(trimmed.to_string()),
        b"summary" => current.summary = Some(trimmed.to_string()),
        b"published" => current.published = Some(trimmed.to_string()),
        b"name" if in_author => current.authors.push(trimmed.to_string()),
        b"doi" => current.doi = Some(trimmed.to_string()),
        _ => {}
    }
}

fn extract_authors(value: Option<&Value>) -> (Option<String>, Option<String>) {
    let authors = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let first = authors.first().cloned();
    let joined = if authors.is_empty() {
        None
    } else {
        Some(authors.join(", "))
    };

    (first, joined)
}

fn extract_doi(summary: &Value) -> Option<String> {
    summary
        .get("articleids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|entry| {
            if entry.get("idtype").and_then(Value::as_str) != Some("doi") {
                return None;
            }

            entry
                .get("value")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn parse_partial_date(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }

    NaiveDate::parse_from_str(value, "%Y %b %d")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(format!("{value} 01").as_str(), "%Y %b %d").ok())
        .or_else(|| NaiveDate::parse_from_str(format!("{value} Jan 01").as_str(), "%Y %b %d").ok())
        .map(|date| date.format("%Y-%m-%d").to_string())
}

fn category_for_source(source: &str) -> &'static str {
    source_label(source).unwrap_or("Unknown")
}

#[derive(Debug, Default)]
struct ArxivOaiPage {
    candidates: Vec<ArticleCandidate>,
    resumption_token: Option<String>,
    no_records_match: bool,
}

#[derive(Debug, Default, Clone)]
struct ArxivOaiEntry {
    id: Option<String>,
    created: Option<String>,
    updated: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    categories: Option<String>,
    authors: Vec<String>,
    doi: Option<String>,
}

impl ArxivOaiEntry {
    fn into_candidate(self) -> Option<ArticleCandidate> {
        let source_id = self.id?.trim().to_string();
        let title = self.title.as_deref().map(clean_text)?;
        if source_id.is_empty() || title.is_empty() {
            return None;
        }

        let first_author = self
            .authors
            .first()
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        let authors = if self.authors.is_empty() {
            None
        } else {
            Some(self.authors.join(", "))
        };
        let pub_date = self.created.or(self.updated);
        let url = format!("https://arxiv.org/pdf/{source_id}.pdf");
        let summary = self.summary.as_deref().map(clean_text);

        Some(ArticleCandidate {
            source: "arxiv".to_string(),
            source_id,
            title,
            summary,
            first_author,
            authors,
            pub_date,
            journal: Some("arXiv".to_string()),
            doi: self.doi.map(|value| value.trim().to_string()),
            url,
        })
    }
}

#[derive(Debug, Default, Clone)]
struct ArxivOaiAuthor {
    keyname: Option<String>,
    forenames: Option<String>,
}

impl ArxivOaiAuthor {
    fn into_author_name(self) -> Option<String> {
        match (self.forenames, self.keyname) {
            (Some(forenames), Some(keyname)) => {
                let name = clean_text(format!("{forenames} {keyname}").as_str());
                (!name.is_empty()).then_some(name)
            }
            (None, Some(keyname)) => {
                let name = clean_text(&keyname);
                (!name.is_empty()).then_some(name)
            }
            (Some(forenames), None) => {
                let name = clean_text(&forenames);
                (!name.is_empty()).then_some(name)
            }
            (None, None) => None,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct ArxivEntry {
    id: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    published: Option<String>,
    authors: Vec<String>,
    pdf_url: Option<String>,
    doi: Option<String>,
}

impl ArxivEntry {
    fn into_candidate(self) -> Option<ArticleCandidate> {
        let id = self.id?;
        let source_id = id
            .trim()
            .trim_start_matches("http://arxiv.org/abs/")
            .trim_start_matches("https://arxiv.org/abs/")
            .to_string();
        let title = self.title?.trim().to_string();
        if source_id.is_empty() || title.is_empty() {
            return None;
        }

        let first_author = self
            .authors
            .first()
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        let authors = if self.authors.is_empty() {
            None
        } else {
            Some(self.authors.join(", "))
        };
        let pub_date = self
            .published
            .as_deref()
            .and_then(|value| value.split('T').next())
            .map(ToOwned::to_owned);
        let url = self
            .pdf_url
            .unwrap_or_else(|| format!("https://arxiv.org/pdf/{source_id}.pdf"));

        Some(ArticleCandidate {
            source: "arxiv".to_string(),
            source_id,
            title,
            summary: self.summary.map(|value| value.trim().to_string()),
            first_author,
            authors,
            pub_date,
            journal: Some("arXiv".to_string()),
            doi: self.doi.map(|value| value.trim().to_string()),
            url,
        })
    }
}

impl ArticleCandidate {
    pub fn uid(&self) -> String {
        format!("{}:{}", self.source, self.source_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_labels_cover_every_gather_source() {
        for source in GATHER_SOURCE_IDS {
            assert!(source_label(source).is_some(), "missing label for {source}");
            assert_ne!(category_for_source(source), "Unknown");
        }
    }

    #[test]
    fn reconstructs_openalex_inverted_abstract() {
        let value = json!({
            "clinical": [1],
            "AI": [0],
            "support": [2]
        });
        let index = value.as_object().expect("object");
        assert_eq!(reconstruct_inverted_abstract(index), "AI clinical support");
    }

    #[test]
    fn maps_crossref_metadata_without_markup() {
        let work = json!({
            "DOI": "https://doi.org/10.1000/test.case",
            "title": ["Clinical AI ethics."],
            "abstract": "<jats:p>Structured abstract text.</jats:p>",
            "author": [{ "given": "Ada", "family": "Lovelace" }],
            "issued": { "date-parts": [[2026, 5, 18]] },
            "container-title": ["Journal of Tests"],
            "URL": "https://doi.org/10.1000/test.case"
        });
        let candidate = crossref_candidate(&work).expect("candidate");

        assert_eq!(candidate.uid(), "crossref:10.1000/test.case");
        assert_eq!(candidate.title, "Clinical AI ethics");
        assert_eq!(
            candidate.summary.as_deref(),
            Some("Structured abstract text")
        );
        assert_eq!(candidate.first_author, "Ada Lovelace");
        assert_eq!(candidate.pub_date.as_deref(), Some("2026-05-18"));
    }

    #[test]
    fn preprint_query_matching_is_token_based() {
        assert!(text_matches_query(
            "Bias concerns in a clinical machine learning model",
            "machine learning bias"
        ));
        assert!(!text_matches_query(
            "Clinical workflow evaluation without model fairness terms",
            "machine learning bias"
        ));
    }

    #[test]
    fn workspace_source_queries_are_deduped_and_capped() {
        let mut profile = diabetes_chatbot_context();
        profile.override_queries = vec![
            "diabetes chatbot HbA1c".to_string(),
            "diabetes chatbot HbA1c".to_string(),
            "diabetes conversational agent".to_string(),
            "diabetes digital coaching".to_string(),
            "diabetes virtual coach".to_string(),
            "diabetes ChatGPT education".to_string(),
            "large language model diabetes counseling".to_string(),
            "CGM conversational agent diabetes counseling".to_string(),
            "diabetes medication adherence chatbot".to_string(),
            "diabetes misinformation chatbot".to_string(),
        ];

        let queries = source_query_terms(&profile);

        assert_eq!(queries.len(), MAX_WORKSPACE_SOURCE_QUERIES);
        assert_eq!(queries[0], "diabetes chatbot HbA1c");
    }

    #[test]
    fn arxiv_workspace_queries_are_split_and_focused() {
        let profile = diabetes_chatbot_context();

        let queries = arxiv_queries(&profile);

        assert_eq!(queries.len(), 3);
        assert!(queries[0].contains("all:diabetes"));
        assert!(queries[0].contains("all:chatbot"));
        assert!(!queries[0].contains(" OR all:\"diabetes conversational"));
        assert!(queries[2].contains(r#"all:"large language model""#));
    }

    #[test]
    fn rxiv_date_windows_are_recent_first() {
        let windows = rxiv_date_windows(
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 20).unwrap(),
        );

        assert_eq!(
            windows[0],
            (
                NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 20).unwrap()
            )
        );
        assert_eq!(
            windows.last().copied(),
            Some((
                NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 21).unwrap()
            ))
        );
    }

    #[test]
    fn rxiv_total_accepts_string_or_numeric_values() {
        assert_eq!(
            rxiv_total_from_body(&json!({ "messages": [{ "total": "927" }] })),
            Some(927)
        );
        assert_eq!(
            rxiv_total_from_body(&json!({ "messages": [{ "total": 12 }] })),
            Some(12)
        );
    }

    #[test]
    fn workspace_prefilter_keeps_focused_diabetes_chatbot_candidates() {
        let profile = diabetes_chatbot_context();
        let mut candidate = test_candidate("focused");
        candidate.title =
            "Conversational agents for medication adherence in adults with diabetes".to_string();

        let filtered = filter_candidates_by_workspace_terms(vec![candidate], &profile);

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn workspace_prefilter_drops_broad_diabetes_trial_candidates() {
        let profile = diabetes_chatbot_context();
        let mut candidate = test_candidate("broad");
        candidate.title =
            "Virtual weight management and continuous glucose monitoring in type 2 diabetes"
                .to_string();

        let filtered = filter_candidates_by_workspace_terms(vec![candidate], &profile);

        assert!(filtered.is_empty());
    }

    #[test]
    fn workspace_prefilter_drops_cgm_only_diabetes_candidates() {
        let mut profile = diabetes_chatbot_context();
        profile.override_queries = vec!["CGM conversational agent diabetes counseling".to_string()];
        let mut candidate = test_candidate("cgm");
        candidate.title =
            "Seasonal fluctuations of CGM metrics in individuals with type 1 diabetes".to_string();

        let filtered = filter_candidates_by_workspace_terms(vec![candidate], &profile);

        assert!(filtered.is_empty());
    }

    #[test]
    fn workspace_prefilter_drops_non_diabetes_cgm_abbreviation_matches() {
        let mut profile = diabetes_chatbot_context();
        profile.override_queries = vec!["CGM conversational agent diabetes counseling".to_string()];
        let mut candidate = test_candidate("cgm-abbreviation");
        candidate.title =
            "Conversational Gesture Model (CGM): Full conversation gestures".to_string();
        candidate.summary =
            Some("A motion generation method for conversational avatars.".to_string());

        let filtered = filter_candidates_by_workspace_terms(vec![candidate], &profile);

        assert!(filtered.is_empty());
    }

    #[test]
    fn workspace_prefilter_drops_human_coaching_without_tech_signal() {
        let mut profile = diabetes_chatbot_context();
        profile.override_queries = vec!["diabetes digital coaching chatbot".to_string()];
        let mut candidate = test_candidate("coaching");
        candidate.title =
            "Enhancing Group Coaching Competencies in the Diabetes Prevention Program".to_string();

        let filtered = filter_candidates_by_workspace_terms(vec![candidate], &profile);

        assert!(filtered.is_empty());
    }

    #[test]
    fn known_date_filter_keeps_unknown_dates() {
        let mut old = test_candidate("old");
        old.pub_date = Some("2025-01-01".to_string());
        let mut current = test_candidate("current");
        current.pub_date = Some("2026-05-18".to_string());
        let unknown = test_candidate("unknown");

        let filtered = filter_candidates_by_known_date(
            vec![old, current, unknown],
            NaiveDate::from_ymd_opt(2026, 5, 17).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 18).unwrap(),
        );
        let ids = filtered
            .into_iter()
            .map(|candidate| candidate.source_id)
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["current", "unknown"]);
    }

    #[test]
    fn parses_arxiv_oai_records() {
        let xml = r#"
            <OAI-PMH>
              <ListRecords>
                <record>
                  <metadata>
                    <arXiv>
                      <id>2605.16113</id>
                      <created>2026-05-15</created>
                      <updated>2026-05-18</updated>
                      <authors>
                        <author>
                          <keyname>Chu</keyname>
                          <forenames>Rui</forenames>
                        </author>
                      </authors>
                      <title>Fair Clinical Large Language Models</title>
                      <categories>cs.CL cs.AI</categories>
                      <doi>10.1000/example</doi>
                      <abstract>Privacy and bias governance for healthcare LLM systems.</abstract>
                    </arXiv>
                  </metadata>
                </record>
                <resumptionToken>abc123</resumptionToken>
              </ListRecords>
            </OAI-PMH>
        "#;

        let page = parse_arxiv_oai_records(xml).expect("OAI records");
        assert_eq!(page.resumption_token.as_deref(), Some("abc123"));
        assert_eq!(page.candidates.len(), 1);
        let candidate = &page.candidates[0];

        assert_eq!(candidate.uid(), "arxiv:2605.16113");
        assert_eq!(candidate.first_author, "Rui Chu");
        assert_eq!(candidate.pub_date.as_deref(), Some("2026-05-15"));
        assert_eq!(candidate.doi.as_deref(), Some("10.1000/example"));
        assert!(arxiv_oai_matches_research_scope(candidate));
    }

    #[test]
    fn parses_arxiv_oai_no_records_match() {
        let xml = r#"
            <OAI-PMH>
              <error code="noRecordsMatch">No records match.</error>
            </OAI-PMH>
        "#;

        let page = parse_arxiv_oai_records(xml).expect("OAI no records");

        assert!(page.no_records_match);
        assert!(page.candidates.is_empty());
    }

    #[test]
    fn parses_retry_after_seconds() {
        let value = header::HeaderValue::from_static("120");

        assert_eq!(
            parse_retry_after(Some(&value)),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn ignores_invalid_retry_after_values() {
        let value = header::HeaderValue::from_static("not-a-date");

        assert_eq!(parse_retry_after(Some(&value)), None);
    }

    #[test]
    fn evaluated_save_updates_existing_metadata_row() {
        let conn = Connection::open_in_memory().unwrap();
        create_save_test_table(&conn);
        let mut candidate = test_candidate("dup");
        candidate.summary = Some("Abstract text".to_string());

        let metadata_save = save_article_sync(&conn, &candidate, None, 1).unwrap();
        assert_eq!(metadata_save.saved, 1);

        let evaluation = serde_json::Map::from_iter([
            ("byline_summary".to_string(), json!("Summary")),
            ("why_it_matters".to_string(), json!("Why it matters")),
            ("scholarly_rigor".to_string(), json!(5)),
            ("novelty".to_string(), json!(4)),
            ("relevance_score".to_string(), json!(5)),
            ("practical_impact".to_string(), json!(4)),
            ("interdisciplinary".to_string(), json!(3)),
            ("critical_concerns".to_string(), json!(-1)),
            ("total_score".to_string(), json!(83)),
            ("priority".to_string(), json!("Tier1")),
        ]);

        let evaluated_save = save_article_sync(&conn, &candidate, Some(&evaluation), 1).unwrap();
        assert_eq!(evaluated_save.saved, 1);
        assert_eq!(evaluated_save.skipped, 0);

        let scores: (Option<i64>, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT scholarly_rigor, total_score, priority FROM haie_rev WHERE uid = ?1",
                [candidate.uid()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(scores, (Some(5), Some(83), Some("Tier1".to_string())));
    }

    #[test]
    fn save_skips_duplicate_articles_by_doi_or_title() {
        let conn = Connection::open_in_memory().unwrap();
        create_save_test_table(&conn);

        let mut first = test_candidate("openalex");
        first.source = "openalex".to_string();
        first.doi = Some("10.1000/diabetes.chat".to_string());
        first.title = "Blinded Multi-Rater Comparative Evaluation".to_string();
        assert_eq!(save_article_sync(&conn, &first, None, 1).unwrap().saved, 1);

        let mut same_doi = test_candidate("semantic");
        same_doi.source = "semantic_scholar".to_string();
        same_doi.doi = Some("https://doi.org/10.1000/diabetes.chat".to_string());
        same_doi.title = "Different source title".to_string();
        let doi_result = save_article_sync(&conn, &same_doi, None, 1).unwrap();
        assert_eq!(doi_result.saved, 0);
        assert_eq!(doi_result.skipped, 1);

        let mut same_title = test_candidate("preprint");
        same_title.source = "openalex".to_string();
        same_title.title = "Blinded Multi-Rater Comparative Evaluation (Preprint)".to_string();
        let title_result = save_article_sync(&conn, &same_title, None, 1).unwrap();
        assert_eq!(title_result.saved, 0);
        assert_eq!(title_result.skipped, 1);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM haie_rev", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    fn create_save_test_table(conn: &Connection) {
        conn.execute_batch(
            r#"
            CREATE TABLE haie_rev (
                uid TEXT PRIMARY KEY,
                url TEXT,
                category TEXT,
                reg_date TEXT,
                title TEXT,
                first_author TEXT,
                authors TEXT,
                pub_date TEXT,
                journal TEXT,
                doi TEXT,
                ai_tech TEXT,
                clinical_domain TEXT,
                ethics_framework TEXT,
                primary_issue TEXT,
                secondary_issues TEXT,
                key_stakeholders TEXT,
                practical_impl TEXT,
                byline_summary TEXT,
                why_it_matters TEXT,
                key_argument TEXT,
                main_findings TEXT,
                normative_claims TEXT,
                limitations TEXT,
                theoretical_strengths TEXT,
                theoretical_weaknesses TEXT,
                empirical_strengths TEXT,
                empirical_weaknesses TEXT,
                scholarly_rigor INTEGER,
                novelty INTEGER,
                relevance_score INTEGER,
                practical_impact INTEGER,
                interdisciplinary INTEGER,
                critical_concerns INTEGER,
                total_score INTEGER,
                priority TEXT,
                full_text TEXT,
                content_type TEXT,
                workspace_id INTEGER,
                updated_at TEXT
            );
            "#,
        )
        .unwrap();
    }

    fn test_candidate(source_id: &str) -> ArticleCandidate {
        ArticleCandidate {
            source: "test".to_string(),
            source_id: source_id.to_string(),
            title: "Test title".to_string(),
            summary: None,
            first_author: "Unknown".to_string(),
            authors: None,
            pub_date: None,
            journal: None,
            doi: None,
            url: "https://example.com".to_string(),
        }
    }

    fn diabetes_chatbot_context() -> WorkspaceResearchContext {
        WorkspaceResearchContext {
            name: "Diabetes chatbot self-management evidence map".to_string(),
            primary_question:
                "Do chatbot/conversational agents improve diabetes self-management outcomes?"
                    .to_string(),
            gap_note: String::new(),
            refined_question: String::new(),
            seed_concepts: vec![
                "Type 2 diabetes".to_string(),
                "Chatbot intervention".to_string(),
                "Conversational agent".to_string(),
            ],
            override_queries: vec![
                "type 2 diabetes chatbot HbA1c adherence randomized trial".to_string(),
                "diabetes conversational agent self-management quality of life".to_string(),
                "large language model diabetes patient education safety escalation misinformation"
                    .to_string(),
            ],
            topic_descriptor: "chatbot and conversational agent interventions for type 2 diabetes"
                .to_string(),
            lookback_days: 30,
        }
    }
}
