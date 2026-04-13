use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use ureq::Agent;

use crate::{
    config::Config,
    models::{
        DashboardData, DeckDetail, DeckImport, DeckModel, GroupedResult, ImportedCard, Material,
        MaterialDetail, MaterialSegment, Search, SearchDetail,
    },
};

const DUMMY_VECTOR: [f32; 1] = [0.0];

#[derive(Clone)]
pub struct Database {
    client: Agent,
    config: Arc<Config>,
    id_counter: Arc<AtomicI64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckRecord {
    pub id: i64,
    pub filename: String,
    pub status: String,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub storage_path: String,
    pub note_count: i64,
    pub card_count: i64,
    pub models: Vec<DeckModel>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardRecord {
    pub point_id: i64,
    pub import_id: i64,
    pub card_id: i64,
    pub original_note_id: i64,
    pub original_card_id: i64,
    pub original_deck_id: i64,
    pub original_model_id: i64,
    pub deck_name: String,
    pub guid: String,
    pub ord: i64,
    pub chunk_index: i64,
    pub note_hash: String,
    pub card_text_clean: String,
    pub fields_json: String,
    pub fields_joined: String,
    pub tags: String,
    pub chunk_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialRecord {
    pub id: i64,
    pub filename: String,
    pub kind: String,
    pub status: String,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub source_text: String,
    pub segments: Vec<MaterialSegment>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRecord {
    pub id: i64,
    pub material_id: Option<i64>,
    pub query_text: String,
    pub selected_deck_import_id: Option<i64>,
    pub status: String,
    pub low_coverage: bool,
    pub error: Option<String>,
    pub results: Vec<GroupedResultRecord>,
    pub cart: Vec<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupedResultRecord {
    pub search_id: i64,
    pub card_id: i64,
    pub rerank_score: f32,
    pub fused_score: f32,
    pub chunk_hits: i64,
    pub lexical_bonus: i64,
    pub best_snippet: String,
    pub matched_labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DenseCardPoint {
    pub point_id: i64,
    pub vector: Vec<f32>,
    pub payload: CardRecord,
}

#[derive(Debug, Deserialize)]
struct QdrantEnvelope<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct ScrollResult {
    #[serde(default)]
    points: Vec<RetrievedPoint>,
    #[serde(default)]
    next_page_offset: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RetrievedPoint {
    payload: Value,
}

impl Database {
    pub fn new(config: Arc<Config>) -> Result<Self> {
        Ok(Self {
            client: Agent::new_with_defaults(),
            config,
            id_counter: Arc::new(AtomicI64::new(Utc::now().timestamp_micros())),
        })
    }

    pub fn init(&self) -> Result<()> {
        for (name, size) in [
            ("decks", 1usize),
            ("materials", 1usize),
            ("searches", 1usize),
            (
                &self.config.qdrant_collection,
                self.config.embedding_vector_size,
            ),
        ] {
            if let Err(error) = self.ensure_collection(name, size) {
                tracing::warn!("qdrant init for {name} skipped: {error}");
            }
        }
        Ok(())
    }

    pub fn next_id(&self) -> i64 {
        self.id_counter.fetch_add(1, Ordering::SeqCst)
    }

    pub fn dashboard(&self) -> Result<DashboardData> {
        Ok(DashboardData {
            decks: self.list_decks().unwrap_or_default(),
            materials: self.list_materials().unwrap_or_default(),
            searches: self.list_searches().unwrap_or_default(),
        })
    }

    pub fn create_deck_import(&self, filename: &str, storage_path: &str) -> Result<i64> {
        let id = self.next_id();
        let record = DeckRecord {
            id,
            filename: filename.to_string(),
            status: "pending".into(),
            warning: None,
            error: None,
            storage_path: storage_path.to_string(),
            note_count: 0,
            card_count: 0,
            models: Vec::new(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.upsert_dummy("decks", id, &record)?;
        Ok(id)
    }

    pub fn replace_deck(&self, record: &DeckRecord) -> Result<()> {
        self.upsert_dummy("decks", record.id, record)
    }

    pub fn update_deck_status(
        &self,
        id: i64,
        status: &str,
        warning: Option<&str>,
        error: Option<&str>,
        note_count: Option<i64>,
        card_count: Option<i64>,
    ) -> Result<()> {
        let mut record = self
            .get_deck_record(id)?
            .ok_or_else(|| anyhow!("deck {id} not found"))?;
        record.status = status.to_string();
        if let Some(value) = warning {
            record.warning = Some(value.to_string());
        }
        record.error = error.map(ToOwned::to_owned);
        if let Some(value) = note_count {
            record.note_count = value;
        }
        if let Some(value) = card_count {
            record.card_count = value;
        }
        self.replace_deck(&record)
    }

    pub fn get_deck_record(&self, id: i64) -> Result<Option<DeckRecord>> {
        self.get_dummy_by_id("decks", id)
    }

    pub fn list_decks(&self) -> Result<Vec<DeckImport>> {
        let mut items = self.scroll_collection::<DeckRecord>("decks", None)?;
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items.into_iter().map(deck_record_to_model).collect())
    }

    pub fn save_card_points(&self, points: Vec<DenseCardPoint>) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let payload_points = points
            .into_iter()
            .map(|point| {
                json!({
                    "id": point.point_id,
                    "vector": point.vector,
                    "payload": serde_json::to_value(point.payload).expect("card payload")
                })
            })
            .collect::<Vec<_>>();
        self.client
            .put(&format!(
                "{}/collections/{}/points?wait=true",
                self.config.qdrant_url, self.config.qdrant_collection
            ))
            .send_json(json!({ "points": payload_points }))
            .map_err(|err| anyhow!("upsert card points: {err}"))?;
        Ok(())
    }

    pub fn get_card_by_id(&self, card_id: i64) -> Result<Option<ImportedCard>> {
        let record = self.get_card_record(card_id)?;
        Ok(record.map(card_record_to_model))
    }

    pub fn get_card_record(&self, card_id: i64) -> Result<Option<CardRecord>> {
        let record = self
            .scroll_collection::<CardRecord>(
                &self.config.qdrant_collection,
                Some(json!({
                    "must": [
                        { "key": "card_id", "match": { "value": card_id } },
                        { "key": "chunk_index", "match": { "value": 0 } }
                    ]
                })),
            )?
            .into_iter()
            .next();
        Ok(record)
    }

    pub fn get_card_chunk(&self, point_id: i64) -> Result<Option<(i64, String)>> {
        let record = self
            .scroll_collection::<CardRecord>(
                &self.config.qdrant_collection,
                Some(json!({
                    "must": [
                        { "key": "point_id", "match": { "value": point_id } }
                    ]
                })),
            )?
            .into_iter()
            .next();
        Ok(record.map(|card| (card.card_id, card.chunk_text)))
    }

    pub fn get_cards_for_deck(&self, deck_id: i64, limit: usize) -> Result<Vec<ImportedCard>> {
        let mut cards = self.scroll_collection::<CardRecord>(
            &self.config.qdrant_collection,
            Some(json!({
                "must": [
                    { "key": "import_id", "match": { "value": deck_id } },
                    { "key": "chunk_index", "match": { "value": 0 } }
                ]
            })),
        )?;
        cards.sort_by_key(|card| card.card_id);
        cards.truncate(limit);
        Ok(cards.into_iter().map(card_record_to_model).collect())
    }

    pub fn create_material(&self, filename: &str, kind: &str) -> Result<i64> {
        let id = self.next_id();
        let record = MaterialRecord {
            id,
            filename: filename.to_string(),
            kind: kind.to_string(),
            status: "pending".into(),
            warning: None,
            error: None,
            source_text: String::new(),
            segments: Vec::new(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.upsert_dummy("materials", id, &record)?;
        Ok(id)
    }

    pub fn replace_material(&self, record: &MaterialRecord) -> Result<()> {
        self.upsert_dummy("materials", record.id, record)
    }

    pub fn update_material(
        &self,
        id: i64,
        status: &str,
        source_text: Option<&str>,
        warning: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let mut record = self
            .get_material_record(id)?
            .ok_or_else(|| anyhow!("material {id} not found"))?;
        record.status = status.to_string();
        if let Some(value) = source_text {
            record.source_text = value.to_string();
        }
        if let Some(value) = warning {
            record.warning = Some(value.to_string());
        }
        record.error = error.map(ToOwned::to_owned);
        self.replace_material(&record)
    }

    pub fn get_material_record(&self, id: i64) -> Result<Option<MaterialRecord>> {
        self.get_dummy_by_id("materials", id)
    }

    pub fn list_materials(&self) -> Result<Vec<Material>> {
        let mut items = self.scroll_collection::<MaterialRecord>("materials", None)?;
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items.into_iter().map(material_record_to_model).collect())
    }

    pub fn create_search(
        &self,
        material_id: Option<i64>,
        query_text: &str,
        selected_deck_import_id: Option<i64>,
    ) -> Result<i64> {
        let id = self.next_id();
        let record = SearchRecord {
            id,
            material_id,
            query_text: query_text.to_string(),
            selected_deck_import_id,
            status: "pending".into(),
            low_coverage: false,
            error: None,
            results: Vec::new(),
            cart: Vec::new(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.upsert_dummy("searches", id, &record)?;
        Ok(id)
    }

    pub fn replace_search(&self, record: &SearchRecord) -> Result<()> {
        self.upsert_dummy("searches", record.id, record)
    }

    pub fn update_search_status(
        &self,
        id: i64,
        status: &str,
        low_coverage: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let mut record = self
            .get_search_record(id)?
            .ok_or_else(|| anyhow!("search {id} not found"))?;
        record.status = status.to_string();
        record.low_coverage = low_coverage;
        record.error = error.map(ToOwned::to_owned);
        self.replace_search(&record)
    }

    pub fn get_search_record(&self, id: i64) -> Result<Option<SearchRecord>> {
        self.get_dummy_by_id("searches", id)
    }

    pub fn list_searches(&self) -> Result<Vec<Search>> {
        let mut items = self.scroll_collection::<SearchRecord>("searches", None)?;
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items.into_iter().map(search_record_to_model).collect())
    }

    pub fn save_search_results(
        &self,
        id: i64,
        results: Vec<GroupedResultRecord>,
        low_coverage: bool,
    ) -> Result<()> {
        let mut record = self
            .get_search_record(id)?
            .ok_or_else(|| anyhow!("search {id} not found"))?;
        record.status = "ready".into();
        record.results = results;
        record.low_coverage = low_coverage;
        record.error = None;
        self.replace_search(&record)
    }

    pub fn upsert_cart_item(&self, search_id: i64, card_id: i64, include: bool) -> Result<()> {
        let mut record = self
            .get_search_record(search_id)?
            .ok_or_else(|| anyhow!("search {search_id} not found"))?;
        let mut cart = record.cart.into_iter().collect::<HashSet<_>>();
        if include {
            cart.insert(card_id);
        } else {
            cart.remove(&card_id);
        }
        record.cart = cart.into_iter().collect();
        self.replace_search(&record)
    }

    pub fn get_selected_cards(&self, search_id: i64) -> Result<Vec<ImportedCard>> {
        let search = self
            .get_search_record(search_id)?
            .ok_or_else(|| anyhow!("search {search_id} not found"))?;
        let cards = self
            .scroll_collection::<CardRecord>(
                &self.config.qdrant_collection,
                Some(json!({
                    "must": [
                        { "key": "chunk_index", "match": { "value": 0 } }
                    ]
                })),
            )?
            .into_iter()
            .filter(|card| search.cart.contains(&card.card_id))
            .map(card_record_to_model)
            .collect();
        Ok(cards)
    }

    pub fn get_note_fields_for_card(&self, card_id: i64) -> Result<(String, String, String)> {
        let card = self
            .scroll_collection::<CardRecord>(
                &self.config.qdrant_collection,
                Some(json!({
                    "must": [
                        { "key": "card_id", "match": { "value": card_id } },
                        { "key": "chunk_index", "match": { "value": 0 } }
                    ]
                })),
            )?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("card {card_id} not found"))?;
        Ok((card.fields_json, card.fields_joined, card.deck_name))
    }

    pub fn lexical_search(
        &self,
        query: &str,
        deck_import_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut candidates = self.scroll_collection::<CardRecord>(
            &self.config.qdrant_collection,
            Some(if let Some(deck_id) = deck_import_id {
                json!({
                    "must": [
                        { "key": "import_id", "match": { "value": deck_id } },
                        { "key": "chunk_index", "match": { "value": 0 } }
                    ]
                })
            } else {
                json!({
                    "must": [
                        { "key": "chunk_index", "match": { "value": 0 } }
                    ]
                })
            }),
        )?;

        let mut scored = candidates
            .drain(..)
            .filter_map(|card| {
                let haystack = format!(
                    "{} {} {} {}",
                    card.card_text_clean, card.fields_joined, card.tags, card.deck_name
                )
                .to_lowercase();
                let overlap = terms
                    .iter()
                    .filter(|term| haystack.contains(term.as_str()))
                    .count();
                if overlap == 0 {
                    None
                } else {
                    Some((card.card_id, overlap as f32 / terms.len() as f32))
                }
            })
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    pub fn get_deck_detail(&self, id: i64) -> Result<Option<DeckDetail>> {
        let Some(deck) = self.get_deck_record(id)? else {
            return Ok(None);
        };
        let sample_cards = self.get_cards_for_deck(id, 25)?;
        Ok(Some(DeckDetail {
            deck: deck_record_to_model(deck.clone()),
            models: deck.models,
            sample_cards,
        }))
    }

    pub fn get_material(&self, id: i64) -> Result<Option<Material>> {
        Ok(self.get_material_record(id)?.map(material_record_to_model))
    }

    pub fn get_material_detail(&self, id: i64) -> Result<Option<MaterialDetail>> {
        let Some(record) = self.get_material_record(id)? else {
            return Ok(None);
        };
        Ok(Some(MaterialDetail {
            material: material_record_to_model(record.clone()),
            segments: record.segments,
        }))
    }

    pub fn get_search(&self, id: i64) -> Result<Option<Search>> {
        Ok(self.get_search_record(id)?.map(search_record_to_model))
    }

    pub fn get_search_detail(&self, id: i64) -> Result<Option<SearchDetail>> {
        let Some(record) = self.get_search_record(id)? else {
            return Ok(None);
        };
        let material = match record.material_id {
            Some(material_id) => self.get_material(material_id)?,
            None => None,
        };
        let cards = self.scroll_collection::<CardRecord>(
            &self.config.qdrant_collection,
            Some(json!({
                "must": [
                    { "key": "chunk_index", "match": { "value": 0 } }
                ]
            })),
        )?;
        let card_lookup = cards
            .into_iter()
            .map(|card| (card.card_id, card))
            .collect::<HashMap<_, _>>();
        let cart_card_ids = record.cart.clone();
        let mut results = record
            .results
            .iter()
            .filter_map(|result| {
                card_lookup
                    .get(&result.card_id)
                    .map(|card| grouped_result_record_to_model(&record, result, card))
            })
            .collect::<Vec<_>>();
        for result in &mut results {
            result.in_cart = cart_card_ids.contains(&result.card_id);
        }
        let cart = record
            .cart
            .iter()
            .filter_map(|card_id| {
                let card = card_lookup.get(card_id)?;
                let result = record
                    .results
                    .iter()
                    .find(|item| item.card_id == *card_id)?;
                Some(grouped_result_record_to_model(&record, result, card))
            })
            .collect::<Vec<_>>();
        Ok(Some(SearchDetail {
            search: search_record_to_model(record),
            material,
            results,
            cart,
            cart_card_ids,
            available_decks: self.list_decks()?,
        }))
    }

    fn ensure_collection(&self, name: &str, vector_size: usize) -> Result<()> {
        let response = self
            .client
            .get(&format!("{}/collections/{}", self.config.qdrant_url, name))
            .call();
        if response.is_ok() {
            return Ok(());
        }
        self.client
            .put(&format!("{}/collections/{}", self.config.qdrant_url, name))
            .send_json(json!({
                "vectors": {
                    "size": vector_size,
                    "distance": "Cosine"
                }
            }))
            .map_err(|err| anyhow!("create qdrant collection {name}: {err}"))?;
        Ok(())
    }

    fn get_dummy_by_id<T>(&self, collection: &str, id: i64) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let items = self.scroll_collection::<T>(
            collection,
            Some(json!({
                "must": [
                    { "key": "id", "match": { "value": id } }
                ]
            })),
        )?;
        Ok(items.into_iter().next())
    }

    fn upsert_dummy<T>(&self, collection: &str, id: i64, payload: &T) -> Result<()>
    where
        T: Serialize,
    {
        self.client
            .put(&format!(
                "{}/collections/{}/points?wait=true",
                self.config.qdrant_url, collection
            ))
            .send_json(json!({
                "points": [
                    {
                        "id": id,
                        "vector": DUMMY_VECTOR,
                        "payload": payload,
                    }
                ]
            }))
            .map_err(|err| anyhow!("upsert qdrant point in {collection}: {err}"))?;
        Ok(())
    }

    fn scroll_collection<T>(&self, collection: &str, filter: Option<Value>) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        let mut items = Vec::new();
        let mut offset: Option<Value> = None;

        loop {
            let mut body = json!({
                "limit": 1000,
                "with_payload": true,
                "with_vector": false,
            });
            if let Some(filter) = &filter {
                body["filter"] = filter.clone();
            }
            if let Some(offset_value) = &offset {
                body["offset"] = offset_value.clone();
            }

            let mut response = self
                .client
                .post(&format!(
                    "{}/collections/{}/points/scroll",
                    self.config.qdrant_url, collection
                ))
                .send_json(body)
                .map_err(|err| anyhow!("scroll qdrant collection {collection}: {err}"))?;
            let envelope: QdrantEnvelope<ScrollResult> =
                response.body_mut().read_json().context("parse qdrant scroll")?;
            let ScrollResult {
                points,
                next_page_offset,
            } = envelope.result;

            items.extend(
                points
                    .into_iter()
                    .map(|point| serde_json::from_value(point.payload).map_err(Into::into))
                    .collect::<Result<Vec<_>>>()?,
            );

            match next_page_offset {
                Some(next_page_offset) => offset = Some(next_page_offset),
                None => break,
            }
        }

        Ok(items)
    }
}

fn deck_record_to_model(record: DeckRecord) -> DeckImport {
    DeckImport {
        id: record.id,
        filename: record.filename,
        status: record.status,
        warning: record.warning,
        error: record.error,
        storage_path: record.storage_path,
        note_count: record.note_count,
        card_count: record.card_count,
        created_at: parse_timestamp(&record.created_at),
    }
}

fn material_record_to_model(record: MaterialRecord) -> Material {
    Material {
        id: record.id,
        filename: record.filename,
        kind: record.kind,
        status: record.status,
        warning: record.warning,
        error: record.error,
        source_text: record.source_text,
        created_at: parse_timestamp(&record.created_at),
    }
}

fn search_record_to_model(record: SearchRecord) -> Search {
    Search {
        id: record.id,
        material_id: record.material_id,
        query_text: record.query_text,
        selected_deck_import_id: record.selected_deck_import_id,
        status: record.status,
        low_coverage: record.low_coverage,
        error: record.error,
        created_at: parse_timestamp(&record.created_at),
    }
}

fn card_record_to_model(record: CardRecord) -> ImportedCard {
    ImportedCard {
        id: record.card_id,
        import_id: record.import_id,
        note_id: record.original_note_id,
        original_card_id: record.original_card_id,
        original_deck_id: record.original_deck_id,
        deck_name: record.deck_name,
        ord: record.ord,
        card_text_clean: record.card_text_clean,
        note_hash: record.note_hash,
    }
}

fn grouped_result_record_to_model(
    search: &SearchRecord,
    result: &GroupedResultRecord,
    card: &CardRecord,
) -> GroupedResult {
    GroupedResult {
        id: stable_id(search.id, result.card_id),
        search_id: search.id,
        card_id: result.card_id,
        rerank_score: result.rerank_score,
        fused_score: result.fused_score,
        chunk_hits: result.chunk_hits,
        lexical_bonus: result.lexical_bonus,
        best_snippet: result.best_snippet.clone(),
        matched_labels_json: serde_json::to_string(&result.matched_labels)
            .unwrap_or_else(|_| "[]".into()),
        matched_labels_text: result.matched_labels.join(", "),
        deck_name: card.deck_name.clone(),
        card_text_clean: card.card_text_clean.clone(),
        in_cart: false,
    }
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn tokenize(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| part.len() >= 2)
        .map(ToOwned::to_owned)
        .collect()
}

fn stable_id(search_id: i64, card_id: i64) -> i64 {
    search_id
        .wrapping_mul(1_000_003)
        .wrapping_add(card_id)
        .abs()
}
