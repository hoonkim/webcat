use base64::Engine;

const CHUNK: usize = 4096;

pub struct KittyGraphics {
    image_id: u32,
    placement_id: u32,
}

impl KittyGraphics {
    pub fn new(image_id: u32) -> Self {
        KittyGraphics { image_id, placement_id: 1 }
    }

    /// Transmit a JPEG and place it at the cursor, replacing any prior placement
    /// with the same (image_id, placement_id). Chunked at CHUNK base64 bytes.
    pub fn transmit_and_place_jpeg(&self, jpeg: &[u8]) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        let bytes = b64.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 256);

        // Split into ≤CHUNK pieces; if it fits in one, that single piece is the last.
        let chunks: Vec<&[u8]> = if bytes.is_empty() {
            vec![&[][..]]
        } else {
            bytes.chunks(CHUNK).collect()
        };
        let last = chunks.len() - 1;

        for (idx, chunk) in chunks.iter().enumerate() {
            out.extend_from_slice(b"\x1b_G");
            if idx == 0 {
                // First chunk carries all control keys.
                let m = if last == 0 { 0 } else { 1 };
                let header = format!(
                    "f=100,a=T,i={},p={},q=2,m={}",
                    self.image_id, self.placement_id, m
                );
                out.extend_from_slice(header.as_bytes());
            } else {
                let m = if idx == last { 0 } else { 1 };
                out.extend_from_slice(format!("m={}", m).as_bytes());
            }
            out.push(b';');
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\x1b\\");
        }
        out
    }

    /// Delete the transmitted image (and its placements) by id.
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
    fn small_jpeg_is_single_chunk() {
        let g = KittyGraphics::new(7);
        let out = payload_of(&g.transmit_and_place_jpeg(&[0xFF, 0xD8, 0xFF, 0xD9]));
        // Begins with the graphics APC opener and the expected control keys.
        assert!(out.starts_with("\x1b_G"), "missing APC opener: {out:?}");
        assert!(out.contains("f=100"), "missing f=100");
        assert!(out.contains("a=T"), "missing a=T");
        assert!(out.contains("i=7"), "missing image id");
        assert!(out.contains("q=2"), "missing quiet flag");
        assert!(out.contains("m=0"), "single chunk must end with m=0");
        assert!(out.ends_with("\x1b\\"), "missing ST terminator");
        // Exactly one APC block for a small image.
        assert_eq!(out.matches("\x1b_G").count(), 1);
    }

    #[test]
    fn large_jpeg_is_chunked_with_m_flags() {
        let g = KittyGraphics::new(1);
        // 10_000 raw bytes -> base64 ~13_336 chars -> 4 chunks of 4096 max.
        let big = vec![0xABu8; 10_000];
        let out = payload_of(&g.transmit_and_place_jpeg(&big));
        let blocks = out.matches("\x1b_G").count();
        assert_eq!(blocks, 4, "expected exactly 4 chunks for 10000 bytes");
        // Control keys only on first block.
        assert_eq!(out.matches("f=100").count(), 1);
        // Every block except the last carries m=1; last carries m=0.
        assert_eq!(out.matches("m=1").count(), blocks - 1);
        assert_eq!(out.matches("m=0").count(), 1);
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
