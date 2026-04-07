use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Size of a single FILEDESCRIPTORW struct in bytes.
/// dwFlags(4) + clsid(16) + sizel(8) + pointl(8) + dwFileAttributes(4) +
/// ftCreationTime(8) + ftLastAccessTime(8) + ftLastWriteTime(8) +
/// nFileSizeHigh(4) + nFileSizeLow(4) + cFileName[260](520) = 592 bytes
pub const FILEDESCRIPTORW_SIZE: usize = 592;

const FD_FILESIZE: u32 = 0x00000040;

#[derive(Clone, Debug)]
pub struct FileDescriptor {
    pub name: String,
    pub size: u64,
}

/// Build FileGroupDescriptorW bytes from file metadata.
pub fn serialize_file_group_descriptor(files: &[FileDescriptor]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + files.len() * FILEDESCRIPTORW_SIZE);

    // cItems
    buf.extend_from_slice(&(files.len() as u32).to_le_bytes());

    for file in files {
        let start = buf.len();

        // dwFlags
        buf.extend_from_slice(&FD_FILESIZE.to_le_bytes());
        // clsid (16 bytes) + sizel (8 bytes) + pointl (8 bytes) = 32 bytes reserved
        buf.extend_from_slice(&[0u8; 32]);
        // dwFileAttributes (0 = normal file)
        buf.extend_from_slice(&0u32.to_le_bytes());
        // ftCreationTime (8 bytes zero)
        buf.extend_from_slice(&[0u8; 8]);
        // ftLastAccessTime (8 bytes zero)
        buf.extend_from_slice(&[0u8; 8]);
        // ftLastWriteTime (8 bytes zero)
        buf.extend_from_slice(&[0u8; 8]);
        // nFileSizeHigh
        buf.extend_from_slice(&((file.size >> 32) as u32).to_le_bytes());
        // nFileSizeLow
        buf.extend_from_slice(&((file.size & 0xFFFFFFFF) as u32).to_le_bytes());

        // cFileName[260] — UTF-16LE, null-padded to 260 chars (520 bytes)
        let utf16: Vec<u16> = file.name.encode_utf16().collect();
        let mut name_buf = [0u8; 520];
        for (i, &code_unit) in utf16.iter().take(259).enumerate() {
            let bytes = code_unit.to_le_bytes();
            name_buf[i * 2] = bytes[0];
            name_buf[i * 2 + 1] = bytes[1];
        }
        buf.extend_from_slice(&name_buf);

        assert_eq!(buf.len() - start, FILEDESCRIPTORW_SIZE);
    }

    buf
}

/// Parse FileGroupDescriptorW bytes into file descriptors.
pub fn parse_file_group_descriptor(data: &[u8]) -> anyhow::Result<Vec<FileDescriptor>> {
    if data.len() < 4 {
        anyhow::bail!("FileGroupDescriptorW too short");
    }

    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut result = Vec::with_capacity(count);
    let mut offset = 4;

    for _ in 0..count {
        if offset + FILEDESCRIPTORW_SIZE > data.len() {
            anyhow::bail!("Truncated FileGroupDescriptorW");
        }

        let entry = &data[offset..offset + FILEDESCRIPTORW_SIZE];

        // nFileSizeHigh at byte offset 64, nFileSizeLow at 68
        // Layout: dwFlags(4)+clsid(16)+sizel(8)+pointl(8)+dwFileAttributes(4)+
        //         ftCreationTime(8)+ftLastAccessTime(8)+ftLastWriteTime(8) = 64 bytes
        let size_high = u32::from_le_bytes([entry[64], entry[65], entry[66], entry[67]]) as u64;
        let size_low = u32::from_le_bytes([entry[68], entry[69], entry[70], entry[71]]) as u64;
        let size = (size_high << 32) | size_low;

        // cFileName at byte offset 72, 520 bytes (260 UTF-16LE chars)
        let name_data = &entry[72..72 + 520];
        let words: Vec<u16> = name_data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&w| w != 0)
            .collect();
        let name = String::from_utf16_lossy(&words);

        // Security: reject path traversal
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            tracing::warn!("Rejected file with suspicious path: {}", name);
            offset += FILEDESCRIPTORW_SIZE;
            continue;
        }

        result.push(FileDescriptor { name, size });
        offset += FILEDESCRIPTORW_SIZE;
    }

    Ok(result)
}

/// Collect file descriptors from local file paths.
pub fn file_descriptors_from_paths(paths: &[PathBuf]) -> Vec<FileDescriptor> {
    paths
        .iter()
        .filter_map(|path| {
            let meta = fs::metadata(path).ok()?;
            let name = path.file_name()?.to_string_lossy().to_string();
            Some(FileDescriptor {
                name,
                size: meta.len(),
            })
        })
        .collect()
}

/// Read file contents at a specific offset and length.
pub fn read_file_range(
    path: &Path,
    offset: u64,
    length: u32,
    max_file_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let meta = fs::metadata(path)?;
    if meta.len() > max_file_size {
        anyhow::bail!("File exceeds maximum size limit");
    }

    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; length as usize];
    let bytes_read = file.read(&mut buf)?;
    buf.truncate(bytes_read);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_file_descriptor_roundtrip() {
        let desc = FileDescriptor {
            name: "test.txt".to_string(),
            size: 1234,
        };
        let bytes = serialize_file_group_descriptor(&[desc]);
        assert_eq!(bytes.len(), 4 + FILEDESCRIPTORW_SIZE);

        let descs = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].name, "test.txt");
        assert_eq!(descs[0].size, 1234);
    }

    #[test]
    fn file_descriptor_unicode_name() {
        let desc = FileDescriptor {
            name: "文档.pdf".to_string(),
            size: 0,
        };
        let bytes = serialize_file_group_descriptor(&[desc]);
        let parsed = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(parsed[0].name, "文档.pdf");
    }

    #[test]
    fn file_descriptor_multiple_files() {
        let descs = vec![
            FileDescriptor { name: "a.txt".to_string(), size: 100 },
            FileDescriptor { name: "b.png".to_string(), size: 200 },
        ];
        let bytes = serialize_file_group_descriptor(&descs);
        assert_eq!(bytes.len(), 4 + 2 * FILEDESCRIPTORW_SIZE);
        let parsed = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "a.txt");
        assert_eq!(parsed[1].size, 200);
    }

    #[test]
    fn file_descriptor_rejects_path_traversal() {
        let desc = FileDescriptor {
            name: "../../../etc/passwd".to_string(),
            size: 0,
        };
        let bytes = serialize_file_group_descriptor(&[desc]);
        let parsed = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(parsed.len(), 0);
    }

    #[test]
    fn file_group_descriptor_too_short() {
        let result = parse_file_group_descriptor(&[0u8; 2]);
        assert!(result.is_err());
    }
}
