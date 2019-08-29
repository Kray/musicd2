#[macro_use]
extern crate log;

mod audio_stream;
mod cache;
mod cue;
mod db_meta;
mod http;
mod http_api;
mod index;
mod logger;
mod media_image;
mod media_info;
mod musicd_c;
mod scan;
mod schema;
mod server;
mod store;
mod stream_thread;

use std::ffi::OsStr;
use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Arg;

use cache::{Cache, CacheSource};
use index::{Index, IndexSource};
use server::Server;
use store::{Store, StoreSource};
use stream_thread::StreamThread;

pub struct Musicd {
    cache_source: CacheSource,
    index_source: IndexSource,
    store_source: StoreSource,
}

pub struct Root {
    pub name: String,
    pub path: PathBuf,
}

pub const MUSICD_VERSION: &str = env!("CARGO_PKG_VERSION");

impl Musicd {
    pub fn cache(&self) -> Cache {
        self.cache_source.get().expect("can't open cache")
    }

    pub fn index(&self) -> Index {
        self.index_source.get().expect("can't open index")
    }

    pub fn store(&self) -> Store {
        self.store_source
            .get(self.index())
            .expect("can't open store")
    }
}

fn main() {
    let matches = clap::App::new("musicd2")
        .version(MUSICD_VERSION)
        .arg(
            Arg::with_name("bind")
                .long("bind")
                .help("HTTP server address and port")
                .default_value("127.0.0.1:6801"),
        )
        .arg(
            Arg::with_name("cache-limit")
                .long("cache-limit")
                .help("Maximum cache size in bytes")
                .default_value("104857600"),
        )
        .arg(
            Arg::with_name("directory")
                .long("directory")
                .help("Database directory")
                .default_value("~/.musicd2"),
        )
        .arg(
            Arg::with_name("log-level")
                .long("log-level")
                .help("Log level")
                .default_value("info")
                .possible_values(&["error", "warn", "info", "debug", "trace"]),
        )
        .arg(
            Arg::with_name("no-scan")
                .long("no-scan")
                .help("Disable scanning"),
        )
        .arg(
            Arg::with_name("root")
                .long("root")
                .help("One or more music root directories with unique names")
                .value_names(&["name", "path"])
                .takes_value(true)
                .multiple(true)
                .number_of_values(2),
        )
        .get_matches();

    let bind: SocketAddr = matches
        .value_of("bind")
        .unwrap()
        .parse()
        .expect("invalid bind address");

    let cache_limit = clap::value_t_or_exit!(matches.value_of("cache-limit"), usize);

    let directory = &shellexpand::tilde(matches.value_of("directory").unwrap()).into_owned();
    let directory = Path::new(directory);

    logger::init(matches.value_of("log-level").unwrap());

    info!("{}", MUSICD_VERSION);

    musicd_c::init();

    let mut roots: Vec<Root> = Vec::new();

    if matches.is_present("root") {
        let mut root_iter = matches.values_of("root").unwrap();
        while let Some(name) = root_iter.next() {
            if let Some(path) = root_iter.next() {
                roots.push(Root {
                    name: name.to_string(),
                    path: Path::new(OsStr::from_bytes(shellexpand::tilde(path).as_bytes()))
                        .to_path_buf(),
                });
            }
        }
    }

    let roots = Arc::new(roots);

    std::fs::create_dir_all(directory).expect("can't create directory");

    let cache_source = CacheSource::create(directory.join("cache.db"), cache_limit)
        .unwrap()
        .unwrap();

    let index_source = IndexSource::create(directory.join("index.db"), roots.clone())
        .unwrap()
        .unwrap();

    let store_source = StoreSource::create(directory.join("store.db"), index_source.get().unwrap())
        .unwrap()
        .unwrap();

    let musicd = Arc::new(Musicd {
        cache_source,
        index_source,
        store_source,
    });

    let index = musicd.index();

    if !matches.is_present("no-scan") {
        scan::scan(index);
    }

    let mut store = musicd.store();
    store.synchronize().unwrap();

    let (server_incoming, server_streaming) = Server::launch_new().unwrap();

    let stream_thread = Arc::new(StreamThread::launch_new(server_streaming).unwrap());

    http_api::run_api(musicd.clone(), bind, server_incoming, stream_thread.clone());
}
