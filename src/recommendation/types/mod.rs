//! Shared domain types for the recommendation module.

/// The four content categories supported by TheraGraph NFTs.
///
/// Use [`ContentType::from_str`] to parse an incoming `contract_type` string
/// (case-insensitive).  Use [`ContentType::as_str`] when you need to pass the
/// canonical lowercase name back to SQL or to a serde payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Snap,
    Art,
    Music,
    Flix,
}

impl ContentType {
    /// Parse a `contract_type` string (case-insensitive).
    ///
    /// Fast path: byte-match for lowercase (the invariant at runtime — all
    /// contract_type values are pre-normalised to lowercase at ingest). Falls
    /// through to an allocating `to_lowercase()` only for unexpected casing,
    /// which in practice never occurs on the hot scoring path.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.as_bytes() {
            b"snap"  => Some(Self::Snap),
            b"art"   => Some(Self::Art),
            b"music" => Some(Self::Music),
            b"flix"  => Some(Self::Flix),
            _ => match s.to_lowercase().as_str() {
                "snap"  => Some(Self::Snap),
                "art"   => Some(Self::Art),
                "music" => Some(Self::Music),
                "flix"  => Some(Self::Flix),
                _       => None,
            }
        }
    }

    /// The canonical lowercase string representation used in SQL and API payloads.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Snap  => "snap",
            Self::Art   => "art",
            Self::Music => "music",
            Self::Flix  => "flix",
        }
    }
}

impl serde::Serialize for ContentType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ContentType {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        ContentType::from_str(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown content type: {raw}")))
    }
}


#[cfg(test)]
mod tests;
