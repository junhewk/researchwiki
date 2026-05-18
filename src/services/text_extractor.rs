use quick_xml::{Reader, events::Event};

#[derive(Debug, Clone)]
pub struct Section {
    pub title: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ExtractedText {
    pub full_text: String,
    pub sections: Vec<Section>,
    pub source_type: String,
}

pub fn extract_from_content(content: &str, content_type: &str) -> ExtractedText {
    match content_type {
        "html" => extract_from_html(content),
        "xml" => extract_from_xml(content),
        _ => ExtractedText {
            full_text: clean_text(content),
            sections: Vec::new(),
            source_type: content_type.to_string(),
        },
    }
}

pub fn extract_from_html(html: &str) -> ExtractedText {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);

    // Remove script, style, nav, footer, header content by collecting visible text.
    let remove_sel = Selector::parse("script, style, nav, footer, header, aside").unwrap();
    let body_sel = Selector::parse("body").unwrap();

    let mut full_text = String::new();
    let mut sections = Vec::new();

    // Try to extract sections from headings.
    let heading_sel = Selector::parse("h1, h2, h3").unwrap();
    let headings: Vec<_> = document.select(&heading_sel).collect();

    if headings.is_empty() {
        // No headings — just extract all visible text.
        if let Some(body) = document.select(&body_sel).next() {
            let remove_ids: std::collections::HashSet<_> =
                document.select(&remove_sel).map(|el| el.id()).collect();
            for text_node in body.text() {
                let trimmed = text_node.trim();
                if !trimmed.is_empty() {
                    if !full_text.is_empty() {
                        full_text.push(' ');
                    }
                    full_text.push_str(trimmed);
                }
            }
            // Filter out removed elements' text (approximate — just use full body text).
            let _ = remove_ids;
        }
    } else {
        // Extract text between headings as sections.
        for heading in &headings {
            let title = heading.text().collect::<String>().trim().to_string();
            let mut section_text = String::new();
            // Collect text from sibling elements until the next heading.
            let mut sibling = heading.next_sibling();
            while let Some(node) = sibling {
                if let Some(element) = node.value().as_element() {
                    let tag = element.name().to_lowercase();
                    if tag == "h1" || tag == "h2" || tag == "h3" {
                        break;
                    }
                }
                for text in node
                    .value()
                    .as_text()
                    .map(|t| t.trim().to_string())
                    .into_iter()
                    .filter(|t| !t.is_empty())
                {
                    if !section_text.is_empty() {
                        section_text.push(' ');
                    }
                    section_text.push_str(&text);
                }
                sibling = node.next_sibling();
            }

            if !section_text.is_empty() {
                if !full_text.is_empty() {
                    full_text.push_str("\n\n");
                }
                full_text.push_str(&title);
                full_text.push('\n');
                full_text.push_str(&section_text);
                sections.push(Section {
                    title: Some(title),
                    content: section_text,
                });
            }
        }
    }

    ExtractedText {
        full_text: clean_text(&full_text),
        sections,
        source_type: "html".to_string(),
    }
}

pub fn extract_from_xml(xml: &str) -> ExtractedText {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut sections = Vec::new();
    let mut full_text = String::new();
    let mut current_section: Option<String> = None;
    let mut current_text = String::new();
    let mut in_abstract = false;
    let mut in_body = false;
    let mut in_sec = false;
    let mut depth = 0u32;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                let tag = event.local_name();
                let tag_bytes = tag.as_ref();
                if tag_bytes.eq_ignore_ascii_case(b"abstract") {
                    in_abstract = true;
                    current_section = Some("Abstract".to_string());
                    current_text.clear();
                } else if tag_bytes.eq_ignore_ascii_case(b"body") {
                    in_body = true;
                } else if tag_bytes.eq_ignore_ascii_case(b"sec") && in_body {
                    in_sec = true;
                    depth += 1;
                    if depth == 1 {
                        current_text.clear();
                        current_section = None;
                    }
                } else if tag_bytes.eq_ignore_ascii_case(b"title") && in_sec && depth == 1 {
                    // Will capture in Text event.
                }
            }
            Ok(Event::Text(event)) => {
                if let Ok(text) = event.decode() {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        // skip
                    } else if in_abstract || in_sec {
                        if !current_text.is_empty() {
                            current_text.push(' ');
                        }
                        current_text.push_str(trimmed);
                    }
                }
            }
            Ok(Event::End(event)) => {
                let tag = event.local_name();
                let tag_bytes = tag.as_ref();
                if tag_bytes.eq_ignore_ascii_case(b"abstract") {
                    in_abstract = false;
                    let content = std::mem::take(&mut current_text);
                    if !content.is_empty() {
                        if !full_text.is_empty() {
                            full_text.push_str("\n\n");
                        }
                        full_text.push_str(&content);
                        sections.push(Section {
                            title: current_section.take(),
                            content,
                        });
                    }
                } else if tag_bytes.eq_ignore_ascii_case(b"body") {
                    in_body = false;
                } else if tag_bytes.eq_ignore_ascii_case(b"sec") && in_sec {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        in_sec = false;
                        let content = std::mem::take(&mut current_text);
                        if !content.is_empty() {
                            if !full_text.is_empty() {
                                full_text.push_str("\n\n");
                            }
                            full_text.push_str(&content);
                            sections.push(Section {
                                title: current_section.take(),
                                content,
                            });
                        }
                    }
                } else if tag_bytes.eq_ignore_ascii_case(b"title") && in_sec && depth == 1 {
                    // The text we just captured is the section title.
                    if current_section.is_none() {
                        let title = current_text.trim().to_string();
                        current_section = Some(title);
                        current_text.clear();
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // If no sections found, use the full text from a simple text extraction.
    if sections.is_empty() && full_text.is_empty() {
        full_text = extract_all_text_from_xml(xml);
    }

    ExtractedText {
        full_text: clean_text(&full_text),
        sections,
        source_type: "xml".to_string(),
    }
}

fn extract_all_text_from_xml(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(event)) => {
                if let Ok(decoded) = event.decode() {
                    let trimmed = decoded.trim();
                    if !trimmed.is_empty() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(trimmed);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    text
}

fn clean_text(text: &str) -> String {
    // Collapse 3+ newlines into 2, collapse multiple spaces.
    let mut result = String::with_capacity(text.len());
    let mut newline_count = 0u32;
    let mut last_was_space = false;

    for ch in text.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                result.push(ch);
            }
            last_was_space = false;
        } else if ch == ' ' || ch == '\t' {
            newline_count = 0;
            if !last_was_space {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            newline_count = 0;
            last_was_space = false;
            result.push(ch);
        }
    }

    result.trim().to_string()
}
