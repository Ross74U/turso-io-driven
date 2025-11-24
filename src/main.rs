use anyhow::{bail, Result};
use std::sync::Arc;
use turso_core::IO;
use turso_io::{io::io_uring::UringIO, IoBuilder};

const DB_FILE: &str = "database.db";

const SQL_CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL
    )";

fn main() {
    let mut io = Arc::new(UringIO::new().unwrap());

    let builder = IoBuilder::new_local_with_io(DB_FILE, io.clone());
    let db = builder.build().unwrap();
    println!("created db\n{:?}", db);

    let connection = db.connect().unwrap();
    let stmt = connection.prepare(SQL_CREATE_TABLE).unwrap();
    let changes = execute_until_done(stmt, &mut io).unwrap();
    println!("execute completed! changes: {:?}", changes);
}

/// this just runs the io loop until the statement is executed
fn execute_until_done(mut stmt: turso_core::Statement, io: &mut Arc<impl IO>) -> Result<u64> {
    loop {
        match stmt.step() {
            Ok(turso_core::StepResult::Row) => bail!("unexpected row during execution"),
            Ok(turso_core::StepResult::Done) => {
                let changes = stmt.n_change();
                assert!(changes >= 0);
                return Ok(changes as u64);
            }
            Ok(turso_core::StepResult::IO) => {
                println!("io");
                io.step().unwrap();
            }
            Ok(turso_core::StepResult::Busy) => {
                bail!("database is locked");
            }
            Ok(turso_core::StepResult::Interrupt) => {
                bail!("interrupted");
            }
            Err(err) => return Err(err.into()),
        }
    }
}
