use std::convert::From;
use std::error::Error as StdError;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cue;
use crate::index::{Image, Index, Node, NodeType, Track};
use crate::media_info;

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    DatabaseError(rusqlite::Error),
    OtherError,
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

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

impl StdError for Error {
    fn description(&self) -> &str {
        match *self {
            Error::IoError(ref e) => e.description(),
            Error::DatabaseError(ref e) => e.description(),
            Error::OtherError => "Other error",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

struct Scan {
    index: Index,
}

struct ScanNode {
    node: Node,
    fs_path: PathBuf,
    modified: i64,
}

#[derive(Debug, Default)]
struct ScanStat {
    tracks: i32,
    images: i32,
}

impl ScanStat {
    fn add(&mut self, other: &ScanStat) {
        self.tracks += other.tracks;
        self.images += other.images;
    }

    fn changed(&self) -> bool {
        self.tracks > 0 || self.images > 0
    }
}

pub fn scan(index: Index) {
    let mut scan = Scan { index };

    scan.scan_core().unwrap();
}

impl Scan {
    fn scan_core(&mut self) -> Result<()> {
        info!("started");

        self.index
            .connection()
            .execute_batch("DELETE FROM AlbumImagePattern;")?;

        self.index.connection().execute_batch(
            "
                INSERT INTO AlbumImagePattern (pattern)
                VALUES
                    ('album cover'),
                    ('albumcover'),
                    ('albumart'),
                    ('album'),
                    ('front'),
                    ('folder'),
                    ('front%'),
                    ('cover%'),
                    ('folder%'),
                    ('%front%'),
                    ('%cover%'),
                    ('%folder%'),
                    ('%albumart%'),
                    ('%album%'),
                    ('%jacket%'),
                    ('%card%')",
        )?;

        let mut stat = ScanStat {
            ..Default::default()
        };

        let roots: Vec<(String, PathBuf)> = self
            .index
            .roots()
            .iter()
            .map(|r| (r.name.to_string(), r.path.to_path_buf()))
            .collect();

        let start_instant = Instant::now();

        for (name, path) in roots {
            debug!("root '{}' = '{}'", name, path.to_string_lossy());

            match self.scan_node(None, Path::new(OsStr::from_bytes(name.as_bytes()))) {
                Ok(s) => {
                    if let Some(s) = s {
                        stat.add(&s);
                    }
                }
                Err(e) => {
                    error!(
                        "can't scan root '{}' -> '{}': {}",
                        name,
                        path.to_string_lossy(),
                        e.description()
                    );
                }
            }
        }

        info!(
            "complete in {}s: {:?}",
            start_instant.elapsed().as_secs(),
            stat
        );

        Ok(())
    }

    fn scan_node(&mut self, parent: Option<&Node>, name: &Path) -> Result<Option<ScanStat>> {
        let ScanNode {
            node,
            fs_path,
            modified,
        } = self.prepare_node(parent, name)?;

        if node.node_type == NodeType::Directory {
            let result = self.process_directory_node(&node, &fs_path)?;

            if let Some(result) = &result {
                if result.changed() {
                    self.index.process_node_updates(node.node_id)?;
                }
            }

            self.index.set_node_modified(node.node_id, modified)?;

            Ok(result)
        } else if node.node_type == NodeType::File && node.modified != modified {
            let parent = match parent {
                Some(n) => n,
                None => {
                    error!("root node '{}' isn't directory", name.to_string_lossy());
                    return Err(Error::OtherError);
                }
            };

            let result = if let Some(_master_id) = node.master_id {
                // TODO should this trigger master rescan?
                None
            } else {
                self.index.clear_node(node.node_id)?;

                self.process_file_node(parent, &node, &fs_path)?
            };

            self.index.set_node_modified(node.node_id, modified)?;

            Ok(result)
        } else {
            Ok(None)
        }
    }

    fn prepare_node(&mut self, parent: Option<&Node>, name: &Path) -> Result<ScanNode> {
        let parent_id = match parent {
            Some(node) => Some(node.node_id),
            None => None,
        };

        let path = match parent {
            Some(parent_node) => PathBuf::from(&parent_node.path).join(name),
            None => PathBuf::from(name),
        };

        let fs_path = match self.index.map_fs_path(&path) {
            Some(p) => p,
            None => {
                error!("can't map path '{}'", path.display());
                return Err(Error::OtherError);
            }
        };

        let node = self.index.node_by_name(parent_id, name)?;

        let metadata = match fs::metadata(&fs_path) {
            Ok(m) => m,
            Err(e) => {
                error!(
                    "metadata error '{}': {}",
                    fs_path.to_string_lossy(),
                    e.description()
                );

                if let Some(node) = node {
                    self.index.delete_node(node.node_id)?;
                }

                return Err(Error::OtherError);
            }
        };

        let modified = match metadata
            .modified()?
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
        {
            Ok(n) => n.as_secs(),
            Err(_) => {
                error!("invalid modified '{}'", fs_path.to_string_lossy());

                if let Some(node) = node {
                    self.index.delete_node(node.node_id)?;
                }

                return Err(Error::OtherError);
            }
        } as i64;

        let node_type = if metadata.is_dir() {
            NodeType::Directory
        } else if metadata.is_file() {
            NodeType::File
        } else {
            NodeType::Other
        };

        let node = match node {
            Some(n) => n,
            None => {
                let node = Node {
                    node_id: 0,
                    node_type,
                    parent_id: match parent {
                        Some(p) => Some(p.node_id),
                        None => None,
                    },
                    master_id: None,
                    name: name.to_path_buf(),
                    path,
                    modified: 0,
                };

                self.index.create_node(&node)?
            }
        };

        // trace!("prepare_node {} = {}", node.node_id, fs_path.to_string_lossy());

        Ok(ScanNode {
            node,
            fs_path,
            modified,
        })
    }

    fn process_directory_node(&mut self, node: &Node, fs_path: &Path) -> Result<Option<ScanStat>> {
        debug!("directory '{}'", fs_path.to_string_lossy());

        let mut stat = ScanStat {
            ..Default::default()
        };

        for entry in fs::read_dir(fs_path)? {
            if let Ok(Some(node_stat)) = self.scan_node(Some(&node), Path::new(&entry?.file_name()))
            {
                stat.add(&node_stat);
            }
        }

        Ok(Some(stat))
    }

    fn process_file_node(
        &mut self,
        parent: &Node,
        node: &Node,
        fs_path: &Path,
    ) -> Result<Option<ScanStat>> {
        let extension = match fs_path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => {
                return Ok(None);
            }
        };

        if let Some(stat) = self.try_process_cue_file(extension, parent, node, &fs_path)? {
            return Ok(Some(stat));
        }

        if let Some(stat) = self.try_process_image_file(extension, node, &fs_path)? {
            return Ok(Some(stat));
        }

        if let Some(stat) = self.try_process_audio_file(node, &fs_path)? {
            return Ok(Some(stat));
        }

        debug!("no handler found for file '{}'", fs_path.to_string_lossy());

        Ok(None)
    }

    fn try_process_cue_file(
        &mut self,
        extension: &str,
        parent: &Node,
        node: &Node,
        fs_path: &Path,
    ) -> Result<Option<ScanStat>> {
        if extension != "cue" {
            return Ok(None);
        }

        debug!("cue file '{}'", fs_path.to_string_lossy());

        let cue_text = std::fs::read_to_string(&fs_path)?;
        let cue = cue::parse_cue(&cue_text);

        if cue.files.is_empty() {
            debug!("no file entries in cue file, ignoring");
            return Ok(None);
        }

        let mut stat = ScanStat {
            ..Default::default()
        };

        for file in cue.files {
            if file.tracks.is_empty() {
                continue;
            }

            let file_node = match self.prepare_node(
                Some(parent),
                Path::new(OsStr::from_bytes(&file.path.as_bytes())),
            ) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let file_tracks = match media_info::media_info_from_path(&file_node.fs_path) {
                Some(t) => t.0,
                None => continue,
            };

            let file_track = match file_tracks.first() {
                Some(t) => t,
                None => continue,
            };

            let mut tracks: Vec<Track> = Vec::new();

            for cue_track in file.tracks {
                tracks.push(Track {
                    track_id: 0,
                    node_id: file_node.node.node_id,
                    stream_index: file_track.stream_index,
                    track_index: file_track.track_index,
                    number: i64::from(cue_track.number),
                    title: cue_track.title.trim().to_string(),
                    artist_id: 0, // Resolved later
                    artist_name: cue_track.performer.trim().to_string(),
                    album_id: 0, // Resolved later
                    album_name: cue.title.trim().to_string(),
                    album_artist_id: None,
                    album_artist_name: if cue.performer.len() > 0 {
                        Some(cue.performer.trim().to_string())
                    } else {
                        None
                    },
                    start: Some(cue_track.start as f64),
                    length: 0f64,
                });
            }

            // Calculate lengths
            let mut last_start = file_track.length;
            for track in tracks.iter_mut().rev() {
                let start = track.start.unwrap_or(0f64);
                track.length = last_start - start;
                last_start = start;
            }

            self.index.clear_node(file_node.node.node_id)?;

            for track in tracks.iter_mut() {
                track.artist_id = match self.index.artist_by_name(&track.artist_name)? {
                    Some(a) => a,
                    None => self.index.create_artist(&track.artist_name)?,
                }
                .artist_id;

                track.album_id = match self
                    .index
                    .find_album(file_node.node.node_id, &track.album_name)?
                {
                    Some(a) => a,
                    None => self.index.create_album(&track.album_name)?,
                }
                .album_id;

                if let Some(album_artist_name) = &track.album_artist_name {
                    track.album_artist_id = Some(
                        match self.index.artist_by_name(&album_artist_name)? {
                            Some(a) => a,
                            None => self.index.create_artist(&album_artist_name)?,
                        }
                        .artist_id,
                    );
                }

                self.index.create_track(&track)?;

                stat.tracks += 1;
            }

            self.index
                .set_node_master(file_node.node.node_id, node.node_id)?;
            self.index
                .set_node_modified(file_node.node.node_id, file_node.modified)?;
        }

        Ok(Some(stat))
    }

    // This list is what extensions image crate recognizes
    const IMAGE_EXTENSIONS: &'static [&'static str] = &[
        "jpg", "jpeg", "png", "gif", "webp", "tif", "tiff", "tga", "bmp", "ico", "hdr", "pbm",
        "pam", "ppm", "pgm",
    ];

    fn try_process_image_file(
        &mut self,
        extension: &str,
        node: &Node,
        fs_path: &Path,
    ) -> Result<Option<ScanStat>> {
        if !Scan::IMAGE_EXTENSIONS.iter().any(|&e| extension == e) {
            return Ok(None);
        }

        debug!("image file '{}'", fs_path.to_string_lossy());

        let dimensions = match image::image_dimensions(fs_path) {
            Ok(i) => i,
            Err(e) => {
                error!(
                    "can't open image file '{}': {}",
                    fs_path.to_string_lossy(),
                    e.description()
                );
                return Ok(None);
            }
        };

        let description = match node.name.file_stem() {
            Some(s) => match s.to_str() {
                Some(s) => s.to_string(),
                None => String::new(),
            },
            None => String::new(),
        };

        self.index.create_image(&Image {
            image_id: 0,
            node_id: node.node_id,
            stream_index: None,
            description,
            width: i64::from(dimensions.0),
            height: i64::from(dimensions.1),
        })?;

        Ok(Some(ScanStat {
            images: 1,
            ..Default::default()
        }))
    }

    fn try_process_audio_file(&mut self, node: &Node, fs_path: &Path) -> Result<Option<ScanStat>> {
        debug!("try audio file '{}'", fs_path.to_string_lossy());

        let (mut tracks, mut images) = match media_info::media_info_from_path(&fs_path) {
            Some(m) => m,
            None => return Ok(None),
        };

        let mut stat = ScanStat {
            ..Default::default()
        };

        for track in tracks.iter_mut() {
            track.node_id = node.node_id;

            track.artist_id = match self.index.artist_by_name(&track.artist_name)? {
                Some(a) => a,
                None => self.index.create_artist(&track.artist_name)?,
            }
            .artist_id;

            track.album_id = match self.index.find_album(node.node_id, &track.album_name)? {
                Some(a) => a,
                None => self.index.create_album(&track.album_name)?,
            }
            .album_id;

            if let Some(album_artist_name) = &track.album_artist_name {
                track.album_artist_id = Some(
                    match self.index.artist_by_name(&album_artist_name)? {
                        Some(a) => a,
                        None => self.index.create_artist(&album_artist_name)?,
                    }
                    .artist_id,
                );
            }

            self.index.create_track(track)?;

            stat.tracks += 1;
        }

        for image in images.iter_mut() {
            image.node_id = node.node_id;

            self.index.create_image(image)?;

            stat.images += 1;
        }

        Ok(Some(stat))
    }
}