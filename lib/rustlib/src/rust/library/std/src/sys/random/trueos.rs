//! TRUEOS kernel random source.

unsafe extern "C" {
    fn sys_rand(recv_buf: *mut u32, words: usize);
}

pub fn fill_bytes(bytes: &mut [u8]) {
    let (pre, words, post) = unsafe { bytes.align_to_mut::<u32>() };
    if !words.is_empty() {
        unsafe {
            sys_rand(words.as_mut_ptr(), words.len());
        }
    }

    let mut buf = [0u32; 2];
    let len = (pre.len() + post.len() + size_of::<u32>() - 1) / size_of::<u32>();
    if len != 0 {
        unsafe { sys_rand(buf.as_mut_ptr(), len) };
    }

    let buf = buf.map(u32::to_ne_bytes);
    let buf = buf.as_flattened();
    pre.copy_from_slice(&buf[..pre.len()]);
    post.copy_from_slice(&buf[pre.len()..pre.len() + post.len()]);
}
