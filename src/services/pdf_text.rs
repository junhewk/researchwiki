//! Column-aware PDF text extraction for scholarly articles.
//!
//! pdf-extract's built-in `PlainTextOutput` emits text in PDF drawing order,
//! which interleaves the columns of two-column papers. This module collects
//! positioned text fragments instead and reconstructs reading order per page:
//! full-width fragments (title, abstract) split the page into vertical bands,
//! and within each band the left column is emitted before the right one.

use std::panic::{AssertUnwindSafe, catch_unwind};

use pdf_extract::{Document, MediaBox, OutputDev, OutputError, Transform};
use thiserror::Error;

const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;
/// Below this many extracted characters the PDF is assumed to be scanned
/// images without a text layer.
const MIN_TEXT_CHARS: usize = 500;

#[derive(Debug, Error)]
pub enum PdfTextError {
    #[error("PDF exceeds {MAX_PDF_BYTES} bytes")]
    TooLarge,
    #[error("PDF parser panicked")]
    ExtractionPanic,
    #[error("extracted text too short, likely a scanned PDF")]
    NoText,
    #[error("PDF parse error: {0}")]
    Parse(String),
}

pub fn extract_pdf_text(bytes: &[u8]) -> Result<String, PdfTextError> {
    if bytes.len() > MAX_PDF_BYTES {
        return Err(PdfTextError::TooLarge);
    }

    let text = match catch_unwind(AssertUnwindSafe(|| extract_positioned(bytes))) {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => plain_text_fallback(bytes).ok_or(PdfTextError::Parse(error))?,
        Err(_) => plain_text_fallback(bytes).ok_or(PdfTextError::ExtractionPanic)?,
    };

    if text.trim().chars().count() >= MIN_TEXT_CHARS {
        return Ok(text);
    }
    // The positioned pass can miss text in unusual PDFs; give the naive
    // extractor a chance before declaring the document scanned.
    match plain_text_fallback(bytes) {
        Some(plain) if plain.trim().chars().count() >= MIN_TEXT_CHARS => Ok(plain),
        _ => Err(PdfTextError::NoText),
    }
}

fn extract_positioned(bytes: &[u8]) -> Result<String, String> {
    let mut doc = Document::load_mem(bytes).map_err(|error| error.to_string())?;
    if doc.is_encrypted() {
        // Empty-password decryption covers PDFs that are merely flagged.
        let _ = doc.decrypt("");
    }
    let mut output = PositionedTextOutput::default();
    pdf_extract::output_doc(&doc, &mut output).map_err(|error| error.to_string())?;
    Ok(order_pages(&output.pages))
}

fn plain_text_fallback(bytes: &[u8]) -> Option<String> {
    catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }))
    .ok()?
    .ok()
}

/// A run of characters that share a baseline and have no large horizontal gap.
#[derive(Debug, Clone)]
struct Fragment {
    x: f64,
    x_end: f64,
    y: f64,
    font_size: f64,
    text: String,
}

#[derive(Debug, Default)]
struct Page {
    width: f64,
    fragments: Vec<Fragment>,
}

#[derive(Default)]
struct PositionedTextOutput {
    pages: Vec<Page>,
    current: Option<Fragment>,
    page_height: f64,
}

impl PositionedTextOutput {
    fn flush_fragment(&mut self) {
        if let Some(fragment) = self.current.take()
            && !fragment.text.trim().is_empty()
            && let Some(page) = self.pages.last_mut()
        {
            page.fragments.push(fragment);
        }
    }
}

impl OutputDev for PositionedTextOutput {
    fn begin_page(
        &mut self,
        _page_num: u32,
        media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.flush_fragment();
        self.page_height = media_box.ury - media_box.lly;
        self.pages.push(Page {
            width: media_box.urx - media_box.llx,
            fragments: Vec::new(),
        });
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        self.flush_fragment();
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        char: &str,
    ) -> Result<(), OutputError> {
        // Skip glyphs rotated away from the horizontal baseline, such as the
        // vertical arXiv watermark in the left margin.
        if trm.m11.abs() < trm.m12.abs() {
            return Ok(());
        }
        let x = trm.m31;
        // Flip so y grows downward and ascending sort reads top to bottom.
        let y = self.page_height - trm.m32;
        // Effective glyph size under the text matrix (geometric mean of the
        // scale of both axes), mirroring pdf-extract's PlainTextOutput.
        let scale = (trm.m11.hypot(trm.m12) * trm.m21.hypot(trm.m22)).sqrt();
        let size = (font_size * scale).abs().max(1.0);
        let x_end = x + width * size;

        if let Some(fragment) = self.current.as_mut() {
            let tolerance = fragment.font_size.max(size);
            let same_line = (y - fragment.y).abs() <= tolerance * 0.5;
            let gap = x - fragment.x_end;
            if same_line && gap > -tolerance * 0.5 && gap < tolerance * 2.0 {
                if gap > tolerance * 0.1 {
                    fragment.text.push(' ');
                }
                fragment.text.push_str(char);
                fragment.x_end = x_end.max(fragment.x_end);
                fragment.font_size = fragment.font_size.max(size);
                return Ok(());
            }
            self.flush_fragment();
        }

        self.current = Some(Fragment {
            x,
            x_end,
            y,
            font_size: size,
            text: char.to_string(),
        });
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

/// Reconstruct reading order across all pages and join into plain text.
fn order_pages(pages: &[Page]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for page in pages {
        if page.fragments.is_empty() {
            continue;
        }
        order_page(page, &mut lines);
        lines.push(String::new());
    }
    repair_hyphenation(&mut lines);
    lines.join("\n").trim().to_string()
}

fn order_page(page: &Page, lines: &mut Vec<String>) {
    let fragments = &page.fragments;
    let width = if page.width > 0.0 {
        page.width
    } else {
        fragments.iter().map(|f| f.x_end).fold(0.0, f64::max)
    };

    let mut by_y: Vec<&Fragment> = fragments.iter().collect();
    by_y.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));

    let Some(split) = detect_column_split(fragments, width) else {
        emit_column(&by_y, lines);
        return;
    };

    // Full-width fragments (title, abstract, section banners) delimit
    // vertical bands; within a band, read the left column then the right.
    let mut left: Vec<&Fragment> = Vec::new();
    let mut right: Vec<&Fragment> = Vec::new();
    for fragment in by_y {
        if crosses(fragment, split) {
            emit_column(&left, lines);
            emit_column(&right, lines);
            left.clear();
            right.clear();
            lines.push(fragment.text.clone());
        } else if (fragment.x + fragment.x_end) / 2.0 < split {
            left.push(fragment);
        } else {
            right.push(fragment);
        }
    }
    emit_column(&left, lines);
    emit_column(&right, lines);
}

fn crosses(fragment: &Fragment, split: f64) -> bool {
    let margin = fragment.font_size;
    fragment.x < split - margin && fragment.x_end > split + margin
}

/// Find a vertical whitespace valley near the page centre. Returns the split
/// x-position when the page is laid out in two columns, `None` otherwise.
fn detect_column_split(fragments: &[Fragment], width: f64) -> Option<f64> {
    if width <= 0.0 || fragments.len() < 8 {
        return None;
    }
    let total = fragments.len();
    let mut best: Option<(usize, f64)> = None;
    let mut candidate = width * 0.35;
    while candidate <= width * 0.65 {
        let crossings = fragments
            .iter()
            .filter(|fragment| crosses(fragment, candidate))
            .count();
        if best.is_none_or(|(count, _)| crossings < count) {
            best = Some((crossings, candidate));
        }
        candidate += width * 0.01;
    }
    let (crossings, split) = best?;
    // Tolerate some full-width fragments (title, abstract, section banners).
    if crossings * 5 > total {
        return None;
    }
    let left = fragments
        .iter()
        .filter(|f| !crosses(f, split) && (f.x + f.x_end) / 2.0 < split)
        .count();
    let right = total - crossings - left;
    if left * 5 >= total && right * 5 >= total {
        Some(split)
    } else {
        None
    }
}

/// Assemble y-sorted fragments of one column into lines, joining fragments
/// that share a baseline.
fn emit_column(fragments: &[&Fragment], lines: &mut Vec<String>) {
    let mut sorted: Vec<&Fragment> = fragments.to_vec();
    sorted.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));

    let mut current = String::new();
    let mut last: Option<&Fragment> = None;
    for fragment in sorted {
        let same_line = last.is_some_and(|previous| {
            (fragment.y - previous.y).abs() <= previous.font_size.max(fragment.font_size) * 0.5
        });
        if same_line {
            current.push(' ');
        } else if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        current.push_str(fragment.text.trim_end());
        last = Some(fragment);
    }
    if !current.is_empty() {
        lines.push(current);
    }
}

/// Merge `transfor-` + `mation` line breaks left over from justified columns.
fn repair_hyphenation(lines: &mut Vec<String>) {
    let mut index = 0;
    while index + 1 < lines.len() {
        let next_starts_lower = lines[index + 1]
            .chars()
            .next()
            .is_some_and(|c| c.is_lowercase());
        if lines[index].ends_with('-') && lines[index].len() > 1 && next_starts_lower {
            let next = lines.remove(index + 1);
            let line = &mut lines[index];
            line.pop();
            line.push_str(&next);
        } else {
            index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(x: f64, x_end: f64, y: f64, text: &str) -> Fragment {
        Fragment {
            x,
            x_end,
            y,
            font_size: 10.0,
            text: text.to_string(),
        }
    }

    fn two_column_page(extra: Vec<Fragment>) -> Page {
        let mut fragments = extra;
        for (row, (left, right)) in [("L1", "R1"), ("L2", "R2"), ("L3", "R3"), ("L4", "R4")]
            .iter()
            .enumerate()
        {
            let y = 200.0 + row as f64 * 14.0;
            fragments.push(fragment(50.0, 280.0, y, left));
            fragments.push(fragment(320.0, 550.0, y, right));
        }
        Page {
            width: 600.0,
            fragments,
        }
    }

    #[test]
    fn two_column_reads_left_column_first() {
        let text = order_pages(&[two_column_page(Vec::new())]);
        assert_eq!(text, "L1\nL2\nL3\nL4\nR1\nR2\nR3\nR4");
    }

    #[test]
    fn full_width_title_precedes_columns() {
        let title = fragment(50.0, 550.0, 50.0, "A Full Width Title");
        let text = order_pages(&[two_column_page(vec![title])]);
        assert_eq!(text, "A Full Width Title\nL1\nL2\nL3\nL4\nR1\nR2\nR3\nR4");
    }

    #[test]
    fn single_column_keeps_top_to_bottom_order() {
        let fragments = (0..10)
            .map(|row| {
                fragment(
                    50.0,
                    550.0,
                    100.0 + row as f64 * 14.0,
                    &format!("line {row}"),
                )
            })
            .collect();
        let page = Page {
            width: 600.0,
            fragments,
        };
        let text = order_pages(&[page]);
        let expected: Vec<String> = (0..10).map(|row| format!("line {row}")).collect();
        assert_eq!(text, expected.join("\n"));
    }

    #[test]
    fn same_baseline_fragments_join_with_space() {
        let fragments = vec![
            fragment(50.0, 200.0, 100.0, "Hello"),
            fragment(400.0, 550.0, 100.0, "world"),
            fragment(50.0, 550.0, 120.0, "next line"),
        ];
        let page = Page {
            width: 600.0,
            fragments,
        };
        assert_eq!(order_pages(&[page]), "Hello world\nnext line");
    }

    /// Smoke test against real arXiv PDFs. Fixtures are not checked in;
    /// run with `cargo test -- --ignored` after placing PDFs under
    /// tests/fixtures/.
    #[test]
    #[ignore]
    fn extracts_real_arxiv_fixtures() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut checked = 0;
        for entry in std::fs::read_dir(fixtures).expect("fixtures dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|ext| ext != "pdf") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read fixture");
            let text = extract_pdf_text(&bytes)
                .unwrap_or_else(|error| panic!("{} failed: {error}", path.display()));
            assert!(
                text.chars().count() > 5_000,
                "{} produced only {} chars",
                path.display(),
                text.chars().count()
            );
            println!(
                "=== {} ({} chars) ===",
                path.display(),
                text.chars().count()
            );
            println!("{}", text.chars().take(600).collect::<String>());
            checked += 1;
        }
        assert!(checked > 0, "no PDF fixtures found");
    }

    #[test]
    fn hyphenated_line_breaks_are_repaired() {
        let mut lines = vec![
            "machine learning enables transfor-".to_string(),
            "mation of clinical workflows".to_string(),
        ];
        repair_hyphenation(&mut lines);
        assert_eq!(
            lines,
            vec!["machine learning enables transformation of clinical workflows".to_string()]
        );
    }
}
