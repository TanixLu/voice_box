use std::path::PathBuf;

use crate::{
    i18n::{Text, current_language},
    utils::ComGuard,
};

pub fn choose_files() -> anyhow::Result<Vec<PathBuf>> {
    choose_paths(false)
}

pub fn choose_folder() -> anyhow::Result<Option<PathBuf>> {
    Ok(choose_paths(true)?.into_iter().next())
}

fn choose_paths(folder: bool) -> anyhow::Result<Vec<PathBuf>> {
    use windows::{
        Win32::{
            System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST,
                FOS_FORCEFILESYSTEM, FOS_NOVALIDATE, FOS_PATHMUSTEXIST, FileOpenDialog,
                IFileOpenDialog, SIGDN_FILESYSPATH,
            },
        },
        core::{HSTRING, PCWSTR, w},
    };
    let _com_guard = ComGuard::sta()?;
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
    let language = current_language();
    let title = HSTRING::from(
        if folder {
            Text::ExportSoundsTitle
        } else {
            Text::ImportSoundsTitle
        }
        .in_language(language),
    );
    let audio_files = HSTRING::from(Text::AudioFiles.in_language(language));
    let all_files = HSTRING::from(Text::AllFiles.in_language(language));
    unsafe {
        dialog.SetTitle(PCWSTR(title.as_ptr()))?;
        let options = if folder {
            // Browse all files; the placeholder permits confirming the current folder.
            let label = HSTRING::from(Text::SelectExportFolder.in_language(language));
            dialog.SetOkButtonLabel(PCWSTR(label.as_ptr()))?;
            dialog.SetFileName(PCWSTR(label.as_ptr()))?;
            dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                pszName: PCWSTR(all_files.as_ptr()),
                pszSpec: w!("*.*"),
            }])?;
            (dialog.GetOptions()? & !FOS_FILEMUSTEXIST & !FOS_ALLOWMULTISELECT)
                | FOS_NOVALIDATE
                | FOS_PATHMUSTEXIST
        } else {
            dialog.GetOptions()? | FOS_FILEMUSTEXIST | FOS_ALLOWMULTISELECT
        };
        dialog.SetOptions(FOS_FORCEFILESYSTEM | options)?;
        if !folder {
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
        }
        if let Err(error) = dialog.Show(None) {
            if error.code().0 as u32 == 0x800704c7 {
                return Ok(Vec::new());
            }
            return Err(error.into());
        }
        if folder {
            let text = dialog.GetFolder()?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let result = text.to_string();
            CoTaskMemFree(Some(text.0.cast()));
            return Ok(vec![PathBuf::from(result?)]);
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
