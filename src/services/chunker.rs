use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

use crate::services::text_extractor::ExtractedText;

const DEFAULT_CHUNK_SIZE: usize = 512;
const DEFAULT_CHUNK_OVERLAP: usize = 64;
const DEFAULT_MAX_CHUNKS: usize = 100;

static TOKENIZER: OnceLock<CoreBPE> = OnceLock::new();

fn tokenizer() -> &'static CoreBPE {
    TOKENIZER
        .get_or_init(|| tiktoken_rs::cl100k_base().expect("failed to load cl100k_base tokenizer"))
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub content: String,
    pub token_count: i32,
    pub chunk_type: String,
    pub source_section: Option<String>,
}

pub struct ArticleChunker {
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub max_chunks: usize,
}

impl Default for ArticleChunker {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            chunk_overlap: DEFAULT_CHUNK_OVERLAP,
            max_chunks: DEFAULT_MAX_CHUNKS,
        }
    }
}

impl ArticleChunker {
    pub fn chunk_text(&self, extracted: &ExtractedText) -> Vec<Chunk> {
        let mut chunks = Vec::new();

        if !extracted.sections.is_empty() {
            for section in &extracted.sections {
                if chunks.len() >= self.max_chunks {
                    break;
                }
                let chunk_type = infer_chunk_type(section.title.as_deref());
                let section_chunks =
                    self.sliding_window(&section.content, &chunk_type, section.title.as_deref());
                for chunk in section_chunks {
                    if chunks.len() >= self.max_chunks {
                        break;
                    }
                    chunks.push(chunk);
                }
            }
        } else {
            chunks = self.sliding_window(&extracted.full_text, "body", None);
            chunks.truncate(self.max_chunks);
        }

        chunks
    }

    fn sliding_window(
        &self,
        text: &str,
        chunk_type: &str,
        section_name: Option<&str>,
    ) -> Vec<Chunk> {
        let bpe = tokenizer();
        let tokens = bpe.encode_ordinary(text);
        if tokens.is_empty() {
            return Vec::new();
        }

        let stride = self.chunk_size.saturating_sub(self.chunk_overlap).max(1);
        let mut chunks = Vec::new();
        let mut start = 0usize;

        while start < tokens.len() {
            let end = (start + self.chunk_size).min(tokens.len());
            let chunk_tokens = &tokens[start..end];

            let mut chunk_text = bpe
                .decode(chunk_tokens.to_vec())
                .unwrap_or_else(|_| String::from_utf8_lossy(&[]).to_string());

            chunk_text = adjust_to_sentence_boundary(&chunk_text);

            let actual_tokens = bpe.encode_ordinary(&chunk_text).len() as i32;
            if !chunk_text.trim().is_empty() {
                chunks.push(Chunk {
                    content: chunk_text,
                    token_count: actual_tokens,
                    chunk_type: chunk_type.to_string(),
                    source_section: section_name.map(str::to_string),
                });
            }

            if end >= tokens.len() {
                break;
            }
            start += stride;
        }

        chunks
    }
}

pub fn count_tokens(text: &str) -> usize {
    tokenizer().encode_ordinary(text).len()
}

fn adjust_to_sentence_boundary(text: &str) -> String {
    let len = text.len();
    if len < 20 {
        return text.to_string();
    }

    // Look for sentence-ending punctuation in the second half.
    let midpoint = len / 2;
    let search_region = &text[midpoint..];

    let last_sentence_end = search_region
        .rfind('.')
        .or_else(|| search_region.rfind('?'))
        .or_else(|| search_region.rfind('!'));

    if let Some(offset) = last_sentence_end {
        let absolute = midpoint + offset + 1;
        text[..absolute].trim().to_string()
    } else {
        text.trim().to_string()
    }
}

fn infer_chunk_type(title: Option<&str>) -> String {
    let Some(title) = title else {
        return "body".to_string();
    };
    let lower = title.to_lowercase();
    if lower.contains("abstract") {
        "abstract".to_string()
    } else if lower.contains("introduction") || lower.contains("background") {
        "introduction".to_string()
    } else if lower.contains("method") {
        "methods".to_string()
    } else if lower.contains("result") {
        "results".to_string()
    } else if lower.contains("discussion") {
        "discussion".to_string()
    } else if lower.contains("conclusion") {
        "conclusion".to_string()
    } else if lower.contains("reference") || lower.contains("bibliography") {
        "references".to_string()
    } else {
        "body".to_string()
    }
}
