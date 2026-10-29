//! Private scratch storage. Values are never trusted protocol inputs.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    sync::{Arc, Mutex, Weak},
};

struct Files {
    db: Connection,
    payload: File,
    spans: File,
    next_table: u64,
    pending: usize,
    _directory: tempfile::TempDir,
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = self.db.execute_batch("COMMIT");
    }
}

/// All scratch tables on a thread share one bounded SQLite page cache.
#[derive(Clone)]
pub struct Archive(Arc<Mutex<Files>>);

thread_local! { static CURRENT: RefCell<Weak<Mutex<Files>>> = RefCell::new(Weak::new()); }

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Archive")
    }
}

/// A range in this session's append-only payload file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub offset: u64,
    pub len: u64,
}

impl Archive {
    pub fn current() -> Self {
        CURRENT.with(|current| {
            if let Some(files) = current.borrow().upgrade() { return Self(files); }
            let root = scratch_root().expect("create Raster scratch root");
            let directory = tempfile::Builder::new().prefix("raster-trace-").tempdir_in(root)
                .expect("create Raster scratch directory");
            let db = Connection::open(directory.path().join("index.sqlite"))
                .expect("open Raster scratch index");
            db.execute_batch("PRAGMA cache_size=-245760; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;
                CREATE TABLE records (space INTEGER, key BLOB, value BLOB, offset INTEGER, length INTEGER,
                    PRIMARY KEY(space,key)) WITHOUT ROWID;
                CREATE TABLE chunks (hash BLOB PRIMARY KEY, offset INTEGER NOT NULL, length INTEGER NOT NULL) WITHOUT ROWID; BEGIN")
                .expect("initialize Raster scratch index");
            let payload = File::options().create_new(true).read(true).write(true)
                .open(directory.path().join("payloads.bin")).expect("create Raster scratch payloads");
            let spans = File::options().create_new(true).read(true).write(true)
                .open(directory.path().join("spans.bin")).expect("create scratch span index");
            let files = Arc::new(Mutex::new(Files { db, payload, spans, next_table: 0, pending: 0, _directory: directory }));
            *current.borrow_mut() = Arc::downgrade(&files);
            Self(files)
        })
    }

    /// Surface deferred SQLite write errors before publishing a successful artifact.
    pub fn flush(&self) -> std::io::Result<()> {
        let mut files = self.0.lock().expect("scratch lock");
        files
            .db
            .execute_batch("COMMIT; BEGIN")
            .map_err(std::io::Error::other)?;
        files.pending = 0;
        files.payload.flush()?;
        files.spans.flush()
    }

    #[cfg(test)]
    pub(crate) fn reopen(&self) {
        self.flush().unwrap();
        let mut files = self.0.lock().unwrap();
        let db = Connection::open(files._directory.path().join("index.sqlite")).unwrap();
        db.execute_batch(
            "PRAGMA cache_size=-245760; PRAGMA mmap_size=0; PRAGMA temp_store=FILE; BEGIN",
        )
        .unwrap();
        files.db = db;
        files.payload = File::options()
            .read(true)
            .write(true)
            .open(files._directory.path().join("payloads.bin"))
            .unwrap();
        files.spans = File::options()
            .read(true)
            .write(true)
            .open(files._directory.path().join("spans.bin"))
            .unwrap();
    }

    pub fn append(&self, bytes: &[u8]) -> std::io::Result<Blob> {
        self.0.lock().expect("scratch lock").append(bytes)
    }

    pub fn read(&self, blob: Blob, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
        self.0.lock().expect("scratch lock").read(blob, offset, len)
    }

    pub fn stored_bytes(&self) -> std::io::Result<u64> {
        let files = self.0.lock().expect("scratch lock");
        let mut bytes = files.payload.metadata()?.len() + files.spans.metadata()?.len();
        for name in ["index.sqlite", "index.sqlite-journal"] {
            if let Ok(metadata) = std::fs::metadata(files._directory.path().join(name)) {
                bytes += metadata.len();
            }
        }
        Ok(bytes)
    }

    pub fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn table<K, V>(&self) -> DiskMap<K, V> {
        let mut files = self.0.lock().expect("scratch lock");
        let space = files.next_table;
        files.next_table += 1;
        DiskMap {
            archive: self.clone(),
            space,
            _types: PhantomData,
        }
    }
}

/// The backend supplies an absolute real-disk path. Standalone CLI callers can
/// also set RASTER_SCRATCH_DIR; otherwise use target/raster-scratch in the working directory.
pub fn scratch_root() -> std::io::Result<std::path::PathBuf> {
    let root = std::env::var_os("RASTER_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .expect("current directory")
                .join("target/raster-scratch")
        });
    std::fs::create_dir_all(&root)?;
    check_space(&root)?;
    Ok(root)
}

/// Leave room for the current write batch, reports, and other processes.
pub fn check_space(root: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::ffi::CString::new(root.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // statvfs initializes the structure on success; path is NUL-terminated.
        if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let info = unsafe { info.assume_init() };
        let available = info.f_bavail as u64 * info.f_frsize as u64;
        if available < 4 * 1024 * 1024 * 1024 {
            return Err(std::io::Error::other(format!(
                "Raster scratch {} has only {} MiB free; stopping before exhausting the filesystem",
                root.display(),
                available / 1024 / 1024
            )));
        }
        #[cfg(target_os = "linux")]
        {
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if unsafe { filesystem.assume_init() }.f_type == 0x01021994 {
                return Err(std::io::Error::other(format!("Raster scratch {} is RAM-backed tmpfs; set RASTER_SCRATCH_DIR to a real-disk directory", root.display())));
            }
        }
    }
    Ok(())
}

// Content-defined boundaries let identical payloads share chunks even when
// their surrounding witness encodings have different lengths. These hashes
// index private scratch bytes only; they never enter a commitment or receipt.
const fn gear(byte: u8) -> u64 {
    let mut x = (byte as u64).wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
const GEARS: [u64; 256] = {
    let mut values = [0; 256];
    let mut i = 0;
    while i < 256 {
        values[i] = gear(i as u8);
        i += 1;
    }
    values
};
impl Files {
    fn wrote(&mut self) -> std::io::Result<()> {
        self.pending += 1;
        if self.pending >= 512 {
            check_space(self._directory.path())?;
            self.db
                .execute_batch("COMMIT; BEGIN")
                .map_err(std::io::Error::other)?;
            self.pending = 0;
        }
        Ok(())
    }
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<Blob> {
        let mut ranges = Vec::new();
        let mut start = 0;
        while start < bytes.len() {
            let limit = (start + 65536).min(bytes.len());
            let mut end = (start + 8192).min(limit);
            let mut rolling = 0u64;
            while end < limit {
                rolling = rolling
                    .wrapping_shl(1)
                    .wrapping_add(GEARS[bytes[end] as usize]);
                end += 1;
                if rolling & 16383 == 0 {
                    break;
                }
            }
            let chunk = &bytes[start..end];
            let hash = Sha256::digest(chunk);
            let existing: Option<(u64, usize)> = self
                .db
                .query_row(
                    "SELECT offset,length FROM chunks WHERE hash=?1",
                    params![hash.as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(std::io::Error::other)?;
            let offset = if let Some((offset, len)) = existing {
                // Reuse only byte-identical data, even in the event of a hash collision.
                let mut previous = vec![0; len];
                self.payload.seek(SeekFrom::Start(offset))?;
                self.payload.read_exact(&mut previous)?;
                if previous != chunk {
                    return Err(std::io::Error::other("scratch chunk hash collision"));
                }
                offset
            } else {
                let offset = self.payload.seek(SeekFrom::End(0))?;
                self.payload.write_all(chunk)?;
                self.db
                    .execute(
                        "INSERT INTO chunks VALUES(?1,?2,?3)",
                        params![hash.as_slice(), offset, chunk.len() as u64],
                    )
                    .map_err(std::io::Error::other)?;
                self.wrote()?;
                offset
            };
            ranges.push((end as u64, offset));
            start = end;
        }
        let offset = self.spans.seek(SeekFrom::End(0))?;
        self.spans.write_all(&(ranges.len() as u64).to_le_bytes())?;
        for (end, position) in ranges {
            self.spans.write_all(&end.to_le_bytes())?;
            self.spans.write_all(&position.to_le_bytes())?;
        }
        Ok(Blob {
            offset,
            len: bytes.len() as u64,
        })
    }

    fn span(&mut self, blob: Blob, index: u64) -> std::io::Result<(u64, u64)> {
        self.spans
            .seek(SeekFrom::Start(blob.offset + 8 + index * 16))?;
        let mut bytes = [0; 16];
        self.spans.read_exact(&mut bytes)?;
        Ok((
            u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        ))
    }

    fn read(&mut self, blob: Blob, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= blob.len)
            .ok_or_else(|| std::io::Error::other("scratch range outside payload"))?;
        let size = usize::try_from(len).map_err(std::io::Error::other)?;
        let mut out = vec![0; size];
        if len == 0 {
            return Ok(out);
        }
        self.spans.seek(SeekFrom::Start(blob.offset))?;
        let mut count = [0; 8];
        self.spans.read_exact(&mut count)?;
        let mut lo = 0;
        let mut hi = u64::from_le_bytes(count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.span(blob, mid)?.0 <= offset {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut base = if lo == 0 {
            0
        } else {
            self.span(blob, lo - 1)?.0
        };
        let mut cursor = offset;
        while cursor < end {
            let (chunk_end, position) = self.span(blob, lo)?;
            if chunk_end <= cursor || chunk_end > blob.len {
                return Err(std::io::Error::other("invalid scratch span"));
            }
            let take = (chunk_end.min(end) - cursor) as usize;
            self.payload
                .seek(SeekFrom::Start(position + cursor - base))?;
            let dest = (cursor - offset) as usize;
            self.payload.read_exact(&mut out[dest..dest + take])?;
            cursor += take as u64;
            base = chunk_end;
            lo += 1;
        }
        Ok(out)
    }
}

/// Owning reads deliberately cannot leave references to every past value in RAM.
#[derive(Debug)]
pub struct DiskMap<K, V> {
    archive: Archive,
    space: u64,
    _types: PhantomData<(K, V)>,
}

impl<K: Serialize, V: Serialize + DeserializeOwned> DiskMap<K, V> {
    pub fn new() -> Self {
        Archive::current().table()
    }
    pub fn contains_key(&self, key: &K) -> bool {
        let key = bincode::serialize(key).expect("encode scratch key");
        let files = self.archive.0.lock().expect("scratch lock");
        files
            .db
            .query_row(
                "SELECT 1 FROM records WHERE space=?1 AND key=?2",
                params![self.space, key],
                |_| Ok(()),
            )
            .optional()
            .expect("read Raster scratch index")
            .is_some()
    }
    pub fn insert(&mut self, key: K, value: V) {
        self.put(&key, &value);
    }
    pub fn put(&self, key: &K, value: &V) {
        let key = bincode::serialize(key).expect("encode scratch key");
        let value = bincode::serialize(value).expect("encode scratch value");
        let mut files = self.archive.0.lock().expect("scratch lock");
        if value.len() <= 4096 {
            files
                .db
                .execute(
                    "INSERT OR REPLACE INTO records VALUES(?1,?2,?3,NULL,NULL)",
                    params![self.space, key, value],
                )
                .expect("write Raster scratch index (check free disk space)");
        } else {
            let blob = files
                .append(&value)
                .expect("write Raster scratch payload (check free disk space)");
            let offset = blob.offset;
            files
                .db
                .execute(
                    "INSERT OR REPLACE INTO records VALUES(?1,?2,NULL,?3,?4)",
                    params![self.space, key, offset, value.len() as u64],
                )
                .expect("write Raster scratch index (check free disk space)");
        }
        files.wrote().expect("flush Raster scratch index");
    }
    pub fn get(&self, key: &K) -> Option<V> {
        let key = bincode::serialize(key).expect("encode scratch key");
        let mut files = self.archive.0.lock().expect("scratch lock");
        let row: Option<(Option<Vec<u8>>, Option<u64>, Option<usize>)> = files
            .db
            .query_row(
                "SELECT value,offset,length FROM records WHERE space=?1 AND key=?2",
                params![self.space, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .expect("read Raster scratch index");
        row.map(|(inline, offset, length)| {
            let bytes = inline.unwrap_or_else(|| {
                let len = length.expect("scratch payload length") as u64;
                files
                    .read(
                        Blob {
                            offset: offset.expect("scratch payload offset"),
                            len,
                        },
                        0,
                        len,
                    )
                    .expect("read Raster scratch payload")
            });
            bincode::deserialize(&bytes).expect("decode Raster scratch value")
        })
    }
}

impl<K: Serialize, V: Serialize + DeserializeOwned> Default for DiskMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct DiskVec<T> {
    values: DiskMap<u64, T>,
    len: u64,
}
impl<T: Serialize + DeserializeOwned> DiskVec<T> {
    pub fn new() -> Self {
        Self {
            values: DiskMap::new(),
            len: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.len.try_into().expect("history length overflow")
    }
    pub fn push(&mut self, value: T) {
        self.values.insert(self.len, value);
        self.len += 1;
    }
    pub fn set(&mut self, index: usize, value: T) {
        assert!(index < self.len());
        self.values.insert(index as u64, value);
    }
    pub fn get(&self, index: usize) -> Option<T> {
        self.values.get(&(index as u64))
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = T> + '_ {
        (0..self.len).map(|i| self.values.get(&i).expect("missing scratch history item"))
    }
}
impl<T: Serialize + DeserializeOwned> Default for DiskVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_payloads_survive_shifted_encodings_and_reopen() {
        let archive = Archive::current();
        let mut state = 7u64;
        let bytes: Vec<u8> = (0..512 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let first = archive.append(&bytes).unwrap();
        let before = archive.0.lock().unwrap().payload.metadata().unwrap().len();
        let shifted = [
            b"a different serialized prefix".as_slice(),
            bytes.as_slice(),
        ]
        .concat();
        let second = archive.append(&shifted).unwrap();
        let after = archive.0.lock().unwrap().payload.metadata().unwrap().len();
        assert!(after - before < 128 * 1024, "repeated payload was copied");
        archive.reopen();
        assert_eq!(archive.read(first, 0, first.len).unwrap(), bytes);
        assert_eq!(archive.read(second, 0, second.len).unwrap(), shifted);
        assert_eq!(
            archive.read(first, 60000, 70000).unwrap(),
            bytes[60000..130000]
        );
    }
}
