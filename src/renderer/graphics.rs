pub struct KittyGraphics {
    image_id: u32,
    placement_id: u32,
}

impl KittyGraphics {
    pub fn new(image_id: u32) -> Self {
        KittyGraphics { image_id, placement_id: 1 }
    }

    /// Build the kitty escape that transmits a `width`×`height` RGBA image from a
    /// POSIX shared-memory object (`f=32,t=s`) and displays it at the cursor at
    /// native pixel size, replacing any prior placement with the same
    /// (image_id, placement_id).
    ///
    /// `name_b64` is the base64-encoded shm object name (the only payload — the
    /// pixels travel through shared memory, not the terminal pipe). Native
    /// placement (no c/r scaling) reproduces the frame 1:1; the caller renders
    /// the frame at the terminal's device resolution so it fills the screen.
    pub fn transmit_shm(&self, name_b64: &str, width: u32, height: u32) -> Vec<u8> {
        format!(
            "\x1b_Gf=32,t=s,s={},v={},a=T,i={},p={},q=2;{}\x1b\\",
            width, height, self.image_id, self.placement_id, name_b64
        )
        .into_bytes()
    }

    /// Delete the transmitted image (and its placements) by id.
    // Called by Renderer::clear; also tested in unit tests.
    #[allow(dead_code)]
    pub fn delete_all(&self) -> Vec<u8> {
        format!("\x1b_Ga=d,d=i,i={},q=2;\x1b\\", self.image_id).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_of(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn transmit_shm_emits_rgba_shared_memory_escape() {
        let g = KittyGraphics::new(7);
        let out = payload_of(&g.transmit_shm("YmFzZTY0", 1144, 880));
        assert!(out.starts_with("\x1b_G"), "missing APC opener: {out:?}");
        assert!(out.contains("f=32"), "missing RGBA format");
        assert!(out.contains("t=s"), "missing shared-memory transmission");
        assert!(out.contains("s=1144"), "missing pixel width");
        assert!(out.contains("v=880"), "missing pixel height");
        assert!(out.contains("a=T"), "missing transmit+display action");
        assert!(out.contains("i=7"), "missing image id");
        assert!(out.contains("q=2"), "missing quiet flag");
        // Payload is the base64 shm name after ';', then ST.
        assert!(out.ends_with(";YmFzZTY0\x1b\\"), "payload must be the shm name: {out:?}");
        // A single, compact escape — no pixel data in the pipe.
        assert_eq!(out.matches("\x1b_G").count(), 1);
    }

    #[test]
    fn delete_all_targets_image_id() {
        let g = KittyGraphics::new(42);
        let out = payload_of(&g.delete_all());
        assert!(out.contains("a=d"), "missing delete action");
        assert!(out.contains("i=42"), "missing image id");
        assert!(out.ends_with("\x1b\\"));
    }
}
