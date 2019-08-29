use std::error::Error as StdError;
use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension, Result, NO_PARAMS};

use crate::db_meta;
use crate::schema;

pub struct CacheSource {
    db_path: PathBuf,
    max_size: usize,
}

pub struct Cache {
    conn: Connection,
    max_size: usize,
}

impl CacheSource {
    pub fn create(db_path: PathBuf, max_size: usize) -> Result<Option<CacheSource>> {
        info!("using '{}', max_size={}", db_path.to_string_lossy(), max_size);

        let source = CacheSource { db_path, max_size };

        let mut cache = source.get()?;
        if !db_meta::ensure_schema(&mut cache.conn, schema::CACHE_SCHEMA)? {
            return Ok(None);
        }

        Ok(Some(source))
    }

    pub fn get(&self) -> Result<Cache> {
        let conn = match Connection::open(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                error!(
                    "can't open sqlite database '{}': {}",
                    self.db_path.to_string_lossy(),
                    e.description()
                );
                return Err(e);
            }
        };

        Ok(Cache {
            conn,
            max_size: self.max_size,
        })
    }
}

impl Cache {
    pub fn get_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
        trace!("get blob '{}'", key);

        let result: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT value FROM cache WHERE key = ?", &[key], |row| {
                row.get(0)
            })
            .optional()?;

        if result.is_some() {
            trace!("update blob '{}' last_access", key);
            self.conn.execute(
                "UPDATE cache SET last_access = strftime('%s','now') WHERE key = ?",
                &[key],
            )?;
        }

        Ok(result)
    }

    pub fn set_blob(&self, key: &str, value: &[u8]) -> Result<()> {
        trace!("set blob '{}'", key);

        let mut st = self.conn.prepare(
            "INSERT OR REPLACE
            INTO cache (key, value, size, last_access)
            VALUES (?, ?, ?, strftime('%s','now'))",
        )?;

        st.execute(params![key, value, value.len() as i64])?;

        loop {
            let size: i64 =
                self.conn
                    .query_row("SELECT SUM(size) FROM cache", NO_PARAMS, |row| row.get(0))?;
            if size as usize <= self.max_size {
                break;
            }

            trace!(
                "max_size reached ({} > {}), clearing 1 item",
                size,
                self.max_size
            );

            self.conn.execute(
                "
                DELETE FROM cache
                WHERE rowid = (SELECT rowid FROM cache ORDER BY last_access ASC LIMIT 1);",
                NO_PARAMS,
            )?;
        }

        Ok(())
    }
}
