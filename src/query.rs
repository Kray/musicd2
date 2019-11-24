use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use rusqlite::types::ToSql;
use rusqlite::{Connection, Statement};
use serde::Serialize;

use crate::http_util::HttpQuery;
use crate::index::{Index, NodeType};

struct QueryOptions {
    clauses: Vec<String>,
    values: Vec<Box<dyn ToSql>>,
    order_string: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl QueryOptions {
    pub fn new() -> QueryOptions {
        QueryOptions {
            clauses: Vec::new(),
            values: Vec::new(),
            order_string: None,
            limit: None,
            offset: None,
        }
    }

    pub fn filter(&mut self, clause: &str) {
        self.clauses.push(clause.to_string());
    }

    pub fn filter_value<T>(&mut self, clause: &str, value: T)
    where
        T: ToSql,
        T: 'static,
    {
        self.clauses.push(clause.to_string());
        self.values.push(Box::new(value));
    }

    pub fn filter_values(&mut self, clause: &str, value: Vec<Box<dyn ToSql>>) {
        self.clauses.push(clause.to_string());

        for v in value {
            self.values.push(Box::new(v));
        }
    }

    pub fn bind_filter_i64(&mut self, query: &HttpQuery, key: &str, clause: &str) {
        if let Some(value) = query.get_i64(key) {
            self.filter_value(clause, value);
        }
    }

    pub fn bind_filter_str(&mut self, query: &HttpQuery, key: &str, clause: &str) {
        if let Some(value) = query.get_str(key) {
            self.filter_value(clause, value.to_string());
        }
    }

    pub fn order_string(&mut self, order_string: &str) {
        self.order_string = Some(order_string.to_string());
    }

    pub fn limit(&mut self, limit: i64) {
        self.limit = Some(limit);
    }

    pub fn offset(&mut self, offset: i64) {
        self.offset = Some(offset);
    }

    pub fn bind_range(&mut self, query: &HttpQuery) {
        if let Some(limit) = query.get_i64("limit") {
            self.limit(limit)
        }

        if let Some(offset) = query.get_i64("offset") {
            self.offset(offset)
        }
    }

    pub fn get_total(&self, conn: &Connection, select_from: &str) -> Result<i64, rusqlite::Error> {
        let mut sql = select_from.to_string();

        if !self.clauses.is_empty() {
            sql += " WHERE ";
            sql += &self.clauses.join(" AND ");
        }

        let mut st = conn.prepare(&sql)?;

        Ok(st.query_row(&self.values, |row| row.get(0))?)
    }

    pub fn into_items_query<'a>(
        mut self,
        conn: &'a Connection,
        select_from: &str,
    ) -> Result<(Statement<'a>, Vec<Box<dyn ToSql>>), rusqlite::Error> {
        let mut sql = select_from.to_string();

        if !self.clauses.is_empty() {
            sql += " WHERE ";
            sql += &self.clauses.join(" AND ");
        }

        if let Some(order) = self.order_string {
            sql += " ORDER BY ";
            sql += &order;
        }

        if let Some(limit) = self.limit {
            sql += " LIMIT ?";
            self.values.push(Box::new(limit));
        }

        if let Some(offset) = self.offset {
            sql += " OFFSET ?";
            self.values.push(Box::new(offset));
        }

        let st = conn.prepare(&sql)?;

        Ok((st, self.values))
    }
}

#[derive(Serialize)]
pub struct NodeItem {
    node_id: i64,
    parent_id: Option<i64>,
    node_type: NodeType,
    name: String,
    path: String,
    track_count: i64,
    image_count: i64,
    all_track_count: i64,
    all_image_count: i64,
}

pub fn query_nodes(
    index: &Index,
    query: &HttpQuery,
) -> Result<(i64, Vec<NodeItem>), rusqlite::Error> {
    let mut opts = QueryOptions::new();

    let mut parent_id_filter = false;

    if let Some(parent_id) = query.get_str("parent_id") {
        if let Ok(parent_id) = parent_id.parse::<i64>() {
            opts.filter_value("Node.parent_id = ?", parent_id);
            parent_id_filter = true;
        } else if parent_id == "null" {
            opts.filter("Node.parent_id IS NULL");
            parent_id_filter = true;
        }
    }

    opts.bind_range(&query);

    let conn = index.connection();

    let total = opts.get_total(&conn, "SELECT COUNT(Node.node_id) FROM Node")?;

    let (mut st, values) = opts.into_items_query(
        &conn,
        if parent_id_filter {
            "SELECT
                Node.node_id,
                Node.parent_id,
                Node.node_type,
                Node.name,
                Node.path,

                (
                    SELECT COUNT(track_id)
                    FROM Track
                    INNER JOIN Node track_node ON track_node.node_id = Track.node_id
                    WHERE track_node.parent_id = Node.node_id
                ) AS track_count,
                (
                    SELECT COUNT(image_id)
                    FROM Image
                    INNER JOIN Node image_node ON image_node.node_id = Image.node_id
                    WHERE image_node.parent_id = Node.node_id
                ) AS image_count,

                (
                    SELECT COUNT(track_id)
                    FROM Node AS child_node
                    INNER JOIN Track ON Track.node_id = child_node.node_id
                    WHERE child_node.path LIKE Node.path || '/%'
                ) AS all_track_count,
                (
                    SELECT COUNT(image_id)
                    FROM Node AS child_node
                    INNER JOIN Image ON Image.node_id = child_node.node_id
                    WHERE child_node.path LIKE Node.path || '/%'
                ) AS all_image_count

            FROM Node"
        } else {
            "SELECT
                Node.node_id,
                Node.parent_id,
                Node.node_type,
                Node.name,
                Node.path,

                (
                    SELECT COUNT(track_id)
                    FROM Track
                    INNER JOIN Node track_node ON track_node.node_id = Track.node_id
                    WHERE track_node.parent_id = Node.node_id
                ) AS track_count,
                (
                    SELECT COUNT(image_id)
                    FROM Image
                    INNER JOIN Node image_node ON image_node.node_id = Image.node_id
                    WHERE image_node.parent_id = Node.node_id
                ) AS image_count,

                0 AS all_track_count,
                0 AS all_image_count

            FROM Node"
        },
    )?;

    let mut rows = st.query(&values)?;
    let mut items: Vec<NodeItem> = Vec::new();

    while let Some(row) = rows.next().unwrap() {
        let name: Vec<u8> = row.get(3)?;
        let path: Vec<u8> = row.get(4)?;

        items.push(NodeItem {
            node_id: row.get(0)?,
            parent_id: row.get(1)?,
            node_type: NodeType::from_i64(row.get(2)?),
            name: OsStr::from_bytes(&name).to_string_lossy().to_string(),
            path: OsStr::from_bytes(&path).to_string_lossy().to_string(),
            track_count: row.get(5)?,
            image_count: row.get(6)?,
            all_track_count: row.get(7)?,
            all_image_count: row.get(8)?,
        });
    }

    Ok((total, items))
}

#[derive(Serialize)]
pub struct TrackItem {
    track_id: i64,
    node_id: i64,
    number: i64,
    title: String,
    artist_id: i64,
    artist_name: String,
    album_id: i64,
    album_name: String,
    length: f64,
    node_path: String,
}

pub fn query_tracks(
    index: &Index,
    query: &HttpQuery,
) -> Result<(i64, Vec<TrackItem>), rusqlite::Error> {
    let mut opts = QueryOptions::new();

    opts.bind_filter_i64(&query, "track_id", "Track.track_id = ?");
    opts.bind_filter_i64(&query, "node_id", "Track.node_id = ?");
    opts.bind_filter_i64(&query, "number", "Track.number = ?");
    opts.bind_filter_str(&query, "title", "Track.title LIKE ? COLLATE NOCASE");
    opts.bind_filter_i64(&query, "artist_id", "Track.artist_id = ?");
    opts.bind_filter_str(
        &query,
        "artist_name",
        "Track.artist_name LIKE ? COLLATE NOCASE",
    );
    opts.bind_filter_i64(&query, "album_id", "Track.album_id = ?");
    opts.bind_filter_str(
        &query,
        "album_name",
        "Track.album_name LIKE ? COLLATE NOCASE",
    );

    if let Some(search) = query.get_str("search") {
        let mut values: Vec<Box<dyn ToSql>> = Vec::new();
        values.push(Box::new(format!("%{}%", search)));
        values.push(Box::new(format!("%{}%", search)));
        values.push(Box::new(format!("%{}%", search)));

        opts.filter_values(
            "(Track.title LIKE ? OR Track.artist_name LIKE ? OR Track.album_name LIKE ?)",
            values,
        );
    }

    opts.order_string("Track.album_name, Track.number, Track.title");

    opts.bind_range(&query);

    let conn = index.connection();

    let total = opts.get_total(&conn, "SELECT COUNT(Track.track_id) FROM Track")?;

    let (mut st, values) = opts.into_items_query(
        &conn,
        "SELECT
            Track.track_id,
            Track.node_id,
            Track.number,
            Track.title,
            Track.artist_id,
            Track.artist_name,
            Track.album_id,
            Track.album_name,
            Track.length,

            (
                SELECT Node.path
                FROM Node
                WHERE Node.node_id = Track.node_id
            ) AS node_path

        FROM Track",
    )?;

    let mut rows = st.query(&values)?;

    let mut items: Vec<TrackItem> = Vec::new();

    while let Some(row) = rows.next()? {
        let path: Vec<u8> = row.get(9)?;

        items.push(TrackItem {
            track_id: row.get(0)?,
            node_id: row.get(1)?,
            number: row.get(2)?,
            title: row.get(3)?,
            artist_id: row.get(4)?,
            artist_name: row.get(5)?,
            album_id: row.get(6)?,
            album_name: row.get(7)?,
            length: row.get(8)?,
            node_path: OsStr::from_bytes(&path).to_string_lossy().to_string(),
        });
    }

    Ok((total, items))
}

#[derive(Serialize)]
pub struct ArtistItem {
    artist_id: i64,
    name: String,
    track_count: i64,
}

pub fn query_artists(
    index: &Index,
    query: &HttpQuery,
) -> Result<(i64, Vec<ArtistItem>), rusqlite::Error> {
    let mut opts = QueryOptions::new();

    opts.bind_filter_i64(&query, "artist_id", "Artist.artist_id = ?");
    opts.bind_filter_str(&query, "name", "Artist.name LIKE ? COLLATE NOCASE");
    opts.bind_filter_str(&query, "search", "Artist.name LIKE ? COLLATE NOCASE");

    opts.order_string("Artist.name");

    opts.bind_range(&query);

    let conn = index.connection();

    let total = opts.get_total(&conn, "SELECT COUNT(Artist.artist_id) FROM Artist")?;

    let (mut st, values) = opts.into_items_query(&conn,
        "SELECT
            Artist.artist_id,
            Artist.name,
            (SELECT count(Track.track_id) FROM Track WHERE Track.artist_id = Artist.artist_id) AS track_count
        FROM Artist")?;

    let mut rows = st.query(&values)?;

    let mut items: Vec<ArtistItem> = Vec::new();

    while let Some(row) = rows.next()? {
        items.push(ArtistItem {
            artist_id: row.get(0)?,
            name: row.get(1)?,
            track_count: row.get(2)?,
        });
    }

    Ok((total, items))
}

#[derive(Serialize)]
pub struct AlbumItem {
    album_id: i64,
    name: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    image_id: Option<i64>,
    track_count: i64,
}

pub fn query_albums(
    index: &Index,
    query: &HttpQuery,
) -> Result<(i64, Vec<AlbumItem>), rusqlite::Error> {
    let mut opts = QueryOptions::new();

    opts.bind_filter_i64(&query, "album_id", "Album.album_id = ?");
    opts.bind_filter_str(&query, "name", "Album.name LIKE ? COLLATE NOCASE");
    opts.bind_filter_i64(&query, "artist_id", "Album.artist_id = ?");
    opts.bind_filter_str(
        &query,
        "artist_name",
        "Album.artist_name LIKE ? COLLATE NOCASE",
    );

    if let Some(search) = query.get_str("search") {
        let mut values: Vec<Box<dyn ToSql>> = Vec::new();
        values.push(Box::new(format!("%{}%", search)));
        values.push(Box::new(format!("%{}%", search)));

        opts.filter_values("(Album.name LIKE ? OR Album.artist_name LIKE ?)", values);
    }

    opts.order_string("Album.artist_name, Album.name");

    opts.bind_range(&query);

    let conn = index.connection();

    let total = opts.get_total(&conn, "SELECT COUNT(Album.album_id) FROM Album")?;

    let (mut st, values) = opts.into_items_query(&conn,
        "SELECT
            Album.album_id,
            Album.name,
            Album.artist_id,
            Album.artist_name,
            Album.image_id,
            (SELECT count(Track.track_id) FROM Track WHERE Track.album_id = Album.album_id) AS track_count
        FROM Album")?;

    let mut rows = st.query(&values)?;

    let mut items: Vec<AlbumItem> = Vec::new();

    while let Some(row) = rows.next()? {
        items.push(AlbumItem {
            album_id: row.get(0)?,
            name: row.get(1)?,
            artist_id: row.get(2)?,
            artist_name: row.get(3)?,
            image_id: row.get(4)?,
            track_count: row.get(5)?,
        });
    }

    Ok((total, items))
}

#[derive(Serialize)]
pub struct ImageItem {
    image_id: i64,
    node_id: i64,
    description: String,
}

pub fn query_images(
    index: &Index,
    query: &HttpQuery,
) -> Result<(i64, Vec<ImageItem>), rusqlite::Error> {
    let mut opts = QueryOptions::new();

    opts.bind_filter_i64(&query, "image_id", "Image.image_id = ?");
    opts.bind_filter_i64(&query, "node_id", "Image.node_id = ?");
    opts.bind_filter_str(&query, "description", "Image.description = ?");
    opts.bind_filter_i64(&query, "album_id", "(SELECT album_id FROM AlbumImage WHERE AlbumImage.album_id = ? AND AlbumImage.image_id = Image.image_id LIMIT 1) IS NOT NULL");

    opts.bind_range(&query);

    let conn = index.connection();

    let total = opts.get_total(&conn, "SELECT COUNT(Image.image_id) FROM Image")?;

    let (mut st, values) = opts.into_items_query(
        &conn,
        "SELECT
            Image.image_id,
            Image.node_id,
            Image.description
        FROM Image",
    )?;

    let mut rows = st.query(&values)?;

    let mut items: Vec<ImageItem> = Vec::new();

    while let Some(row) = rows.next()? {
        items.push(ImageItem {
            image_id: row.get(0)?,
            node_id: row.get(1)?,
            description: row.get(2)?,
        });
    }

    Ok((total, items))
}
