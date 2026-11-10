//! Win32 helpers that several EXE modules share. Helpers shared with the APO live in
//! `voice_box_shared::win`; a helper with one user lives with that user.

use std::{marker::PhantomData, rc::Rc};

pub struct ComGuard(PhantomData<Rc<()>>);
impl ComGuard {
    pub fn mta() -> anyhow::Result<Self> {
        wasapi::initialize_mta().ok()?;
        Ok(Self(PhantomData))
    }
    pub fn sta() -> anyhow::Result<Self> {
        wasapi::initialize_sta().ok()?;
        Ok(Self(PhantomData))
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}
