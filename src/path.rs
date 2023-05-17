use std::{fs::create_dir_all, io, path::Path};

pub trait CreateParentDirs {
    fn create_parent_dirs(&self) -> io::Result<()>;
}

impl CreateParentDirs for Path {
    fn create_parent_dirs(&self) -> io::Result<()> {
        if let Some(parent) = self.parent() {
            create_dir_all(parent)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Parent directory unavailable",
            ))
        }
    }
}
