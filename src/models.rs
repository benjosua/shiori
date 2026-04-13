use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardData {
    pub decks: Vec<DeckImport>,
    pub materials: Vec<Material>,
    pub searches: Vec<Search>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckImport {
    pub id: i64,
    pub filename: String,
    pub status: String,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub storage_path: String,
    pub note_count: i64,
    pub card_count: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckModel {
    pub id: i64,
    pub import_id: i64,
    pub original_model_id: i64,
    pub name: String,
    pub css: String,
    pub model_json: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedNote {
    pub id: i64,
    pub import_id: i64,
    pub original_note_id: i64,
    pub original_model_id: i64,
    pub guid: String,
    pub tags: String,
    pub fields_json: String,
    pub fields_joined: String,
    pub sort_field: String,
    pub checksum: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedCard {
    pub id: i64,
    pub import_id: i64,
    pub note_id: i64,
    pub original_card_id: i64,
    pub original_deck_id: i64,
    pub deck_name: String,
    pub ord: i64,
    pub card_text_clean: String,
    pub note_hash: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedMedia {
    pub id: i64,
    pub import_id: i64,
    pub media_name: String,
    pub extracted_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub id: i64,
    pub filename: String,
    pub kind: String,
    pub status: String,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub source_text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialSegment {
    pub id: i64,
    pub material_id: i64,
    pub ordinal: i64,
    pub label: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Search {
    pub id: i64,
    pub material_id: Option<i64>,
    pub query_text: String,
    pub selected_deck_import_id: Option<i64>,
    pub status: String,
    pub low_coverage: bool,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQueryChunk {
    pub id: i64,
    pub search_id: i64,
    pub chunk_index: i64,
    pub text: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub card_id: i64,
    pub query_chunk_id: i64,
    pub source: String,
    pub score: f32,
    pub snippet: String,
    pub matched_labels_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupedResult {
    pub id: i64,
    pub search_id: i64,
    pub card_id: i64,
    pub rerank_score: f32,
    pub fused_score: f32,
    pub chunk_hits: i64,
    pub lexical_bonus: i64,
    pub best_snippet: String,
    pub matched_labels_json: String,
    pub matched_labels_text: String,
    pub deck_name: String,
    pub card_text_clean: String,
    pub in_cart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckDetail {
    pub deck: DeckImport,
    pub models: Vec<DeckModel>,
    pub sample_cards: Vec<ImportedCard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialDetail {
    pub material: Material,
    pub segments: Vec<MaterialSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDetail {
    pub search: Search,
    pub material: Option<Material>,
    pub results: Vec<GroupedResult>,
    pub cart: Vec<GroupedResult>,
    pub cart_card_ids: Vec<i64>,
    pub available_decks: Vec<DeckImport>,
}
