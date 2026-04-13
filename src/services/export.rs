use std::{collections::HashSet, path::PathBuf};

use anyhow::Result;
use genanki_rs::{Deck, Field, Model, Note, Package, Template};

use crate::AppState;

pub fn export_search(state: &AppState, search_id: i64) -> Result<PathBuf> {
    let cards = state.db.get_selected_cards(search_id)?;
    let mut deck = Deck::new(
        2_147_000_000_i64 + search_id,
        &format!("Shiori Search {}", search_id),
        "Generated from selected search matches",
    );

    let model =
        Model::new(
            1_600_000_000_i64 + search_id,
            "RecoveredCard",
            vec![
                Field::new("Front"),
                Field::new("Back"),
                Field::new("Source"),
            ],
            vec![Template::new("Card 1").qfmt("{{Front}}").afmt(
                "{{FrontSide}}<hr id=\"answer\">{{Back}}<div><small>{{Source}}</small></div>",
            )],
        );

    let mut seen = HashSet::new();
    for card in cards {
        if !seen.insert(card.id) {
            continue;
        }
        let (fields_json, fields_joined, deck_name) = state.db.get_note_fields_for_card(card.id)?;
        let fields: Vec<String> = serde_json::from_str(&fields_json).unwrap_or_default();
        let front = fields
            .iter()
            .find(|field| !field.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| card.card_text_clean.clone());
        let back = if fields.len() > 1 {
            fields.join("<br><br>")
        } else {
            fields_joined.clone()
        };
        let source = format!("{deck_name} / card {}", card.original_card_id);
        let note = Note::new(model.clone(), vec![&front, &back, &source])?;
        deck.add_note(note);
    }

    let export_path = state
        .config
        .exports_dir
        .join(format!("search-{}.apkg", search_id));
    let mut package = Package::new(vec![deck], vec![])?;
    package.write_to_file(&export_path.to_string_lossy())?;
    Ok(export_path)
}
