//! A writer for tiny synthetic Unity bundles, for tests in this workspace.
//!
//! Spec section 11 forbids committing extracted assets, so tests that need a
//! bundle build one. The result is an uncompressed `UnityFS` archive holding one
//! serialized file (format 22, no type tree) of `TextAsset` and `Texture2D`
//! objects. It is the shape the reader here parses, not a claim about what any
//! particular Unity version writes; real bundles are covered by the gated tests.

/// Builds a bundle out of named text assets and RGBA textures.
#[derive(Default)]
pub struct BundleBuilder {
    objects: Vec<(i32, Vec<u8>)>,
}

fn pad(out: &mut Vec<u8>, to: usize) {
    while out.len() % to != 0 {
        out.push(0);
    }
}

fn string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as i32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    pad(out, 4);
}

impl BundleBuilder {
    pub fn new() -> BundleBuilder {
        BundleBuilder::default()
    }

    /// Adds a `TextAsset`: a name, then the payload as a length-prefixed blob.
    pub fn text_asset(mut self, name: &str, payload: &[u8]) -> BundleBuilder {
        let mut data = Vec::new();
        string(&mut data, name);
        data.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        data.extend_from_slice(payload);
        self.objects.push((49, data));
        self
    }

    /// Adds a `TextAsset` whose declared length runs past its object, which
    /// the importer cannot read.
    pub fn broken_text_asset(mut self, name: &str) -> BundleBuilder {
        let mut data = Vec::new();
        string(&mut data, name);
        data.extend_from_slice(&1000i32.to_le_bytes());
        data.extend_from_slice(&[1, 2]);
        self.objects.push((49, data));
        self
    }
    /// Adds an uncompressed (RGBA32) `Texture2D`. `rgba` is bottom row first,
    /// the order Unity stores, and must hold `width * height * 4` bytes.
    pub fn texture(self, name: &str, width: u32, height: u32, rgba: &[u8]) -> BundleBuilder {
        self.texture_with_format(name, width, height, 4, rgba)
    }

    /// Adds a `Texture2D` carrying `data` under a Unity format id, which the
    /// reader refuses when it is not one it decodes (999, say).
    pub fn texture_with_format(self, name: &str, width: u32, height: u32, format: i32, rgba: &[u8]) -> BundleBuilder {
        self.texture_mips(name, width, height, format, 1, rgba)
    }

    /// As `texture_with_format`, declaring `mip_count` levels; `rgba` is then
    /// the whole chain, as Unity stores it.
    pub fn texture_mips(
        mut self,
        name: &str,
        width: u32,
        height: u32,
        format: i32,
        mip_count: i32,
        rgba: &[u8],
    ) -> BundleBuilder {
        let mut data = Vec::new();
        string(&mut data, name);
        data.extend_from_slice(&1i32.to_le_bytes()); // forced fallback format
        data.extend_from_slice(&[0, 0]); // downscale fallback, alpha optional
        pad(&mut data, 4);
        data.extend_from_slice(&(width as i32).to_le_bytes());
        data.extend_from_slice(&(height as i32).to_le_bytes());
        data.extend_from_slice(&(rgba.len() as i32).to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes()); // mips stripped
        data.extend_from_slice(&format.to_le_bytes());
        data.extend_from_slice(&mip_count.to_le_bytes());
        data.extend_from_slice(&(rgba.len() as i32).to_le_bytes()); // image data length
        data.extend_from_slice(rgba);
        self.objects.push((28, data));
        self
    }

    /// The serialized file alone.
    pub fn serialized_file(&self) -> Vec<u8> {
        // Fixed header: 16 bytes, 4 endian/reserved, then the wide fields.
        const HEADER: usize = 48;
        let mut meta = Vec::new();
        meta.extend_from_slice(b"2022.3.20f1\0");
        meta.extend_from_slice(&19i32.to_le_bytes()); // target platform
        meta.push(0); // no type tree
                      // Two types, indexed 0 = TextAsset and 1 = Texture2D.
        meta.extend_from_slice(&2i32.to_le_bytes());
        for class in [49i32, 28] {
            meta.extend_from_slice(&class.to_le_bytes());
            meta.push(0); // not stripped
            meta.extend_from_slice(&(-1i16).to_le_bytes());
            meta.extend_from_slice(&[0u8; 16]); // old type hash
        }
        meta.extend_from_slice(&(self.objects.len() as i32).to_le_bytes());
        let mut start = 0u64;
        for (index, (class, data)) in self.objects.iter().enumerate() {
            // The reader aligns from the start of the file, header included.
            while (HEADER + meta.len()) % 4 != 0 {
                meta.push(0);
            }
            meta.extend_from_slice(&(index as i64 + 1).to_le_bytes());
            meta.extend_from_slice(&start.to_le_bytes());
            meta.extend_from_slice(&(data.len() as u32).to_le_bytes());
            meta.extend_from_slice(&(if *class == 49 { 0i32 } else { 1 }).to_le_bytes());
            start += (data.len() as u64).div_ceil(8) * 8;
        }
        meta.extend_from_slice(&0i32.to_le_bytes()); // script types
        meta.extend_from_slice(&0i32.to_le_bytes()); // externals

        let data_offset = (HEADER + meta.len()).div_ceil(16) * 16;
        let mut body = Vec::new();
        for (_, data) in &self.objects {
            body.extend_from_slice(data);
            pad(&mut body, 8);
        }
        let total = data_offset + body.len();

        let mut out = Vec::new();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&22u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(meta.len() as u32).to_be_bytes());
        out.extend_from_slice(&(total as u64).to_be_bytes());
        out.extend_from_slice(&(data_offset as u64).to_be_bytes());
        out.extend_from_slice(&[0u8; 8]);
        debug_assert_eq!(out.len(), HEADER);
        out.extend_from_slice(&meta);
        out.resize(data_offset, 0);
        out.extend_from_slice(&body);
        out
    }

    /// The whole `UnityFS` archive, stored uncompressed.
    pub fn build(&self) -> Vec<u8> {
        let payload = self.serialized_file();
        let mut info = Vec::new();
        info.extend_from_slice(&[0u8; 16]); // hash
        info.extend_from_slice(&1i32.to_be_bytes()); // one block
        info.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        info.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        info.extend_from_slice(&0u16.to_be_bytes()); // stored
        info.extend_from_slice(&1i32.to_be_bytes()); // one node
        info.extend_from_slice(&0u64.to_be_bytes());
        info.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        info.extend_from_slice(&4u32.to_be_bytes()); // serialized file
        info.extend_from_slice(b"CAB-synthetic\0");

        let mut out = Vec::new();
        out.extend_from_slice(b"UnityFS\0");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(b"5.x.x\0");
        out.extend_from_slice(b"2022.3.20f1\0");
        let size_at = out.len();
        out.extend_from_slice(&0i64.to_be_bytes());
        out.extend_from_slice(&(info.len() as u32).to_be_bytes());
        out.extend_from_slice(&(info.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&info);
        out.extend_from_slice(&payload);
        let total = out.len() as i64;
        out[size_at..size_at + 8].copy_from_slice(&total.to_be_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_textures, Bundle, Inventory, SerializedFile};

    #[test]
    fn the_builder_writes_what_the_reader_reads() {
        let bytes = BundleBuilder::new().text_asset("a.skel", b"xyz").texture("page", 1, 1, &[1, 2, 3, 4]).build();
        let bundle = Bundle::parse(&bytes).expect("bundle");
        let node = bundle.nodes.iter().find(|n| n.is_serialized()).expect("node");
        let file = SerializedFile::parse(bundle.node_data(node).expect("data")).expect("file");
        assert_eq!(file.objects.len(), 2);
        let inventory = Inventory::build(&file);
        assert_eq!(inventory.objects[1].name.as_deref(), Some("page"));
        let textures = read_textures(&file).expect("textures");
        assert_eq!(textures.len(), 1);
        assert_eq!(textures[0].rgba, vec![1, 2, 3, 4]);
    }
}
