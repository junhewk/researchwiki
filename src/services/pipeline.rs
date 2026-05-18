use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Datelike, Days, NaiveDate, Utc};
use quick_xml::{Reader, events::Event};
use reqwest::{Client, StatusCode};
use rusqlite::{Connection, params};
use serde_json::Value;
use tokio::task;

const ARXIV_API_URL: &str = "https://export.arxiv.org/api/query";
const NCBI_ESEARCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi";
const NCBI_ESUMMARY_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi";
const NCBI_ELINK_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/elink.fcgi";
const EUROPE_PMC_SEARCH_URL: &str = "https://www.ebi.ac.uk/europepmc/webservices/rest/search";
const RXIV_MAX_CANDIDATES: usize = 200;
const OPENALEX_SEARCH_URL: &str = "https://api.openalex.org/works";
const CROSSREF_SEARCH_URL: &str = "https://api.crossref.org/works";
const UNPAYWALL_SEARCH_URL: &str = "https://api.unpaywall.org/v2/search";
const SEMANTIC_SCHOLAR_SEARCH_URL: &str = "https://api.semanticscholar.org/graph/v1/paper/search";
const CLINICAL_TRIALS_SEARCH_URL: &str = "https://clinicaltrials.gov/api/v2/studies";
const POLITE_POOL_UA: &str = "articlegatherer-rust-backend/0.1 (mailto:junhewk.kim@gmail.com)";
const DEFAULT_CONTACT_EMAIL: &str = "junhewk.kim@gmail.com";
const DEFAULT_SOURCE_QUERY_LIMIT: i32 = 50;

const ARXIV_QUERIES: &[&str] = &[
    r#"(all:"artificial intelligence" OR all:"machine learning" OR all:"large language model") AND (all:clinical OR all:healthcare OR all:medical OR all:medicine) AND (all:ethics OR all:bias OR all:fairness OR all:privacy OR all:governance)"#,
    r#"all:"clinical decision support" AND (all:ethics OR all:bias OR all:fairness OR all:accountability)"#,
    r#"(all:"human in the loop" OR all:"human oversight") AND (all:healthcare OR all:clinical OR all:medical)"#,
    r#"(all:"AI governance" OR all:"algorithmic fairness") AND (all:healthcare OR all:clinical OR all:medical)"#,
];

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
    "unpaywall",
    "semantic_scholar",
    "clinical_trials",
];

#[derive(Clone)]
pub struct PipelineService {
    database_path: Arc<std::path::PathBuf>,
    client: Client,
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
    pub fn new(database_path: std::path::PathBuf) -> Self {
        let client = Client::builder()
            .user_agent("articlegatherer-rust-backend/0.1")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client should build");

        Self {
            database_path: Arc::new(database_path),
            client,
        }
    }

    pub async fn list_source(&self, source: &str, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        match source {
            "arxiv" => self.list_arxiv(days_back).await,
            "pmc" => self.list_pmc(days_back).await,
            "pubmed" => self.list_pubmed(days_back).await,
            "europepmc" => self.list_europe_pmc(days_back).await,
            "medrxiv" => self.list_rxiv("medrxiv", days_back).await,
            "biorxiv" => self.list_rxiv("biorxiv", days_back).await,
            "openalex" => self.list_openalex(days_back).await,
            "crossref" => self.list_crossref(days_back).await,
            "unpaywall" => self.list_unpaywall(days_back).await,
            "semantic_scholar" => self.list_semantic_scholar(days_back).await,
            "clinical_trials" => self.list_clinical_trials(days_back).await,
            _ => bail!("unsupported source: {source}"),
        }
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
            let sql = format!("SELECT uid FROM haie_rev WHERE uid IN ({placeholders})");
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

    pub async fn save_candidates(&self, candidates: Vec<ArticleCandidate>) -> Result<SaveCounters> {
        let database_path = self.database_path.clone();

        task::spawn_blocking(move || save_candidates_sync(&database_path, candidates))
            .await
            .context("candidate save task failed")?
    }

    pub async fn save_evaluated_candidate(
        &self,
        candidate: &ArticleCandidate,
        evaluation: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<SaveCounters> {
        let database_path = self.database_path.clone();
        let candidate = candidate.clone();
        let evaluation = evaluation.clone();

        task::spawn_blocking(move || {
            let conn = crate::db::open_connection(&*database_path).with_context(|| {
                format!("failed to open database at {}", database_path.display())
            })?;
            save_article_sync(&conn, &candidate, Some(&evaluation))
        })
        .await
        .context("evaluated candidate save task failed")?
    }

    async fn list_arxiv(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let today = Utc::now().date_naive();
        let start = today
            .checked_sub_days(Days::new(days_back.max(1) as u64))
            .unwrap_or(today);

        let mut merged = std::collections::BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in ARXIV_QUERIES.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(Duration::from_secs(3)).await;
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
                    tracing::warn!("arXiv query '{query}' failed: {error}");
                    errors.push(error);
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

        const MAX_ATTEMPTS: usize = 4;
        const RETRY_DELAYS: [Duration; 3] = [
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(90),
        ];

        for attempt in 1..=MAX_ATTEMPTS {
            let response = self
                .client
                .get(ARXIV_API_URL)
                .query(&[
                    ("search_query", search_query.as_str()),
                    ("sortBy", "lastUpdatedDate"),
                    ("sortOrder", "descending"),
                    ("max_results", "100"),
                ])
                .send()
                .await
                .with_context(|| format!("failed to request arXiv query '{query}'"))?;

            let status = response.status();
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
                let delay = RETRY_DELAYS[attempt - 1];
                tracing::warn!(
                    "arXiv query '{query}' returned HTTP {}; retrying in {} seconds",
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

    async fn list_pmc(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let ids = self
            .get_ncbi_json(
                NCBI_ESEARCH_URL,
                vec![
                    ("db", "pmc".to_string()),
                    ("term", PMC_QUERY.to_string()),
                    ("reldate", days_back.clamp(1, 30).to_string()),
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

    async fn list_pubmed(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
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
                    ("term", PUBMED_QUERY.to_string()),
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

    async fn list_europe_pmc(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in EUROPE_PMC_QUERIES.iter().enumerate() {
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
    ) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);
        let collection = self.fetch_rxiv_collection(server, start, today).await?;
        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        for query in RXIV_MEDICAL_AI_ETHICS_QUERIES {
            merge_candidates(
                &mut merged,
                rxiv_candidates_from_collection(server, &collection, query),
            );
        }

        Ok(merged.into_values().collect())
    }

    async fn fetch_rxiv_collection(
        &self,
        server: &'static str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Value>> {
        let url = format!(
            "https://api.biorxiv.org/details/{server}/{}/{}/0",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );
        let params: Vec<(&str, String)> = Vec::new();
        let label = rxiv_label(server);
        let body = self
            .get_json_with_retries(&url, &params, label, None)
            .await?;
        Ok(body
            .get("collection")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_openalex(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in SCHOLARLY_FREE_TEXT_QUERIES.iter().enumerate() {
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
                Some(POLITE_POOL_UA),
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

    async fn list_crossref(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in SCHOLARLY_FREE_TEXT_QUERIES.iter().enumerate() {
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
            ("query.bibliographic", query.to_string()),
            ("filter", filter),
            ("rows", DEFAULT_SOURCE_QUERY_LIMIT.clamp(1, 100).to_string()),
        ];
        let body = self
            .get_json_with_retries(
                CROSSREF_SEARCH_URL,
                &params,
                "Crossref",
                Some(POLITE_POOL_UA),
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

    async fn list_unpaywall(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);
        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in SCHOLARLY_FREE_TEXT_QUERIES.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_unpaywall_query(query).await {
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

    async fn fetch_unpaywall_query(&self, query: &str) -> Result<Vec<ArticleCandidate>> {
        let email =
            std::env::var("UNPAYWALL_EMAIL").unwrap_or_else(|_| DEFAULT_CONTACT_EMAIL.to_string());
        let params = vec![
            ("query", query.to_string()),
            ("is_oa", "true".to_string()),
            ("email", email),
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

    async fn list_semantic_scholar(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);

        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in SCHOLARLY_FREE_TEXT_QUERIES.iter().enumerate() {
            pause_between_source_queries(index).await;
            match self.fetch_semantic_scholar_query(query, start, today).await {
                Ok(candidates) => merge_candidates(&mut merged, candidates),
                Err(error) => {
                    tracing::warn!("Semantic Scholar query '{query}' failed: {error}");
                    errors.push(error);
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
            .get_json_with_retries(
                SEMANTIC_SCHOLAR_SEARCH_URL,
                &params,
                "Semantic Scholar",
                None,
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

    async fn list_clinical_trials(&self, days_back: i32) -> Result<Vec<ArticleCandidate>> {
        let (start, today) = date_window(days_back);
        let mut merged = BTreeMap::<String, ArticleCandidate>::new();
        let mut errors = Vec::new();
        for (index, query) in CLINICAL_TRIAL_QUERIES.iter().enumerate() {
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
        const MAX_ATTEMPTS: usize = 4;
        let mut delay = Duration::from_secs(2);

        for attempt in 1..=MAX_ATTEMPTS {
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
            if retryable && attempt < MAX_ATTEMPTS {
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

fn date_window(days_back: i32) -> (NaiveDate, NaiveDate) {
    let today = Utc::now().date_naive();
    let start = today
        .checked_sub_days(Days::new(days_back.max(1) as u64))
        .unwrap_or(today);
    (start, today)
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
                .map_or(true, |date| date >= start && date <= end)
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
    let haystack = text.to_lowercase();
    query
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|token| token.len() > 2)
        .all(|token| haystack.contains(token.as_str()))
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
) -> Result<SaveCounters> {
    let conn = crate::db::open_connection(database_path)
        .with_context(|| format!("failed to open database at {}", database_path.display()))?;
    let mut counters = SaveCounters::default();

    conn.execute_batch("BEGIN")?;

    for candidate in &candidates {
        let result = save_article_sync(&conn, candidate, None)?;
        counters.saved += result.saved;
        counters.skipped += result.skipped;
        counters.errors += result.errors;
    }

    conn.execute_batch("COMMIT")?;

    Ok(counters)
}

fn save_article_sync(
    conn: &Connection,
    candidate: &ArticleCandidate,
    evaluation: Option<&serde_json::Map<String, serde_json::Value>>,
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

    let why_it_matters = get_str("why_it_matters").unwrap_or_else(|| {
        if evaluation.is_some() {
            String::new()
        } else {
            format!(
                "Imported from {category} metadata. Detailed Rust evaluation is not ported yet."
            )
        }
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
            full_text, content_type
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24, ?25,
            ?26, ?27,
            ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35,
            ?36, ?37
         )",
        params![
            candidate.uid(),
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
            byline_summary,
            why_it_matters,
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
        ],
    );

    match changed {
        Ok(0) => counters.skipped += 1,
        Ok(_) => counters.saved += 1,
        Err(_) => counters.errors += 1,
    }

    Ok(counters)
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
}
