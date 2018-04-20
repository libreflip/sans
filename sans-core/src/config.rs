//! Configuration module for sans

use std::fs::OpenOptions;
use std::io::{Read, Write};
use toml as serde_toml;

#[derive(Serialize, Deserialize)]
struct ConfigBackend {
    cameras: Vec<String>,
}

pub struct SansConfig {
    path: String,
    backend: ConfigBackend,
}

impl SansConfig {
    pub fn save(&self) {
        OpenOptions::new()
            .create(true)
            .write(true)
            .open(&self.path)
            .expect("Failed to open sans config! Does the directory exist?")
            .write_all(&serde_toml::to_string_pretty(&self.backend)
                .unwrap_or_else(|_| {
                    serde_toml::to_string_pretty(&ConfigBackend {
                        cameras: Vec::new(),
                    }).unwrap()
                })
                .as_bytes())
            .expect("Failed to write sans config. Is your disk full?");
    }
}
