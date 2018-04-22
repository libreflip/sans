//! Configuration module for sans

use std::fs::{File, OpenOptions};
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

pub enum ConfigError {
    FileNotFount,
    FailedToReadFile,
    CorruptFile,
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

    /// Attempt to load the configuration for sans. Will return
    /// None if that was not possible.SansConfig
    ///
    /// In this case sans needs to terminate!
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        use self::ConfigError::{CorruptFile, FailedToReadFile, FileNotFount};

        let mut f = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Err(FileNotFount),
        };

        let mut content = String::new();
        if f.read_to_string(&mut content).is_err() {
            return Err(FailedToReadFile);
        }

        return Ok(SansConfig {
            path: path.into(),
            backend: match serde_toml::from_str(&content) {
                Ok(s) => s,
                Err(_) => return Err(CorruptFile),
            },
        });
    }
}
