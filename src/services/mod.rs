pub mod deck;
pub mod export;
pub mod external;
pub mod material;
pub mod search;
pub mod text;

use std::sync::Arc;

use crate::config::Config;

#[derive(Clone)]
pub struct AppServices {
    pub external: external::ExternalServices,
}

impl AppServices {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            external: external::ExternalServices::new(config),
        }
    }
}
