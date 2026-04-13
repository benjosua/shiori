use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use genanki_rs::{Deck, Field, Model, Note, Package, Template};

fn main() -> Result<()> {
    let out_path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/examples/imci-fixture.apkg"));

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let deck = build_deck()?;
    let mut package = Package::new(vec![deck], vec![])?;
    package
        .write_to_file(&out_path.to_string_lossy())
        .with_context(|| format!("write {}", out_path.display()))?;

    println!("{}", out_path.display());
    Ok(())
}

fn build_deck() -> Result<Deck> {
    let mut deck = Deck::new(
        1_700_000_001,
        "Shiori IMCI Fixture",
        "Fixture deck for Shiori end-to-end testing",
    );
    let model = Model::new(
        1_700_000_101,
        "BasicFixture",
        vec![Field::new("Front"), Field::new("Back")],
        vec![
            Template::new("Card 1")
                .qfmt("{{Front}}")
                .afmt("{{FrontSide}}<hr id=\"answer\">{{Back}}"),
        ],
    );

    let notes = vec![
        (
            "What does IMCI stand for?",
            "Integrated Management of Child Health.",
        ),
        (
            "Where are IMCI clinical guidelines mainly used?",
            "At outpatient level for management of children under five in hospitals, health centres, or the community.",
        ),
        (
            "Which teaching skills are emphasized in community medicine IMCI sessions?",
            "Counselling skills, communication, and analysis of family practices related to child care.",
        ),
        (
            "Welche Rolle spielen medizinische Fakultäten in der IMCI-Ausbildung?",
            "Sie bereiten künftige Gesundheitsfachkräfte auf die Versorgung von Kindern im öffentlichen und privaten Sektor vor.",
        ),
    ];

    for (front, back) in notes {
        deck.add_note(Note::new(model.clone(), vec![front, back])?);
    }

    Ok(deck)
}
