use super::*;
    use serde_json::json;

    fn run(metadata: serde_json::Value) -> NftFeatures {
        extract_features("00000000-0000-0000-0000-000000000001", "0xdeadbeef", 1, "art", &metadata, 0.0)
    }

    #[test]
    fn output_ids_match_input() {
        let features = extract_features("00000000-0000-0000-0000-000000000042", "0xAbCdEf", 7, "snap", &json!({}), 0.0);
        assert_eq!(features.nft_id, "00000000-0000-0000-0000-000000000042");
        assert_eq!(features.token_id, 7);
        assert_eq!(features.contract_address, "0xabcdef");
    }

    #[test]
    fn quality_score_from_creator() {
        let score = 0.75_f32;
        let features = extract_features("00000000-0000-0000-0000-000000000002", "0xdeadbeef", 1, "art", &json!({}), score);
        assert!((features.quality_score - score).abs() < f32::EPSILON);
    }

    #[test]
    fn style_extracted_from_name() {
        let features = run(json!({ "name": "An abstract composition" }));
        assert_eq!(features.style, Some("abstract".to_string()));
        assert!(features.tags.contains(&"abstract".to_string()));
    }

    #[test]
    fn genre_is_not_guessed_from_free_text() {
        // The naive 20-keyword substring guesser (MUSIC_GENRES) was removed as
        // dead weight — genre is never inferred from name/description text.
        // The real genre signal now comes from an explicit attribute (see
        // `genre_extracted_from_attribute`) or, at ingestion time, from the
        // Elixir `nfts.genre`/`genres[]` columns folded into `tags` (see
        // `event_processor::elixir_db::process_enrichment`).
        let features = run(json!({ "description": "A smooth jazz inspired piece" }));
        assert_eq!(features.genre, None);
        assert!(!features.tags.contains(&"jazz".to_string()));
    }

    #[test]
    fn genre_extracted_from_attribute() {
        let features = run(json!({
            "attributes": [{ "trait_type": "Genre", "value": "Deep House" }]
        }));
        assert_eq!(features.genre, Some("deep house".to_string()));
        assert!(features.tags.contains(&"deep house".to_string()));
    }

    #[test]
    fn mood_extracted() {
        let features = run(json!({ "name": "A peaceful mountain scene" }));
        assert_eq!(features.mood, Some("peaceful".to_string()));
        assert!(features.tags.contains(&"peaceful".to_string()));
    }

    #[test]
    fn color_extracted() {
        let features = run(json!({ "description": "A vivid blue sky painting" }));
        assert_eq!(features.primary_color, Some("blue".to_string()));
        assert!(features.tags.contains(&"blue".to_string()));
    }

    #[test]
    fn attributes_extracted_as_tags() {
        let features = run(json!({
            "attributes": [
                { "trait_type": "Rarity",  "value": "Legendary" },
                { "trait_type": "Element", "value": "Fire" }
            ]
        }));
        assert!(features.tags.contains(&"legendary".to_string()));
        assert!(features.tags.contains(&"fire".to_string()));
    }

    #[test]
    fn empty_metadata_no_features() {
        let features = run(json!({}));
        assert!(features.style.is_none());
        assert!(features.genre.is_none());
        assert!(features.mood.is_none());
        assert!(features.primary_color.is_none());
        assert_eq!(features.tags, vec!["art".to_string()]);
        assert_eq!(features.engagement_score, 0.0);
        assert_eq!(features.trending_score, 0.0);
    }

    #[test]
    fn tags_are_lowercase() {
        let features = run(json!({ "tags": ["Nature", "OCEAN", "Sunset"] }));
        for tag in &features.tags {
            assert_eq!(*tag, tag.to_lowercase(), "tag '{tag}' should be lowercase");
        }
        assert!(features.tags.contains(&"nature".to_string()));
        assert!(features.tags.contains(&"ocean".to_string()));
        assert!(features.tags.contains(&"sunset".to_string()));
    }

    #[test]
    fn attribute_style_trait() {
        let features = run(json!({
            "attributes": [{ "trait_type": "Style", "value": "Impressionist" }]
        }));
        assert_eq!(features.style, Some("impressionist".to_string()));
        assert!(features.tags.contains(&"impressionist".to_string()));
    }
