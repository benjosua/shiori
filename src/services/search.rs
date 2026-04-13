use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Result, anyhow};

use crate::{
    AppState,
    db::{CardRecord, GroupedResultRecord, MaterialRecord},
    services::text::{chunk_text, normalize_text, reciprocal_rank_fusion},
};

#[derive(Debug, Clone)]
struct Candidate {
    card: CardRecord,
    fused_score: f32,
    rerank_score: f32,
    lexical_bonus: usize,
    best_snippet: String,
    matched_labels: BTreeSet<String>,
    chunk_hits: usize,
    best_query_text: String,
    best_query_overlap: usize,
}

#[derive(Debug, Clone)]
struct QueryChunk {
    label: String,
    text: String,
    terms: Vec<String>,
}

pub async fn run_search(state: AppState, search_id: i64) -> Result<()> {
    let search = state
        .db
        .get_search_record(search_id)?
        .ok_or_else(|| anyhow!("search {search_id} not found"))?;

    let (query_text, query_chunks) = if let Some(material_id) = search.material_id {
        let material = state
            .db
            .get_material_record(material_id)?
            .ok_or_else(|| anyhow!("material {material_id} not found"))?;
        let query_text = material.source_text.clone();
        let query_chunks = build_material_query_chunks(&material);
        (query_text, query_chunks)
    } else {
        let query_text = search.query_text.clone();
        let query_chunks = build_text_query_chunks(&query_text);
        (query_text, query_chunks)
    };

    let rerank_query = build_rerank_query(&query_text, &query_chunks);

    let dense_embeddings = state
        .services
        .external
        .embed(
            &query_chunks
                .iter()
                .map(|chunk| format!("query: {}", chunk.text))
                .collect::<Vec<_>>(),
        )
        .await
        .ok();

    let mut candidates: HashMap<i64, Candidate> = HashMap::new();

    for (index, query_chunk) in query_chunks.iter().enumerate() {
        let lexical_hits = state
            .db
            .lexical_search(&query_chunk.text, search.selected_deck_import_id, 20)
            .unwrap_or_default();
        for (rank, (card_id, score)) in lexical_hits.into_iter().enumerate() {
            if let Some(card) = state.db.get_card_record(card_id)? {
                let entry = candidates.entry(card_id).or_insert_with(|| Candidate {
                    card: card.clone(),
                    fused_score: 0.0,
                    rerank_score: score,
                    lexical_bonus: 0,
                    best_snippet: card.card_text_clean.clone(),
                    matched_labels: BTreeSet::new(),
                    chunk_hits: 0,
                    best_query_text: query_chunk.text.clone(),
                    best_query_overlap: 0,
                });
                entry.fused_score += reciprocal_rank_fusion(rank);
                entry.rerank_score = entry.rerank_score.max(score);
                entry.lexical_bonus += 1;
                entry.chunk_hits += 1;
                entry.best_snippet = select_better_snippet(
                    &entry.best_snippet,
                    &card.card_text_clean,
                    &query_chunk.terms,
                );
                push_match_label(&mut entry.matched_labels, &query_chunk.label);
                maybe_update_best_query(entry, query_chunk, &card.card_text_clean);
            }
        }

        if let Some(vector) = dense_embeddings
            .as_ref()
            .and_then(|emb| emb.get(index).cloned())
        {
            let dense_hits = state
                .services
                .external
                .dense_search(vector, search.selected_deck_import_id, 40)
                .await
                .unwrap_or_default();
            for (rank, dense_hit) in dense_hits.into_iter().enumerate() {
                if let Some(card) = state.db.get_card_record(dense_hit.card_id)? {
                    let entry = candidates.entry(card.card_id).or_insert_with(|| Candidate {
                        card: card.clone(),
                        fused_score: 0.0,
                        rerank_score: dense_hit.score,
                        lexical_bonus: 0,
                        best_snippet: dense_hit.snippet.clone(),
                        matched_labels: BTreeSet::new(),
                        chunk_hits: 0,
                        best_query_text: query_chunk.text.clone(),
                        best_query_overlap: 0,
                    });
                    entry.fused_score += reciprocal_rank_fusion(rank);
                    entry.rerank_score = entry.rerank_score.max(dense_hit.score);
                    entry.best_snippet = select_better_snippet(
                        &entry.best_snippet,
                        &dense_hit.snippet,
                        &query_chunk.terms,
                    );
                    entry.chunk_hits += 1;
                    push_match_label(&mut entry.matched_labels, &query_chunk.label);
                    maybe_update_best_query(entry, query_chunk, &dense_hit.snippet);
                }
            }
        }
    }

    let mut ordered = candidates.into_values().collect::<Vec<_>>();
    ordered.retain(is_useful_candidate);
    ordered.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ordered = collapse_redundant_candidates(ordered);
    ordered.truncate(100);

    let reranked = if state.config.rerank_strategy.eq_ignore_ascii_case("summary") {
        state
            .services
            .external
            .rerank(
                &rerank_query,
                &ordered
                    .iter()
                    .map(|candidate| candidate.best_snippet.clone())
                    .collect::<Vec<_>>(),
            )
            .await
            .ok()
    } else {
        state
            .services
            .external
            .rerank_pairs(
                &ordered
                    .iter()
                    .map(|candidate| truncate_for_rerank(&candidate.best_query_text, 1_000))
                    .collect::<Vec<_>>(),
                &ordered
                    .iter()
                    .map(|candidate| candidate.best_snippet.clone())
                    .collect::<Vec<_>>(),
            )
            .await
            .ok()
    };
    if let Some(scores) = reranked {
        for (idx, score) in scores {
            if let Some(candidate) = ordered.get_mut(idx) {
                candidate.rerank_score = score;
            }
        }
    }

    ordered.sort_by(|a, b| {
        b.rerank_score
            .partial_cmp(&a.rerank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.chunk_hits.cmp(&a.chunk_hits))
            .then_with(|| b.lexical_bonus.cmp(&a.lexical_bonus))
    });

    let low_coverage = ordered.len() < 3;
    let results = ordered
        .into_iter()
        .map(|candidate| GroupedResultRecord {
            search_id,
            card_id: candidate.card.card_id,
            rerank_score: candidate.rerank_score,
            fused_score: candidate.fused_score,
            chunk_hits: candidate.chunk_hits as i64,
            lexical_bonus: candidate.lexical_bonus as i64,
            best_snippet: candidate.best_snippet,
            matched_labels: candidate.matched_labels.into_iter().collect(),
        })
        .collect::<Vec<_>>();

    state
        .db
        .save_search_results(search_id, results, low_coverage)?;
    Ok(())
}

fn build_material_query_chunks(material: &MaterialRecord) -> Vec<QueryChunk> {
    if material.segments.len() <= 1 {
        return build_sampled_document_chunks(&material.source_text, 12);
    }

    let mut chunks = Vec::new();
    for segment in &material.segments {
        let normalized = normalize_text(&segment.text);
        if normalized.is_empty() {
            continue;
        }
        let pieces = sample_evenly(chunk_text(&normalized, 100, 20), 3);
        for (index, piece) in pieces.into_iter().enumerate() {
            if !is_informative_text(&piece) {
                continue;
            }
            let label = if index == 0 {
                segment.label.clone()
            } else {
                format!("{} part {}", segment.label, index + 1)
            };
            chunks.push(QueryChunk {
                label,
                terms: tokenize_query(&piece),
                text: piece,
            });
            if chunks.len() >= 24 {
                return chunks;
            }
        }
    }
    if chunks.is_empty() {
        build_text_query_chunks(&material.source_text)
    } else {
        chunks
    }
}

fn build_sampled_document_chunks(query_text: &str, max_chunks: usize) -> Vec<QueryChunk> {
    let normalized = normalize_text(query_text);
    let mut chunks = Vec::new();
    for (index, piece) in sample_evenly(chunk_text(&normalized, 100, 20), max_chunks)
        .into_iter()
        .enumerate()
    {
        if !is_informative_text(&piece) {
            continue;
        }
        chunks.push(QueryChunk {
            label: format!("Document section {}", index + 1),
            terms: tokenize_query(&piece),
            text: piece,
        });
    }
    if chunks.is_empty() {
        build_text_query_chunks(query_text)
    } else {
        chunks
    }
}

fn build_text_query_chunks(query_text: &str) -> Vec<QueryChunk> {
    let normalized = normalize_text(query_text);
    let mut seen = HashSet::new();
    let mut chunks = Vec::new();
    for (index, chunk) in chunk_text(&normalized, 120, 20).into_iter().enumerate() {
        if chunk.len() < 24 || !seen.insert(chunk.clone()) {
            continue;
        }
        chunks.push(QueryChunk {
            label: format!("Query chunk {}", index + 1),
            terms: tokenize_query(&chunk),
            text: chunk,
        });
        if chunks.len() >= 24 {
            return chunks;
        }
    }
    if chunks.is_empty() && !normalized.is_empty() {
        chunks.push(QueryChunk {
            label: "Query chunk 1".into(),
            terms: tokenize_query(&normalized),
            text: normalized,
        });
    }
    chunks
}

fn build_rerank_query(query_text: &str, query_chunks: &[QueryChunk]) -> String {
    if query_chunks.is_empty() {
        return truncate_for_rerank(query_text, 4_000);
    }

    let summary = query_chunks
        .iter()
        .take(8)
        .map(|chunk| format!("{}: {}", chunk.label, truncate_for_rerank(&chunk.text, 600)))
        .collect::<Vec<_>>()
        .join("\n\n");
    truncate_for_rerank(&summary, 4_000)
}

fn is_useful_candidate(candidate: &Candidate) -> bool {
    if candidate.lexical_bonus > 0 {
        return true;
    }

    let normalized = normalize_text(&candidate.card.card_text_clean);
    let informative_tokens = normalized
        .split_whitespace()
        .filter(|token| token.chars().any(|ch| ch.is_alphabetic()) && token.len() >= 3)
        .count();
    let alpha_chars = normalized.chars().filter(|ch| ch.is_alphabetic()).count();

    informative_tokens >= 6 || alpha_chars >= 40
}

fn is_informative_text(text: &str) -> bool {
    let normalized = normalize_text(text);
    let informative_tokens = normalized
        .split_whitespace()
        .filter(|token| token.chars().any(|ch| ch.is_alphabetic()) && token.len() >= 4)
        .count();
    informative_tokens >= 8
}

fn collapse_redundant_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen_note_ids = HashSet::new();
    let mut seen_texts = HashSet::new();
    let mut kept = Vec::new();

    for candidate in candidates {
        let note_id = candidate.card.original_note_id;
        let normalized_text = normalize_text(&candidate.card.card_text_clean);
        if !seen_note_ids.insert(note_id) {
            continue;
        }
        if !seen_texts.insert(normalized_text) {
            continue;
        }
        kept.push(candidate);
    }

    kept
}

fn sample_evenly(chunks: Vec<String>, max_chunks: usize) -> Vec<String> {
    if chunks.len() <= max_chunks || max_chunks <= 1 {
        return chunks;
    }

    let mut sampled = Vec::with_capacity(max_chunks);
    let last = chunks.len() - 1;
    for index in 0..max_chunks {
        let mapped = index * last / (max_chunks - 1);
        if sampled.last() != chunks.get(mapped) {
            sampled.push(chunks[mapped].clone());
        }
    }
    sampled
}

fn select_better_snippet(current: &str, candidate: &str, query_terms: &[String]) -> String {
    let current_score = score_snippet(current, query_terms);
    let candidate_score = score_snippet(candidate, query_terms);
    if candidate_score > current_score {
        candidate.to_string()
    } else {
        current.to_string()
    }
}

fn maybe_update_best_query(candidate: &mut Candidate, query_chunk: &QueryChunk, snippet: &str) {
    let overlap = score_query_overlap(snippet, &query_chunk.terms);
    if overlap > candidate.best_query_overlap {
        candidate.best_query_overlap = overlap;
        candidate.best_query_text = query_chunk.text.clone();
    }
}

fn score_snippet(snippet: &str, query_terms: &[String]) -> (usize, usize, usize) {
    let normalized = normalize_text(snippet);
    let overlap = score_query_overlap(&normalized, query_terms);
    let informative_tokens = normalized
        .split_whitespace()
        .filter(|token| token.chars().any(|ch| ch.is_alphabetic()) && token.len() >= 4)
        .count();
    (overlap, informative_tokens, normalized.len())
}

fn score_query_overlap(snippet: &str, query_terms: &[String]) -> usize {
    let normalized = normalize_text(snippet);
    query_terms
        .iter()
        .filter(|term| normalized.contains(term.as_str()))
        .count()
}

fn tokenize_query(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| part.len() >= 3)
        .map(ToOwned::to_owned)
        .collect()
}

fn push_match_label(labels: &mut BTreeSet<String>, label: &str) {
    if labels.len() < 6 || labels.contains(label) {
        labels.insert(label.to_string());
    }
}

fn truncate_for_rerank(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
