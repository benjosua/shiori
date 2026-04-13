use askama::Template;
use axum::response::{Html, IntoResponse, Response};

use crate::models::{DashboardData, DeckDetail, MaterialDetail, SearchDetail};

pub struct HtmlTemplate<T>(pub T);

impl<T> IntoResponse for HtmlTemplate<T>
where
    T: Template,
{
    fn into_response(self) -> Response {
        match self.0.render() {
            Ok(html) => Html(html).into_response(),
            Err(err) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Template error: {err}"),
            )
                .into_response(),
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub data: DashboardData,
}

#[derive(Template)]
#[template(path = "deck_detail.html")]
pub struct DeckDetailTemplate {
    pub data: DeckDetail,
    pub should_refresh: bool,
}

#[derive(Template)]
#[template(path = "material_detail.html")]
pub struct MaterialDetailTemplate {
    pub data: MaterialDetail,
    pub decks: Vec<crate::models::DeckImport>,
    pub should_refresh: bool,
}

#[derive(Template)]
#[template(path = "search_detail.html")]
pub struct SearchDetailTemplate {
    pub data: SearchDetail,
    pub should_refresh: bool,
}
