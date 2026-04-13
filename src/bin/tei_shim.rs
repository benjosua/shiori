use std::{
    collections::HashMap,
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
struct AppState {
    client: Client,
    ollama_url: String,
    embedding_model: String,
    llm_model: Option<String>,
    rerank_top_n: usize,
    rerank_batch_size: usize,
}

#[derive(Debug, Deserialize)]
struct EmbedRequest {
    inputs: Vec<String>,
    #[serde(default)]
    normalize: bool,
    #[serde(default)]
    truncate: bool,
}

#[derive(Debug, Serialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct OllamaGenerateResponse {
    response: String,
}

#[derive(Debug, Deserialize)]
struct RerankRequest {
    query: String,
    texts: Vec<String>,
    #[serde(default)]
    truncate: bool,
}

#[derive(Debug, Deserialize)]
struct RerankPairsRequest {
    queries: Vec<String>,
    texts: Vec<String>,
    #[serde(default)]
    truncate: bool,
}

#[derive(Debug, Serialize)]
struct RerankResponse {
    results: Vec<RerankItem>,
}

#[derive(Debug, Serialize)]
struct RerankItem {
    index: usize,
    score: f32,
}

#[derive(Debug, Deserialize)]
struct LlmRerankEnvelope {
    scores: Vec<LlmRerankItem>,
}

#[derive(Debug, Deserialize)]
struct LlmRerankItem {
    index: usize,
    score: f32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tei_shim=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let bind_addr = SocketAddr::new(
        env::var("TEI_SHIM_HOST")
            .ok()
            .and_then(|value| value.parse::<IpAddr>().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        env::var("TEI_SHIM_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(8080),
    );
    let state = Arc::new(AppState {
        client: Client::new(),
        ollama_url: env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into()),
        embedding_model: optional_env(&["EMBEDDING_MODEL"])
            .unwrap_or_else(|| "nomic-embed-text:latest".into()),
        llm_model: optional_env(&["LLM_MODEL"]),
        rerank_top_n: env::var("OLLAMA_RERANK_TOP_N")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8),
        rerank_batch_size: env::var("OLLAMA_RERANK_BATCH_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/embed", post(embed))
        .route("/rerank", post(rerank))
        .route("/rerank_pairs", post(rerank_pairs))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("bind {bind_addr}"))?;
    tracing::info!("tei shim listening on http://{bind_addr}");
    axum::serve(listener, app).await.context("serve tei shim")?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

async fn embed(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EmbedRequest>,
) -> Result<Json<EmbedResponse>, (axum::http::StatusCode, String)> {
    if request.inputs.is_empty() {
        return Ok(Json(EmbedResponse {
            embeddings: Vec::new(),
        }));
    }

    let prepared = request
        .inputs
        .into_iter()
        .map(|text| adapt_prompt_prefix(&text, false, request.truncate))
        .collect::<Vec<_>>();
    let mut embeddings = embed_many(&state, prepared).await.map_err(internal_error)?;
    if request.normalize {
        for vector in &mut embeddings {
            normalize_vector(vector);
        }
    }
    Ok(Json(EmbedResponse { embeddings }))
}

async fn rerank(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RerankRequest>,
) -> Result<Json<RerankResponse>, (axum::http::StatusCode, String)> {
    if request.texts.is_empty() {
        return Ok(Json(RerankResponse {
            results: Vec::new(),
        }));
    }

    let mut query_embedding = embed_many(
        &state,
        vec![adapt_prompt_prefix(&request.query, true, request.truncate)],
    )
    .await
    .map_err(internal_error)?
    .into_iter()
    .next()
    .ok_or_else(|| internal_error(anyhow!("missing query embedding")))?;
    normalize_vector(&mut query_embedding);

    let candidate_inputs = request
        .texts
        .iter()
        .map(|text| adapt_prompt_prefix(text, false, request.truncate))
        .collect::<Vec<_>>();
    let mut candidate_embeddings = embed_many(&state, candidate_inputs)
        .await
        .map_err(internal_error)?;

    let mut results = candidate_embeddings
        .iter_mut()
        .enumerate()
        .map(|(index, vector)| {
            normalize_vector(vector);
            RerankItem {
                index,
                score: cosine_similarity(&query_embedding, vector),
            }
        })
        .collect::<Vec<_>>();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if state.llm_model.is_some() {
        let baseline_scores = results
            .iter()
            .map(|item| (item.index, item.score))
            .collect::<HashMap<_, _>>();
        let selected = results
            .iter()
            .take(state.rerank_top_n.min(results.len()))
            .map(|item| item.index)
            .collect::<Vec<_>>();
        let llm_scores = llm_rerank(&state, &request.query, &request.texts, &selected)
            .await
            .map_err(internal_error)?;
        for item in &mut results {
            if let Some(llm_score) = llm_scores.get(&item.index) {
                let baseline = baseline_scores
                    .get(&item.index)
                    .copied()
                    .unwrap_or(item.score);
                item.score = (baseline * 0.4) + (llm_score * 0.6);
            }
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    Ok(Json(RerankResponse { results }))
}

async fn rerank_pairs(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RerankPairsRequest>,
) -> Result<Json<RerankResponse>, (axum::http::StatusCode, String)> {
    if request.texts.is_empty() || request.queries.is_empty() {
        return Ok(Json(RerankResponse {
            results: Vec::new(),
        }));
    }
    if request.texts.len() != request.queries.len() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "queries/texts length mismatch".into(),
        ));
    }

    let paired_inputs = request
        .texts
        .iter()
        .zip(request.queries.iter())
        .map(|(text, query)| {
            (
                adapt_prompt_prefix(query, true, request.truncate),
                adapt_prompt_prefix(text, false, request.truncate),
            )
        })
        .collect::<Vec<_>>();

    let mut results = Vec::with_capacity(paired_inputs.len());
    for (index, (query, text)) in paired_inputs.iter().enumerate() {
        let mut embeddings = embed_many(&state, vec![query.clone(), text.clone()])
            .await
            .map_err(internal_error)?;
        if embeddings.len() != 2 {
            return Err(internal_error(anyhow!(
                "pair rerank embedding response malformed"
            )));
        }
        let mut query_embedding = embeddings.remove(0);
        let mut text_embedding = embeddings.remove(0);
        normalize_vector(&mut query_embedding);
        normalize_vector(&mut text_embedding);
        let mut score = cosine_similarity(&query_embedding, &text_embedding);

        if state.llm_model.is_some() {
            let llm_scores = llm_rerank(&state, query, std::slice::from_ref(text), &[0])
                .await
                .map_err(internal_error)?;
            if let Some(llm_score) = llm_scores.get(&0) {
                score = (score * 0.4) + (llm_score * 0.6);
            }
        }

        results.push(RerankItem { index, score });
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(Json(RerankResponse { results }))
}

async fn embed_many(state: &AppState, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let response = state
        .client
        .post(format!("{}/api/embed", state.ollama_url))
        .json(&json!({
            "model": state.embedding_model,
            "input": inputs,
        }))
        .send()
        .await
        .context("call ollama embed")?;
    if !response.status().is_success() {
        return Err(anyhow!("ollama embed failed with {}", response.status()));
    }
    let body: OllamaEmbedResponse = response.json().await.context("parse ollama embed")?;
    Ok(body.embeddings)
}

async fn llm_rerank(
    state: &AppState,
    query: &str,
    texts: &[String],
    selected_indices: &[usize],
) -> Result<HashMap<usize, f32>> {
    let Some(model) = state.llm_model.as_ref() else {
        return Ok(HashMap::new());
    };

    let mut scores = HashMap::new();
    for batch in selected_indices.chunks(state.rerank_batch_size.max(1)) {
        let prompt = build_llm_rerank_prompt(query, texts, batch);
        let response = state
            .client
            .post(format!("{}/api/generate", state.ollama_url))
            .json(&json!({
                "model": model,
                "prompt": prompt,
                "stream": false,
                "format": "json",
                "options": {
                    "temperature": 0
                }
            }))
            .send()
            .await
            .context("call ollama rerank")?;
        if !response.status().is_success() {
            return Err(anyhow!("ollama rerank failed with {}", response.status()));
        }
        let body: OllamaGenerateResponse = response.json().await.context("parse ollama rerank")?;
        let parsed: LlmRerankEnvelope =
            serde_json::from_str(&body.response).context("parse shiori rerank json")?;
        for item in parsed.scores {
            if batch.contains(&item.index) {
                scores.insert(item.index, item.score.clamp(0.0, 1.0));
            }
        }
    }
    Ok(scores)
}

fn adapt_prompt_prefix(text: &str, is_query: bool, truncate: bool) -> String {
    let trimmed = if truncate {
        truncate_chars(text, 8_000)
    } else {
        text.trim().to_string()
    };
    let body = trimmed
        .strip_prefix("query:")
        .or_else(|| trimmed.strip_prefix("passage:"))
        .map(str::trim)
        .unwrap_or(trimmed.as_str());
    let prefix = if is_query || text.trim_start().starts_with("query:") {
        "search_query:"
    } else {
        "search_document:"
    };
    format!("{prefix} {body}")
}

fn optional_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn build_llm_rerank_prompt(query: &str, texts: &[String], selected_indices: &[usize]) -> String {
    let mut prompt = String::from(
        "You are reranking flashcards against study material.\n\
Score each candidate from 0.0 to 1.0.\n\
Use high scores only when the card would directly help a student study concepts, findings, tests, anatomy, pathology, or management present in the query material.\n\
Prefer concrete examinable facts over generic history-taking or catch-all cards when the query covers a whole curriculum.\n\
Penalize generic overlap, off-topic system overlap, and content-poor or image-only cards.\n\
Return JSON only in this exact shape: {\"scores\":[{\"index\":0,\"score\":0.0}]}\n\n",
    );
    prompt.push_str("Query material:\n");
    prompt.push_str(&truncate_chars(query, 4_000));
    prompt.push_str("\n\nCandidates:\n");
    for index in selected_indices {
        prompt.push('[');
        prompt.push_str(&index.to_string());
        prompt.push_str("] ");
        prompt.push_str(&truncate_chars(&texts[*index], 1_200));
        prompt.push('\n');
    }
    prompt
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|value| (*value as f64) * (*value as f64))
        .sum::<f64>()
        .sqrt() as f32;
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return -1.0;
    }
    a.iter().zip(b).map(|(left, right)| left * right).sum()
}

fn internal_error(error: anyhow::Error) -> (axum::http::StatusCode, String) {
    tracing::error!("{error:?}");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        adapt_prompt_prefix, build_llm_rerank_prompt, cosine_similarity, normalize_vector,
    };

    #[test]
    fn maps_query_prefix_to_nomic_format() {
        assert_eq!(
            adapt_prompt_prefix("query: nephron physiology", false, false),
            "search_query: nephron physiology"
        );
        assert_eq!(
            adapt_prompt_prefix("passage: renal tubule transport", false, false),
            "search_document: renal tubule transport"
        );
    }

    #[test]
    fn normalizes_and_scores_vectors() {
        let mut a = vec![3.0, 4.0];
        let mut b = vec![6.0, 8.0];
        normalize_vector(&mut a);
        normalize_vector(&mut b);
        let score = cosine_similarity(&a, &b);
        assert!(score > 0.999);
    }

    #[test]
    fn builds_llm_rerank_prompt_with_indices() {
        let prompt = build_llm_rerank_prompt(
            "ophthalmology basics",
            &["red reflex".into(), "neonatal sepsis".into()],
            &[0, 1],
        );
        assert!(prompt.contains("[0] red reflex"));
        assert!(prompt.contains("[1] neonatal sepsis"));
    }
}
