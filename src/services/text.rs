use sha2::{Digest, Sha256};

pub fn normalize_text(input: &str) -> String {
    let cleaned = ammonia::clean(input);
    let plain = html2text::from_read(cleaned.as_bytes(), 80);
    plain
        .replace('\u{0c}', "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
fn split_sentences(input: &str) -> Vec<String> {
    use regex::Regex;

    let regex = Regex::new(r"(?m)([^.!?\n]+[.!?]?)").expect("valid regex");
    let mut sentences = Vec::new();
    for capture in regex.captures_iter(input) {
        let candidate = capture
            .get(1)
            .map(|value| value.as_str().trim())
            .unwrap_or_default();
        if candidate.len() >= 4 {
            sentences.push(candidate.to_string());
        }
    }
    if sentences.is_empty() && !input.trim().is_empty() {
        sentences.push(input.trim().to_string());
    }
    sentences
}

pub fn chunk_text(input: &str, max_words: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = input.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < words.len() {
        let end = usize::min(start + max_words, words.len());
        chunks.push(words[start..end].join(" "));
        if end == words.len() {
            break;
        }
        start = end.saturating_sub(overlap);
    }
    chunks
}

pub fn hash_text(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn reciprocal_rank_fusion(rank: usize) -> f32 {
    1.0 / (60.0 + rank as f32)
}

#[cfg(test)]
mod tests {
    use super::{chunk_text, split_sentences};

    #[test]
    fn chunks_with_overlap() {
        let text = (0..900)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk_text(&text, 400, 60);
        assert_eq!(chunks.len(), 3);
        assert!(chunks[1].contains("w340"));
    }

    #[test]
    fn sentence_split_finds_multiple_sentences() {
        let parts = split_sentences("Das ist ein Test. Noch ein Test! Und noch einer?");
        assert_eq!(parts.len(), 3);
    }
}
