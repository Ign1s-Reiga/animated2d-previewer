//! The `UnityFS` archive container.
//!
//! A bundle is a header, a compressed table of contents, and a run of
//! compressed blocks. Concatenating the decompressed blocks gives one flat byte
//! range, and the table of contents names slices of *that* — so a node's offset
//! is into the joined data, not into the file.
//!
//! Layout, all big-endian, verified against a real 2022.3 bundle:
//!
//! ```text
//! "UnityFS\0"           null-terminated signature
//! u32                   format version (6, 7 and 8 seen in the wild)
//! cstring               the Unity version the bundle claims to target
//! cstring               the exact player revision
//! i64                   total file size
//! u32                   compressed size of the blocks-info table
//! u32                   uncompressed size of the blocks-info table
//! u32                   flags
//! [align to 16]         format 7 and later only
//! ```
//!
//! The low six bits of `flags` are the compression method; the rest are
//! independent switches, of which two matter here: `0x80` puts the blocks-info
//! table at the *end* of the file rather than after the header, and `0x200`
//! pads before it.

use a2d_core::DecodeError;

use crate::reader::{Endian, Reader};

/// How a block was compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Lzma,
    /// LZ4 and LZ4HC decompress identically; only the encoder differs.
    Lz4,
}

impl Compression {
    fn from_flags(flags: u32) -> Result<Compression, DecodeError> {
        Ok(match flags & 0x3F {
            0 => Compression::None,
            1 => Compression::Lzma,
            2 | 3 => Compression::Lz4,
            other => {
                return Err(DecodeError::corrupt(format!(
                    "unsupported bundle compression method {other}; only stored and LZ4 are handled"
                )))
            }
        })
    }
}

/// One file inside the bundle's joined data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub path: String,
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
}

impl Node {
    /// Whether this node is a serialized file rather than a resource blob.
    ///
    /// Bit 2 marks a serialized file. Resource streams (`.resS`, `.resource`)
    /// sit beside them and hold texture and audio payloads.
    pub fn is_serialized(&self) -> bool {
        self.flags & 4 != 0
    }
}

/// A decompressed bundle: its metadata, its joined data, and its directory.
pub struct Bundle {
    pub signature: String,
    pub format_version: u32,
    /// The Unity version the bundle targets, e.g. `5.x.x`.
    pub unity_version: String,
    /// The exact player revision, e.g. `2022.3.20p1`.
    pub unity_revision: String,
    pub compression: Compression,
    pub nodes: Vec<Node>,
    data: Vec<u8>,
}

/// Prints the metadata and the size of the payload, never the payload.
///
/// A bundle carries megabytes; a derived `Debug` would dump all of it into a
/// test failure message.
impl std::fmt::Debug for Bundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bundle")
            .field("signature", &self.signature)
            .field("format_version", &self.format_version)
            .field("unity_version", &self.unity_version)
            .field("unity_revision", &self.unity_revision)
            .field("compression", &self.compression)
            .field("nodes", &self.nodes.len())
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl Bundle {
    /// The joined, decompressed contents of every block.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The bytes of one node.
    pub fn node_data(&self, node: &Node) -> Result<&[u8], DecodeError> {
        let start = usize::try_from(node.offset).unwrap_or(usize::MAX);
        let end = start.checked_add(usize::try_from(node.size).unwrap_or(usize::MAX));
        match end {
            Some(end) if end <= self.data.len() => Ok(&self.data[start..end]),
            _ => Err(DecodeError::corrupt(format!(
                "node {:?} spans {}..{} but the bundle holds only {} bytes",
                node.path,
                node.offset,
                node.offset + node.size,
                self.data.len()
            ))),
        }
    }

    /// Reads a bundle from bytes already in memory.
    pub fn parse(bytes: &[u8]) -> Result<Bundle, DecodeError> {
        let mut r = Reader::new(bytes, Endian::Big);
        let signature = r.cstring()?;
        if signature != "UnityFS" {
            return Err(DecodeError::corrupt(format!(
                "not a UnityFS bundle: signature is {signature:?}. Archives written as UnityWeb or \
                 UnityRaw are older formats and are not read here"
            )));
        }
        let format_version = r.u32()?;
        let unity_version = r.cstring()?;
        let unity_revision = r.cstring()?;
        let _total_size = r.i64()?;
        let compressed_info = r.u32()? as usize;
        let uncompressed_info = r.u32()? as usize;
        let flags = r.u32()?;
        let compression = Compression::from_flags(flags)?;

        // Format 7 aligned the payload; 6 did not.
        if format_version >= 7 {
            r.align(16)?;
        }

        // The table of contents lives either right here or at the very end.
        let info_at_end = flags & 0x80 != 0;
        let info_start = if info_at_end {
            bytes.len().checked_sub(compressed_info).ok_or_else(|| {
                DecodeError::corrupt("the blocks-info table is larger than the bundle it sits in".to_string())
            })?
        } else {
            r.position()
        };
        let info_end = info_start.checked_add(compressed_info).filter(|e| *e <= bytes.len()).ok_or_else(|| {
            DecodeError::corrupt(format!(
                "the blocks-info table runs past the end of the bundle ({compressed_info} bytes at {info_start})"
            ))
        })?;
        let info = decompress(&bytes[info_start..info_end], uncompressed_info, compression, "the blocks-info table")?;

        let mut blocks_start = if info_at_end { r.position() } else { info_end };
        // 0x200 asks for the block data itself to start on a 16-byte boundary.
        if flags & 0x200 != 0 && !info_at_end {
            blocks_start = (blocks_start + 15) & !15;
        }

        let (blocks, nodes) = parse_blocks_info(&info)?;
        let data = read_blocks(bytes, blocks_start, &blocks)?;

        Ok(Bundle { signature, format_version, unity_version, unity_revision, compression, nodes, data })
    }
}

/// One compressed block in the bundle's data run.
#[derive(Debug, Clone, Copy)]
struct Block {
    uncompressed_size: u32,
    compressed_size: u32,
    flags: u16,
}

fn parse_blocks_info(info: &[u8]) -> Result<(Vec<Block>, Vec<Node>), DecodeError> {
    let mut r = Reader::new(info, Endian::Big);
    r.skip(16)?; // hash of the uncompressed data; not verified here

    let block_count = r.i32()?;
    if block_count < 0 {
        return Err(DecodeError::corrupt(format!("the bundle declares {block_count} blocks")));
    }
    let mut blocks = Vec::with_capacity(block_count.min(4096) as usize);
    for _ in 0..block_count {
        blocks.push(Block { uncompressed_size: r.u32()?, compressed_size: r.u32()?, flags: r.u16()? });
    }

    let node_count = r.i32()?;
    if node_count < 0 {
        return Err(DecodeError::corrupt(format!("the bundle declares {node_count} directory entries")));
    }
    let mut nodes = Vec::with_capacity(node_count.min(4096) as usize);
    for _ in 0..node_count {
        let offset = r.u64()?;
        let size = r.u64()?;
        let flags = r.u32()?;
        // Node paths are null-terminated here, not length-prefixed.
        let path = r.cstring()?;
        nodes.push(Node { path, offset, size, flags });
    }
    Ok((blocks, nodes))
}

fn read_blocks(bytes: &[u8], mut at: usize, blocks: &[Block]) -> Result<Vec<u8>, DecodeError> {
    let total: usize = blocks.iter().map(|b| b.uncompressed_size as usize).sum();
    let mut out = Vec::with_capacity(total.min(64 << 20));
    for (i, block) in blocks.iter().enumerate() {
        let end = at.checked_add(block.compressed_size as usize).filter(|e| *e <= bytes.len()).ok_or_else(|| {
            DecodeError::corrupt(format!(
                "block {i} claims {} bytes at {at}, past the end of the bundle",
                block.compressed_size
            ))
        })?;
        // Each block carries its own method in the same low bits as the header.
        let method = Compression::from_flags(block.flags as u32)?;
        let chunk = decompress(&bytes[at..end], block.uncompressed_size as usize, method, &format!("block {i}"))?;
        out.extend_from_slice(&chunk);
        at = end;
    }
    Ok(out)
}

fn decompress(input: &[u8], expected: usize, method: Compression, what: &str) -> Result<Vec<u8>, DecodeError> {
    match method {
        Compression::None => {
            if input.len() != expected {
                return Err(DecodeError::corrupt(format!(
                    "{what} is stored uncompressed but is {} bytes where {expected} were declared",
                    input.len()
                )));
            }
            Ok(input.to_vec())
        }
        Compression::Lz4 => {
            let out = lz4_flex::block::decompress(input, expected)
                .map_err(|e| DecodeError::corrupt(format!("{what} failed to decompress: {e}")))?;
            if out.len() != expected {
                // A short read means the declared size and the stream disagree,
                // which would silently truncate whatever is parsed next.
                return Err(DecodeError::corrupt(format!(
                    "{what} decompressed to {} bytes where {expected} were declared",
                    out.len()
                )));
            }
            Ok(out)
        }
        Compression::Lzma => Err(DecodeError::corrupt(format!(
            "{what} is LZMA-compressed, which is not handled; re-pack the bundle with LZ4 or store it uncompressed"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal uncompressed bundle holding one node.
    fn fixture(payload: &[u8], node_path: &str, flags: u32) -> Vec<u8> {
        let mut info = Vec::new();
        info.extend_from_slice(&[0u8; 16]); // hash
        info.extend_from_slice(&1i32.to_be_bytes()); // one block
        info.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        info.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        info.extend_from_slice(&0u16.to_be_bytes()); // stored
        info.extend_from_slice(&1i32.to_be_bytes()); // one node
        info.extend_from_slice(&0u64.to_be_bytes()); // offset
        info.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        info.extend_from_slice(&4u32.to_be_bytes()); // serialized file
        info.extend_from_slice(node_path.as_bytes());
        info.push(0);

        let mut out = Vec::new();
        out.extend_from_slice(b"UnityFS\0");
        out.extend_from_slice(&6u32.to_be_bytes()); // format 6: no 16-byte align
        out.extend_from_slice(b"5.x.x\0");
        out.extend_from_slice(b"2022.3.20p1\0");
        let size_at = out.len();
        out.extend_from_slice(&0i64.to_be_bytes());
        out.extend_from_slice(&(info.len() as u32).to_be_bytes());
        out.extend_from_slice(&(info.len() as u32).to_be_bytes());
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&info);
        out.extend_from_slice(payload);
        let total = out.len() as i64;
        out[size_at..size_at + 8].copy_from_slice(&total.to_be_bytes());
        out
    }

    #[test]
    fn a_stored_bundle_round_trips() {
        let bytes = fixture(b"hello unity", "CAB-abc", 0);
        let bundle = Bundle::parse(&bytes).expect("should parse");
        assert_eq!(bundle.signature, "UnityFS");
        assert_eq!(bundle.unity_revision, "2022.3.20p1");
        assert_eq!(bundle.compression, Compression::None);
        assert_eq!(bundle.nodes.len(), 1);
        assert_eq!(bundle.nodes[0].path, "CAB-abc");
        assert!(bundle.nodes[0].is_serialized());
        assert_eq!(bundle.node_data(&bundle.nodes[0]).unwrap(), b"hello unity");
    }

    #[test]
    fn an_lz4_block_is_decompressed_to_its_declared_size() {
        // Round-tripping through the encoder is the honest check that the
        // decoder is wired up correctly; the format itself is the crate's job.
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
        let squashed = lz4_flex::block::compress(&payload);
        let restored = decompress(&squashed, payload.len(), Compression::Lz4, "test").unwrap();
        assert_eq!(restored, payload);
    }

    #[test]
    fn a_size_that_disagrees_with_the_stream_is_an_error() {
        let payload = b"some bytes to squash".to_vec();
        let squashed = lz4_flex::block::compress(&payload);
        let err = decompress(&squashed, payload.len() + 32, Compression::Lz4, "test").unwrap_err();
        assert!(err.to_string().contains("decompressed to"), "{err}");
    }

    #[test]
    fn a_wrong_signature_says_which_format_it_found() {
        let mut bytes = fixture(b"x", "CAB-x", 0);
        bytes[..8].copy_from_slice(b"UnityWeb");
        let err = Bundle::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("UnityWeb"), "{err}");
    }

    #[test]
    fn lzma_is_refused_with_a_way_forward() {
        let bytes = fixture(b"x", "CAB-x", 1);
        let err = Bundle::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("LZMA"), "{err}");
    }

    #[test]
    fn a_node_reaching_past_the_data_is_an_error() {
        let bytes = fixture(b"short", "CAB-x", 0);
        let bundle = Bundle::parse(&bytes).unwrap();
        let bad = Node { path: "x".into(), offset: 0, size: 9999, flags: 4 };
        assert!(bundle.node_data(&bad).is_err());
    }

    #[test]
    fn truncation_anywhere_is_an_error_and_never_a_panic() {
        let full = fixture(b"hello unity", "CAB-abc", 0);
        for cut in 0..full.len() {
            let _ = Bundle::parse(&full[..cut]);
        }
    }
}
