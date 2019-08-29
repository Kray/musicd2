use rusqlite::OptionalExtension;
use rusqlite::{Connection, Result, NO_PARAMS};

use crate::schema;

pub fn ensure_schema(conn: &mut Connection, schema: &str) -> Result<bool> {
    trace!("trying to get schema version");

    conn.execute_batch(schema::META_SCHEMA)?;

    let schema_version: Option<u32> = conn
        .query_row(
            "SELECT value FROM Musicd WHERE key = 'schema'",
            NO_PARAMS,
            |row| row.get(0),
        )
        .optional()?;

    if let Some(schema_version) = schema_version {
        if schema_version != schema::SCHEMA_VERSION {
            error!(
                "unsupported schema version: got {}, expected {}",
                schema_version,
                schema::SCHEMA_VERSION
            );
            return Ok(false);
        }

        debug!("schema version up-to-date, doing nothing");
    } else {
        debug!("schema meta not present, creating schema");

        let tran = conn.transaction()?;

        tran.execute(
            "INSERT INTO Musicd (key, value) VALUES ('schema', ?)",
            &[schema::SCHEMA_VERSION],
        )?;
        tran.execute_batch(schema)?;

        tran.commit()?;
    }

    Ok(true)
}
