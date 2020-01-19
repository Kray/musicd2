use std::error::Error as StdError;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use bytes::{BytesMut, buf::ext::BufExt};
use tokio::sync::mpsc::Sender;

use crate::musicd_c;

extern "C" fn stream_c_callback(opaque: *const c_void, data: *const u8, len: c_int) -> c_int {
    let closure: &mut &mut dyn FnMut(&[u8]) -> usize =
        unsafe { &mut *(opaque as *mut &mut dyn for<'r> std::ops::FnMut(&'r [u8]) -> usize) };

    let slice = unsafe { std::slice::from_raw_parts(data, len as usize) };

    closure(slice) as i32
}

pub struct AudioStream {
    stream: *const c_void,
}

unsafe impl Send for AudioStream {}

impl Drop for AudioStream {
    fn drop(&mut self) {
        unsafe {
            musicd_c::audio_stream_close(self.stream);
        }
    }
}

impl AudioStream {
    pub fn open(
        path: &Path,
        stream_index: i32,
        track_index: i32,
        start: f64,
        length: f64,
        target_codec: &str,
    ) -> Option<AudioStream> {
        let tmp_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let tmp_codec = CString::new(target_codec).unwrap();

        let config = musicd_c::AudioStreamOptions {
            path: tmp_path.as_ptr(),
            stream_index,
            track_index,
            start,
            length,
            target_codec: tmp_codec.as_ptr(),
        };

        let result = unsafe { musicd_c::audio_stream_open(&config) };

        if result.is_null() {
            None
        } else {
            Some(AudioStream { stream: result })
        }
    }

    pub fn next<F>(&mut self, mut callback: F) -> bool
    where
        F: FnMut(&[u8]) -> usize,
    {
        let mut cb: &mut dyn FnMut(&[u8]) -> usize = &mut callback;
        let cb = &mut cb;

        unsafe {
            musicd_c::audio_stream_next(self.stream, cb as *mut _ as *mut c_void, stream_c_callback)
                > 0
        }
    }

    pub async fn execute(
        mut self,
        mut sender: Sender<Result<Vec<u8>, Box<dyn StdError + Send + Sync>>>,
    ) {
        loop {
            let mut buf = BytesMut::new();

            let mut result = true;

            while result && buf.len() < 10 * 1024 {
                result = self.next(|data| {
                    buf.extend_from_slice(&data);
                    data.len()
                });
            }

            trace!("read {} bytes from audio stream, feeding", buf.len());

            let result = if result {
                let len = buf.len();
                sender.send(Ok(buf.take(len).into_inner().to_vec())).await
            } else {
                debug!("audio stream finished, flushing channel");
                let _ = sender.send(Ok(vec![])).await;
                break;
            };

            if result.is_err() {
                debug!("channel disconnected, stopping audio stream");
                break;
            }
        }
    }
}
