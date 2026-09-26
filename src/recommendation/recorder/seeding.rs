//! Onboarding and cold-start seeding operations for user preferences.
//!
//! These functions are called once (or rarely) per user: at onboarding to populate
//! initial preferences, and from AiFriendz to apply AI-derived tag signals.
//! They are separate from the per-interaction recording loop in [`super`].

use anyhow::Result;
use sqlx::PgPool;
use tracing::info;

use crate::recommendation::cache::RecCache;
use crate::recommendation::model::{
    apply_genesis_eureka_seeds, apply_preset_seeds, EurekaPayload, GenesisResult,
};

/// Seed initial preferences from onboarding preset selections.
///
/// Only writes values where the existing score is still at the neutral default (≤ 0.5),
/// so real interaction data accumulated before onboarding completes is never overwritten.
///
/// Idempotent: calling twice with the same presets is a no-op for any key that was
/// elevated by the first call (because 0.85 > 0.5 → guard blocks the second write).
///
/// FIX seed-prefs-race: The original code called `get_or_create_preferences` (plain
/// SELECT, no lock) and then wrote back via `save_preferences`.  A concurrent first
/// interaction racing the onboarding seed could read the same unfilled row, apply its
/// own field updates, and the last writer would silently discard the other's changes.
///
/// This function now opens a transaction and uses `load_or_insert_prefs_for_update`
/// (SELECT … FOR UPDATE) — the same pattern used by `update_preferences_from_interaction`
/// — so the seed and any concurrent interaction are serialized at the database level.
///
/// FIX seed-prefs-cache-stale: After a successful seed the Redis prefs key is
/// explicitly deleted so the next feed request reads the freshly seeded weights
/// rather than the unseeded snapshot that was cached before onboarding ran.
pub async fn seed_from_presets(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
    presets: &[String],
) -> Result<()> {
    let normalized = user_address.to_lowercase();
    let mut tx = pool.begin().await?;

    // FIX seed-prefs-race: acquire a FOR UPDATE lock so concurrent interactions
    // are serialized; never reads a stale in-memory default.
    let mut prefs = super::load_or_insert_prefs_for_update(&mut tx, &normalized).await?;

    for preset in presets {
        apply_preset_seeds(&mut prefs, preset.as_str());
    }

    super::save_preferences(&mut *tx, &prefs).await?;
    tx.commit().await?;

    // C2: seed initial creator_preferences from top creators per preset type.
    // Only runs when creator_preferences is empty (never overwrites learned values).
    // Best-effort: failure here must not fail the seed — creator signal is a bonus.
    if prefs.creator_preferences.is_empty() {
        let contract_types: Vec<&str> = presets.iter().flat_map(|p| match p.as_str() {
            "art_lover"    => vec!["art"],
            "music_fan"    => vec!["music"],
            "movie_buff"   => vec!["flix"],
            "snap_creator" => vec!["snap"],
            "collector"    => vec!["art", "snap", "flix", "music"],
            _              => vec![],
        }).collect();

        let mut creator_seeds: std::collections::HashMap<String, f32> = Default::default();
        for ctype in contract_types {
            if let Ok(creators) = top_creators_for_type(pool, ctype, 3).await {
                for (i, addr) in creators.into_iter().enumerate() {
                    // Descending initial weights: 0.65, 0.60, 0.55
                    let w = 0.65_f32 - (i as f32 * 0.05);
                    creator_seeds.entry(addr).or_insert(w);
                }
            }
        }

        if !creator_seeds.is_empty() {
            let creator_json = serde_json::to_value(&creator_seeds)?;
            sqlx::query(
                r#"
                UPDATE user_preferences
                SET creator_preferences = $1, updated_at = NOW()
                WHERE user_address = $2
                  AND (creator_preferences IS NULL OR creator_preferences = '{}'::jsonb)
                "#,
            )
            .bind(&creator_json)
            .bind(&normalized)
            .execute(pool)
            .await?;

            if let Some(c) = cache {
                c.delete_user_prefs(&normalized).await;
            }
        }
    }

    // FIX seed-prefs-cache-stale: evict the unseeded snapshot so the next feed
    // request loads the freshly written weights from DB.
    if let Some(c) = cache {
        c.delete_user_prefs(&normalized).await;
        c.delete_recommendations(&normalized).await;
    }

    info!("🌱 Seeded onboarding preferences for {} ({} presets)", user_address, presets.len());
    Ok(())
}

/// Seed initial preferences from a DarkChamber eureka payload.
///
/// Full-fidelity alternative to `seed_from_presets` — all eureka dimensions
/// (values, aesthetic, curiosity_vector, emotional_bias, focus_topics,
/// selected_boards, genesis_moments) are applied to tag_preferences and
/// creator_preferences at their respective weights.
///
/// Returns `GenesisResult` so the Elixir caller can update `alignment_level`
/// and flag `pending_aischool_trigger` on `social_users`.
pub async fn seed_from_genesis_eureka(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
    payload: EurekaPayload,
) -> Result<GenesisResult> {
    let normalized = user_address.to_lowercase();
    let mut tx = pool.begin().await?;

    let mut prefs = super::load_or_insert_prefs_for_update(&mut tx, &normalized).await?;

    let result = apply_genesis_eureka_seeds(&mut prefs, &payload);

    super::save_preferences(&mut *tx, &prefs).await?;
    tx.commit().await?;

    if let Some(c) = cache {
        c.delete_user_prefs(&normalized).await;
        c.delete_recommendations(&normalized).await;
    }

    info!(
        "🌱 Genesis-seeded preferences for {} ({} tags, {} creators, watch={})",
        user_address, result.tags_seeded, result.creators_seeded, result.alignment_watch
    );

    Ok(result)
}

/// Write tag preference deltas from AiFriendz daily digest.
///
/// Applies a map of tag → weight updates, skipping any tag that already has
/// a weight above neutral (0.5 + dead_band) unless the new weight is higher.
/// Called by the Elixir AiSignals context on behalf of IngestSignalsWorker.
/// Common freeform-conversation variants of the controlled NFT tag vocabulary
/// (see `feature_extractor`'s ART_STYLES/MOOD_KEYWORDS/NATURE_TAGS/
/// COLOR_KEYWORDS, plus the genre slug vocabulary folded into `tags` at
/// ingestion — see `event_processor::elixir_db::process_enrichment`) mapped
/// to the canonical tag scoring matches
/// against. AiFriendz topic strings come from free-text conversation
/// extraction; NFT tags come from that fixed, curated vocabulary — without
/// this, a topic-affinity delta for e.g. "photo" is written under a key
/// (`"photo"`) that `compute_feature_scores`'s exact-string match against
/// `"photography"` will never see. Conservative on purpose: only plural/
/// singular, alternate-spelling, and compound-word variants of vocabulary
/// terms that actually exist downstream — no semantic guessing (e.g. NOT
/// mapping "hope" → "hopeful", too ambiguous a jump for a single word).
const TAG_SYNONYMS: &[(&str, &str)] = &[
    // Art styles
    ("photo", "photography"), ("photograph", "photography"), ("photographs", "photography"),
    ("photographic", "photography"), ("surrealism", "surreal"), ("minimalism", "minimalist"),
    ("maximalism", "maximalist"), ("impressionism", "impressionist"), ("expressionism", "expressionist"),
    ("popart", "pop art"), ("3-d", "3d"), ("three-d", "3d"), ("threed", "3d"),
    ("handdrawn", "hand-drawn"), ("hand drawn", "hand-drawn"),
    ("artificial intelligence", "ai"), ("generative art", "generative"),
    // Music genres
    ("hiphop", "hip-hop"), ("hip hop", "hip-hop"), ("rnb", "r&b"), ("r and b", "r&b"),
    ("rhythm and blues", "r&b"), ("lofi", "lo-fi"), ("lo fi", "lo-fi"), ("dub step", "dubstep"),
    // Moods
    ("melancholy", "melancholic"), ("peace", "peaceful"), ("mystery", "mysterious"),
    ("romance", "romantic"), ("nostalgia", "nostalgic"), ("dreamlike", "dreamy"),
    ("relax", "relaxing"), ("relaxed", "relaxing"),
    // Nature
    ("oceans", "ocean"), ("mountains", "mountain"), ("forests", "forest"), ("flower", "flowers"),
    ("animal", "animals"), ("beaches", "beach"), ("rivers", "river"), ("waterfalls", "waterfall"),
    ("tree", "trees"), ("gardens", "garden"), ("sunsets", "sunset"), ("sunrises", "sunrise"),
    // Colors
    ("colourful", "colorful"), ("vibrance", "vibrant"), ("neons", "neon"), ("pastels", "pastel"),
    ("golden", "gold"), ("silvery", "silver"),
];

/// Normalize a freeform topic string to its canonical NFT-tag-vocabulary form
/// via `TAG_SYNONYMS`, or return it unchanged (lowercased/trimmed) if it isn't
/// a known variant — an unmapped topic still gets written, it just won't
/// match any NFT tag in scoring until a mapping is added for it.
fn normalize_topic_tag(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    TAG_SYNONYMS
        .iter()
        .find(|(variant, _)| *variant == lower)
        .map(|(_, canonical)| canonical.to_string())
        .unwrap_or(lower)
}

pub async fn write_tag_preferences_delta(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
    tag_map: std::collections::HashMap<String, f32>,
) -> Result<()> {
    let normalized = user_address.to_lowercase();
    let mut tx = pool.begin().await?;

    let mut prefs = super::load_or_insert_prefs_for_update(&mut tx, &normalized).await?;

    const DEAD_BAND: f32 = 0.15;
    let mut updated = 0usize;

    for (tag, weight) in &tag_map {
        let tag = normalize_topic_tag(tag);
        let weight_clamped = weight.clamp(0.0, 1.0);
        let current = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.0);
        // Only write if delta exceeds dead-band or there's no existing entry
        if (weight_clamped - current).abs() > DEAD_BAND || current == 0.0 {
            prefs.tag_preferences.insert(tag.as_str().into(), weight_clamped);
            updated += 1;
        }
    }

    if updated > 0 {
        super::save_preferences(&mut *tx, &prefs).await?;
        tx.commit().await?;

        if let Some(c) = cache {
            c.delete_user_prefs(&normalized).await;
        }

        info!("📡 AiSignals: wrote {} tag preference deltas for {}", updated, user_address);
    } else {
        tx.rollback().await?;
    }

    Ok(())
}

/// Atomically accumulate a creator affinity signal from a view event into
/// `creator_preferences` in Postgres.
///
/// Called fire-and-forget from `dispatch_graph_interaction` so view signals
/// reach the recommendation scorer without requiring a like or purchase.
/// Uses a JSONB atomic merge: current + delta, clamped to [0, 1].
///
/// `dur_secs < 3` → no-op (bounce views produce no signal).
pub async fn sync_creator_affinity_to_prefs(
    pool: &PgPool,
    user_address: &str,
    creator_address: &str,
    dur_secs: u32,
) -> Result<()> {
    if dur_secs < 3 {
        return Ok(());
    }
    // Delta per view: max 0.15 (saturates at ~5 min). Accumulates across views
    // so a user who watches 5 × 3-minute videos scores ~0.60 on that creator.
    let delta = 0.15_f64 * (1.0 - (-(dur_secs as f64) / 300.0).exp());
    let user = user_address.to_lowercase();
    let creator = creator_address.to_lowercase();

    sqlx::query(
        r#"
        UPDATE user_preferences
        SET
            creator_preferences = jsonb_set(
                COALESCE(creator_preferences, '{}'::jsonb),
                ARRAY[$1],
                to_jsonb(LEAST(1.0::float8,
                    COALESCE((creator_preferences ->> $1)::float8, 0.0) + $2)),
                true
            ),
            updated_at = NOW()
        WHERE user_address = $3
        "#,
    )
    .bind(&creator)
    .bind(delta)
    .bind(&user)
    .execute(pool)
    .await?;

    Ok(())
}

/// Query the top-N creators by engagement for a given contract type.
/// Used during cold-start seeding so new users get creator signal on first load.
async fn top_creators_for_type(
    pool: &PgPool,
    contract_type: &str,
    limit: i32,
) -> Result<Vec<String>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        creator_address: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"
        SELECT creator_address,
               SUM(likes_count + buys_count * 3) AS score
        FROM nfts
        WHERE contract_type = $1
          AND is_blocked = false
          AND is_deleted = false
          AND creator_address IS NOT NULL
          AND creator_address <> ''
        GROUP BY creator_address
        ORDER BY score DESC
        LIMIT $2
        "#,
    )
    .bind(contract_type)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.creator_address.to_lowercase()).collect())
}

#[cfg(test)]
mod tests {
    use super::normalize_topic_tag;

    #[test]
    fn maps_known_synonyms_to_canonical_vocabulary() {
        assert_eq!(normalize_topic_tag("photo"), "photography");
        assert_eq!(normalize_topic_tag("hiphop"), "hip-hop");
        assert_eq!(normalize_topic_tag("rnb"), "r&b");
        assert_eq!(normalize_topic_tag("melancholy"), "melancholic");
        assert_eq!(normalize_topic_tag("oceans"), "ocean");
        assert_eq!(normalize_topic_tag("colourful"), "colorful");
    }

    #[test]
    fn is_case_and_whitespace_insensitive() {
        assert_eq!(normalize_topic_tag("  Photo  "), "photography");
        assert_eq!(normalize_topic_tag("HIPHOP"), "hip-hop");
    }

    #[test]
    fn leaves_an_already_canonical_tag_unchanged() {
        assert_eq!(normalize_topic_tag("photography"), "photography");
        assert_eq!(normalize_topic_tag("surreal"), "surreal");
    }

    #[test]
    fn leaves_an_unknown_tag_unchanged_except_for_lowercasing_and_trim() {
        assert_eq!(normalize_topic_tag("  Quantum Doodles  "), "quantum doodles");
    }
}
