use std::error::Error as StdError;
use std::ffi::OsStr;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use rusqlite::types::ToSql;
use rusqlite::{Connection, Statement};
use serde::Serialize;
use serde_json::json;

use crate::audio_stream::AudioStream;
use crate::http_util::HttpQuery;
use crate::index::{NodeType, TrackLyrics};
use crate::lyrics;
use crate::media;
use crate::Musicd;

#[derive(Debug)]
pub enum Error {
    HyperError(hyper::Error),
    IoError(std::io::Error),
    DatabaseError(rusqlite::Error),
    ImageError(image::ImageError),
}

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Error {
        Error::HyperError(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::IoError(err)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Error {
        Error::DatabaseError(err)
    }
}

impl From<image::ImageError> for Error {
    fn from(err: image::ImageError) -> Error {
        Error::ImageError(err)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

impl StdError for Error {
    fn description(&self) -> &str {
        match *self {
            Error::HyperError(ref e) => e.description(),
            Error::IoError(ref e) => e.description(),
            Error::DatabaseError(ref e) => e.description(),
            Error::ImageError(ref e) => e.description(),
        }
    }
}

pub async fn run_api(musicd: Arc<crate::Musicd>, bind: SocketAddr) {
    let make_service = make_service_fn(move |_socket: &AddrStream| {
        let musicd = musicd.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                process_request(req, musicd.clone())
            }))
        }
    });

    Server::bind(&bind)
        .serve(make_service)
        .await
        .expect("running server failed");
}

struct ApiRequest {
    request: Request<Body>,
    musicd: Arc<Musicd>,
    query: HttpQuery,
}

async fn process_request(
    request: Request<Body>,
    musicd: Arc<Musicd>,
) -> Result<Response<Body>, hyper::Error> {
    let query = HttpQuery::from(request.uri().query().unwrap_or_default());

    let api_request = ApiRequest {
        request,
        musicd,
        query,
    };

    let result = match (
        api_request.request.method(),
        api_request.request.uri().path(),
    ) {
        (&Method::GET, "/api/musicd") => api_musicd(&api_request),
        (&Method::GET, "/api/audio_stream") => api_audio_stream(&api_request),
        (&Method::GET, "/api/image_file") => api_image_file(&api_request),
        (&Method::GET, "/api/track_lyrics") => api_track_lyrics(&api_request),
        (&Method::GET, "/api/nodes") => api_nodes(&api_request),
        (&Method::GET, "/api/tracks") => api_tracks(&api_request),
        (&Method::GET, "/api/artists") => api_artists(&api_request),
        (&Method::GET, "/api/albums") => api_albums(&api_request),
        (&Method::GET, "/api/images") => api_images(&api_request),
        (&Method::GET, "/share") => res_share(&api_request),
        _ => Ok(not_found()),
    };

    match result {
        Ok(res) => Ok(res),
        Err(_e) => Ok(server_error()),
    }
}

static BAD_REQUEST: &[u8] = b"Bad Request";
static NOT_FOUND: &[u8] = b"Not Found";
static INTERNAL_SERVER_ERROR: &[u8] = b"Internal Server Error";

fn bad_request() -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(BAD_REQUEST.into())
        .unwrap()
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(NOT_FOUND.into())
        .unwrap()
}

fn server_error() -> Response<Body> {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(INTERNAL_SERVER_ERROR.into())
        .unwrap()
}

fn json_ok(json: &str) -> Response<Body> {
    Response::builder()
        .header("Content-Type", "application/json; charset=utf8")
        .body(json.to_string().into())
        .unwrap()
}

fn api_musicd(_: &ApiRequest) -> Result<Response<Body>, Error> {
    Ok(json_ok("{}"))
}

fn api_audio_stream(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let track_id = match r.query.get_i64("track_id") {
        Some(id) => id,
        None => {
            return Ok(bad_request());
        }
    };

    let start = r.query.get_i64("start").unwrap_or(0) as f64;
    if start < 0f64 {
        return Ok(bad_request());
    }

    let index = r.musicd.index();

    let track = match index.track(track_id)? {
        Some(t) => t,
        None => {
            return Ok(not_found());
        }
    };

    let node = index.node(track.node_id)?.unwrap();
    let fs_path = index.map_fs_path(&node.path).unwrap();

    let start = start + track.start.unwrap_or(0f64);

    let audio_stream = AudioStream::open(
        &fs_path,
        track.stream_index as i32,
        track.track_index.unwrap_or(0) as i32,
        start,
        if track.start.is_some() {
            track.length - start
        } else {
            0f64
        },
        "mp3",
    );

    let audio_stream = match audio_stream {
        Some(s) => s,
        None => {
            error!(
                "can't open audio stream from '{}'",
                fs_path.to_string_lossy()
            );
            return Ok(server_error());
        }
    };

    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<Vec<u8>, Box<dyn StdError + Send + Sync>>>(5);

    tokio::spawn(async move {
        audio_stream.execute(sender).await;
    });

    Ok(Response::builder()
        .header("Content-Type", "audio/mpeg")
        .body(Body::wrap_stream(receiver))
        .unwrap())
}

fn api_image_file(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let image_id = match r.query.get_i64("image_id") {
        Some(id) => id,
        None => {
            return Ok(bad_request());
        }
    };

    let size = r.query.get_i64("size").unwrap_or(0);

    let index = r.musicd.index();

    let image = match index.image(image_id)? {
        Some(i) => i,
        None => {
            return Ok(not_found());
        }
    };

    let cache_str = format!("image:{}_{}", image_id, size);

    let cache = r.musicd.cache();
    let image_data = if let Some(image_data) = cache.get_blob(&cache_str)? {
        image_data
    } else {
        let node = index.node(image.node_id)?.unwrap();
        let fs_path = match index.map_fs_path(&node.path) {
            Some(p) => p,
            None => {
                return Ok(not_found());
            }
        };

        let mut image_obj = if let Some(stream_index) = image.stream_index {
            let image_data = match media::media_image_data_read(&fs_path, stream_index as i32) {
                Some(i) => i,
                None => {
                    return Ok(not_found());
                }
            };

            image::load_from_memory_with_format(&image_data, image::ImageFormat::JPEG)
        } else {
            image::open(&fs_path)
        }?;

        if size > 0 && size < std::cmp::max(image.width, image.height) {
            image_obj = image_obj.resize(size as u32, size as u32, image::FilterType::Lanczos3);
        }

        let mut c = Cursor::new(Vec::new());
        image_obj.write_to(&mut c, image::ImageOutputFormat::JPEG(70))?;

        c.seek(SeekFrom::Start(0))?;
        let mut image_data = Vec::new();
        c.read_to_end(&mut image_data)?;

        cache.set_blob(&cache_str, &image_data)?;

        image_data
    };

    Ok(Response::builder()
        .header("Content-Type", "image/jpeg")
        .body(image_data.into())
        .unwrap())
}

fn api_track_lyrics(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let track_id = match r.query.get_i64("track_id") {
        Some(id) => id,
        None => {
            return Ok(bad_request());
        }
    };

    let index = r.musicd.index();

    let track = match index.track(track_id)? {
        Some(t) => t,
        None => {
            return Ok(not_found());
        }
    };

    let lyrics = if let Some(track_lyrics) = index.track_lyrics(track_id)? {
        track_lyrics
    } else {
        let track_lyrics = match lyrics::try_fetch_lyrics(&track.artist_name, &track.title) {
            Ok(lyrics) => match lyrics {
                Some(l) => TrackLyrics {
                    track_id,
                    lyrics: Some(l.lyrics),
                    provider: Some(l.provider),
                    source: Some(l.source),
                    modified: 0,
                },
                None => TrackLyrics {
                    track_id,
                    lyrics: None,
                    provider: None,
                    source: None,
                    modified: 0,
                },
            },
            Err(e) => {
                error!("fetching lyrics failed: {}", e.description());
                return Ok(server_error());
            }
        };

        index.set_track_lyrics(&track_lyrics)?
    };

    Ok(json_ok(
        &json!({
            "track_id": lyrics.track_id,
            "lyrics": lyrics.lyrics,
            "provider": lyrics.provider,
            "source": lyrics.source,
            "modified": lyrics.modified,
        })
        .to_string(),
    ))
}

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
struct NodeItem {
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

fn api_nodes(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let query = &r.query;
    let mut opts = QueryOptions::new();

    if let Some(parent_id) = query.get_str("parent_id") {
        if let Ok(parent_id) = parent_id.parse::<i64>() {
            opts.filter_value("Node.parent_id = ?", parent_id);
        } else if parent_id == "null" {
            opts.filter("Node.parent_id IS NULL");
        }
    }

    opts.bind_range(&query);

    let index = r.musicd.index();
    let conn = index.connection();

    let total = opts.get_total(&conn, "SELECT COUNT(Node.node_id) FROM Node")?;

    let (mut st, values) = opts.into_items_query(
        &conn,
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

        FROM Node",
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

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

#[derive(Serialize)]
struct TrackItem {
    track_id: i64,
    node_id: i64,
    number: i64,
    title: String,
    artist_id: i64,
    artist_name: String,
    album_id: i64,
    album_name: String,
    length: f64,
}

fn api_tracks(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let query = &r.query;
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

    let index = r.musicd.index();
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
            Track.length
        FROM Track",
    )?;

    let mut rows = st.query(&values)?;

    let mut items: Vec<TrackItem> = Vec::new();

    while let Some(row) = rows.next()? {
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
        });
    }

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

#[derive(Serialize)]
struct ArtistItem {
    artist_id: i64,
    name: String,
    track_count: i64,
}

fn api_artists(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let query = &r.query;
    let mut opts = QueryOptions::new();

    opts.bind_filter_i64(&query, "artist_id", "Artist.artist_id = ?");
    opts.bind_filter_str(&query, "name", "Artist.name LIKE ? COLLATE NOCASE");
    opts.bind_filter_str(&query, "search", "Artist.name LIKE ? COLLATE NOCASE");

    opts.order_string("Artist.name");

    opts.bind_range(&query);

    let index = r.musicd.index();
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

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

#[derive(Serialize)]
struct AlbumItem {
    album_id: i64,
    name: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    image_id: Option<i64>,
    track_count: i64,
}

fn api_albums(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let query = &r.query;
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

    let index = r.musicd.index();
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

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

#[derive(Serialize)]
struct ImageItem {
    image_id: i64,
    node_id: i64,
    description: String,
}

fn api_images(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let query = &r.query;
    let mut opts = QueryOptions::new();

    opts.bind_filter_i64(&query, "image_id", "Image.image_id = ?");
    opts.bind_filter_i64(&query, "node_id", "Image.node_id = ?");
    opts.bind_filter_str(&query, "description", "Image.description = ?");
    opts.bind_filter_i64(&query, "album_id", "(SELECT album_id FROM AlbumImage WHERE AlbumImage.album_id = ? AND AlbumImage.image_id = Image.image_id LIMIT 1) IS NOT NULL");

    opts.bind_range(&query);

    let index = r.musicd.index();
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

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

static SHARE_HTML: &[u8] = include_bytes!("./share.html");

fn res_share(_r: &ApiRequest) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .header("Content-Type", "text/html; charset=utf-8")
        .body(SHARE_HTML.into())
        .unwrap())
}
