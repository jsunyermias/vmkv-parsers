//! Opened source files: identity for the header and positioned reads.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    /// Captured when the file was hashed; `None` when the filesystem does
    /// not report one. Used only to catch a change of content, never
    /// written to the output.
    mtime: Option<SystemTime>,
}

impl SourceFile {
    /// Opens `path` and hashes its whole content.
    pub fn open(id: u64, path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        // Metadata from both before and after hashing (decision 48): a
        // write that lands entirely inside the read loop and finishes with
        // the same final size would otherwise pass the size-only check
        // below while `sha256` matches neither the old nor the new content.
        let before = file.metadata()?;
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
        // Test-only seam (decision 48): lets a test land a rewrite exactly
        // here, in the window a real concurrent writer could also hit —
        // one that mutating the file only after `open()` returns cannot
        // reach. Compiled out of, and a no-op cost in, non-test builds.
        #[cfg(test)]
        tests::run_mid_hash_hook();
        let after = file.metadata()?;
        let mtime_unchanged = match (before.modified(), after.modified()) {
            (Ok(a), Ok(b)) => a == b,
            // No mtime on this filesystem: nothing more to check than size.
            _ => true,
        };
        if size != after.len() || before.len() != after.len() || !mtime_unchanged {
            return Err(io::Error::other("source changed while it was being hashed"));
        }
        let mtime = after.modified().ok();
        Ok(SourceFile { id, path, size, sha256, file, mtime })
    }

    /// Whether the file's size and modification time (when available) still
    /// match what was seen at [`open`](Self::open). A cooperative check
    /// only: it catches an ordinary concurrent write between hashing and the
    /// end of parsing, not a hostile rewrite that restores both, nor one
    /// that lands between this check and the parser's last read. Parsers
    /// that need a strong guarantee against concurrent modification should
    /// parse a private snapshot instead.
    pub fn verify_unchanged(&self) -> bool {
        match self.file.metadata() {
            Ok(meta) => meta.len() == self.size && self.mtime.is_none_or(|t| meta.modified().is_ok_and(|m| m == t)),
            Err(_) => false,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    fn temp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vtj-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    thread_local! {
        /// Set by a test to run exactly once, between `open`'s read loop and
        /// its post-hash `metadata()` call — the window a same-size
        /// concurrent rewrite finishing there would otherwise slip through.
        static MID_HASH_HOOK: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    }

    pub(super) fn run_mid_hash_hook() {
        MID_HASH_HOOK.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f();
            }
        });
    }

    /// Runs `open` with `hook` firing once mid-hash, resetting the hook
    /// afterwards regardless of outcome (tests may share a worker thread).
    fn open_with_mid_hash_hook(path: &Path, hook: impl FnMut() + 'static) -> io::Result<SourceFile> {
        MID_HASH_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        let result = SourceFile::open(0, path);
        MID_HASH_HOOK.with(|h| *h.borrow_mut() = None);
        result
    }

    #[test]
    fn unchanged_file_verifies() {
        let p = temp("unchanged.bin", &[1, 2, 3]);
        let src = SourceFile::open(0, &p).unwrap();
        assert!(src.verify_unchanged());
    }

    #[test]
    fn a_size_change_is_caught() {
        let p = temp("resized.bin", &[1, 2, 3]);
        let src = SourceFile::open(0, &p).unwrap();
        std::fs::write(&p, [1, 2, 3, 4]).unwrap();
        assert!(!src.verify_unchanged(), "same content prefix, but the file grew");
    }

    #[test]
    fn same_size_different_content_is_caught_by_mtime() {
        let p = temp("rewritten.bin", &[1, 2, 3]);
        let src = SourceFile::open(0, &p).unwrap();
        // Same length, different bytes: only mtime can tell the two apart.
        // Set it explicitly instead of relying on the clock actually
        // advancing between the two writes, which a fast test can outrun on
        // a filesystem with coarse timestamp resolution.
        std::fs::write(&p, [9, 9, 9]).unwrap();
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        assert!(!src.verify_unchanged());
    }

    /// The window `verify_unchanged` cannot see: a same-size rewrite that
    /// finishes strictly between the read loop hashing the old content and
    /// `open`'s own post-hash `metadata()` call, before a `SourceFile` even
    /// exists to check later. Only reachable through the mid-hash seam — a
    /// rewrite issued after `open()` returns exercises a different, already
    /// covered window (the tests above).
    #[test]
    fn a_same_size_rewrite_finishing_during_the_hash_is_caught() {
        let p = temp("racy.bin", &[1, 2, 3]);
        let result = open_with_mid_hash_hook(&p, {
            let p = p.clone();
            move || {
                std::fs::write(&p, [9, 9, 9]).unwrap();
                // Force a different mtime regardless of clock resolution,
                // same as the other mtime-only test above.
                File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_modified(SystemTime::now() + Duration::from_secs(60))
                    .unwrap();
            }
        });
        assert!(result.is_err(), "a rewrite that finished during the hash must not be accepted as the baseline");
    }

    /// The hook itself must be inert unless a test actually sets it: no
    /// false positives on an ordinary, uncontended open.
    #[test]
    fn the_mid_hash_hook_is_a_no_op_when_unset() {
        let p = temp("quiet.bin", &[1, 2, 3]);
        assert!(SourceFile::open(0, &p).is_ok());
    }
}
