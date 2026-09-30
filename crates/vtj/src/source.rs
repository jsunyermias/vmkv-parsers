//! Opened source files: identity for the header and positioned reads.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::types::Source;

/// A source file opened by the runner. Source ids follow the order of the
/// inputs on the command line, starting at 0.
#[derive(Debug)]
pub struct SourceFile {
    id: u64,
    path: PathBuf,
    size: u64,
    sha256: String,
    file: File,
}

impl SourceFile {
    /// Opens `path` and hashes its whole content.
    pub fn open(id: u64, path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut size = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let digest = hasher.finalize();
        let sha256 = digest.iter().map(|b| format!("{b:02x}")).collect();
        if size != file.metadata()?.len() {
            return Err(io::Error::other("source changed while it was being hashed"));
        }
        Ok(SourceFile { id, path, size, sha256, file })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The header entry. The path is not recorded: it is informational only
    /// and would make the output depend on how the parser was invoked.
    pub fn header_entry(&self) -> Source {
        Source { id: self.id, size: self.size, sha256: Some(self.sha256.clone()), path: None }
    }

    /// Reads exactly `buf.len()` bytes at `offset`. A read past the end
    /// returns `ErrorKind::UnexpectedEof`.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)
    }

    /// A buffered sequential reader starting at `offset` that tracks its position.
    pub fn stream_from(&mut self, offset: u64) -> io::Result<SourceStream<'_>> {
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(SourceStream { inner: BufReader::with_capacity(1 << 16, &mut self.file), pos: offset, size: self.size })
    }
}

/// Sequential reader over a source that knows its absolute byte offset.
pub struct SourceStream<'a> {
    inner: BufReader<&'a mut File>,
    pos: u64,
    size: u64,
}

impl SourceStream<'_> {
    /// Absolute offset of the next byte.
    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn remaining(&self) -> u64 {
        self.size.saturating_sub(self.pos)
    }

    /// Reads up to `buf.len()` bytes; returns fewer only at end of file.
    pub fn read_up_to(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            let n = self.inner.read(&mut buf[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        self.pos += got as u64;
        Ok(got)
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buf)?;
        self.pos += buf.len() as u64;
        Ok(())
    }

    /// Skips `n` bytes forward.
    pub fn skip(&mut self, n: u64) -> io::Result<()> {
        let delta = i64::try_from(n).map_err(|_| io::Error::other("skip too large"))?;
        self.inner.seek_relative(delta)?;
        self.pos += n;
        Ok(())
    }
}
