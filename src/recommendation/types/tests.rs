use super::*;
    use super::ContentType;
    use serde_json;

    #[test]
    fn from_str_roundtrips_all_variants() {
        for (input, expected) in &[
            ("snap",  ContentType::Snap),
            ("art",   ContentType::Art),
            ("music", ContentType::Music),
            ("flix",  ContentType::Flix),
        ] {
            let parsed = ContentType::from_str(input)
                .unwrap_or_else(|| panic!("from_str({input:?}) returned None"));
            assert_eq!(&parsed, expected, "variant mismatch for {input:?}");
            assert_eq!(parsed.as_str(), *input, "as_str roundtrip failed for {input:?}");
        }
    }

    #[test]
    fn from_str_is_case_insensitive() {
        assert_eq!(ContentType::from_str("SNAP"),  Some(ContentType::Snap));
        assert_eq!(ContentType::from_str("Art"),   Some(ContentType::Art));
        assert_eq!(ContentType::from_str("MUSIC"), Some(ContentType::Music));
        assert_eq!(ContentType::from_str("FLiX"),  Some(ContentType::Flix));
    }

    #[test]
    fn from_str_returns_none_for_unknown() {
        assert_eq!(ContentType::from_str("video"), None);
        assert_eq!(ContentType::from_str(""),      None);
        assert_eq!(ContentType::from_str("nft"),   None);
    }

    #[test]
    fn as_str_returns_canonical_lowercase() {
        assert_eq!(ContentType::Snap.as_str(),  "snap");
        assert_eq!(ContentType::Art.as_str(),   "art");
        assert_eq!(ContentType::Music.as_str(), "music");
        assert_eq!(ContentType::Flix.as_str(),  "flix");
    }

    /// Every value produced by `as_str` must be parseable back to the same variant.
    /// This guards against `as_str` and `from_str` drifting out of sync.
    #[test]
    fn as_str_is_stable_roundtrip_source() {
        let variants = [
            ContentType::Snap,
            ContentType::Art,
            ContentType::Music,
            ContentType::Flix,
        ];
        for variant in &variants {
            let s = variant.as_str();
            let reparsed = ContentType::from_str(s)
                .unwrap_or_else(|| panic!("from_str({s:?}) returned None — as_str produced a value that cannot be parsed back"));
            assert_eq!(
                &reparsed, variant,
                "as_str/from_str mismatch: as_str produced {s:?} but from_str mapped it to a different variant"
            );
        }
    }

    #[test]
    fn content_type_serde_roundtrip() {
        for variant in &[ContentType::Snap, ContentType::Art, ContentType::Music, ContentType::Flix] {
            let json = serde_json::to_string(variant).expect("serialize");
            let back: ContentType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&back, variant, "serde roundtrip failed for {variant:?}");
        }
    }

    #[test]
    fn content_type_deserializes_from_legacy_string() {
        let back: ContentType = serde_json::from_str("\"snap\"").expect("deserialize snap");
        assert_eq!(back, ContentType::Snap);
        let back: ContentType = serde_json::from_str("\"MUSIC\"").expect("deserialize MUSIC");
        assert_eq!(back, ContentType::Music);
    }
