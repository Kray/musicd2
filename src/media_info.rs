use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::index::{Image, Track};
use crate::musicd_c;

unsafe fn convert_string(s: *const c_char) -> String {
    if s.is_null() {
        return String::new();
    }

    String::from_utf8_lossy(CStr::from_ptr(s).to_bytes()).into_owned()
}

pub fn media_info_from_path(path: &Path) -> Option<(Vec<Track>, Vec<Image>)> {
    let tmp_path = CString::new(path.as_os_str().as_bytes()).unwrap();

    let file_info = unsafe { musicd_c::media_info_from_path(tmp_path.as_ptr()) };
    if file_info.is_null() {
        return None;
    }

    let mut tracks: Vec<Track> = Vec::new();
    let mut images: Vec<Image> = Vec::new();

    let mut cur: *const musicd_c::TrackInfo = unsafe { (*file_info).tracks };
    while !cur.is_null() {
        tracks.push(unsafe {
            let track_info = &(&(*cur));

            Track {
                track_id: 0i64,
                node_id: 0i64,
                stream_index: i64::from(track_info.stream_index),
                track_index: Some(i64::from(track_info.track_index)),
                start: None,
                number: i64::from(track_info.number),
                title: convert_string(track_info.title).trim().to_string(),
                artist_id: 0,
                artist_name: convert_string(track_info.artist).trim().to_string(),
                album_id: 0,
                album_name: convert_string(track_info.album).trim().to_string(),
                album_artist_id: None,
                album_artist_name: if track_info.album_artist.is_null() {
                    None
                } else {
                    Some(convert_string(track_info.album_artist).trim().to_string())
                },
                length: track_info.duration,
            }
        });

        cur = unsafe { (*cur).next };
    }

    let mut cur: *const musicd_c::ImageInfo = unsafe { (*file_info).images };
    while !cur.is_null() {
        images.push(unsafe {
            let image_info = &(*cur);

            Image {
                image_id: 0i64,
                node_id: 0i64,
                stream_index: Some(i64::from(image_info.stream_index)),
                description: convert_string(image_info.description),
                width: i64::from(image_info.width),
                height: i64::from(image_info.height),
            }
        });

        cur = unsafe { (*cur).next };
    }

    unsafe {
        musicd_c::media_info_free(file_info);
    }

    Some((tracks, images))
}
