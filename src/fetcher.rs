use dom_smoothie::{Config as ReadabilityConfig, Readability, TextMode};
use serde_json::Value;

/// Extracted article data ready for display.
#[derive(Debug, Clone)]
pub struct ArticleData {
    pub url: String,
    pub title: String,
    pub byline: String,
    pub html_content: String,
    pub text_content: String,
    pub reading_time_mins: i32,
}

static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) Folio/0.1")
        .timeout(std::time::Duration::from_secs(30))
        .pool_idle_timeout(std::time::Duration::from_secs(15))
        .pool_max_idle_per_host(2)
        .build()
        .unwrap_or_default()
});

/// Fetch a URL and extract article content.
///
/// This function runs on a tokio thread and returns the extracted article.
pub async fn fetch_article(url: &str) -> Result<ArticleData, String> {
    let response = CLIENT
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch URL: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    extract_article(url, &html)
}

/// Extract article content from raw HTML.
///
/// Combines Mozilla Readability (via `dom_smoothie`) with embedded client-side
/// framework data extractors (such as Next.js `__NEXT_DATA__` and JSON-LD).
/// When client-side rendered sites return only an empty shell with footer clutter,
/// the embedded data extractor retrieves the complete article payload.
pub fn extract_article(url: &str, html: &str) -> Result<ArticleData, String> {
    // 1. Try Readability (dom_smoothie)
    let readability_result: Option<ArticleData> = (|| {
        let mut config = ReadabilityConfig::default();
        config.text_mode = TextMode::Formatted;

        let mut readability = Readability::new(html, Some(url), Some(config)).ok()?;
        let article = readability.parse().ok()?;

        let title = article.title.clone();
        let byline = article.byline.clone().unwrap_or_default();
        let html_content = article.content.to_string();
        let text_content = article.text_content.to_string();

        let word_count = text_content.split_whitespace().count();
        let reading_time_mins = ((word_count as f64) / 238.0).ceil() as i32;

        Some(ArticleData {
            url: url.to_string(),
            title,
            byline,
            html_content,
            text_content,
            reading_time_mins: reading_time_mins.max(1),
        })
    })();

    // 2. Try embedded JSON (Next.js __NEXT_DATA__, JSON-LD, etc.)
    let json_result = extract_from_embedded_json(url, html);

    match (readability_result, json_result) {
        (Some(read_art), Some(json_art)) => {
            let read_words = read_art.text_content.split_whitespace().count();
            let json_words = json_art.text_content.split_whitespace().count();

            // If embedded JSON has significantly more content (e.g. Next.js SPA like animeanyway.com
            // where Readability only grabbed footer boilerplate while JSON has the entire chapter)
            if json_words > read_words * 2 || (read_words < 150 && json_words >= 150) {
                Ok(json_art)
            } else {
                Ok(read_art)
            }
        }
        (Some(read_art), None) => Ok(read_art),
        (None, Some(json_art)) => Ok(json_art),
        (None, None) => Err("Failed to extract article content from HTML or embedded data".to_string()),
    }
}

/// Try to extract article data from embedded scripts (__NEXT_DATA__, JSON-LD, etc.)
pub fn extract_from_embedded_json(url: &str, html: &str) -> Option<ArticleData> {
    // 1. Try __NEXT_DATA__
    if let Some(data) = extract_next_data(url, html) {
        if !data.text_content.trim().is_empty() {
            return Some(data);
        }
    }

    // 2. Try JSON-LD
    if let Some(data) = extract_json_ld(url, html) {
        if !data.text_content.trim().is_empty() {
            return Some(data);
        }
    }

    None
}

/// Extract content from Next.js `<script id="__NEXT_DATA__">`
fn extract_next_data(url: &str, html: &str) -> Option<ArticleData> {
    let script_start = html.find("id=\"__NEXT_DATA__\"")?;
    let tag_close = html[script_start..].find('>')? + script_start;
    let end_tag = html[tag_close..].find("</script>")? + tag_close;
    let json_str = &html[tag_close + 1..end_tag];

    let json: Value = serde_json::from_str(json_str).ok()?;
    let page_props = json.get("props")?.get("pageProps")?;

    // Candidate keys inside pageProps
    let candidate_keys = [
        "chapter", "article", "post", "story", "novel", "book", "entry", "content", "data",
    ];

    let mut found_candidate: Option<&Value> = None;
    for &key in &candidate_keys {
        if let Some(val) = page_props.get(key) {
            if val.is_object() || val.is_array() {
                found_candidate = Some(val);
                break;
            }
        }
    }

    let target = found_candidate.unwrap_or(page_props);

    // Title resolution
    let page_volume = page_props.get("volumeTitle").and_then(|v| v.as_str());
    let page_title = target
        .get("title")
        .or_else(|| target.get("name"))
        .or_else(|| target.get("heading"))
        .or_else(|| page_props.get("title"))
        .and_then(|v| v.as_str());

    let title = match (page_volume, page_title) {
        (Some(vol), Some(tit)) if !tit.contains(vol) => format!("{vol} - {tit}"),
        (_, Some(tit)) => tit.to_string(),
        (Some(vol), None) => vol.to_string(),
        (None, None) => extract_html_title(html).unwrap_or_else(|| "Article".to_string()),
    };

    // Byline resolution
    let byline = target
        .get("byline")
        .or_else(|| target.get("author"))
        .or_else(|| page_props.get("author"))
        .or_else(|| page_props.get("translator"))
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else if let Some(obj) = v.as_object() {
                obj.get("name").and_then(|n| n.as_str()).map(|n| n.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    // Content extraction
    let content_val = target
        .get("content")
        .or_else(|| target.get("body"))
        .or_else(|| target.get("text"))
        .or_else(|| target.get("blocks"))
        .or_else(|| target.get("paragraphs"))
        .unwrap_or(target);

    let (html_content, text_content) = parse_json_content(content_val)?;

    let word_count = text_content.split_whitespace().count();
    if word_count == 0 {
        return None;
    }

    let reading_time_mins = ((word_count as f64) / 238.0).ceil() as i32;

    Some(ArticleData {
        url: url.to_string(),
        title,
        byline,
        html_content,
        text_content,
        reading_time_mins: reading_time_mins.max(1),
    })
}

/// Parse JSON content representing portable text blocks, rich text AST, or strings.
fn parse_json_content(val: &Value) -> Option<(String, String)> {
    match val {
        Value::Array(arr) => {
            let mut html_blocks = Vec::new();
            let mut text_blocks = Vec::new();

            for item in arr {
                if let Value::Object(obj) = item {
                    let itype = obj.get("_type").and_then(|v| v.as_str()).unwrap_or("");
                    match itype {
                        "block" => {
                            let style = obj.get("style").and_then(|v| v.as_str()).unwrap_or("normal");
                            let mut span_html = String::new();
                            let mut span_text = String::new();

                            if let Some(Value::Array(children)) = obj.get("children") {
                                for child in children {
                                    if let Some(txt) = child.get("text").and_then(|t| t.as_str()) {
                                        let mut h = escape_html(txt);
                                        if let Some(Value::Array(marks)) = child.get("marks") {
                                            for m in marks {
                                                if let Some(m_str) = m.as_str() {
                                                    match m_str {
                                                        "strong" => h = format!("<strong>{h}</strong>"),
                                                        "em" => h = format!("<em>{h}</em>"),
                                                        "code" => h = format!("<code>{h}</code>"),
                                                        "underline" => h = format!("<u>{h}</u>"),
                                                        "strike-through" => h = format!("<s>{h}</s>"),
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }
                                        span_html.push_str(&h);
                                        span_text.push_str(txt);
                                    }
                                }
                            }

                            let trimmed_text = span_text.trim();
                            if !trimmed_text.is_empty() {
                                match style {
                                    "h1" => html_blocks.push(format!("<h1>{span_html}</h1>")),
                                    "h2" => html_blocks.push(format!("<h2>{span_html}</h2>")),
                                    "h3" => html_blocks.push(format!("<h3>{span_html}</h3>")),
                                    "blockquote" => html_blocks.push(format!("<blockquote>{span_html}</blockquote>")),
                                    _ => html_blocks.push(format!("<p>{span_html}</p>")),
                                }
                                text_blocks.push(span_text);
                            }
                        }
                        "readerSection" => {
                            if let Some(label) = obj.get("label").and_then(|l| l.as_str()) {
                                let trimmed = label.trim();
                                if !trimmed.is_empty() {
                                    html_blocks.push(format!("<h2>{}</h2>", escape_html(trimmed)));
                                    text_blocks.push(trimmed.to_string());
                                }
                            }
                        }
                        "borderedTextBox" => {
                            if let Some(txt) = obj.get("text").and_then(|t| t.as_str()) {
                                for line in txt.lines() {
                                    let trimmed = line.trim();
                                    if !trimmed.is_empty() {
                                        html_blocks.push(format!("<blockquote>{}</blockquote>", escape_html(trimmed)));
                                        text_blocks.push(trimmed.to_string());
                                    }
                                }
                            }
                        }
                        _ => {
                            // Check for Slate/Contentful or fallback to text property
                            if let Some(txt) = obj.get("text").or_else(|| obj.get("value")).and_then(|t| t.as_str()) {
                                let trimmed = txt.trim();
                                if !trimmed.is_empty() {
                                    html_blocks.push(format!("<p>{}</p>", escape_html(trimmed)));
                                    text_blocks.push(trimmed.to_string());
                                }
                            }
                        }
                    }
                } else if let Some(str_val) = item.as_str() {
                    let trimmed = str_val.trim();
                    if !trimmed.is_empty() {
                        html_blocks.push(format!("<p>{}</p>", escape_html(trimmed)));
                        text_blocks.push(trimmed.to_string());
                    }
                }
            }

            if text_blocks.is_empty() {
                None
            } else {
                Some((html_blocks.join("\n"), text_blocks.join("\n\n")))
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            if trimmed.contains("<p>") || trimmed.contains("<div") || trimmed.contains("<h") {
                // Already HTML!
                Some((trimmed.to_string(), strip_html_tags(trimmed)))
            } else {
                // Plain text / Markdown
                let paras: Vec<&str> = trimmed.split("\n\n").map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
                let html = paras.iter().map(|p| format!("<p>{}</p>", escape_html(p))).collect::<Vec<_>>().join("\n");
                Some((html, trimmed.to_string()))
            }
        }
        _ => None,
    }
}

/// Extract content from `<script type="application/ld+json">`
fn extract_json_ld(url: &str, html: &str) -> Option<ArticleData> {
    let mut search_from = 0;
    while let Some(idx) = html[search_from..].find("application/ld+json") {
        let abs_idx = search_from + idx;
        if let Some(tag_close) = html[abs_idx..].find('>') {
            let content_start = abs_idx + tag_close + 1;
            if let Some(end_tag) = html[content_start..].find("</script>") {
                let json_slice = &html[content_start..content_start + end_tag];
                if let Ok(val) = serde_json::from_str::<Value>(json_slice) {
                    if let Some(art) = parse_json_ld_article(url, &val) {
                        return Some(art);
                    }
                }
                search_from = content_start + end_tag;
                continue;
            }
        }
        break;
    }
    None
}

fn parse_json_ld_article(url: &str, val: &Value) -> Option<ArticleData> {
    let body = val.get("articleBody")
        .or_else(|| val.get("text"))
        .and_then(|v| v.as_str())?;

    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    let title = val.get("headline")
        .or_else(|| val.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Article")
        .to_string();

    let byline = val.get("author")
        .and_then(|a| {
            if let Some(s) = a.as_str() {
                Some(s.to_string())
            } else if let Some(name) = a.get("name").and_then(|n| n.as_str()) {
                Some(name.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    let (html_content, text_content) = if trimmed.contains("<p>") {
        (trimmed.to_string(), strip_html_tags(trimmed))
    } else {
        let paras: Vec<&str> = trimmed.split("\n\n").map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        let h = paras.iter().map(|p| format!("<p>{}</p>", escape_html(p))).collect::<Vec<_>>().join("\n");
        (h, trimmed.to_string())
    };

    let word_count = text_content.split_whitespace().count();
    let reading_time_mins = ((word_count as f64) / 238.0).ceil() as i32;

    Some(ArticleData {
        url: url.to_string(),
        title,
        byline,
        html_content,
        text_content,
        reading_time_mins: reading_time_mins.max(1),
    })
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

fn extract_html_title(html: &str) -> Option<String> {
    let start = html.find("<title")?;
    let close = html[start..].find('>')? + start;
    let end = html[close..].find("</title>")? + close;
    let title = html[close + 1..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_article() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head><title>Test Article Title</title></head>
            <body>
                <header><nav>Navigation links to ignore</nav></header>
                <article>
                    <h1>Test Article Title</h1>
                    <p class="byline">By Jane Doe</p>
                    <p>This is the first paragraph of the article. It contains insightful information.</p>
                    <h2>Subheading</h2>
                    <p>This is the second paragraph with more detailed discussion about Rust and GTK4.</p>
                </article>
                <footer>Footer clutter to ignore</footer>
            </body>
            </html>
        "#;

        let result = extract_article("https://example.com/test", html).expect("Extraction should succeed");
        assert!(result.title.contains("Test Article"));
        assert!(result.text_content.contains("insightful information"));
        assert_eq!(result.reading_time_mins, 1);
    }

    #[test]
    fn test_extract_next_data_portable_text() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head><title>Chapter Page</title></head>
            <body>
                <main class="empty-shell"></main>
                <script id="__NEXT_DATA__" type="application/json">
                {
                    "props": {
                        "pageProps": {
                            "volumeTitle": "Volume 1",
                            "chapter": {
                                "title": "Chapter 1",
                                "content": [
                                    {
                                        "_type": "readerSection",
                                        "label": "Part 1"
                                    },
                                    {
                                        "_type": "block",
                                        "style": "normal",
                                        "children": [
                                            {
                                                "text": "The wind howled softly across the courtyard.",
                                                "marks": ["strong"]
                                            }
                                        ]
                                    },
                                    {
                                        "_type": "borderedTextBox",
                                        "text": "Rule 1: Always check the logs\nRule 2: Don't give up"
                                    }
                                ]
                            }
                        }
                    }
                }
                </script>
                <footer>Site links and footer clutter that readability might pick up</footer>
            </body>
            </html>
        "#;

        let result = extract_article("https://example.com/novel/ch1", html).expect("Extraction should succeed");
        assert_eq!(result.title, "Volume 1 - Chapter 1");
        assert!(result.html_content.contains("<h2>Part 1</h2>"));
        assert!(result.html_content.contains("<strong>The wind howled softly across the courtyard.</strong>"));
        assert!(result.text_content.contains("The wind howled softly across the courtyard."));
        assert!(result.text_content.contains("Rule 1: Always check the logs"));
    }

    #[test]
    fn test_extract_real_animeanyway_html_if_present() {
        if let Ok(html) = std::fs::read_to_string("/tmp/animeanyway.html") {
            let result = extract_article("https://animeanyway.com/y3v5/ch1", &html).expect("Must extract real novel");
            println!("Title: {}", result.title);
            println!("Words: {}", result.text_content.split_whitespace().count());
            println!("Reading time: {} mins", result.reading_time_mins);
            assert!(result.title.contains("Year 3 Volume 5"));
            assert!(result.text_content.contains("It was 10 a.m."));
            assert!(result.text_content.split_whitespace().count() > 10000);
            assert!(result.reading_time_mins > 50);
        }
    }
}
