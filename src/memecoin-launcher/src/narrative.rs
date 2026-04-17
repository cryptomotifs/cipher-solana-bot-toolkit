//! Narrative detection — Reddit RSS + PumpPortal WS + Google Trends.
//!
//! [VERIFIED 2026] narrative_detection_twitter_2026.md s1-s4: trend detection sources
//! [VERIFIED 2026] narrative_detection_twitter_2026.md s3: "FREE RSS feeds"

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::LauncherConfig;

/// A detected narrative signal from one of our sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrativeSignal {
    pub topic: String,
    pub category: String,
    pub score: u32,
    pub source: String,
}

/// Detect trending narratives from all free sources.
pub async fn detect_narratives(
    http: &reqwest::Client,
    config: &LauncherConfig,
) -> Result<Vec<NarrativeSignal>> {
    let mut signals = Vec::new();

    // Source 1: Reddit RSS (free, no API key)
    match detect_reddit_trends(http, &config.reddit_subreddits).await {
        Ok(mut s) => signals.append(&mut s),
        Err(e) => tracing::warn!("Reddit RSS failed: {}", e),
    }

    // Source 2: Google Trends (free, confirming signal)
    match detect_google_trends(http).await {
        Ok(mut s) => signals.append(&mut s),
        Err(e) => tracing::warn!("Google Trends failed: {}", e),
    }

    // Sort by score descending
    signals.sort_by(|a, b| b.score.cmp(&a.score));

    tracing::info!("Narrative detection: {} signals found", signals.len());
    Ok(signals)
}

/// Fetch Reddit RSS feeds and extract trending terms.
/// [VERIFIED 2026] narrative_detection_twitter_2026.md s3: "append .rss to any Reddit URL"
async fn detect_reddit_trends(
    http: &reqwest::Client,
    subreddits: &[String],
) -> Result<Vec<NarrativeSignal>> {
    let mut signals = Vec::new();
    let mut term_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    for sub in subreddits {
        let url = format!("https://www.reddit.com/r/{}/hot.rss", sub);
        let resp = http.get(&url)
            .header("User-Agent", "PredatorLauncher/1.0")
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(body) = resp.text().await {
                // Simple title extraction from RSS XML
                for title in extract_rss_titles(&body) {
                    for word in title.split_whitespace() {
                        let word = word.to_lowercase()
                            .trim_matches(|c: char| !c.is_alphanumeric())
                            .to_string();
                        if word.len() >= 3 && word.len() <= 15 && !is_stop_word(&word) {
                            *term_counts.entry(word).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
    }

    // Top terms by frequency become signals
    let mut sorted: Vec<_> = term_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    for (term, count) in sorted.into_iter().take(10) {
        if count >= 2 {
            signals.push(NarrativeSignal {
                topic: term,
                category: "trending".to_string(),
                score: count * 10,
                source: "reddit".to_string(),
            });
        }
    }

    Ok(signals)
}

/// Simple RSS title extraction (no XML parser dependency).
fn extract_rss_titles(xml: &str) -> Vec<String> {
    let mut titles = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find("<title>") {
        remaining = &remaining[start + 7..];
        if let Some(end) = remaining.find("</title>") {
            let title = &remaining[..end];
            // Skip RSS feed title (usually subreddit name)
            if !title.contains("reddit") && title.len() > 10 {
                titles.push(title.to_string());
            }
            remaining = &remaining[end + 8..];
        } else {
            break;
        }
    }
    titles
}

/// Detect trends from Google Trends daily searches.
/// [VERIFIED 2026] narrative_detection_twitter_2026.md s4: "48h lag = confirming"
async fn detect_google_trends(http: &reqwest::Client) -> Result<Vec<NarrativeSignal>> {
    let url = "https://trends.google.com/trends/api/dailytrends?hl=en-US&tz=-300&geo=US&ns=15";
    let resp = http.get(url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await?;

    let body = resp.text().await?;
    // Google Trends prefixes response with ")]}'" — skip it
    let json_str = if body.starts_with(")]}'") {
        &body[5..]
    } else {
        &body
    };

    let mut signals = Vec::new();

    // Try to parse and extract trending searches
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
        if let Some(days) = val.get("default").and_then(|d| d.get("trendingSearchesDays")).and_then(|d| d.as_array()) {
            for day in days.iter().take(1) {
                if let Some(searches) = day.get("trendingSearches").and_then(|s| s.as_array()) {
                    for (i, search) in searches.iter().take(5).enumerate() {
                        if let Some(title) = search.get("title").and_then(|t| t.get("query")).and_then(|q| q.as_str()) {
                            signals.push(NarrativeSignal {
                                topic: title.to_lowercase(),
                                category: "google_trends".to_string(),
                                score: ((5 - i) * 15) as u32,
                                source: "google".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(signals)
}

/// Common stop words to filter out of trending terms.
fn is_stop_word(word: &str) -> bool {
    matches!(word,
        "the" | "and" | "for" | "that" | "this" | "with" | "are" | "was" | "has"
        | "have" | "will" | "from" | "not" | "but" | "what" | "can" | "all" | "been"
        | "would" | "there" | "their" | "which" | "when" | "one" | "could" | "more"
        | "about" | "into" | "than" | "its" | "just" | "some" | "very" | "after"
        | "also" | "how" | "our" | "you" | "your" | "they" | "who" | "may" | "should"
        | "any" | "each" | "most" | "other" | "were" | "then" | "them" | "being"
        | "same" | "much" | "well" | "only" | "new" | "now" | "way" | "these"
        | "like" | "get" | "got" | "did" | "does" | "over" | "still" | "going"
    )
}
