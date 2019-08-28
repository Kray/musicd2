use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::musicd_c;

pub fn media_image_data_read(path: &Path, stream_index: i32) -> Option<Vec<u8>> {
    let tmp_path = CString::new(path.as_os_str().as_bytes()).unwrap();

    let mut data: *mut u8 = std::ptr::null_mut();
    let mut len: usize = 0;

    unsafe {
        if musicd_c::media_image_data_read(
            tmp_path.as_ptr(),
            stream_index,
            &mut data as *mut *mut u8,
            &mut len as *mut usize,
        ) == 0
        {
            return None;
        }
    }

    let result = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();

    unsafe {
        musicd_c::media_image_data_free(data);
    }

    Some(result)
}
