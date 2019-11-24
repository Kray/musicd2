use std::collections::HashMap;
use std::error::Error as StdError;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use serde_json::json;

use crate::audio_stream::AudioStream;
use crate::http_util::HttpQuery;
use crate::index::TrackLyrics;
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

    info!("listening on {}", bind);

    Server::bind(&bind)
        .serve(make_service)
        .await
        .expect("running server failed");
}

static OK: &[u8] = b"OK";
static BAD_REQUEST: &[u8] = b"Bad Request";
static UNAUTHORIZED: &[u8] = b"Unauthorized";
static NOT_FOUND: &[u8] = b"Not Found";
static INTERNAL_SERVER_ERROR: &[u8] = b"Internal Server Error";

fn bad_request() -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(BAD_REQUEST.into())
        .unwrap()
}

fn unauthorized() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(UNAUTHORIZED.into())
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

struct ApiRequest {
    request: Request<Body>,
    musicd: Arc<Musicd>,
    query: HttpQuery,
    cookies: HashMap<String, String>,
}

async fn process_request(
    request: Request<Body>,
    musicd: Arc<Musicd>,
) -> Result<Response<Body>, hyper::Error> {
    debug!("request {}", request.uri());

    let query = HttpQuery::from(request.uri().query().unwrap_or_default());

    let cookies = match crate::http_util::parse_cookies(request.headers()) {
        Ok(c) => c,
        Err(e) => {
            debug!("invalid cookies {}", e);
            return Ok(bad_request());
        }
    };

    let api_request = ApiRequest {
        request,
        musicd,
        query,
        cookies,
    };

    let result = match (
        api_request.request.method(),
        api_request.request.uri().path(),
    ) {
        (&Method::GET, "/api/musicd") => Some(api_musicd(&api_request)),
        (&Method::GET, "/api/auth") => Some(api_auth(&api_request)),
        _ => None,
    };

    if let Some(result) = result {
        return match result {
            Ok(res) => Ok(res),
            Err(_e) => Ok(server_error()),
        };
    }

    if let Some(auth_password) = api_request.cookies.get("musicd2-auth") {
        if !api_request.musicd.password.is_empty() && api_request.musicd.password != *auth_password
        {
            debug!("invalid auth");
            return Ok(unauthorized());
        }
    }

    let result = match (
        api_request.request.method(),
        api_request.request.uri().path(),
    ) {
        (&Method::GET, "/api/audio_stream") => api_audio_stream(&api_request),
        (&Method::GET, "/api/image_file") => api_image_file(&api_request),
        (&Method::GET, "/api/track_lyrics") => api_track_lyrics(&api_request).await,
        (&Method::GET, "/api/nodes") => api_nodes(&api_request),
        (&Method::GET, "/api/tracks") => api_tracks(&api_request),
        (&Method::GET, "/api/artists") => api_artists(&api_request),
        (&Method::GET, "/api/albums") => api_albums(&api_request),
        (&Method::GET, "/api/images") => api_images(&api_request),
        (&Method::GET, "/api/scan") => api_scan(&api_request),
        (&Method::POST, "/api/scan") => api_scan(&api_request),
        (&Method::GET, "/share") => res_share(&api_request),
        _ => Ok(not_found()),
    };

    match result {
        Ok(res) => Ok(res),
        Err(_e) => Ok(server_error()),
    }
}

fn api_musicd(_: &ApiRequest) -> Result<Response<Body>, Error> {
    Ok(json_ok("{}"))
}

fn api_auth(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let password = r.query.get_str("password").unwrap_or_default();
    if r.musicd.password != password {
        return Ok(unauthorized());
    }

    Ok(Response::builder()
        .header("Set-Cookie", format!("musicd2-auth={}", r.musicd.password))
        .body(OK.into())
        .unwrap())
}

static CODECS: &[(&str, &str)] = &[
    ("mp3", "audio/mpeg"),
    ("opus", "audio/ogg"),
    ("ogg", "audio/ogg"),
];

fn api_audio_stream(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let track_id = match r.query.get_i64("track_id") {
        Some(id) => id,
        None => {
            return Ok(bad_request());
        }
    };

    let codec_req = r.query.get_str("codec").unwrap_or(CODECS[0].0);
    let target_codec = match CODECS.iter().find(|c| c.0 == codec_req) {
        Some(c) => c,
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
        target_codec.0,
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
        .header("Content-Type", target_codec.1)
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

async fn api_track_lyrics(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let track_id = match r.query.get_i64("track_id") {
        Some(id) => id,
        None => {
            return Ok(bad_request());
        }
    };

    let (track, lyrics) = {
        let index = r.musicd.index();

        (
            match index.track(track_id)? {
                Some(t) => t,
                None => {
                    return Ok(not_found());
                }
            },
            index.track_lyrics(track_id)?,
        )
    };

    let lyrics = match lyrics {
        Some(lyrics) => lyrics,
        None => {
            let lyrics = match lyrics::try_fetch_lyrics(&track.artist_name, &track.title).await {
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

            r.musicd.index().set_track_lyrics(&lyrics)?
        }
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

fn api_nodes(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let (total, items) = crate::query::query_nodes(&r.musicd.index(), &r.query)?;

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

fn api_tracks(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let (total, items) = crate::query::query_tracks(&r.musicd.index(), &r.query)?;

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

fn api_artists(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let (total, items) = crate::query::query_artists(&r.musicd.index(), &r.query)?;

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

fn api_albums(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let (total, items) = crate::query::query_albums(&r.musicd.index(), &r.query)?;

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

fn api_images(r: &ApiRequest) -> Result<Response<Body>, Error> {
    let (total, items) = crate::query::query_images(&r.musicd.index(), &r.query)?;

    Ok(json_ok(
        &json!({
            "total": total,
            "items": items
        })
        .to_string(),
    ))
}

fn api_scan(r: &ApiRequest) -> Result<Response<Body>, Error> {
    if let Some(action) = r.query.get_str("action") {
        match action {
            "start" => r.musicd.scan_thread.start(r.musicd.index()),
            "restart" => {
                r.musicd.scan_thread.stop();
                r.musicd.scan_thread.start(r.musicd.index());
            }
            "stop" => {
                r.musicd.scan_thread.stop();
            }
            _ => {}
        }
    }

    Ok(json_ok(
        &json!({
            "running": r.musicd.scan_thread.is_running()
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
