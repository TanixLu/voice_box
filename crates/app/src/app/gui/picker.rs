use crate::app::i18n::Text;
use std::path::PathBuf;

pub(super) fn choose_files() -> anyhow::Result<Vec<PathBuf>> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST,
                FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
            },
        },
        core::{HSTRING, PCWSTR, w},
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _apartment = Apartment;
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
    let language = crate::app::i18n::current_language();
    let title = HSTRING::from(Text::ImportSoundsTitle.in_language(language));
    let audio_files = HSTRING::from(Text::AudioFiles.in_language(language));
    let all_files = HSTRING::from(Text::AllFiles.in_language(language));
    unsafe {
        dialog.SetTitle(PCWSTR(title.as_ptr()))?;
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM | FOS_ALLOWMULTISELECT,
        )?;
        dialog.SetFileTypes(&[
            COMDLG_FILTERSPEC {
                pszName: PCWSTR(audio_files.as_ptr()),
                pszSpec: w!("*.mp3;*.wav;*.flac;*.ogg"),
            },
            COMDLG_FILTERSPEC {
                pszName: PCWSTR(all_files.as_ptr()),
                pszSpec: w!("*.*"),
            },
        ])?;
        if let Err(error) = dialog.Show(None) {
            if error.code().0 as u32 == 0x800704c7 {
                return Ok(Vec::new());
            }
            return Err(error.into());
        }
        let items = dialog.GetResults()?;
        let mut paths = Vec::new();
        for index in 0..items.GetCount()? {
            let text = items.GetItemAt(index)?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let result = text.to_string();
            CoTaskMemFree(Some(text.0.cast()));
            paths.push(PathBuf::from(result?));
        }
        Ok(paths)
    }
}
