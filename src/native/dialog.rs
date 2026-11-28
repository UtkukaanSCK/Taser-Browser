//! The system's Open and Save file dialogs, for bookmark import and export
//! (`bookmarks::transfer`). wry and tao have none.
//!
//! The dialog runs on a thread of its own, in its own COM apartment, owned
//! by the browser window (which it disables while it is open), the way
//! Chromium shows its file dialogs: the event loop keeps running and is
//! never re-entered from a nested modal loop.

use std::path::PathBuf;

/// Which dialog to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDialog {
    /// Pick an existing `.html` file to import.
    OpenBookmarks,
    /// Pick where to write the exported `.html` file.
    SaveBookmarks,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FileOpenDialog,
        FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
    };
    use windows::core::{HRESULT, Interface, w};

    /// The window's handle as a number, for `pick_file` on another thread.
    pub fn owner_handle(window: &tao::window::Window) -> isize {
        use tao::platform::windows::WindowExtWindows;
        window.hwnd()
    }

    /// Shows `kind` owned by the window `owner` (an `HWND` as a number, so it
    /// can cross to this thread) and waits for it to close. The picked file,
    /// or None when the user cancelled or the dialog failed. Call it on a
    /// thread of its own: it sets up and tears down that thread's COM.
    pub fn pick_file(owner: isize, kind: FileDialog) -> Option<PathBuf> {
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let picked = match unsafe { show(owner, kind) } {
            Ok(p) => p,
            Err(e) => {
                eprintln!("taser: file dialog failed: {e}");
                None
            }
        };
        if init.is_ok() {
            unsafe { CoUninitialize() };
        }
        picked
    }

    unsafe fn show(owner: isize, kind: FileDialog) -> windows::core::Result<Option<PathBuf>> {
        unsafe {
            let (dialog, title, extra): (IFileDialog, _, _) = match kind {
                FileDialog::OpenBookmarks => (
                    CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?.cast()?,
                    w!("Yer imlerini içe aktar"),
                    FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST,
                ),
                FileDialog::SaveBookmarks => (
                    CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?.cast()?,
                    w!("Yer imlerini dışa aktar"),
                    FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST,
                ),
            };
            let filters = [COMDLG_FILTERSPEC { pszName: w!("Yer imi dosyası (*.html)"), pszSpec: w!("*.html;*.htm") }];
            dialog.SetFileTypes(&filters)?;
            dialog.SetFileTypeIndex(1)?;
            dialog.SetDefaultExtension(w!("html"))?;
            dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | extra)?;
            dialog.SetTitle(title)?;
            if kind == FileDialog::SaveBookmarks {
                dialog.SetFileName(w!("yer-imleri.html"))?;
            }
            match dialog.Show(Some(HWND(owner as _))) {
                Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
                shown => shown?,
            }
            let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = name.to_string();
            CoTaskMemFree(Some(name.0 as _));
            Ok(Some(PathBuf::from(path?)))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn owner_handle(_: &tao::window::Window) -> isize {
        0
    }

    pub fn pick_file(_: isize, _: FileDialog) -> Option<PathBuf> {
        None
    }
}

pub use imp::*;
