use std::{cell::Cell, sync::mpsc};

use egui_winit::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4,
                NOTIFYICONDATAW, RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, ChangeWindowMessageFilterEx, CreatePopupMenu, DestroyMenu, GWL_STYLE,
                GetCursorPos, GetSystemMetrics, HICON, IMAGE_ICON, LR_SHARED, LoadImageW,
                MF_GRAYED, MF_STRING, MSGFLT_ALLOW, PostMessageW, RegisterWindowMessageW,
                SM_CXSMICON, SM_CYSMICON, STYLESTRUCT, SWP_SHOWWINDOW, SetForegroundWindow,
                TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WINDOWPOS, WM_APP,
                WM_CONTEXTMENU, WM_NCDESTROY, WM_NULL, WM_STYLECHANGING, WM_WINDOWPOSCHANGING,
                WS_VISIBLE,
            },
        },
    },
    core::{PCWSTR, w},
};

use voice_box_shared::utils::wide_null;

use crate::i18n::Text;

const CALLBACK: u32 = WM_APP + 20;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

pub enum Action {
    Show = 1,
    Exit,
}

pub struct Tray {
    data: Box<Data>,
    pub receiver: mpsc::Receiver<Action>,
}

struct Data {
    icon: NOTIFYICONDATAW,
    taskbar_created: u32,
    exit_enabled: Cell<bool>,
    available: Cell<bool>,
    startup_hidden: Cell<bool>,
    ctx: egui::Context,
    sender: mpsc::Sender<Action>,
}

impl Tray {
    pub fn new(cc: &eframe::CreationContext<'_>, start_in_tray: bool) -> anyhow::Result<Self> {
        // get the HWND of the main window
        let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
            anyhow::bail!("Expected a Win32 window");
        };
        let window = HWND(handle.hwnd.get() as _);

        let (sender, receiver) = mpsc::channel();
        unsafe {
            // LR_SHARED leaves the embedded icon owned by the executable module.
            let icon = LoadImageW(
                Some(GetModuleHandleW(None)?.into()),
                PCWSTR(1 as _),
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_SHARED,
            )?;
            let mut data = Box::new(Data {
                icon: NOTIFYICONDATAW {
                    cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: window,
                    uID: 1,
                    uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
                    uCallbackMessage: CALLBACK,
                    hIcon: HICON(icon.0),
                    ..Default::default()
                },
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                exit_enabled: Cell::new(false),
                available: Cell::new(false),
                startup_hidden: Cell::new(start_in_tray),
                ctx: cc.egui_ctx.clone(),
                sender,
            });
            let tip = wide_null("VoiceBox");
            data.icon.szTip[..tip.len()].copy_from_slice(&tip);
            anyhow::ensure!(data.taskbar_created != 0, "Cannot register TaskbarCreated");
            ChangeWindowMessageFilterEx(window, data.taskbar_created, MSGFLT_ALLOW, None)?;
            SetWindowSubclass(
                window,
                Some(window_proc),
                0,
                (&*data as *const Data) as usize,
            )
            .ok()?;
            let tray = Self { data, receiver };
            tray.data.add_icon()?;
            Ok(tray)
        }
    }

    pub fn set_exit_enabled(&self, enabled: bool) {
        self.data.exit_enabled.set(enabled);
    }

    pub fn available(&self) -> bool {
        self.data.available.get()
    }
}

impl Data {
    fn add_icon(&self) -> windows::core::Result<()> {
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &self.icon).ok()?;
            let mut icon = self.icon;
            icon.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let result = Shell_NotifyIconW(NIM_SETVERSION, &icon).ok();
            self.available.set(result.is_ok());
            if result.is_err() {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.icon);
            }
            result
        }
    }

    fn send(&self, action: Action) {
        if matches!(action, Action::Show) {
            self.startup_hidden.set(false);
        }
        let _ = self.sender.send(action);
        self.ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    fn show_menu(&self) -> windows::core::Result<()> {
        unsafe {
            let menu = CreatePopupMenu()?;
            let result = (|| {
                let show = wide_null(Text::ShowMainWindow.text());
                let exit = wide_null(Text::Exit.text());
                AppendMenuW(
                    menu,
                    MF_STRING,
                    Action::Show as usize,
                    PCWSTR(show.as_ptr()),
                )?;
                AppendMenuW(
                    menu,
                    if self.exit_enabled.get() {
                        MF_STRING
                    } else {
                        MF_STRING | MF_GRAYED
                    },
                    Action::Exit as usize,
                    PCWSTR(exit.as_ptr()),
                )?;
                let mut point = Default::default();
                GetCursorPos(&mut point)?;
                let _ = SetForegroundWindow(self.icon.hWnd);
                let selected = TrackPopupMenu(
                    menu,
                    TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    None,
                    self.icon.hWnd,
                    None,
                )
                .0 as usize;
                PostMessageW(Some(self.icon.hWnd), WM_NULL, WPARAM(0), LPARAM(0))?;
                if selected == Action::Show as usize {
                    self.send(Action::Show);
                } else if selected == Action::Exit as usize && self.exit_enabled.get() {
                    self.send(Action::Exit);
                } else {
                    let _ = Shell_NotifyIconW(NIM_SETFOCUS, &self.icon);
                }
                Ok(())
            })();
            let cleanup = DestroyMenu(menu);
            result.and(cleanup)
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data.icon);
            let _ = RemoveWindowSubclass(self.data.icon.hWnd, Some(window_proc), 0);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    data: usize,
) -> LRESULT {
    // The boxed data stays alive until the subclass is removed on this UI thread.
    let data = unsafe { &*(data as *const Data) };
    if message == WM_WINDOWPOSCHANGING && data.startup_hidden.get() {
        // eframe 0.37 unconditionally shows the root window after its first paint.
        let position = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        position.flags &= !SWP_SHOWWINDOW;
    } else if message == WM_STYLECHANGING
        && wparam.0 as i32 == GWL_STYLE.0
        && data.startup_hidden.get()
    {
        let style = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        style.styleNew &= !WS_VISIBLE.0;
    } else if message == data.taskbar_created {
        data.available.set(false);
        if let Err(error) = data.add_icon() {
            tracing::warn!("Failed to restore tray icon: {error}");
            data.send(Action::Show);
        }
    } else if message == CALLBACK {
        match lparam.0 as u32 & 0xffff {
            NIN_SELECT | NIN_KEYSELECT => data.send(Action::Show),
            WM_CONTEXTMENU => {
                if let Err(error) = data.show_menu() {
                    tracing::warn!("Failed to show tray menu: {error}");
                }
            }
            _ => {}
        }
        return LRESULT(0);
    }
    unsafe {
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(window, Some(window_proc), subclass_id);
        }
        DefSubclassProc(window, message, wparam, lparam)
    }
}
