use crate::audio::speaker::alignment::TokenWord;

/// Extract per-token word timestamps from a Whisper state's segments.
/// Returns a JSON string of TokenWord array, or None if no timestamps available.
pub fn extract_token_timestamps(
    state: &whisper_rs::WhisperState,
    num_segments: i32,
) -> Option<String> {
    let mut words = Vec::new();

    for seg_idx in 0..num_segments {
        let segment = match state.get_segment(seg_idx) {
            Some(s) => s,
            None => continue,
        };

        let n_tokens = segment.n_tokens();
        for tok_idx in 0..n_tokens {
            let token = match segment.get_token(tok_idx) {
                Some(t) => t,
                None => continue,
            };

            let text = match token.to_str_lossy() {
                Ok(t) => t,
                Err(_) => continue,
            };

            let text = text.trim();
            if text.is_empty() {
                continue;
            }

            // Skip special timestamp tokens (they start with _)
            let id = token.token_id();
            if id < 0 {
                continue;
            }

            let data = token.token_data();
            // t0 and t1 are in centiseconds (10ms units)
            let start_ms = data.t0 as i64 * 10;
            let end_ms = data.t1 as i64 * 10;

            // Skip tokens with invalid timestamps
            if start_ms < 0 || end_ms < 0 || end_ms < start_ms {
                continue;
            }

            words.push(TokenWord {
                word: text.to_string(),
                start_ms,
                end_ms,
            });
        }
    }

    if words.is_empty() {
        return None;
    }

    serde_json::to_string(&words).ok()
}

#[cfg(test)]
mod tests {
    use crate::audio::speaker::alignment::TokenWord;

    #[test]
    fn serialize_token_words() {
        let words = vec![
            TokenWord {
                word: "Hello".to_string(),
                start_ms: 0,
                end_ms: 500,
            },
            TokenWord {
                word: "world".to_string(),
                start_ms: 500,
                end_ms: 1000,
            },
        ];
        let json = serde_json::to_string(&words).unwrap();
        assert!(json.contains("Hello"));
        assert!(json.contains("world"));

        let parsed: Vec<TokenWord> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].word, "Hello");
        assert_eq!(parsed[0].start_ms, 0);
        assert_eq!(parsed[0].end_ms, 500);
    }

    #[test]
    fn serialize_non_ascii_tokens() {
        let words = vec![
            TokenWord {
                word: "café".to_string(),
                start_ms: 0,
                end_ms: 500,
            },
            TokenWord {
                word: "niño".to_string(),
                start_ms: 500,
                end_ms: 1000,
            },
            TokenWord {
                word: "über".to_string(),
                start_ms: 1000,
                end_ms: 1500,
            },
        ];
        let json = serde_json::to_string(&words).unwrap();
        let parsed: Vec<TokenWord> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0].word, "café");
        assert_eq!(parsed[1].word, "niño");
        assert_eq!(parsed[2].word, "über");
    }

    #[test]
    fn serialize_large_segment() {
        let words: Vec<TokenWord> = (0..600)
            .map(|i| TokenWord {
                word: format!("word{}", i),
                start_ms: i * 100,
                end_ms: (i + 1) * 100,
            })
            .collect();
        let json = serde_json::to_string(&words).unwrap();
        let parsed: Vec<TokenWord> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 600);
    }

    #[test]
    fn deserialize_malformed_json_falls_back() {
        let bad_json = r#"not valid json"#;
        let result: Result<Vec<TokenWord>, _> = serde_json::from_str(bad_json);
        assert!(result.is_err());

        // Missing fields
        let missing = r#"[{"word": "test"}]"#;
        let result: Result<Vec<TokenWord>, _> = serde_json::from_str(missing);
        assert!(result.is_err());

        // Negative timestamps — our code validates in alignment
        let negative = r#"[{"word": "test", "start_ms": -1, "end_ms": 100}]"#;
        let parsed: Vec<TokenWord> = serde_json::from_str(negative).unwrap();
        assert_eq!(parsed[0].start_ms, -1); // deserialized OK, validated at alignment
    }
}
