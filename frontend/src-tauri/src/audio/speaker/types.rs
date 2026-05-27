use serde::{Deserialize, Serialize};
use std::fmt;

/// Validated speaker label.
///
/// Either an anonymous cluster label ("Speaker 1"), an unknown label
/// ("Unknown Speaker"), a suggestion ("Unknown Speaker (possibly Alice)"),
/// or a named label ("Alice").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpeakerLabel(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeakerLabelError {
    Empty,
    TooLong(usize),
    InvalidCharacters,
}

impl fmt::Display for SpeakerLabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "speaker label cannot be empty"),
            Self::TooLong(n) => write!(f, "speaker label too long: {} chars (max 200)", n),
            Self::InvalidCharacters => write!(f, "speaker label contains control characters"),
        }
    }
}

impl std::error::Error for SpeakerLabelError {}

impl SpeakerLabel {
    pub fn new(label: impl Into<String>) -> Result<Self, SpeakerLabelError> {
        let s = label.into();
        if s.trim().is_empty() {
            return Err(SpeakerLabelError::Empty);
        }
        if s.len() > 200 {
            return Err(SpeakerLabelError::TooLong(s.len()));
        }
        if s.contains(|c: char| c.is_control()) {
            return Err(SpeakerLabelError::InvalidCharacters);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn unknown() -> Self {
        Self("Unknown Speaker".to_string())
    }

    pub fn suggestion(name: &str) -> Result<Self, SpeakerLabelError> {
        Self::new(format!("Unknown Speaker (possibly {})", name))
    }

    pub fn cluster(index: u32) -> Self {
        Self(format!("Speaker {}", index))
    }

    pub fn is_unknown(&self) -> bool {
        self.0 == "Unknown Speaker" || self.0.starts_with("Unknown Speaker (possibly ")
    }

    pub fn is_cluster(&self) -> bool {
        self.0.starts_with("Speaker ") && self.0[8..].parse::<u32>().is_ok()
    }
}

impl fmt::Display for SpeakerLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for SpeakerLabel {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Named speaker profile with persistent color.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerProfile {
    pub id: String,
    pub name: String,
    pub color: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeakerProfileError {
    EmptyName,
    NameTooLong(usize),
}

impl fmt::Display for SpeakerProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(f, "speaker name cannot be empty"),
            Self::NameTooLong(n) => write!(f, "speaker name too long: {} chars (max 200)", n),
        }
    }
}

impl std::error::Error for SpeakerProfileError {}

impl SpeakerProfile {
    pub fn validate_name(name: &str) -> Result<(), SpeakerProfileError> {
        if name.trim().is_empty() {
            return Err(SpeakerProfileError::EmptyName);
        }
        if name.len() > 200 {
            return Err(SpeakerProfileError::NameTooLong(name.len()));
        }
        Ok(())
    }
}

/// Validated embedding vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingVector(pub Vec<f32>);

#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingError {
    WrongDimension { expected: usize, got: usize },
    NonFiniteValue(usize),
    Empty,
}

impl fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongDimension { expected, got } => {
                write!(f, "wrong embedding dimension: expected {}, got {}", expected, got)
            }
            Self::NonFiniteValue(idx) => {
                write!(f, "non-finite value at index {}", idx)
            }
            Self::Empty => write!(f, "embedding cannot be empty"),
        }
    }
}

impl std::error::Error for EmbeddingError {}

impl EmbeddingVector {
    pub fn from_slice(values: &[f32], expected_dim: usize) -> Result<Self, EmbeddingError> {
        if values.is_empty() {
            return Err(EmbeddingError::Empty);
        }
        if values.len() != expected_dim {
            return Err(EmbeddingError::WrongDimension {
                expected: expected_dim,
                got: values.len(),
            });
        }
        for (i, &v) in values.iter().enumerate() {
            if !v.is_finite() {
                return Err(EmbeddingError::NonFiniteValue(i));
            }
        }
        Ok(Self(values.to_vec()))
    }

    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }
}

/// Speaker segment from diarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub speaker_id: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task 1.1: SpeakerLabel rejects empty and malformed ─────────────

    #[test]
    fn speaker_label_rejects_empty_string() {
        assert!(matches!(SpeakerLabel::new(""), Err(SpeakerLabelError::Empty)));
    }

    #[test]
    fn speaker_label_rejects_only_whitespace() {
        let result = SpeakerLabel::new("   ");
        assert!(matches!(result, Err(SpeakerLabelError::Empty)));
    }

    #[test]
    fn speaker_label_rejects_control_characters() {
        assert!(matches!(
            SpeakerLabel::new("Alice\x00Bob"),
            Err(SpeakerLabelError::InvalidCharacters)
        ));
        assert!(matches!(
            SpeakerLabel::new("Alice\nBob"),
            Err(SpeakerLabelError::InvalidCharacters)
        ));
        assert!(matches!(
            SpeakerLabel::new("Alice\tBob"),
            Err(SpeakerLabelError::InvalidCharacters)
        ));
    }

    #[test]
    fn speaker_label_rejects_too_long() {
        let long = "A".repeat(201);
        assert!(matches!(SpeakerLabel::new(&long), Err(SpeakerLabelError::TooLong(201))));
    }

    #[test]
    fn speaker_label_accepts_cluster_label() {
        let label = SpeakerLabel::cluster(1);
        assert_eq!(label.as_str(), "Speaker 1");
        assert!(label.is_cluster());
    }

    #[test]
    fn speaker_label_accepts_unknown() {
        let label = SpeakerLabel::unknown();
        assert_eq!(label.as_str(), "Unknown Speaker");
        assert!(label.is_unknown());
    }

    #[test]
    fn speaker_label_accepts_suggestion() {
        let label = SpeakerLabel::suggestion("Alice").unwrap();
        assert_eq!(label.as_str(), "Unknown Speaker (possibly Alice)");
        assert!(label.is_unknown());
    }

    #[test]
    fn speaker_label_accepts_named() {
        let label = SpeakerLabel::new("Alice").unwrap();
        assert_eq!(label.as_str(), "Alice");
        assert!(!label.is_unknown());
        assert!(!label.is_cluster());
    }

    #[test]
    fn speaker_label_accepts_non_latin() {
        assert!(SpeakerLabel::new("José García").is_ok());
        assert!(SpeakerLabel::new("Карлос").is_ok());
        assert!(SpeakerLabel::new("田中太郎").is_ok());
    }

    // ── Task 1.3: SpeakerProfile rejects empty and too-long names ──────

    #[test]
    fn speaker_profile_rejects_empty_name() {
        assert!(matches!(
            SpeakerProfile::validate_name(""),
            Err(SpeakerProfileError::EmptyName)
        ));
        assert!(matches!(
            SpeakerProfile::validate_name("   "),
            Err(SpeakerProfileError::EmptyName)
        ));
    }

    #[test]
    fn speaker_profile_rejects_too_long_name() {
        let long = "A".repeat(201);
        assert!(matches!(
            SpeakerProfile::validate_name(&long),
            Err(SpeakerProfileError::NameTooLong(201))
        ));
    }

    #[test]
    fn speaker_profile_accepts_normal_name() {
        assert!(SpeakerProfile::validate_name("Alice").is_ok());
    }

    #[test]
    fn speaker_profile_accepts_name_at_limit() {
        let exactly_200 = "A".repeat(200);
        assert!(SpeakerProfile::validate_name(&exactly_200).is_ok());
    }

    // ── Task 1.5: EmbeddingVector rejects wrong dimension and NaN/Inf ──

    #[test]
    fn embedding_rejects_wrong_dimension() {
        let values = vec![0.1f32; 128];
        assert!(matches!(
            EmbeddingVector::from_slice(&values, 256),
            Err(EmbeddingError::WrongDimension { expected: 256, got: 128 })
        ));
    }

    #[test]
    fn embedding_rejects_nan() {
        let mut values = vec![0.1f32; 256];
        values[42] = f32::NAN;
        assert!(matches!(
            EmbeddingVector::from_slice(&values, 256),
            Err(EmbeddingError::NonFiniteValue(42))
        ));
    }

    #[test]
    fn embedding_rejects_infinity() {
        let mut values = vec![0.1f32; 256];
        values[99] = f32::INFINITY;
        assert!(matches!(
            EmbeddingVector::from_slice(&values, 256),
            Err(EmbeddingError::NonFiniteValue(99))
        ));
    }

    #[test]
    fn embedding_rejects_neg_infinity() {
        let mut values = vec![0.1f32; 256];
        values[0] = f32::NEG_INFINITY;
        assert!(matches!(
            EmbeddingVector::from_slice(&values, 256),
            Err(EmbeddingError::NonFiniteValue(0))
        ));
    }

    #[test]
    fn embedding_rejects_empty() {
        assert!(matches!(
            EmbeddingVector::from_slice(&[], 256),
            Err(EmbeddingError::Empty)
        ));
    }

    #[test]
    fn embedding_accepts_valid() {
        let values = vec![0.5f32; 256];
        let emb = EmbeddingVector::from_slice(&values, 256).unwrap();
        assert_eq!(emb.dim(), 256);
        assert_eq!(emb.as_slice().len(), 256);
    }
}
