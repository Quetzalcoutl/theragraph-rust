use serde::{Deserialize, Serialize};

use super::UserPreferences;

// ── Genesis eureka seeding ────────────────────────────────────────────────────

/// Full eureka payload from the DarkChamber 20-question ritual.
/// Raw Q&A text is stripped client-side before this reaches the server —
/// only extracted topics and confidence scores are present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EurekaPayload {
    pub eureka: EurekaTraits,
    pub profile: EurekaProfile,
    #[serde(default)]
    pub wallet_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EurekaTraits {
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub curiosity_vector: Vec<String>,
    #[serde(default)]
    pub aesthetic: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub emotional_bias: Vec<String>,
    #[serde(default)]
    pub moral_weight: std::collections::HashMap<String, f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EurekaProfile {
    #[serde(default)]
    pub selected_boards: Vec<String>,
    #[serde(default)]
    pub focus_topics: Vec<String>,
    #[serde(default)]
    pub genesis_moments: Vec<GenesisMoment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisMoment {
    pub resonance: f32,
    #[serde(default)]
    pub topics_extracted: Vec<String>,
}

/// Static value → content-tag mapping.
/// Maps core human values to the content tags that signal alignment with them.
static VALUE_TAG_MAP: std::sync::LazyLock<std::collections::HashMap<&'static str, Vec<(&'static str, f32)>>> =
    std::sync::LazyLock::new(|| {
        let mut m = std::collections::HashMap::with_capacity(10);
        m.insert("justice",      vec![("social_activism", 0.75), ("documentary", 0.65), ("protest_art", 0.70)]);
        m.insert("creativity",   vec![("experimental", 0.80), ("generative", 0.75), ("abstract", 0.70)]);
        m.insert("authenticity", vec![("raw", 0.75), ("street_photography", 0.70), ("documentary", 0.65)]);
        m.insert("freedom",      vec![("street_art", 0.75), ("improvisation", 0.70), ("underground", 0.65)]);
        m.insert("community",    vec![("collective", 0.75), ("collaboration", 0.70), ("portrait", 0.60)]);
        m.insert("beauty",       vec![("fine_art", 0.80), ("aesthetic", 0.75), ("photography", 0.65)]);
        m.insert("knowledge",    vec![("documentary", 0.75), ("education", 0.70), ("research_art", 0.65)]);
        m.insert("nature",       vec![("landscape", 0.80), ("nature", 0.80), ("environmental", 0.70)]);
        m.insert("love",         vec![("portrait", 0.75), ("intimacy", 0.70), ("family", 0.65)]);
        m.insert("rebellion",    vec![("punk", 0.80), ("underground", 0.75), ("protest_art", 0.70)]);
        m
    });

/// Static aesthetic → tag mapping.
static AESTHETIC_TAG_MAP: std::sync::LazyLock<std::collections::HashMap<&'static str, Vec<(&'static str, f32)>>> =
    std::sync::LazyLock::new(|| {
        let mut m = std::collections::HashMap::with_capacity(10);
        m.insert("minimalist",    vec![("minimalism", 0.75), ("clean_design", 0.70), ("negative_space", 0.65)]);
        m.insert("maximalist",    vec![("maximalism", 0.75), ("baroque", 0.70), ("ornate", 0.65)]);
        m.insert("analog",        vec![("film_photography", 0.80), ("analog", 0.80), ("grain", 0.70)]);
        m.insert("digital",       vec![("digital_art", 0.75), ("glitch", 0.65), ("generative", 0.70)]);
        m.insert("dark",          vec![("dark_art", 0.75), ("noir", 0.70), ("dramatic", 0.65)]);
        m.insert("vibrant",       vec![("colorful", 0.75), ("pop_art", 0.70), ("saturated", 0.65)]);
        m.insert("surreal",       vec![("surreal", 0.80), ("dreamlike", 0.75), ("fantasy", 0.65)]);
        m.insert("raw",           vec![("raw", 0.80), ("unfiltered", 0.70), ("street_photography", 0.65)]);
        m.insert("retro",         vec![("vintage", 0.75), ("nostalgia", 0.70), ("retro", 0.70)]);
        m.insert("futuristic",    vec![("futurism", 0.75), ("sci_fi", 0.70), ("cyberpunk", 0.65)]);
        m
    });

/// Seed user preferences from a full DarkChamber eureka payload.
/// Higher fidelity than preset seeding — all eureka dimensions are used.
/// Never overwrites preferences above the neutral threshold (0.5) to avoid
/// overwriting real interaction data accumulated before genesis completes.
pub(crate) fn apply_genesis_eureka_seeds(prefs: &mut UserPreferences, payload: &EurekaPayload) -> GenesisResult {
    let mut tags_seeded: usize = 0;
    let mut creators_seeded: usize = 0;
    let mut alignment_watch = false;

    // 1. Values → tag preferences
    for value in &payload.eureka.values {
        if let Some(tag_seeds) = VALUE_TAG_MAP.get(value.to_lowercase().as_str()) {
            for (tag, seed) in tag_seeds {
                let current = prefs.tag_preferences.get(*tag).copied().unwrap_or(0.5);
                if current <= 0.5 {
                    prefs.tag_preferences.insert((*tag).into(), *seed);
                    tags_seeded += 1;
                }
            }
        }
    }

    // 2. Aesthetic → tag preferences
    for (aesthetic_key, _aesthetic_val) in &payload.eureka.aesthetic {
        if let Some(tag_seeds) = AESTHETIC_TAG_MAP.get(aesthetic_key.to_lowercase().as_str()) {
            for (tag, seed) in tag_seeds {
                let current = prefs.tag_preferences.get(*tag).copied().unwrap_or(0.5);
                if current <= 0.5 {
                    prefs.tag_preferences.insert((*tag).into(), *seed);
                    tags_seeded += 1;
                }
            }
        }
    }

    // 3. Curiosity vector → discovery-boosted tag preferences (weight ×0.85 for novelty)
    for topic in &payload.eureka.curiosity_vector {
        let tag = topic.to_lowercase().replace(' ', "_");
        let current = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
        if current <= 0.5 {
            prefs.tag_preferences.insert(tag.into(), 0.72);
            tags_seeded += 1;
        }
    }

    // 4. Emotional bias → sentiment-adjacent preferences (secondary weight ×0.6)
    for emotion in &payload.eureka.emotional_bias {
        let tag = format!("{}_art", emotion.to_lowercase().replace(' ', "_"));
        let current = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
        if current <= 0.5 {
            prefs.tag_preferences.insert(tag.into(), 0.62);
            tags_seeded += 1;
        }
    }

    // 5. Moral weight — authority > 0.7 flags alignment_level = 'watch' at genesis
    if let Some(&authority) = payload.eureka.moral_weight.get("authority") {
        if authority > 0.7 {
            alignment_watch = true;
        }
    }

    // 6. Focus topics → direct tag_preferences at 0.70
    for topic in &payload.profile.focus_topics {
        let tag = topic.to_lowercase().replace(' ', "_");
        let current = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
        if current <= 0.5 {
            prefs.tag_preferences.insert(tag.into(), 0.70);
            tags_seeded += 1;
        }
    }

    // 7. Selected boards → creator_preferences seed
    for board_addr in &payload.profile.selected_boards {
        let current = prefs.creator_preferences.get(board_addr.as_str()).copied().unwrap_or(0.5);
        if current <= 0.5 {
            prefs.creator_preferences.insert(board_addr.as_str().into(), 0.72);
            creators_seeded += 1;
        }
    }

    // 8. Genesis moments → topic seeds weighted by resonance
    for moment in &payload.profile.genesis_moments {
        let weight = (0.55 + moment.resonance * 0.20).clamp(0.55, 0.75);
        for topic in &moment.topics_extracted {
            let tag = topic.to_lowercase().replace(' ', "_");
            let current = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
            if current <= 0.5 {
                prefs.tag_preferences.insert(tag.into(), weight);
                tags_seeded += 1;
            }
        }
    }

    GenesisResult {
        tags_seeded,
        creators_seeded,
        alignment_watch,
    }
}

#[derive(Debug, Serialize)]
pub struct GenesisResult {
    pub tags_seeded: usize,
    pub creators_seeded: usize,
    pub alignment_watch: bool,
}
