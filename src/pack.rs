use byteorder::{BigEndian, ReadBytesExt};
use color_eyre::{eyre::bail, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The signature at the beginning of a Git packfile index.
const IDX_SIGNATURE: &[u8; 4] = b"\xfftOc";

type PackSha1 = [u8; 20];
trait Sha1Ext {
    fn object_type(&self) -> u8;
}

impl Sha1Ext for PackSha1 {
    /// Returns the type of the object this entry refers to.
    fn object_type(&self) -> u8 {
        (self[0] >> 4) & 0b111
    }
}

/// Parses the header of a Git packfile index from the given reader
/// and extracts the number of entries in the index.
fn parse_entry_count<R: Read>(reader: &mut R) -> Result<u32> {
    let mut signature_buf = [0u8; 4];
    reader.read_exact(&mut signature_buf)?;

    if signature_buf != *IDX_SIGNATURE {
        bail!("Invalid packfile index signature");
    }
    let _version = reader.read_u32::<BigEndian>()?;

    Ok(reader.read_u32::<BigEndian>()?)
}

/// Parses an entry in a Git packfile index from the given reader
/// and extracts the SHA-1 hash of the object being referred to.
fn parse_entry<R: Read>(reader: &mut R) -> Result<PackSha1> {
    // The SHA-1 hash of the object this entry refers to.
    let mut sha1 = [0u8; 20];
    reader.read_exact(&mut sha1)?;
    let _offset = reader.read_u32::<BigEndian>()?;
    let _crc32 = reader.read_u32::<BigEndian>()?;
    Ok(sha1)
}

/// Parses a Git packfile index file and returns a vector of object hashes.
pub fn parse<P: AsRef<Path>>(file_path: P) -> Result<Vec<String>> {
    let mut reader = std::io::BufReader::new(File::open(file_path)?);
    let entry_count = parse_entry_count(&mut reader)?;

    let mut hashes = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let sha1 = parse_entry(&mut reader)?;
        if sha1.object_type() == 0 {
            continue; // Skip the 'bad' object type
        }

        hashes.push(hex::encode(sha1));
    }

    Ok(hashes)
}
