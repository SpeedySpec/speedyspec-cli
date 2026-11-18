//! Builds zip archives byte by byte, so tests can produce entries (hostile names, duplicates,
//! symlinks) that a well-behaved zip writer would refuse to write.

/// One entry of an archive under construction.
#[derive(Clone, Debug)]
pub enum Entry {
    /// Contents and Unix mode bits (for example `0o644`).
    File(Vec<u8>, u32),
    Dir,
    /// A symbolic link to this target.
    Symlink(String),
}

impl Entry {
    /// A regular file with mode `0o644`.
    pub fn file(contents: &str) -> Entry {
        Entry::File(contents.as_bytes().to_vec(), 0o644)
    }
}

/// A stored (uncompressed) zip archive holding `entries`, in order, with the names exactly as
/// given.
pub fn build_zip(entries: &[(&str, Entry)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, entry) in entries {
        let (data, mode): (Vec<u8>, u32) = match entry {
            Entry::File(data, mode) => (data.clone(), 0o100_000 | (mode & 0o7777)),
            Entry::Dir => (Vec::new(), 0o040_755),
            Entry::Symlink(target) => (target.as_bytes().to_vec(), 0o120_777),
        };
        let crc = crc32(&data);
        let size = u32::try_from(data.len()).expect("test entries are small");
        let name_len = u16::try_from(name.len()).expect("test names are short");
        let offset = u32::try_from(out.len()).expect("test archives are small");

        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0x0800u16.to_le_bytes()); // flags: UTF-8 names
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0x0021u16.to_le_bytes()); // date: 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&data);

        // Central directory header.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&0x031eu16.to_le_bytes()); // made by: Unix, 3.0
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x0021u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&(mode << 16).to_le_bytes()); // external: Unix mode
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let count = u16::try_from(entries.len()).expect("test archives are small");
    let central_offset = u32::try_from(out.len()).expect("test archives are small");
    let central_size = u32::try_from(central.len()).expect("test archives are small");
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }
}
