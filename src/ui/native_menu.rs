//! The Win32 menu bar (a real `HMENU` on the window, not egui), with a dark theme to match the
//! app: dark title bar (`DWMWA_USE_IMMERSIVE_DARK_MODE`), dark popups (uxtheme preferred app
//! mode), and the bar itself painted through `WM_UAHDRAWMENU` / `WM_UAHDRAWMENUITEM` — the
//! messages Windows uses to draw menu bars, handled the same way Notepad++ and others do, since
//! Windows always draws the bar light.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use windows::core::{w, BOOL, PCSTR, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_GRAYED, ODS_HOTLIGHT, ODS_INACTIVE, ODS_NOACCEL, ODS_SELECTED};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

/// What a menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuCmd {
    ToggleStream = 100,
    ToggleRecord,
    ShowRecordings,
    ShowSettingsFolder,
    Settings,
    Exit,
    Fit = 200,
    Stretch,
    Center,
    ResetTransform,
    RemoveLayer,
    NewScene = 300,
    SaveScenes,
    ExportScenes,
    ImportScenes,
    ViewAudio = 400,
    ViewProperties,
    ViewHealth,
    DarkTheme,
    About = 500,
}

impl MenuCmd {
    const ALL: [MenuCmd; 19] = [
        MenuCmd::ToggleStream,
        MenuCmd::ToggleRecord,
        MenuCmd::ShowRecordings,
        MenuCmd::ShowSettingsFolder,
        MenuCmd::Settings,
        MenuCmd::Exit,
        MenuCmd::Fit,
        MenuCmd::Stretch,
        MenuCmd::Center,
        MenuCmd::ResetTransform,
        MenuCmd::RemoveLayer,
        MenuCmd::NewScene,
        MenuCmd::SaveScenes,
        MenuCmd::ExportScenes,
        MenuCmd::ImportScenes,
        MenuCmd::ViewAudio,
        MenuCmd::ViewProperties,
        MenuCmd::ViewHealth,
        MenuCmd::DarkTheme,
    ];

    fn from_id(id: u32) -> Option<MenuCmd> {
        Self::ALL.iter().copied().chain([MenuCmd::About]).find(|c| *c as u32 == id)
    }
}

static QUEUE: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();
static DARK: AtomicBool = AtomicBool::new(true);
static BAR_BG: AtomicU32 = AtomicU32::new(0x001E1E1E);
static BAR_HOT: AtomicU32 = AtomicU32::new(0x00383838);
static BAR_TEXT: AtomicU32 = AtomicU32::new(0x00E8E8E8);

const WM_UAHDRAWMENU: u32 = 0x0091;
const WM_UAHDRAWMENUITEM: u32 = 0x0092;

#[repr(C)]
struct UahMenu {
    hmenu: HMENU,
    hdc: HDC,
    flags: u32,
}

#[repr(C)]
struct UahMenuItem {
    position: i32,
    // UAHMENUITEMMETRICS (union of 2 or 4 SIZEs) + UAHMENUPOPUPMETRICS (4 DWORDs + flags).
    metrics: [u32; 8],
    popup_metrics: [u32; 5],
}

#[repr(C)]
struct UahDrawMenuItem {
    dis: DRAWITEMSTRUCT,
    um: UahMenu,
    umi: UahMenuItem,
}

fn colorref(c: egui::Color32) -> u32 {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

pub struct NativeMenu {
    hwnd: HWND,
    bar: HMENU,
    /// Popup holding each command, for enable / check / relabel.
    owner: HashMap<u32, HMENU>,
    enabled: HashMap<u32, bool>,
    checked: HashMap<u32, bool>,
    labels: HashMap<u32, String>,
    /// The language the items are in.
    lang: crate::i18n::Lang,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl NativeMenu {
    /// Build the menu bar on the eframe window.
    pub fn install(cc: &eframe::CreationContext<'_>) -> Option<NativeMenu> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = cc.window_handle().ok()?;
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return None };
        let hwnd = HWND(h.hwnd.get() as *mut _);
        let _ = CTX.set(cc.egui_ctx.clone());
        unsafe {
            let bar = CreateMenu().ok()?;
            let mut m = NativeMenu { hwnd, bar, owner: HashMap::new(), enabled: HashMap::new(), checked: HashMap::new(), labels: HashMap::new(), lang: crate::i18n::lang() };
            m.fill()?;
            SetMenu(hwnd, Some(bar)).ok()?;
            let _ = SetWindowSubclass(hwnd, Some(subclass), 0x4C42, 0);
            let _ = DrawMenuBar(hwnd);
            Some(m)
        }
    }

    /// The menus and their items, in the current language.
    unsafe fn fill(&mut self) -> Option<()> {
        self.popup(
            tr!("&File"),
            &[
                Some((MenuCmd::ToggleStream, tr!("&Go Live"))),
                Some((MenuCmd::ToggleRecord, tr!("Start &Recording"))),
                None,
                Some((MenuCmd::ShowRecordings, tr!("Show Re&cordings"))),
                Some((MenuCmd::ShowSettingsFolder, tr!("Show Settings &Folder"))),
                None,
                Some((MenuCmd::Settings, tr!("&Settings…\tCtrl+,"))),
                None,
                Some((MenuCmd::Exit, tr!("E&xit\tAlt+F4"))),
            ],
        )?;
        self.popup(
            tr!("&Edit"),
            &[
                Some((MenuCmd::Fit, tr!("&Fit to Canvas"))),
                Some((MenuCmd::Stretch, tr!("&Stretch to Canvas"))),
                Some((MenuCmd::Center, tr!("&Center"))),
                Some((MenuCmd::ResetTransform, tr!("&Reset Transform"))),
                None,
                Some((MenuCmd::RemoveLayer, tr!("Re&move Layer\tDel"))),
            ],
        )?;
        self.popup(
            tr!("&Scenes"),
            &[
                Some((MenuCmd::NewScene, tr!("&New Scene"))),
                Some((MenuCmd::SaveScenes, tr!("&Save Now\tCtrl+S"))),
                None,
                Some((MenuCmd::ExportScenes, tr!("&Export Collection…"))),
                Some((MenuCmd::ImportScenes, tr!("&Import Collection…"))),
            ],
        )?;
        self.popup(
            tr!("&View"),
            &[
                Some((MenuCmd::ViewAudio, tr!("&Audio"))),
                Some((MenuCmd::ViewProperties, tr!("&Properties"))),
                Some((MenuCmd::ViewHealth, tr!("&Health"))),
                None,
                Some((MenuCmd::DarkTheme, tr!("&Dark Theme"))),
            ],
        )?;
        self.popup(tr!("&Help"), &[Some((MenuCmd::About, tr!("&About LiveBroadcaster")))])?;
        Some(())
    }

    /// Rebuild the menus when the interface language changed (states are re-applied by the
    /// next frame's `set_*` calls).
    pub fn follow_language(&mut self) {
        let lang = crate::i18n::lang();
        if lang == self.lang {
            return;
        }
        self.lang = lang;
        unsafe {
            let Ok(bar) = CreateMenu() else { return };
            let old = std::mem::replace(&mut self.bar, bar);
            self.owner.clear();
            self.enabled.clear();
            self.checked.clear();
            self.labels.clear();
            if self.fill().is_none() {
                return;
            }
            let _ = SetMenu(self.hwnd, Some(bar));
            let _ = DestroyMenu(old);
            let _ = DrawMenuBar(self.hwnd);
        }
    }

    unsafe fn popup(&mut self, title: &str, items: &[Option<(MenuCmd, &str)>]) -> Option<()> {
        let p = CreatePopupMenu().ok()?;
        for it in items {
            match it {
                Some((cmd, label)) => {
                    let w = wide(label);
                    AppendMenuW(p, MF_STRING, *cmd as usize, PCWSTR(w.as_ptr())).ok()?;
                    self.owner.insert(*cmd as u32, p);
                    self.labels.insert(*cmd as u32, label.to_string());
                }
                None => {
                    AppendMenuW(p, MF_SEPARATOR, 0, PCWSTR::null()).ok()?;
                }
            }
        }
        let w = wide(title);
        AppendMenuW(self.bar, MF_POPUP, p.0 as usize, PCWSTR(w.as_ptr())).ok()?;
        Some(())
    }

    /// Commands chosen since the last call.
    pub fn take_commands(&self) -> Vec<MenuCmd> {
        let ids = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
        ids.into_iter().filter_map(MenuCmd::from_id).collect()
    }

    pub fn set_enabled(&mut self, cmd: MenuCmd, on: bool) {
        let id = cmd as u32;
        if self.enabled.get(&id) == Some(&on) {
            return;
        }
        if let Some(p) = self.owner.get(&id) {
            unsafe {
                let _ = EnableMenuItem(*p, id, MF_BYCOMMAND | if on { MF_ENABLED } else { MF_GRAYED });
            }
        }
        self.enabled.insert(id, on);
    }

    pub fn set_checked(&mut self, cmd: MenuCmd, on: bool) {
        let id = cmd as u32;
        if self.checked.get(&id) == Some(&on) {
            return;
        }
        if let Some(p) = self.owner.get(&id) {
            unsafe {
                let _ = CheckMenuItem(*p, id, (MF_BYCOMMAND | if on { MF_CHECKED } else { MF_UNCHECKED }).0);
            }
        }
        self.checked.insert(id, on);
    }

    pub fn set_label(&mut self, cmd: MenuCmd, label: &str) {
        let id = cmd as u32;
        if self.labels.get(&id).map(String::as_str) == Some(label) {
            return;
        }
        if let Some(p) = self.owner.get(&id) {
            let w = wide(label);
            unsafe {
                let _ = ModifyMenuW(*p, id, MF_BYCOMMAND | MF_STRING, id as usize, PCWSTR(w.as_ptr()));
            }
        }
        self.labels.insert(id, label.to_string());
    }

    /// Match the window chrome to the app theme.
    pub fn set_theme(&self, dark: bool, p: &sphere_ui::Palette) {
        DARK.store(dark, Ordering::Relaxed);
        BAR_BG.store(colorref(p.surface_titlebar), Ordering::Relaxed);
        BAR_HOT.store(colorref(sphere_ui::composite(p.surface_titlebar, p.state_selected)), Ordering::Relaxed);
        BAR_TEXT.store(colorref(p.text_primary), Ordering::Relaxed);
        unsafe {
            // Documented since Windows 10 20H1 (attribute 20).
            let v: BOOL = BOOL(dark as i32);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(20), &v as *const _ as *const _, 4);
            set_app_dark_mode(self.hwnd, dark);
            let _ = DrawMenuBar(self.hwnd);
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_FRAME | RDW_INVALIDATE);
        }
    }
}

/// uxtheme's (ordinal-only) preferred-app-mode switch: dark popup menus.
unsafe fn set_app_dark_mode(hwnd: HWND, dark: bool) {
    static UX: OnceLock<Option<(usize, usize, usize)>> = OnceLock::new();
    let fns = UX.get_or_init(|| {
        let lib = LoadLibraryW(w!("uxtheme.dll")).ok()?;
        let allow_window = GetProcAddress(lib, PCSTR(133 as *const u8))? as usize;
        let set_mode = GetProcAddress(lib, PCSTR(135 as *const u8))? as usize;
        let flush = GetProcAddress(lib, PCSTR(136 as *const u8))? as usize;
        Some((allow_window, set_mode, flush))
    });
    if let Some((allow_window, set_mode, flush)) = *fns {
        let allow: extern "system" fn(HWND, BOOL) -> BOOL = std::mem::transmute(allow_window);
        let mode: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
        let flush: extern "system" fn() = std::mem::transmute(flush);
        let _ = allow(hwnd, BOOL(dark as i32));
        // 2 = ForceDark, 3 = ForceLight.
        mode(if dark { 2 } else { 3 });
        flush();
    }
}

unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_COMMAND if (wp.0 >> 16) & 0xFFFF == 0 => {
            QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push((wp.0 & 0xFFFF) as u32);
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
            LRESULT(0)
        }
        WM_UAHDRAWMENU if DARK.load(Ordering::Relaxed) => {
            let um = &*(lp.0 as *const UahMenu);
            let mut mbi = MENUBARINFO { cbSize: std::mem::size_of::<MENUBARINFO>() as u32, ..Default::default() };
            let mut wr = RECT::default();
            if GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi).is_ok() && GetWindowRect(hwnd, &mut wr).is_ok() {
                let mut r = mbi.rcBar;
                let _ = OffsetRect(&mut r, -wr.left, -wr.top);
                // Reach the window's right edge (the bar rect stops after the last item).
                r.right = wr.right - wr.left;
                fill(um.hdc, &r, BAR_BG.load(Ordering::Relaxed));
            }
            LRESULT(0)
        }
        WM_UAHDRAWMENUITEM if DARK.load(Ordering::Relaxed) => {
            let it = &mut *(lp.0 as *mut UahDrawMenuItem);
            let mut buf = [0u16; 256];
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_STRING,
                dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
                cch: buf.len() as u32 - 1,
                ..Default::default()
            };
            let _ = GetMenuItemInfoW(it.um.hmenu, it.umi.position as u32, true, &mut mii);
            let state = it.dis.itemState;
            let hot = state.0 & (ODS_HOTLIGHT.0 | ODS_SELECTED.0) != 0;
            let bg = if hot { BAR_HOT.load(Ordering::Relaxed) } else { BAR_BG.load(Ordering::Relaxed) };
            let mut r = it.dis.rcItem;
            fill(it.um.hdc, &r, bg);
            let mut fmt = DT_CENTER | DT_SINGLELINE | DT_VCENTER;
            if state.0 & ODS_NOACCEL.0 != 0 {
                fmt |= DT_HIDEPREFIX;
            }
            let mut text = BAR_TEXT.load(Ordering::Relaxed);
            if state.0 & (ODS_INACTIVE.0 | ODS_GRAYED.0 | ODS_DISABLED.0) != 0 {
                text = (text >> 1) & 0x7F7F7F;
            }
            let _ = SetBkMode(it.um.hdc, TRANSPARENT);
            let _ = SetTextColor(it.um.hdc, COLORREF(text));
            let n = mii.cch as usize;
            let _ = DrawTextW(it.um.hdc, &mut buf[..n], &mut r, fmt);
            LRESULT(0)
        }
        WM_NCPAINT | WM_NCACTIVATE if DARK.load(Ordering::Relaxed) => {
            let r = DefSubclassProc(hwnd, msg, wp, lp);
            paint_menu_underline(hwnd);
            r
        }
        _ => DefSubclassProc(hwnd, msg, wp, lp),
    }
}

unsafe fn fill(hdc: HDC, r: &RECT, color: u32) {
    let brush = CreateSolidBrush(COLORREF(color));
    FillRect(hdc, r, brush);
    let _ = DeleteObject(brush.into());
}

/// Windows draws a 1 px light line between the menu bar and the client area.
unsafe fn paint_menu_underline(hwnd: HWND) {
    let mut client = RECT::default();
    let mut wr = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() || GetWindowRect(hwnd, &mut wr).is_err() {
        return;
    }
    let mut pts = [POINT { x: client.left, y: client.top }, POINT { x: client.right, y: client.bottom }];
    MapWindowPoints(Some(hwnd), None, &mut pts);
    let mut line = RECT { left: pts[0].x - wr.left, top: pts[0].y - wr.top - 1, right: pts[1].x - wr.left, bottom: pts[0].y - wr.top };
    let dc = GetWindowDC(Some(hwnd));
    if !dc.is_invalid() {
        fill(dc, &line, BAR_BG.load(Ordering::Relaxed));
        ReleaseDC(Some(hwnd), dc);
    }
    let _ = &mut line;
}
