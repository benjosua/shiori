use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use anyhow::{Context, Result};

#[derive(Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub decks_dir: PathBuf,
    pub materials_dir: PathBuf,
    pub exports_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub static_dir: PathBuf,
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub tei_url: String,
    pub unoconvert_bin: String,
    pub rerank_strategy: String,
    pub embedding_vector_size: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let cwd = env::current_dir().context("read current directory")?;
        let data_dir = env::var("APP_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| cwd.join("data"));
        let static_dir = cwd.join("assets");

        let host = env::var("APP_HOST")
            .ok()
            .and_then(|value| value.parse::<IpAddr>().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let port = env::var("APP_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(3000);

        Ok(Self {
            bind_addr: SocketAddr::new(host, port),
            decks_dir: data_dir.join("decks"),
            materials_dir: data_dir.join("materials"),
            exports_dir: data_dir.join("exports"),
            temp_dir: data_dir.join("tmp"),
            data_dir,
            static_dir,
            qdrant_url: env::var("QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6333".into()),
            qdrant_collection: env::var("QDRANT_COLLECTION")
                .unwrap_or_else(|_| "shiori_card_chunks".into()),
            tei_url: env::var("TEI_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
            unoconvert_bin: env::var("UNOCONVERT_BIN").unwrap_or_else(|_| "unoconvert".into()),
            rerank_strategy: env::var("RERANK_STRATEGY").unwrap_or_else(|_| "best_chunk".into()),
            embedding_vector_size: env::var("EMBEDDING_VECTOR_SIZE")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1024),
        })
    }

    pub fn ensure_directories(&self) -> Result<()> {
        for path in [
            &self.data_dir,
            &self.decks_dir,
            &self.materials_dir,
            &self.exports_dir,
            &self.temp_dir,
            &self.static_dir,
        ] {
            std::fs::create_dir_all(path)
                .with_context(|| format!("create directory {}", path.display()))?;
        }
        Ok(())
    }
}
