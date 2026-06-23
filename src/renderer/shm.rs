use base64::Engine;
use std::ffi::CString;
use std::io;

/// A POSIX shared-memory object used to hand raw RGBA pixels to kitty via the
/// graphics protocol's `t=s` (shared memory) transmission. The escape carries
/// only the (base64-encoded) shm name, so megabytes of pixel data never go
/// through the terminal pipe — this is what keeps rendering smooth at HiDPI.
///
/// kitty reads the object and unlinks it after a successful `t=s` transmission;
/// we additionally unlink before each write so a fresh object is created every
/// frame (this also sidesteps macOS only allowing `ftruncate` on a newly
/// created shm object).
pub struct Shm {
    name: String,
    name_b64: String,
}

impl Shm {
    pub fn new() -> Shm {
        // POSIX shm names must start with '/' and stay short (macOS PSHMNAMLEN
        // is 31). "/webcat_<pid>" fits comfortably.
        let name = format!("/webcat_{}", std::process::id());
        let name_b64 = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
        Shm { name, name_b64 }
    }

    /// The shm name, base64-encoded, for use as the `t=s` escape payload.
    pub fn name_base64(&self) -> &str {
        &self.name_b64
    }

    /// Write `rgba` (length must be width*height*4) into a fresh shm object.
    /// Returns Err on any syscall failure; the caller should skip the frame.
    pub fn write(&self, rgba: &[u8]) -> io::Result<()> {
        let cname = CString::new(self.name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "shm name has NUL"))?;
        let len = rgba.len();
        unsafe {
            // Start fresh: unlink any stale object so the create+ftruncate below
            // always succeeds on a brand-new object.
            libc::shm_unlink(cname.as_ptr());
            let fd = libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_RDWR,
                0o600 as libc::c_uint,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // Ensure the cleanup of fd/name happens on every error path.
            let cleanup = |fd: libc::c_int| {
                libc::close(fd);
                libc::shm_unlink(cname.as_ptr());
            };
            if libc::ftruncate(fd, len as libc::off_t) != 0 {
                let e = io::Error::last_os_error();
                cleanup(fd);
                return Err(e);
            }
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if ptr == libc::MAP_FAILED {
                let e = io::Error::last_os_error();
                cleanup(fd);
                return Err(e);
            }
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), ptr as *mut u8, len);
            libc::munmap(ptr, len);
            libc::close(fd);
        }
        Ok(())
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        if let Ok(cname) = CString::new(self.name.as_bytes()) {
            unsafe {
                libc::shm_unlink(cname.as_ptr());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip: write bytes via Shm, then open the same shm object and read
    /// them back, verifying POSIX shm works on this platform.
    #[test]
    fn write_then_read_back() {
        let shm = Shm::new();
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        shm.write(&data).expect("write");

        // Re-open the object by name and read the bytes back.
        let cname = CString::new(shm.name.as_bytes()).unwrap();
        let read_back = unsafe {
            let fd = libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0o600 as libc::c_uint);
            assert!(fd >= 0, "shm_open for read failed: {}", io::Error::last_os_error());
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                data.len(),
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            );
            assert!(ptr != libc::MAP_FAILED, "mmap failed: {}", io::Error::last_os_error());
            let slice = std::slice::from_raw_parts(ptr as *const u8, data.len()).to_vec();
            libc::munmap(ptr, data.len());
            libc::close(fd);
            slice
        };
        assert_eq!(read_back, data, "round-trip data mismatch");
    }

    #[test]
    fn name_is_valid_posix_shm() {
        let shm = Shm::new();
        assert!(shm.name.starts_with('/'));
        assert!(shm.name.len() <= 31, "name too long for macOS PSHMNAMLEN");
        // base64 of the name decodes back to the name.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(shm.name_base64())
            .unwrap();
        assert_eq!(decoded, shm.name.as_bytes());
    }
}
