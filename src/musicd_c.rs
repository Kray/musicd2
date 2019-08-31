use std::os::raw::{c_char, c_int, c_void};

#[repr(C)]
pub struct MediaInfo {
    pub tracks: *const TrackInfo,
    pub images: *const ImageInfo,
}

#[repr(C)]
pub struct TrackInfo {
    pub next: *const TrackInfo,
    pub stream_index: i32,
    pub track_index: i32,
    pub number: i32,
    pub title: *const c_char,
    pub artist: *const c_char,
    pub album: *const c_char,
    pub album_artist: *const c_char,
    pub start: f64,
    pub duration: f64,
}

#[repr(C)]
pub struct ImageInfo {
    pub next: *const ImageInfo,
    pub stream_index: i32,
    pub description: *const c_char,
    pub width: i32,
    pub height: i32,
}

#[repr(C)]
pub struct AudioStreamOptions {
    pub path: *const c_char,
    pub stream_index: i32,
    pub track_index: i32,
    pub start: f64,
    pub length: f64,
    pub target_codec: *const c_char,
}

pub enum LogLevel {
    LogLevelError = 1,
    LogLevelWarn = 2,
    LogLevelInfo = 3,
    LogLevelDebug = 4,
    LogLevelTrace = 5,
}

extern "C" {
    pub fn musicd_log_setup(callback: extern "C" fn(level: c_int, message: *const c_char));

    pub fn media_info_from_path(path: *const c_char) -> *const MediaInfo;
    pub fn media_info_free(track: *const MediaInfo);

    pub fn audio_stream_open(config: *const AudioStreamOptions) -> *const c_void;
    pub fn audio_stream_next(
        audio_stream: *const c_void,
        opaque: *const c_void,
        callback: extern "C" fn(opaque: *const c_void, buf: *const u8, len: c_int) -> c_int,
    ) -> c_int;
    pub fn audio_stream_close(audio_stream: *const c_void);

    pub fn media_image_data_read(
        path: *const c_char,
        stream_index: i32,
        out_data: *mut *mut u8,
        out_len: *mut usize,
    ) -> c_int;
    pub fn media_image_data_free(data: *mut u8);
}
