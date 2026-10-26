//! Windows helpers used by both the management application and APO.
use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

pub fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
