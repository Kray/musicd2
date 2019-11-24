pub const SCHEMA_VERSION: u32 = 1;

pub const META_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS Musicd (
    key TEXT PRIMARY KEY,
    value);
";

pub const CACHE_SCHEMA: &str = "
CREATE TABLE Cache (
    key TEXT PRIMARY KEY,
    value BLOB,
    size INTEGER,
    last_access INTEGER);
";

pub const INDEX_SCHEMA: &str = "
CREATE TABLE Node (
    node_id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_type INTEGER NOT NULL,
    parent_id INTEGER,
    master_id INTEGER,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    modified INTEGER NOT NULL,
    FOREIGN KEY(parent_id) REFERENCES Node(node_id) ON DELETE CASCADE,
    FOREIGN KEY(master_id) REFERENCES Node(node_id) ON DELETE SET NULL);

CREATE INDEX Node_parent_id ON Node (parent_id);
CREATE INDEX Node_master_id ON Node (master_id);
    
CREATE TABLE Track (
    track_id INTEGER PRIMARY KEY AUTOINCREMENT, 
    node_id INTEGER NOT NULL,
    stream_index INTEGER NOT NULL,
    track_index INTEGER,
    start REAL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    artist_id INTEGER NOT NULL,
    artist_name TEXT NOT NULL,
    album_id INTEGER NOT NULL,
    album_name TEXT NOT NULL,
    album_artist_id INTEGER,
    album_artist_name TEXT,
    length REAL NOT NULL,
    FOREIGN KEY(node_id) REFERENCES Node(node_id) ON DELETE CASCADE,
    FOREIGN KEY(artist_id) REFERENCES Artist(artist_id),
    FOREIGN KEY(album_id) REFERENCES Album(album_id),
    FOREIGN KEY(album_artist_id) REFERENCES Artist(artist_id));

CREATE INDEX Track_node_id ON Track (node_id);
CREATE INDEX Track_artist_id ON Track (artist_id);
CREATE INDEX Track_album_id ON Track (album_id);
CREATE INDEX Track_album_artist_id ON Track (album_artist_id);
    
CREATE TABLE Image (
    image_id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id INTEGER NOT NULL,
    stream_index INTEGER,
    description TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    FOREIGN KEY(node_id) REFERENCES Node(node_id) ON DELETE CASCADE);

CREATE INDEX Image_node_id ON Image (node_id);
    
CREATE TABLE Artist (
    artist_id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL);
    
CREATE TABLE Album (
    album_id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    artist_id INTEGER,
    artist_name TEXT,
    image_id INTEGER,
    FOREIGN KEY(image_id) REFERENCES Image(image_id) ON DELETE SET NULL);

CREATE INDEX Album_artist_id ON Album (album_id);

CREATE TABLE AlbumImage (
    album_id INTEGER NOT NULL,
    image_id INTEGER NOT NULL,
    PRIMARY KEY(album_id, image_id),
    FOREIGN KEY(album_id) REFERENCES Album(album_id) ON DELETE CASCADE,
    FOREIGN KEY(image_id) REFERENCES Image(image_id) ON DELETE CASCADE);

CREATE TABLE AlbumImagePattern (
    pattern TEXT);

CREATE TABLE TrackLyrics (
    track_id INTEGER PRIMARY KEY,
    lyrics TEXT,
    provider TEXT,
    source TEXT,
    modified INTEGER NOT NULL,
    FOREIGN KEY(track_id) REFERENCES Track(track_id) ON DELETE CASCADE);
    
CREATE TABLE StoreTrack (
    store_track_id INTEGER PRIMARY KEY,
    track_id INTEGER NOT NULL,
    play_count INTEGER,
    last_play INTEGER,
    FOREIGN KEY(track_id) REFERENCES Track(track_id) ON DELETE CASCADE);
    
CREATE TABLE StoreList (
    list_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL);

CREATE TABLE StoreListTrack (
    list_id INTEGER NOT NULL,
    store_track_id INTEGER NOT NULL,
    FOREIGN KEY(list_id) REFERENCES StoreList(list_id) ON DELETE CASCADE,
    FOREIGN KEY(store_track_id) REFERENCES StoreTrack(store_track_id) ON DELETE CASCADE);
";

pub const STORE_SCHEMA: &str = "
CREATE TABLE Track (
    store_track_id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT NOT NULL,
    length INTEGER NOT NULL,
    play_count INTEGER,
    last_play INTEGER);

CREATE TABLE List (
    list_id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL);

CREATE TABLE ListTrack (
    list_id INTEGER NOT NULL,
    store_track_id INTEGER NOT NULL,
    sort_index INTEGER,
    FOREIGN KEY(list_id) REFERENCES List(list_id) ON DELETE CASCADE,
    FOREIGN KEY(store_track_id) REFERENCES Track(store_track_id) ON DELETE CASCADE);
";
