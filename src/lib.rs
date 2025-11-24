use anyhow::Result;
use std::sync::Arc;
pub mod io;

pub struct IoBuilder {
    path: String,
    io: Arc<dyn turso_core::IO>,
}

impl IoBuilder {
    pub fn new_local_with_io(path: &str, io: Arc<dyn turso_core::IO>) -> Self {
        Self {
            path: path.to_string(),
            io,
        }
    }

    pub fn build(self) -> Result<Arc<turso_core::Database>> {
        let db = turso_core::Database::open_file_with_flags(
            self.io,
            self.path.as_str(),
            turso_core::OpenFlags::default(),
            turso_core::DatabaseOpts::default(),
            None,
        )?;
        Ok(db)
    }
}
