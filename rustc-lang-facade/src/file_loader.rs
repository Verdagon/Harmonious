
use rustc_data_structures::sync::Lrc;
use rustc_span::source_map::{FileLoader, RealFileLoader};
use std::io;
use std::path::Path;

pub struct LangFileLoader {
    inner: RealFileLoader,
    stubs: String,
}

impl LangFileLoader {
    pub fn new(stubs: String) -> Self {
        Self { inner: RealFileLoader, stubs }
    }
}

impl FileLoader for LangFileLoader {
    fn file_exists(&self, path: &Path) -> bool {
        if is_stubs_path(path) { return true; }
        self.inner.file_exists(path)
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        if is_stubs_path(path) { return Ok(self.stubs.clone()); }
        self.inner.read_file(path)
    }

    fn read_binary_file(&self, path: &Path) -> io::Result<Lrc<[u8]>> {
        if is_stubs_path(path) {
            return Ok(Lrc::from(self.stubs.as_bytes()));
        }
        self.inner.read_binary_file(path)
    }
}

fn is_stubs_path(path: &Path) -> bool {
    path.file_name().map_or(false, |n| n == "__lang_stubs.rs")
}
