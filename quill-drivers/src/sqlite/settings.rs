use crate::settings::Params;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct SqliteSettings {
    pub path: PathBuf,
    #[serde(default)]
    pub params: Params,
}
