use dom_smoothie::{Config as ReadabilityConfig, Readability, TextMode};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, UPGRADE_INSECURE_REQUESTS};
use serde_json::Value;

/// Represents a navigation link (Previous / Next chapter or page).
#[derive(Debug, Clone, PartialEq)]
pub struct NavigationLink {
    pub url: String,
    pub label: String,
}

/// Extracted article data ready for display.
#[derive(Debug, Clone)]
pub struct ArticleData {
    pub url: String,
    pub title: String,
    pub byline: String,
    pub html_content: String,
    pub text_content: String,
    pub reading_time_mins: i32,
    pub prev_link: Option<NavigationLink>,
    pub next_link: Option<NavigationLink>,
}

static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9,id;q=0.8"),
    );
    headers.insert(
        UPGRADE_INSECURE_REQUESTS,
        HeaderValue::from_static("1"),
    );
    headers.insert(
        "Sec-Fetch-Dest",
        HeaderValue::from_static("document"),
    );
    headers.insert(
        "Sec-Fetch-Mode",
        HeaderValue::from_static("navigate"),
    );
    headers.insert(
        "Sec-Fetch-Site",
        HeaderValue::from_static("none"),
    );
    headers.insert(
        "Sec-Fetch-User",
        HeaderValue::from_static("?1"),
    );

    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0")
        .default_headers(headers)
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::limited(10))
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
/// Combines Direct Content Container Extractor (for WordPress, light novels, and blogs),
/// Mozilla Readability (via `dom_smoothie` for general news/articles),
/// and embedded JSON extractors (such as Next.js `__NEXT_DATA__` and JSON-LD).
///
/// Compares the text length extracted by each method and selects the most complete
/// (longest) content so chapters and articles are never truncated.
pub fn extract_article(url: &str, html: &str) -> Result<ArticleData, String> {
    // 1. Direct Content Container Extractor (.entry-content, .post-content, etc.)
    let container_result = extract_direct_container(url, html);

    // 2. Mozilla Readability (dom_smoothie)
    let readability_result: Option<ArticleData> = (|| {
        let mut config = ReadabilityConfig::default();
        config.text_mode = TextMode::Formatted;
        config.char_threshold = 150;
        config.n_top_candidates = 10;
        config.readable_min_score = 10.0;
        config.readable_min_content_length = 80;

        let mut readability = Readability::new(html, Some(url), Some(config)).ok()?;
        let article = readability.parse().ok()?;

        let title = html_escape::decode_html_entities(&article.title).to_string();
        let byline = article
            .byline
            .as_deref()
            .map(|b| html_escape::decode_html_entities(b).to_string())
            .unwrap_or_default();
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
            prev_link: None,
            next_link: None,
        })
    })();

    // 3. Embedded JSON Extractor (__NEXT_DATA__, JSON-LD)
    let json_result = extract_from_embedded_json(url, html);

    // Diagnostic logging
    let container_chars = container_result.as_ref().map(|a| a.text_content.trim().chars().count()).unwrap_or(0);
    let readability_chars = readability_result.as_ref().map(|a| a.text_content.trim().chars().count()).unwrap_or(0);
    let json_chars = json_result.as_ref().map(|a| a.text_content.trim().chars().count()).unwrap_or(0);

    eprintln!(
        "[folio::fetcher] Text length comparison — Container: {} chars, Readability: {} chars, Embedded JSON: {} chars",
        container_chars, readability_chars, json_chars
    );

    let mut candidates: Vec<(&'static str, ArticleData)> = Vec::new();
    if let Some(art) = container_result {
        candidates.push(("Direct Container", art));
    }
    if let Some(art) = readability_result {
        candidates.push(("dom_smoothie", art));
    }
    if let Some(art) = json_result {
        candidates.push(("Embedded JSON", art));
    }

    if candidates.is_empty() {
        return Err("Failed to extract article content from HTML or embedded data".to_string());
    }

    // Sort descending by characters in text_content to pick the most complete result
    candidates.sort_by(|a, b| {
        b.1.text_content.trim().chars().count().cmp(&a.1.text_content.trim().chars().count())
    });

    let (chosen_source, mut chosen_art) = candidates.remove(0);
    eprintln!(
        "[folio::fetcher] Selected '{}' with {} characters ({} words)",
        chosen_source,
        chosen_art.text_content.trim().chars().count(),
        chosen_art.text_content.split_whitespace().count()
    );

    // Extract navigation links (Previous / Next chapter or page) from the full HTML
    let (nav_prev, nav_next) = extract_navigation_links(url, html);
    if chosen_art.prev_link.is_none() {
        chosen_art.prev_link = nav_prev;
    }
    if chosen_art.next_link.is_none() {
        chosen_art.next_link = nav_next;
    }

    Ok(chosen_art)
}

/// Helper function to clean text for navigation keyword matching.
fn clean_nav_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "«»‹›←→<>[]()-|•_#~".contains(c) {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Check if text/rel/class indicates a "Previous" link.
fn is_prev_indicator(text: &str, rel: &str, class: &str, aria_or_title: &str) -> bool {
    let rel_lower = rel.to_lowercase();
    if rel_lower.split_whitespace().any(|r| r == "prev" || r == "previous") {
        return true;
    }

    let class_lower = class.to_lowercase();
    if class_lower.contains("nav-previous")
        || class_lower.contains("previous-post")
        || class_lower.contains("prev-chapter")
        || class_lower.contains("previous-chapter")
        || class_lower.contains("ch-nav-prev")
        || class_lower.contains("nav-prev")
    {
        return true;
    }

    let clean = clean_nav_text(text);
    const PREV_KEYS: &[&str] = &[
        "prev",
        "previous",
        "previous chapter",
        "prev chapter",
        "previous post",
        "prev post",
        "previous page",
        "prev page",
        "sebelumnya",
        "chapter sebelumnya",
        "bab sebelumnya",
        "halaman sebelumnya",
        "back to previous",
        "earlier",
        "chap sebelumnya",
    ];

    for key in PREV_KEYS {
        if clean == *key || clean.starts_with(&format!("{key} ")) || clean.ends_with(&format!(" {key}")) {
            return true;
        }
    }

    let meta_clean = clean_nav_text(aria_or_title);
    for key in PREV_KEYS {
        if meta_clean == *key || meta_clean.starts_with(&format!("{key} ")) {
            return true;
        }
    }

    false
}

/// Check if text/rel/class indicates a "Next" link.
fn is_next_indicator(text: &str, rel: &str, class: &str, aria_or_title: &str) -> bool {
    let rel_lower = rel.to_lowercase();
    if rel_lower.split_whitespace().any(|r| r == "next") {
        return true;
    }

    let class_lower = class.to_lowercase();
    if class_lower.contains("nav-next")
        || class_lower.contains("next-post")
        || class_lower.contains("next-chapter")
        || class_lower.contains("ch-nav-next")
        || class_lower.contains("nav-next")
    {
        return true;
    }

    let clean = clean_nav_text(text);
    const NEXT_KEYS: &[&str] = &[
        "next",
        "next chapter",
        "next post",
        "next page",
        "selanjutnya",
        "chapter selanjutnya",
        "bab selanjutnya",
        "halaman selanjutnya",
        "forward",
        "chap selanjutnya",
    ];

    for key in NEXT_KEYS {
        if clean == *key || clean.starts_with(&format!("{key} ")) || clean.ends_with(&format!(" {key}")) {
            return true;
        }
    }

    let meta_clean = clean_nav_text(aria_or_title);
    for key in NEXT_KEYS {
        if meta_clean == *key || meta_clean.starts_with(&format!("{key} ")) {
            return true;
        }
    }

    false
}

/// Extract Previous and Next navigation links from HTML document.
pub fn extract_navigation_links(current_url: &str, html: &str) -> (Option<NavigationLink>, Option<NavigationLink>) {
    let base_url = url::Url::parse(current_url).ok();

    let resolve_url = |href: &str| -> Option<String> {
        let trimmed = href.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("javascript:") {
            return None;
        }
        let resolved = if let Some(ref b) = base_url {
            b.join(trimmed).ok()?.to_string()
        } else {
            url::Url::parse(trimmed).ok()?.to_string()
        };
        if resolved == current_url {
            return None;
        }
        Some(resolved)
    };

    let mut prev_link: Option<NavigationLink> = None;
    let mut next_link: Option<NavigationLink> = None;

    let doc = dom_query::Document::from(html);

    // 1. Check <link rel="prev"> and <link rel="next"> in <head>
    for link in doc.select("link[rel*='prev'], link[rel*='next']").iter() {
        let rel = link.attr("rel").unwrap_or_default().to_lowercase();
        let href = link.attr("href").unwrap_or_default().to_string();
        if let Some(target) = resolve_url(&href) {
            if rel.contains("prev") && prev_link.is_none() {
                prev_link = Some(NavigationLink {
                    url: target,
                    label: "Previous Chapter".to_string(),
                });
            } else if rel.contains("next") && next_link.is_none() {
                next_link = Some(NavigationLink {
                    url: target,
                    label: "Next Chapter".to_string(),
                });
            }
        }
    }

    // 2. Scan dedicated navigation CSS containers & anchor elements
    let anchor_sel = "a[rel*='prev'], a[rel*='next'], .nav-previous a, .nav-next a, .previous-post a, .next-post a, .nav-links a, .prev-chapter, .next-chapter, a[class*='prev'], a[class*='next'], a";
    for a in doc.select(anchor_sel).iter() {
        if prev_link.is_some() && next_link.is_some() {
            break;
        }

        let href = match a.attr("href") {
            Some(h) => h.to_string(),
            None => continue,
        };
        let resolved = match resolve_url(&href) {
            Some(r) => r,
            None => continue,
        };

        let text = a.text().trim().to_string();
        if text.chars().count() > 80 {
            continue; // Skip lengthy content links
        }

        let rel = a.attr("rel").unwrap_or_default().to_string();
        let class = a.attr("class").unwrap_or_default().to_string();
        let aria_title = a
            .attr("aria-label")
            .or_else(|| a.attr("title"))
            .unwrap_or_default()
            .to_string();

        if prev_link.is_none() && is_prev_indicator(&text, &rel, &class, &aria_title) {
            let label = if text.is_empty() {
                "Previous Chapter".to_string()
            } else {
                let clean = clean_nav_text(&text);
                if clean == "prev" || clean == "previous" {
                    "Previous Chapter".to_string()
                } else {
                    html_escape::decode_html_entities(text.trim()).to_string()
                }
            };
            prev_link = Some(NavigationLink { url: resolved.clone(), label });
        }

        if next_link.is_none() && is_next_indicator(&text, &rel, &class, &aria_title) {
            let label = if text.is_empty() {
                "Next Chapter".to_string()
            } else {
                let clean = clean_nav_text(&text);
                if clean == "next" {
                    "Next Chapter".to_string()
                } else {
                    html_escape::decode_html_entities(text.trim()).to_string()
                }
            };
            next_link = Some(NavigationLink { url: resolved, label });
        }
    }

    // 3. Check Next.js __NEXT_DATA__ (e.g. animeanyway.com)
    if prev_link.is_none() || next_link.is_none() {
        let next_script = doc.select("script#__NEXT_DATA__").first().text().trim().to_string();
        if !next_script.is_empty() {
            if let Ok(data) = serde_json::from_str::<Value>(&next_script) {
                let props = &data["props"]["pageProps"];
                if let Some(keys) = props["chapterKeys"].as_array() {
                    let cur_key = props["currentKey"].as_str().unwrap_or_default();
                    if let Some(ref base) = base_url {
                        let base_str = base.as_str();
                        let base_dir = base_str
                            .trim_end_matches('/')
                            .rsplit_once('/')
                            .map(|(p, _)| format!("{p}/"))
                            .unwrap_or_else(|| base_str.to_string());

                        if keys.len() == 2 {
                            let k0 = keys[0].as_str().unwrap_or_default();
                            let k1 = keys[1].as_str().unwrap_or_default();
                            if prev_link.is_none() && !k0.is_empty() && k0 != cur_key {
                                if let Some(target) = resolve_url(&format!("{base_dir}{k0}")) {
                                    prev_link = Some(NavigationLink {
                                        url: target,
                                        label: format!("Previous ({k0})"),
                                    });
                                }
                            }
                            if next_link.is_none() && !k1.is_empty() && k1 != cur_key {
                                if let Some(target) = resolve_url(&format!("{base_dir}{k1}")) {
                                    next_link = Some(NavigationLink {
                                        url: target,
                                        label: format!("Next ({k1})"),
                                    });
                                }
                            }
                        } else if keys.len() == 1 {
                            let k0 = keys[0].as_str().unwrap_or_default();
                            if !k0.is_empty() && k0 != cur_key {
                                if let Some(target) = resolve_url(&format!("{base_dir}{k0}")) {
                                    if k0.contains("ch2") || k0 > cur_key {
                                        if next_link.is_none() {
                                            next_link = Some(NavigationLink {
                                                url: target,
                                                label: format!("Next ({k0})"),
                                            });
                                        }
                                    } else if prev_link.is_none() {
                                        prev_link = Some(NavigationLink {
                                            url: target,
                                            label: format!("Previous ({k0})"),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (prev_link, next_link)
}

/// Direct Content Container Extractor.
///
/// Searches for primary content containers by priority CSS selectors:
/// `.entry-content`, `.post-content`, `.chapter-content`, `.reading-content`,
/// `#chapter-content`, `article`, `main`, `[role="main"]`.
///
/// Strips distracting elements (scripts, styles, ads, social share buttons,
/// navigation links, comments, sidebars) and returns the full container content.
pub fn extract_direct_container(url: &str, html: &str) -> Option<ArticleData> {
    let doc = dom_query::Document::from(html);

    let candidates = [
        "#mw-content-text > .mw-parser-output",
        ".mw-parser-output",
        ".entry-content",
        ".post-content",
        ".chapter-content",
        ".reading-content",
        "#chapter-content",
        "article",
        "main",
        r#"[role="main"]"#,
        r#"div[id^="readability-page"]"#,
    ];

    let mut matched_sel: Option<dom_query::Selection> = None;
    let mut best_word_count = 0;

    for selector in &candidates {
        let sel = doc.select(selector);
        if sel.length() > 0 {
            for item in sel.iter() {
                let text = item.text();
                let words = text.split_whitespace().count();
                if words > best_word_count && words >= 30 {
                    best_word_count = words;
                    matched_sel = Some(item);
                }
            }
            if matched_sel.is_some() {
                break;
            }
        }
    }

    let node = matched_sel?;

    // Remove noise elements inside the container
    let noise_selectors = "script, style, noscript, iframe, ins, nav, footer, aside, form, svg, .wp-block-buttons, [class*='ads'], [id*='ads'], [class*='adsbygoogle'], [id*='adsbygoogle'], [class*='share'], [id*='share'], [class*='related'], [id*='related'], [class*='navigation'], [id*='navigation'], [class*='nav-links'], [id*='nav-links'], [class*='comments'], [id*='comments'], [class*='sidebar'], [id*='sidebar']";
    node.select(noise_selectors).remove();

    let text_content = node.text().to_string();
    let word_count = text_content.split_whitespace().count();
    if word_count < 20 {
        return None;
    }

    let title = extract_html_title(html)
        .or_else(|| {
            let h1 = doc.select("h1").first().text().trim().to_string();
            if !h1.is_empty() {
                Some(html_escape::decode_html_entities(&h1).to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "Article".to_string());

    let byline = doc
        .select(r#"meta[name="author"], meta[property="article:author"]"#)
        .attr("content")
        .map(|s| html_escape::decode_html_entities(s.as_ref()).to_string())
        .or_else(|| {
            let a = doc.select(r#"[rel="author"], .byline, .author, .post-author"#).first().text().trim().to_string();
            if !a.is_empty() && a.len() < 80 {
                Some(html_escape::decode_html_entities(&a).to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    let html_content = node.inner_html().to_string();
    let reading_time_mins = ((word_count as f64) / 238.0).ceil() as i32;

    Some(ArticleData {
        url: url.to_string(),
        title,
        byline,
        html_content,
        text_content,
        reading_time_mins: reading_time_mins.max(1),
        prev_link: None,
        next_link: None,
    })
}

/// Alias for extract_direct_container for backward compatibility.
#[allow(dead_code)]
pub fn extract_from_semantic_tags(url: &str, html: &str) -> Option<ArticleData> {
    extract_direct_container(url, html)
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
        prev_link: None,
        next_link: None,
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
        prev_link: None,
        next_link: None,
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
        Some(html_escape::decode_html_entities(title).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_semantic_fallback() {
        // HTML where dom_smoothie fails/struggles due to multi-section divs or low score,
        // but semantic <article> has rich text
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head><title>Semantic Fallback Test &mdash; Site</title></head>
            <body>
                <nav>Header Nav</nav>
                <article>
                    <h1>The True Story of Linux</h1>
                    <div class="author">By Linus Torvalds</div>
                    <section>
                        <p>Linux began in 1991 as a personal project by Finnish student Linus Torvalds.</p>
                        <p>He created a new free operating system kernel modeled after MINIX.</p>
                    </section>
                    <section>
                        <p>Today, Linux powers everything from Android smartphones to the world's fastest supercomputers.</p>
                        <p>Open source collaboration transformed computing forever.</p>
                    </section>
                </article>
                <div class="ads">Ad banner</div>
                <div class="comments">Comments section</div>
            </body>
            </html>
        "#;

        let result = extract_article("https://example.com/linux", html).expect("Extraction should succeed");
        assert!(result.title.contains("The True Story of Linux") || result.title.contains("Semantic Fallback Test"));
        assert!(result.text_content.contains("Linux began in 1991"));
        assert!(result.text_content.contains("Open source collaboration transformed computing forever."));
        assert!(!result.text_content.contains("Ad banner"));
        assert!(!result.text_content.contains("Comments section"));
    }

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
    fn test_extract_wordpress_novel_full_container() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head><title>Chapter 10: The Hidden Chamber - Light Novel World</title></head>
            <body>
                <header>
                    <nav class="site-navigation"><a href="/">Home</a><a href="/novels">Novels</a></nav>
                </header>
                <div class="content-wrapper">
                    <div class="entry-content">
                        <h1>Chapter 10: The Hidden Chamber</h1>
                        <p class="post-meta">Written by Author San</p>
                        <div class="code-block ads-top">Ad space top</div>
                        <p>"Is this the place?" asked Ren.</p>
                        <p>"Yes," replied Mia softly.</p>
                        <div class="sharedaddy sd-sharing-enabled">Share this button</div>
                        <p>Part 1 of the journey ended at the gate. The stone walls were covered in ancient runes.</p>
                        <div class="adsbygoogle in-article-ad">Ad in middle of chapter</div>
                        <p>Part 2 began deep within the labyrinth. Ren stepped forward cautiously.</p>
                        <p>"Watch out!" Mia shouted.</p>
                        <div class="related-posts">Related chapters: Chapter 9, Chapter 11</div>
                        <p>Part 3 was the climax where they uncovered the hidden chamber filled with ancient artifacts.</p>
                        <p>"We finally made it," Ren whispered with a smile.</p>
                        <div class="wp-block-buttons chapter-nav"><a href="/ch9">Prev</a><a href="/ch11">Next</a></div>
                        <div id="comments" class="comments-area">Comments section</div>
                    </div>
                </div>
                <footer>Footer links</footer>
            </body>
            </html>
        "#;

        let result = extract_article("https://example.com/novel/ch10", html).expect("Extraction should succeed");
        assert!(result.title.contains("Chapter 10"));
        // Check that all parts of the chapter are present (beginning, middle, end)
        assert!(result.text_content.contains("Is this the place?"));
        assert!(result.text_content.contains("Part 1 of the journey ended"));
        assert!(result.text_content.contains("Part 2 began deep within"));
        assert!(result.text_content.contains("Part 3 was the climax"));
        assert!(result.text_content.contains("We finally made it"));
        // Check that noise elements were stripped
        assert!(!result.text_content.contains("Ad space top"));
        assert!(!result.text_content.contains("Share this button"));
        assert!(!result.text_content.contains("Ad in middle of chapter"));
        assert!(!result.text_content.contains("Related chapters"));
        assert!(!result.text_content.contains("Comments section"));
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

    #[test]
    fn test_extract_wikipedia_full_content_if_present() {
        let html_opt = std::fs::read_to_string("/tmp/wikipedia_rust.html").ok().or_else(|| {
            let conn = rusqlite::Connection::open("/home/dheovanwa/.local/share/folio/library.db").ok()?;
            let content: String = conn.query_row(
                "SELECT html_content FROM articles WHERE url LIKE '%wikipedia%'",
                [],
                |row| row.get(0),
            ).ok()?;
            if content.contains("mw-parser-output") {
                Some(content)
            } else {
                None
            }
        });

        if let Some(html) = html_opt {
            let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
            let full_article = extract_article(url, &html).expect("Wikipedia extraction must succeed");
            let blocks = crate::ui::window::parse_article_blocks(&full_article);

            assert!(blocks.len() >= 450, "Wikipedia article should have 450+ blocks, got {}", blocks.len());

            let has_cargo = blocks.iter().any(|b| {
                matches!(b, crate::ui::window::ContentBlock::Paragraph { plain, .. } if plain.contains("Cargo is Rust's"))
            });
            assert!(has_cargo, "Article must contain Cargo paragraph");

            let has_tiobe = blocks.iter().any(|b| {
                matches!(b, crate::ui::window::ContentBlock::Paragraph { plain, .. } if plain.contains("TIOBE"))
            });
            assert!(has_tiobe, "Article must contain TIOBE paragraph");

            let has_reception_heading = blocks.iter().any(|b| {
                matches!(b, crate::ui::window::ContentBlock::Heading { text, .. } if text == "Reception")
            });
            assert!(has_reception_heading, "Article must contain Reception heading");

            let has_academic = blocks.iter().any(|b| {
                matches!(b, crate::ui::window::ContentBlock::Heading { text, .. } if text == "In academic research")
            });
            assert!(has_academic, "Article must contain In academic research heading");

            let has_foundation = blocks.iter().any(|b| {
                matches!(b, crate::ui::window::ContentBlock::Heading { text, .. } if text == "Rust Foundation")
            });
            assert!(has_foundation, "Article must contain Rust Foundation heading");
        }
    }

    #[test]
    fn test_extract_navigation_links_html_and_keywords() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <title>Chapter 2</title>
                <link rel="prev" href="https://example.com/ch1" />
                <link rel="next" href="https://example.com/ch3" />
            </head>
            <body>
                <div class="content"><p>Body of chapter 2</p></div>
                <div class="nav-links">
                    <a href="/ch1" class="prev-chapter">« Previous Chapter</a>
                    <a href="/ch3" class="next-chapter">Next Chapter »</a>
                </div>
            </body>
            </html>
        "#;
        let (prev, next) = extract_navigation_links("https://example.com/ch2", html);
        assert!(prev.is_some(), "Must extract previous chapter link");
        assert!(next.is_some(), "Must extract next chapter link");
        assert_eq!(prev.unwrap().url, "https://example.com/ch1");
        assert_eq!(next.unwrap().url, "https://example.com/ch3");
    }

    #[test]
    fn test_extract_navigation_links_indonesian_keywords() {
        let html = r#"
            <article>
                <p>Isi artikel bab 5.</p>
                <div class="pagination">
                    <a href="https://example.com/bab-4">Sebelumnya</a>
                    <a href="https://example.com/bab-6">Selanjutnya</a>
                </div>
            </article>
        "#;
        let (prev, next) = extract_navigation_links("https://example.com/bab-5", html);
        assert_eq!(prev.unwrap().url, "https://example.com/bab-4");
        assert_eq!(next.unwrap().url, "https://example.com/bab-6");
    }
}
