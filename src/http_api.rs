use std::error::Error as StdError;
use std::ffi::OsStr;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use rusqlite::types::ToSql;
use serde::Serialize;
use serde_json::json;
use threadpool::ThreadPool;

use crate::audio_stream::AudioStream;
use crate::http::{self, HttpError, HttpRequest, HttpResponse};
use crate::index::NodeType;
use crate::media_image;
use crate::server::{Client, Receive, ServerIncoming};
use crate::stream_thread::StreamThread;
use crate::Musicd;

#[derive(Debug)]
pub enum Error {
    HttpError(HttpError),
    IoError(std::io::Error),
    DatabaseError(rusqlite::Error),
    ImageError(image::ImageError),
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
            Error::HttpError(ref e) => e.description(),
            Error::IoError(ref e) => e.description(),
            Error::DatabaseError(ref e) => e.description(),
            Error::ImageError(ref e) => e.description(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

struct ApiRequest {
    request: HttpRequest,
    musicd: Arc<Musicd>,
    client: Option<Client>,
}

impl ApiRequest {
    fn err<T>(&self, code: i32, description: &str) -> Result<T> {
        Err(Error::HttpError(HttpError::new(code, description)))
    }

    fn json(&self, json: &str) -> Result<HttpResponse> {
        let mut response = HttpResponse::new();

        response
            .status("200 OK")
            .content_type("application/json; charset=utf-8")
            .text_body(json);

        Ok(response)
    }

    fn take_client(&mut self) -> Client {
        self.client.take().unwrap()
    }
}

pub fn run_api(
    musicd: Arc<crate::Musicd>,
    server: ServerIncoming,
    stream_thread: Arc<StreamThread>,
) {
    let threadpool = ThreadPool::new(4);

    let server = Arc::new(server);

    loop {
        let (client, request) = server
            .receive_next_fn(|b| match http::parse_request_headers(b) {
                Ok(r) => match r {
                    Some(r) => Receive::Receive(r),
                    None => Receive::None,
                },
                Err(_) => {
                    error!("invalid http headers");
                    Receive::Invalid
                }
            })
            .unwrap();

        let mut api_request = ApiRequest {
            request,
            musicd: musicd.clone(),
            client: Some(client),
        };

        let stream_thread = stream_thread.clone();

        threadpool.execute(move || {
            debug!("{}", api_request.request.path());

            let result = match api_request.request.path() {
                "/api/musicd" => api_musicd(&api_request),
                "/api/open" => api_open(&mut api_request, &stream_thread),
                "/api/image" => api_image(&api_request),
                "/api/nodes" => api_nodes(&api_request),
                "/api/tracks" => api_tracks(&api_request),
                "/api/artists" => api_artists(&api_request),
                "/api/albums" => api_albums(&api_request),
                _ => {
                    let mut response = HttpResponse::new();
                    response.status("404 Not Found").text_body("404 Not Found");

                    Ok(response)
                }
            };

            match result {
                Ok(response) => {
                    if let Some(client) = api_request.client {
                        client.send(&response.to_bytes()).unwrap();
                    }
                }
                Err(e) => {
                    let response = match e {
                        Error::HttpError(e) => {
                            let mut response = HttpResponse::new();
                            response.status(&e.to_string()).text_body(&e.to_string());
                            response
                        }
                        _ => {
                            error!("error processing request: {}", e.description());

                            let mut response = HttpResponse::new();
                            response
                                .status("500 Internal Server Error")
                                .text_body("500 Internal Server Error");
                            response
                        }
                    };

                    if let Some(client) = api_request.client {
                        client.send(&response.to_bytes()).unwrap();
                    } else {
                        error!(
                            "response stream already consumed when trying to send error response"
                        );
                    }
                }
            }
        });
    }
}

fn api_musicd(r: &ApiRequest) -> Result<HttpResponse> {
    r.json("{}")
}

fn api_open(r: &mut ApiRequest, stream_thread: &StreamThread) -> Result<HttpResponse> {
    let track_id = match r.request.query().get_i64("track_id") {
        Some(id) => id,
        None => {
            return r.err(400, "Bad Request");
        }
    };

    let index = r.musicd.index();

    let track = match index.track(track_id)? {
        Some(t) => t,
        None => {
            return r.err(404, "Not Found");
        }
    };

    let node = index.node(track.node_id)?.unwrap();
    let fs_path = index.map_fs_path(&node.path).unwrap();

    let audio_stream = AudioStream::open(
        &fs_path,
        track.stream_index as i32,
        track.track_index.unwrap_or(0) as i32,
        track.start.unwrap_or(0f64),
        if track.start.is_some() {
            track.length
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
            return r.err(500, "Internal Server Error");
        }
    };

    stream_thread.add_audio_stream(r.take_client(), audio_stream)?;

    Ok(HttpResponse::new())
}

fn api_image(r: &ApiRequest) -> Result<HttpResponse> {
    let image_id = match r.request.query().get_i64("image_id") {
        Some(id) => id,
        None => {
            return r.err(400, "Bad Request");
        }
    };

    let size = r.request.query().get_i64("size").unwrap_or(0);

    let index = r.musicd.index();

    let image = match index.image(image_id)? {
        Some(i) => i,
        None => {
            return r.err(404, "Not Found");
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
                return r.err(404, "Not Found");
            }
        };

        let mut image_obj = if let Some(stream_index) = image.stream_index {
            let image_data = match media_image::media_image_data_read(&fs_path, stream_index as i32)
            {
                Some(i) => i,
                None => {
                    return r.err(404, "Not Found");
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
        image_obj.write_to(&mut c, image::ImageOutputFormat::JPEG(200))?;

        c.seek(SeekFrom::Start(0))?;
        let mut image_data = Vec::new();
        c.read_to_end(&mut image_data)?;

        cache.set_blob(&cache_str, &image_data)?;

        image_data
    };

    let mut response = HttpResponse::new();
    response.content_type("image/jpeg").bytes_body(&image_data);

    Ok(response)
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

fn api_nodes(r: &ApiRequest) -> Result<HttpResponse> {
    let query = r.request.query();

    let index = r.musicd.index();
    let conn = index.connection();

    let mut clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();

    if let Some(parent_id) = query.get_str("parent_id") {
        if let Ok(parent_id) = parent_id.parse::<i64>() {
            clauses.push("Node.parent_id = ?");
            values.push(Box::new(parent_id));
        } else if parent_id == "null" {
            clauses.push("Node.parent_id IS NULL");
        }
    }

    let mut sql = "SELECT COUNT(Node.node_id) FROM Node".to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    let mut st = conn.prepare(&sql)?;

    let total_results: i64 = st.query_row(&values, |row| row.get(0))?;

    let mut sql = "
            SELECT
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
        .to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    if let Some(limit) = query.get_i64("limit") {
        sql += " LIMIT ?";
        values.push(Box::new(limit));
    }

    if let Some(offset) = query.get_i64("offset") {
        sql += " OFFSET ?";
        values.push(Box::new(offset));
    }

    let mut st = conn.prepare(&sql)?;

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

    r.json(
        &json!({
            "total": total_results,
            "items": items
        })
        .to_string(),
    )
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

fn api_tracks(r: &ApiRequest) -> Result<HttpResponse> {
    let query = r.request.query();

    let index = r.musicd.index();
    let conn = index.connection();

    let mut clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();

    if let Some(track_id) = query.get_i64("track_id") {
        clauses.push("Track.track_id = ?");
        values.push(Box::new(track_id));
    }

    if let Some(node_id) = query.get_i64("node_id") {
        clauses.push("Track.node_id = ?");
        values.push(Box::new(node_id));
    }

    if let Some(number) = query.get_i64("number") {
        clauses.push("Track.number = ?");
        values.push(Box::new(number));
    }

    if let Some(title) = query.get_str("title") {
        clauses.push("Track.title LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", title)));
    }

    if let Some(artist_id) = query.get_i64("artist_id") {
        clauses.push("Track.artist_id = ?");
        values.push(Box::new(artist_id));
    }

    if let Some(artist_name) = query.get_str("artist_name") {
        clauses.push("Track.artist_name LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", artist_name)));
    }

    if let Some(album_id) = query.get_i64("album_id") {
        clauses.push("Track.album_id = ?");
        values.push(Box::new(album_id));
    }

    if let Some(album_name) = query.get_str("album_name") {
        clauses.push("Track.album_name LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", album_name)));
    }

    if let Some(search) = query.get_str("search") {
        clauses.push("(Track.title LIKE ? OR Track.artist_name LIKE ? OR Track.album_name LIKE ?)");
        values.push(Box::new(format!("%{}%", search)));
        values.push(Box::new(format!("%{}%", search)));
        values.push(Box::new(format!("%{}%", search)));
    }

    let mut sql = "SELECT COUNT(Track.track_id) FROM Track".to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    let mut st = conn.prepare(&sql)?;

    let total_results: i64 = st.query_row(&values, |row| row.get(0))?;

    let mut sql = "
            SELECT
                Track.track_id,
                Track.node_id,
                Track.number,
                Track.title,
                Track.artist_id,
                Track.artist_name,
                Track.album_id,
                Track.album_name,
                Track.length
            FROM Track"
        .to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    if let Some(limit) = query.get_i64("limit") {
        sql += " LIMIT ?";
        values.push(Box::new(limit));
    }

    if let Some(offset) = query.get_i64("offset") {
        sql += " OFFSET ?";
        values.push(Box::new(offset));
    }

    let mut st = conn.prepare(&sql)?;

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

    r.json(
        &json!({
            "total": total_results,
            "items": items
        })
        .to_string(),
    )
}

#[derive(Serialize)]
struct ArtistItem {
    artist_id: i64,
    name: String,
    track_count: i64,
}

fn api_artists(r: &ApiRequest) -> Result<HttpResponse> {
    let query = r.request.query();

    let index = r.musicd.index();
    let conn = index.connection();

    let mut clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();

    if let Some(artist_id) = query.get_i64("artist_id") {
        clauses.push("Artist.artist_id = ?");
        values.push(Box::new(artist_id));
    }

    if let Some(name) = query.get_str("name") {
        clauses.push("Artist.name LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", name)));
    }

    let mut sql = "SELECT COUNT(Artist.artist_id) FROM Artist".to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    let mut st = conn.prepare(&sql)?;

    let total_results: i64 = st.query_row(&values, |row| row.get(0))?;

    let mut sql = "
            SELECT
                Artist.artist_id,
                Artist.name,
                (SELECT count(Track.track_id) FROM Track WHERE Track.artist_id = Artist.artist_id) AS track_count
            FROM Artist"
        .to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    if let Some(limit) = query.get_i64("limit") {
        sql += " LIMIT ?";
        values.push(Box::new(limit));
    }

    if let Some(offset) = query.get_i64("offset") {
        sql += " OFFSET ?";
        values.push(Box::new(offset));
    }

    let mut st = conn.prepare(&sql)?;

    let mut rows = st.query(&values)?;

    let mut items: Vec<ArtistItem> = Vec::new();

    while let Some(row) = rows.next()? {
        items.push(ArtistItem {
            artist_id: row.get(0)?,
            name: row.get(1)?,
            track_count: row.get(2)?,
        });
    }

    r.json(
        &json!({
            "total": total_results,
            "items": items
        })
        .to_string(),
    )
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

fn api_albums(r: &ApiRequest) -> Result<HttpResponse> {
    let query = r.request.query();

    let index = r.musicd.index();
    let conn = index.connection();

    let mut clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();

    if let Some(album_id) = query.get_i64("album_id") {
        clauses.push("Album.album_id = ?");
        values.push(Box::new(album_id));
    }

    if let Some(name) = query.get_str("name") {
        clauses.push("Album.name LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", name)));
    }

    if let Some(node_id) = query.get_i64("artist_id") {
        clauses.push("Album.artist_id = ?");
        values.push(Box::new(node_id));
    }

    if let Some(artist_name) = query.get_str("artist_name") {
        clauses.push("Album.artist_name LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", artist_name)));
    }

    if let Some(search) = query.get_str("search") {
        clauses.push("Album.name LIKE ? COLLATE NOCASE");
        values.push(Box::new(format!("%{}%", search)));
    }

    let mut sql = "SELECT COUNT(Album.album_id) FROM Album".to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    let mut st = conn.prepare(&sql)?;

    let total_results: i64 = st.query_row(&values, |row| row.get(0))?;

    let mut sql = "
            SELECT
                Album.album_id,
                Album.name,
                Album.artist_id,
                Album.artist_name,
                Album.image_id,
                (SELECT count(Track.track_id) FROM Track WHERE Track.album_id = Album.album_id) AS track_count
            FROM Album"
        .to_string();

    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }

    if let Some(limit) = query.get_i64("limit") {
        sql += " LIMIT ?";
        values.push(Box::new(limit));
    }

    if let Some(offset) = query.get_i64("offset") {
        sql += " OFFSET ?";
        values.push(Box::new(offset));
    }

    let mut st = conn.prepare(&sql)?;

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

    r.json(
        &json!({
            "total": total_results,
            "items": items
        })
        .to_string(),
    )
}
