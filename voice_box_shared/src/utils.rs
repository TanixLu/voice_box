//! Windows helpers used by both the management application and APO.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io,
    os::windows::ffi::OsStrExt,
    path::Path,
};

use atomic_write_file::AtomicWriteFile;

#[macro_export]
macro_rules! field_name {
    ($obj:expr, $field:ident) => {{
        if false {
            let _ = &$obj.$field;
        }
        stringify!($field)
    }};
}

pub fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub enum WriteMode {
    AtomicReplace,
    Append,
}

pub fn write_file_with_parents(
    path: impl AsRef<Path>,
    mode: WriteMode,
    write: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match mode {
        WriteMode::AtomicReplace => {
            let mut file = AtomicWriteFile::open(path)?;
            write(&mut file)?;
            file.commit()
        }
        WriteMode::Append => {
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            write(&mut file)
        }
    }
}
