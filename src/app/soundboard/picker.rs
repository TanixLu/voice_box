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
        core::w,
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
    unsafe {
        dialog.SetTitle(w!("批量导入音效"))?;
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM | FOS_ALLOWMULTISELECT,
        )?;
        dialog.SetFileTypes(&[
            COMDLG_FILTERSPEC {
                pszName: w!("音频文件"),
                pszSpec: w!("*.mp3;*.wav;*.flac;*.ogg"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("所有文件"),
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
