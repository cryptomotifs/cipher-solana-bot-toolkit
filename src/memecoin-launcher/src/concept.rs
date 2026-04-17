//! Token concept generator — template-based name/symbol/description.
//!
//! [VERIFIED 2026] narrative_detection_twitter_2026.md s1: "3-7 chars, memorable"
//! [VERIFIED 2026] narrative_detection_twitter_2026.md s5: AI name generation patterns

use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::narrative::NarrativeSignal;
use crate::tracker::LauncherPnL;

/// A generated token concept ready for launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConcept {
    pub name: String,
    pub symbol: String,
    pub description: String,
    pub narrative_category: String,
}

/// Name templates per category.
/// [VERIFIED 2026] narrative_detection_twitter_2026.md s1: naming patterns
const SUFFIXES: &[&str] = &[
    "INU", "COIN", "MOON", "PEPE", "DOGE", "CAT", "AI", "PUMP",
    "SOL", "BONK", "WIF", "HAT", "KING", "GOD", "MEME", "CHAD",
    "FROG", "APE", "BULL", "BEAR", "RICH", "GEM", "SEND", "YOLO",
];

const PREFIXES: &[&str] = &[
    "BABY", "MEGA", "SUPER", "DARK", "BASED", "DANK", "TURBO",
    "GIGA", "ULTRA", "HYPER", "SMOL", "BIG", "LIL", "EPIC",
];

const DESCRIPTION_TEMPLATES: &[&str] = &[
    "The official {} memecoin on Solana. Community-driven. No rug. Pure vibes.",
    "{} just hit different. The memecoin for those who know.",
    "When {} meets crypto, legends are born. {} is that legend.",
    "{} — the people's memecoin. By degens, for degens.",
    "You missed BONK. You missed WIF. Don't miss {}.",
    "{} is not just a token, it's a movement. Solana's next 100x.",
    "They said {} couldn't be a coin. We said hold my SOL.",
    "{} — because the best memes deserve their own token.",
    "The {} community is unstoppable. Join before it's too late.",
    "Born from the memes, built on Solana. {} to the moon.",
];

/// Generate a token concept from a narrative signal.
pub fn generate_concept(
    signal: &NarrativeSignal,
    tracker: &LauncherPnL,
) -> Result<TokenConcept> {
    let mut rng = rand::thread_rng();
    let topic = signal.topic.to_uppercase();
    let topic_clean: String = topic.chars().filter(|c| c.is_alphanumeric()).collect();

    // Try different name patterns until we find one not recently used
    let mut attempts = 0;
    let (name, symbol) = loop {
        attempts += 1;
        if attempts > 50 {
            // Fallback: random name
            let suffix = SUFFIXES[rng.gen_range(0..SUFFIXES.len())];
            let name = format!("{}{}", &topic_clean[..topic_clean.len().min(4)], suffix);
            let symbol = name[..name.len().min(6)].to_string();
            break (name, symbol);
        }

        let pattern = rng.gen_range(0..5);
        let name = match pattern {
            0 => {
                // TOPIC + suffix: "TRUMPINU", "AIKING"
                let suffix = SUFFIXES[rng.gen_range(0..SUFFIXES.len())];
                format!("{}{}", &topic_clean[..topic_clean.len().min(5)], suffix)
            }
            1 => {
                // prefix + TOPIC: "BABYDOGE", "MEGAAI"
                let prefix = PREFIXES[rng.gen_range(0..PREFIXES.len())];
                format!("{}{}", prefix, &topic_clean[..topic_clean.len().min(4)])
            }
            2 => {
                // $TOPIC: just the topic word
                topic_clean[..topic_clean.len().min(7)].to_string()
            }
            3 => {
                // TOPIC + random number
                format!("{}{}", &topic_clean[..topic_clean.len().min(5)], rng.gen_range(1..100))
            }
            _ => {
                // Double suffix: "DOGECAT", "PEPEFROG"
                let s1 = SUFFIXES[rng.gen_range(0..SUFFIXES.len())];
                let s2 = SUFFIXES[rng.gen_range(0..SUFFIXES.len())];
                format!("{}{}", s1, s2)
            }
        };

        // Ensure valid length (3-10 chars for symbol, 3-32 for name)
        if name.len() < 3 || name.len() > 10 {
            continue;
        }

        // Check dedup (not used in last 30 days)
        if tracker.name_recently_used(&name, 30) {
            continue;
        }

        let symbol = name[..name.len().min(6)].to_string();
        break (name, symbol);
    };

    // Generate description
    let template = DESCRIPTION_TEMPLATES[rng.gen_range(0..DESCRIPTION_TEMPLATES.len())];
    let description = template
        .replacen("{}", &name, 1)
        .replacen("{}", &name, 1); // Replace up to 2 occurrences

    Ok(TokenConcept {
        name,
        symbol,
        description,
        narrative_category: signal.category.clone(),
    })
}
