use byteorder::{BigEndian, ReadBytesExt};
use color_eyre::{eyre::bail, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The signature at the beginning of a Git packfile index.
const IDX_SIGNATURE: &[u8; 4] = b"\xfftOc";

/// A header for a Git packfile index.
struct PackIdxHeader {
    /// The number of entries in the index.
    num_entries: u32,
}

/// An entry in a Git packfile index.
struct PackIdxEntry {
    /// The SHA-1 hash of the object this entry refers to.
    sha1: [u8; 20],
}

impl PackIdxEntry {
    /// Returns the type of the object this entry refers to.
    fn object_type(&self) -> u8 {
        (self.sha1[0] >> 4) & 0b111
    }
}

/// Parses the header of a Git packfile index from the given reader.
fn parse_pack_idx_header<R: Read>(reader: &mut R) -> Result<PackIdxHeader> {
    let mut signature_buf = [0u8; 4];
    reader.read_exact(&mut signature_buf)?;

    if signature_buf != *IDX_SIGNATURE {
        bail!("Invalid packfile index signature");
    }

    let _version = reader.read_u32::<BigEndian>()?;
    let num_entries = reader.read_u32::<BigEndian>()?;

    Ok(PackIdxHeader { num_entries })
}

/// Parses an entry in a Git packfile index from the given reader.
fn parse_pack_idx_entry<R: Read>(reader: &mut R) -> Result<PackIdxEntry> {
    let mut sha1 = [0u8; 20];
    reader.read_exact(&mut sha1)?;
    let _offset = reader.read_u32::<BigEndian>()?;
    let _crc32 = reader.read_u32::<BigEndian>()?;

    Ok(PackIdxEntry { sha1 })
}

/// Parses a Git packfile index file and returns a vector of object hashes.
pub fn parse<P: AsRef<Path>>(file_path: P) -> Result<Vec<String>> {
    let mut reader = std::io::BufReader::new(File::open(file_path)?);
    let header = parse_pack_idx_header(&mut reader)?;

    let mut hashes = Vec::with_capacity(header.num_entries as usize);
    for _ in 0..header.num_entries {
        let entry = parse_pack_idx_entry(&mut reader)?;
        if entry.object_type() == 0 {
            continue; // Skip the 'bad' object type
        }

        hashes.push(hex::encode(entry.sha1));
    }

    Ok(hashes)
}
