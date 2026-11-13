//! Serving an index from a read-only filesystem (a mounted volume, or baked into an image run
//! as an arbitrary UID). tantivy takes a lock file while opening segments, to stop a writer
//! deleting them meanwhile; a served index has no writer, so the lock can be skipped.

use std::io;
use std::path::Path;
use std::sync::Arc;

use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, WatchCallback, WatchHandle, WritePtr,
};

#[derive(Clone, Debug)]
pub struct ReadOnlyDirectory(MmapDirectory);

impl ReadOnlyDirectory {
    pub fn open(path: &Path) -> tantivy::Result<ReadOnlyDirectory> {
        Ok(ReadOnlyDirectory(MmapDirectory::open(path)?))
    }
}

impl Directory for ReadOnlyDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.0.get_file_handle(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.0.exists(path)
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.0.atomic_read(path)
    }

    fn acquire_lock(&self, _lock: &Lock) -> Result<DirectoryLock, LockError> {
        Ok(DirectoryLock::from(Box::new(())))
    }

    fn watch(&self, _watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        Ok(WatchHandle::empty())
    }

    // Writes go to the underlying directory, and fail there if the filesystem is read-only.

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.0.delete(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        self.0.open_write(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.0.atomic_write(path, data)
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.0.sync_directory()
    }
}
