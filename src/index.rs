use std::error::Error as StdError;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection, Result, Row, NO_PARAMS};
use serde::Serialize;

use crate::db_meta;
use crate::schema;
use crate::Root;

#[derive(Debug, Copy, Clone, PartialEq, Serialize)]
pub enum NodeType {
    Other = 0,
    Directory = 1,
    File = 2,
}

impl NodeType {
    pub fn from_i64(v: i64) -> NodeType {
        match v {
            1 => NodeType::Directory,
            2 => NodeType::File,
            _ => NodeType::Other,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub node_id: i64,
    pub node_type: NodeType,
    pub parent_id: Option<i64>,
    pub master_id: Option<i64>,
    pub name: PathBuf,
    pub path: PathBuf,
    pub modified: i64,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub track_id: i64,
    pub node_id: i64,
    pub stream_index: i64,
    pub track_index: Option<i64>,
    pub start: Option<f64>,
    pub number: i64,
    pub title: String,
    pub artist_id: i64,
    pub artist_name: String,
    pub album_id: i64,
    pub album_name: String,
    pub album_artist_id: Option<i64>,
    pub album_artist_name: Option<String>,
    pub length: f64,
}

#[derive(Debug, Clone)]
pub struct Image {
    pub image_id: i64,
    pub node_id: i64,
    pub stream_index: Option<i64>,
    pub description: String,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone)]
pub struct Album {
    pub album_id: i64,
    pub name: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub image_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Artist {
    pub artist_id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct TrackLyrics {
    pub track_id: i64,
    pub lyrics: Option<String>,
    pub provider: Option<String>,
    pub source: Option<String>,
    pub modified: i64,
}

pub struct IndexSource {
    db_path: PathBuf,
    roots: Arc<Vec<Root>>,
}

pub struct Index {
    conn: Connection,
    roots: Arc<Vec<Root>>,
}

impl IndexSource {
    pub fn create(db_path: PathBuf, roots: Arc<Vec<Root>>) -> Result<Option<IndexSource>> {
        info!("using '{}'", db_path.to_string_lossy());

        let source = IndexSource { db_path, roots };

        let mut index = source.get()?;
        if !db_meta::ensure_schema(&mut index.conn, schema::INDEX_SCHEMA)? {
            return Ok(None);
        }

        Ok(Some(source))
    }

    pub fn get(&self) -> Result<Index> {
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

        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;",
        )?;

        Ok(Index {
            conn,
            roots: self.roots.clone(),
        })
    }
}

impl Index {
    pub fn roots(&self) -> &Vec<Root> {
        &self.roots
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn map_fs_path(&self, path: &Path) -> Option<PathBuf> {
        let mut iter = path.iter();

        let root_name = match iter.next() {
            Some(name) => match name.to_str() {
                Some(name) => name,
                None => return None,
            },
            None => return None,
        };

        let root_dir = match self.roots.iter().find(|&r| r.name == root_name) {
            Some(name) => name,
            None => return None,
        };

        let mut result = PathBuf::from(&root_dir.path);

        for component in iter {
            result.push(component);
        }

        Some(result)
    }

    fn _get_node(row: &Row) -> Result<Node> {
        let node_type: i64 = row.get(1)?;
        let name_bytes: Vec<u8> = row.get(4)?;
        let path_bytes: Vec<u8> = row.get(5)?;

        Ok(Node {
            node_id: row.get(0)?,
            node_type: NodeType::from_i64(node_type as i64),
            parent_id: row.get(2)?,
            master_id: row.get(3)?,
            name: Path::new(OsStr::from_bytes(&name_bytes)).to_path_buf(),
            path: Path::new(OsStr::from_bytes(&path_bytes)).to_path_buf(),
            modified: row.get(6)?,
        })
    }

    pub fn node(&self, node_id: i64) -> Result<Option<Node>> {
        trace!("get node node_id={}", node_id);

        let mut st = self.conn.prepare(
            "SELECT node_id, node_type, parent_id, master_id, name, path, modified
            FROM Node
            WHERE node_id = ?",
        )?;

        let mut rows = st.query(&[node_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_node(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn node_by_name(&self, parent_id: Option<i64>, name: &Path) -> Result<Option<Node>> {
        trace!(
            "get node parent_id={:?} name='{}'",
            parent_id,
            name.to_string_lossy()
        );

        let mut st = self.conn.prepare(match parent_id {
            Some(_) => "
                SELECT node_id, node_type, parent_id, master_id, name, path, modified
                FROM Node
                WHERE name = ? AND parent_id = ?",
            None => "
                SELECT node_id, node_type, parent_id, master_id, name, path, modified
                FROM Node
                WHERE name = ? AND parent_id IS NULL",
        })?;

        let name_bytes = name.as_os_str().as_bytes();

        let mut rows = match parent_id {
            Some(id) => st.query(params![name_bytes, id])?,
            None => st.query(&[name_bytes])?,
        };

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_node(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn node_by_path(&self, path: &Path) -> Result<Option<Node>> {
        trace!("get node path='{}'", path.to_string_lossy());

        let mut st = self.conn.prepare(
            "SELECT node_id, node_type, parent_id, master_id, name, path, modified
            FROM Node
            WHERE path = ?",
        )?;

        let path_bytes = path.as_os_str().as_bytes();

        let mut rows = st.query(&[path_bytes])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_node(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn nodes_by_parent(&self, parent_id: Option<i64>) -> Result<Vec<Node>> {
        trace!("list nodes by parent_id={:?}", parent_id);

        let mut st = self.conn.prepare(match parent_id {
            Some(_) => "
                SELECT node_id, node_type, parent_id, master_id, name, path, modified
                FROM Node
                WHERE parent_id = ?",
            None => "
                SELECT node_id, node_type, parent_id, master_id, name, path, modified
                FROM Node
                WHERE parent_id IS NULL",
        })?;

        let mut rows = match parent_id {
            Some(id) => st.query(&[id])?,
            None => st.query(NO_PARAMS)?,
        };

        let mut result = Vec::new();

        while let Some(row) = rows.next()? {
            result.push(Self::_get_node(row)?);
        }

        Ok(result)
    }

    pub fn create_node(&self, node: &Node) -> Result<Node> {
        let mut st = self.conn.prepare(
            "INSERT INTO Node (node_type, parent_id, master_id, name, path, modified)
            VALUES (?, ?, ?, ?, ?, ?)",
        )?;

        st.execute(params![
            node.node_type as i64,
            node.parent_id,
            node.master_id,
            node.name.as_os_str().as_bytes(),
            node.path.as_os_str().as_bytes(),
            node.modified,
        ])?;

        let result = self.node(self.conn.last_insert_rowid())?.unwrap();

        debug!("create {:?}", result);

        Ok(result)
    }

    pub fn delete_node(&self, node_id: i64) -> Result<()> {
        trace!("delete node node_id={}", node_id);

        self.conn
            .execute("DELETE FROM Node WHERE node_id = ?", &[node_id])?;
        Ok(())
    }

    pub fn set_node_modified(&self, node_id: i64, modified: i64) -> Result<()> {
        trace!("set node node_id={} modified={}", node_id, modified);

        self.conn.execute(
            "UPDATE Node SET modified = ? WHERE node_id = ?",
            params![modified, node_id],
        )?;
        Ok(())
    }

    pub fn set_node_master(&self, node_id: i64, master_id: i64) -> Result<()> {
        trace!("set node node_id={} master_id={}", node_id, master_id);

        self.conn.execute(
            "UPDATE Node SET master_id = ? WHERE node_id = ?",
            params![master_id, node_id],
        )?;
        Ok(())
    }

    pub fn clear_node(&self, node_id: i64) -> Result<()> {
        trace!("clear node node_id={}", node_id);

        self.conn
            .execute("DELETE FROM Track WHERE node_id = ?", &[node_id])?;

        self.conn
            .execute("DELETE FROM Image WHERE node_id = ?", &[node_id])?;

        Ok(())
    }

    fn _get_track(row: &Row) -> Result<Track> {
        Ok(Track {
            track_id: row.get(0)?,
            node_id: row.get(1)?,
            stream_index: row.get(2)?,
            track_index: row.get(3)?,
            start: row.get(4)?,
            number: row.get(5)?,
            title: row.get(6)?,
            artist_id: row.get(7)?,
            artist_name: row.get(8)?,
            album_id: row.get(9)?,
            album_name: row.get(10)?,
            album_artist_id: row.get(11)?,
            album_artist_name: row.get(12)?,
            length: row.get(13)?,
        })
    }

    pub fn track(&self, track_id: i64) -> Result<Option<Track>> {
        trace!("get track track_id={}", track_id);

        let mut st = self.conn
            .prepare(
                "SELECT track_id, node_id, stream_index, track_index, start, number, title, artist_id, artist_name, album_id, album_name, album_artist_id, album_artist_name, length
                FROM Track
                WHERE track_id = ?"
            )?;

        let mut rows = st.query(&[track_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_track(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn create_track(&self, track: &Track) -> Result<Track> {
        let mut st = self.conn
            .prepare(
                "INSERT INTO Track (node_id, stream_index, track_index, start, number, title, artist_id, artist_name, album_id, album_name, album_artist_id, album_artist_name, length)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )?;

        st.execute(params![
            track.node_id,
            track.stream_index,
            track.track_index,
            track.start,
            track.number,
            track.title,
            track.artist_id,
            track.artist_name,
            track.album_id,
            track.album_name,
            track.album_artist_id,
            track.album_artist_name,
            track.length,
        ])?;

        let result = self.track(self.conn.last_insert_rowid())?.unwrap();

        debug!("create {:?}", result);

        Ok(result)
    }

    fn _get_image(row: &Row) -> Result<Image> {
        Ok(Image {
            image_id: row.get(0)?,
            node_id: row.get(1)?,
            stream_index: row.get(2)?,
            description: row.get(3)?,
            width: row.get(4)?,
            height: row.get(5)?,
        })
    }

    pub fn image(&self, image_id: i64) -> Result<Option<Image>> {
        trace!("get image image_id={}", image_id);

        let mut st = self.conn.prepare(
            "SELECT image_id, node_id, stream_index, description, width, height
            FROM Image
            WHERE image_id = ?",
        )?;

        let mut rows = st.query(&[image_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_image(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn create_image(&self, image: &Image) -> Result<Image> {
        let mut st = self.conn.prepare(
            "INSERT INTO Image (node_id, stream_index, description, width, height)
            VALUES (?, ?, ?, ?, ?)",
        )?;

        st.execute(params![
            image.node_id,
            image.stream_index,
            image.description,
            image.width,
            image.height
        ])?;

        let result = self.image(self.conn.last_insert_rowid())?.unwrap();

        debug!("create {:?}", result);

        Ok(result)
    }

    fn _get_artist(row: &Row) -> Result<Artist> {
        Ok(Artist {
            artist_id: row.get(0)?,
            name: row.get(1)?,
        })
    }

    pub fn artist(&self, artist_id: i64) -> Result<Option<Artist>> {
        trace!("get artist artist_id={}", artist_id);

        let mut st = self.conn.prepare(
            "SELECT artist_id, name
            FROM Artist
            WHERE artist_id = ?",
        )?;

        let mut rows = st.query(&[artist_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_artist(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn artist_by_name(&self, name: &str) -> Result<Option<Artist>> {
        trace!("get artist name={}", name);

        let mut st = self.conn.prepare(
            "SELECT artist_id, name
            FROM Artist
            WHERE name = ?",
        )?;

        let mut rows = st.query(&[name])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_artist(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn create_artist(&self, name: &str) -> Result<Artist> {
        let mut st = self.conn.prepare(
            "INSERT INTO Artist (name)
            VALUES (?)",
        )?;

        st.execute(params![name])?;

        let result = self.artist(self.conn.last_insert_rowid())?.unwrap();

        debug!("create {:?}", result);

        Ok(result)
    }

    fn _get_album(row: &Row) -> Result<Album> {
        Ok(Album {
            album_id: row.get(0)?,
            name: row.get(1)?,
            artist_id: row.get(2)?,
            artist_name: row.get(3)?,
            image_id: row.get(4)?,
        })
    }

    pub fn album(&self, album_id: i64) -> Result<Option<Album>> {
        trace!("get album album_id={}", album_id);

        let mut st = self.conn.prepare(
            "SELECT Album.album_id, Album.name, Album.artist_id, Album.artist_name, Album.image_id
                FROM Album
                WHERE album_id = ?",
        )?;

        let mut rows = st.query(&[album_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_album(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn create_album(&self, name: &str) -> Result<Album> {
        let mut st = self.conn.prepare("INSERT INTO Album (name) VALUES (?)")?;

        st.execute(params![name])?;

        let result = self.album(self.conn.last_insert_rowid())?.unwrap();

        debug!("create {:?}", result);

        Ok(result)
    }

    pub fn find_album(&self, track_node_id: i64, album_name: &str) -> Result<Option<Album>> {
        trace!(
            "find album track_node_id={} album_name={}",
            track_node_id,
            album_name
        );

        // Search the same directory
        let mut st = self.conn.prepare(
            "SELECT Album.album_id, Album.name, Album.artist_id, Album.artist_name, Album.image_id
                FROM Album
                INNER JOIN Node AS node ON node.node_id = ?
                INNER JOIN Node AS other_node ON node.parent_id = node.parent_id
                INNER JOIN Track AS track ON track.node_id = other_node.node_id
                WHERE track.album_id = Album.album_id AND Album.name = ?",
        )?;

        let mut rows = st.query(params![track_node_id, album_name])?;

        if let Some(row) = rows.next()? {
            return Ok(Some(Self::_get_album(row)?));
        }

        // See if there's an unused album
        let mut st = self.conn.prepare(
            "SELECT Album.album_id, Album.name, Album.artist_id, Album.artist_name, Album.image_id
                FROM Album
                LEFT OUTER JOIN Track ON Track.album_id = Album.album_id
                WHERE Track.track_id IS NULL AND Album.name = ?",
        )?;

        let mut rows = st.query(params![album_name])?;

        if let Some(row) = rows.next()? {
            return Ok(Some(Self::_get_album(row)?));
        }

        Ok(None)
    }

    fn _get_track_lyrics(row: &Row) -> Result<TrackLyrics> {
        Ok(TrackLyrics {
            track_id: row.get(0)?,
            lyrics: row.get(1)?,
            provider: row.get(2)?,
            source: row.get(3)?,
            modified: row.get(4)?,
        })
    }

    pub fn track_lyrics(&self, track_id: i64) -> Result<Option<TrackLyrics>> {
        trace!("get track lyrics track_id={}", track_id);

        let mut st = self.conn.prepare(
            "SELECT TrackLyrics.track_id, TrackLyrics.lyrics, TrackLyrics.provider, TrackLyrics.source, TrackLyrics.modified
                FROM TrackLyrics
                WHERE track_id = ?",
        )?;

        let mut rows = st.query(&[track_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Self::_get_track_lyrics(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn set_track_lyrics(&self, track_lyrics: &TrackLyrics) -> Result<TrackLyrics> {
        let mut st = self.conn.prepare("INSERT OR REPLACE INTO TrackLyrics (track_id, lyrics, provider, source, modified) VALUES (?, ?, ?, ?, strftime('%s','now'))")?;

        st.execute(params![
            track_lyrics.track_id,
            track_lyrics.lyrics,
            track_lyrics.provider,
            track_lyrics.source,
        ])?;

        let result = self.track_lyrics(self.conn.last_insert_rowid())?.unwrap();

        debug!("set {:?}", result);

        Ok(result)
    }

    pub fn process_node_updates(&self, node_id: i64) -> Result<()> {
        trace!("process node updates node_id={}", node_id);

        self.conn
            .execute(
                "UPDATE Album
                SET (artist_id, artist_name) =
                    (
                        SELECT id, name FROM
                            (
                                SELECT id, name FROM
                                    (
                                        SELECT Track.album_artist_id AS id, Track.album_artist_name AS name
                                        FROM Track
                                        WHERE Track.album_id = Album.album_id
                                        GROUP BY Track.album_artist_id, Track.album_artist_name
                                        ORDER BY count(Track.album_artist_name) DESC
                                    )
                                UNION
                                SELECT id, name FROM
                                    (
                                        SELECT Track.artist_id AS id, Track.artist_name AS name
                                        FROM Track
                                        WHERE Track.album_id = Album.album_id
                                        GROUP BY Track.album_artist_id, Track.album_artist_name
                                        ORDER BY count(Track.artist_name) DESC
                                    )
                            )
                        WHERE id IS NOT NULL
                        LIMIT 1
                    )
                WHERE Album.album_id IN
                    (
                        SELECT Track.album_id
                        FROM Track
                        INNER JOIN Node ON Node.parent_id = ?
                        WHERE Track.node_id = Node.node_id
                    )",
                &[node_id]
            )?;

        self.conn.execute(
            "WITH RECURSIVE
                iter(node_id, depth) AS
                    (
                        VALUES(?, 0)
                        UNION ALL
                        SELECT Node.node_id, iter.depth + 1 From Node, iter
                        WHERE iter.depth < 1 AND Node.parent_id = iter.node_id
                    )
            INSERT OR IGNORE INTO AlbumImage (album_id, image_id)    
            SELECT album.album_id, image.image_id
            FROM iter
            INNER JOIN Node image_node ON image_node.parent_id = iter.node_id
            INNER JOIN Image image ON image.node_id = image_node.node_id
            INNER JOIN Node track_node ON track_node.parent_id = ?
            INNER JOIN Track track ON track.node_id = track_node.node_id
            INNER JOIN Album album ON album.album_id = track.album_id
            WHERE
                iter.depth = 0
                OR
                (
                    SELECT count(track.track_id)
                    FROM Node track_node
                    INNER JOIN Track track ON track.node_id = track_node.node_id
                    WHERE
                        track_node.parent_id = iter.node_id
                    LIMIT 1
                ) = 0",
            &[node_id, node_id],
        )?;

        self.conn
            .execute(
                "UPDATE Album
                SET image_id = 
                    (
                        SELECT image.image_id
                        FROM AlbumImage album_image
                        INNER JOIN Image image ON image.image_id = album_image.image_id
                        LEFT OUTER JOIN AlbumImagePattern pattern ON image.description LIKE pattern.pattern
                        WHERE album_image.album_id = Album.album_id
                        ORDER BY
                            pattern.rowid IS NULL ASC,
                            pattern.rowid ASC,
                            image.description COLLATE NOCASE ASC
                    )
                WHERE Album.album_id IN
                    (
                        SELECT Track.album_id
                        FROM Track
                        INNER JOIN Node ON Node.parent_id = ?
                        WHERE Track.node_id = Node.node_id
                    )",
                &[node_id]
            )?;

        Ok(())
    }

    pub fn debug_truncate(&self) -> Result<()> {
        trace!("debug truncate");

        self.conn.execute_batch(
            "DELETE FROM Track;
            DELETE FROM Image;
            DELETE FROM Artist;
            DELETE FROM Album;
            DELETE FROM Node;",
        )?;

        Ok(())
    }
}
