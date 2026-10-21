//! Tray icon owned by the service: click opens the window, menu has restart/quit.
//! Talks to the service loop through the flags in `daemon`.
use crate::daemon::{QUIT, RESET, TIP};
use crate::system::open_gui;
use std::sync::atomic::Ordering::Relaxed;

const ICON: u32 = 32;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicPtr, AtomicU32};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY: u32 = WM_APP + 1;
    const OPEN: usize = 1;
    const RESTART: usize = 2;
    const EXIT: usize = 3;
    static HWND_: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(null_mut());
    static HICON_: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(null_mut());
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    pub struct Tray;

    impl Tray {
        pub fn refresh(&self) {} // tooltip refreshes on a timer in the tray thread
        pub fn close(&self) {
            let hwnd = HWND_.load(Relaxed);
            if !hwnd.is_null() {
                unsafe { Shell_NotifyIconW(NIM_DELETE, &nid(hwnd)) };
            }
        }
    }

    pub fn start() -> Option<Tray> {
        std::thread::spawn(|| unsafe { run() });
        Some(Tray)
    }

    unsafe fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut n: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = hwnd;
        n.uID = 1;
        n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        n.uCallbackMessage = WM_TRAY;
        n.hIcon = HICON_.load(Relaxed);
        let tip = format!("RingLeader\n{}", TIP.lock().unwrap());
        for (d, s) in n.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *d = s;
        }
        n
    }

    unsafe fn run() {
        unsafe {
            let inst = GetModuleHandleW(null());
            // CreateIcon wants BGRA; the alpha channel makes the AND mask irrelevant.
            let mut px = crate::effects::icon_rgba(ICON);
            px.chunks_mut(4).for_each(|p| p.swap(0, 2));
            let mask = vec![0u8; (ICON * ICON / 8) as usize];
            HICON_.store(CreateIcon(inst, ICON as i32, ICON as i32, 1, 32, mask.as_ptr(), px.as_ptr()), Relaxed);

            let class = wide("RingLeaderTray");
            let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&wc);
            // A real (hidden) top-level window, not message-only: those miss "TaskbarCreated".
            let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), inst, null());
            HWND_.store(hwnd, Relaxed);
            TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Relaxed);
            Shell_NotifyIconW(NIM_ADD, &nid(hwnd));
            SetTimer(hwnd, 1, 2000, None);
            let mut msg = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_TRAY => {
                    match (lp & 0xFFFF) as u32 {
                        WM_LBUTTONUP => open_gui(),
                        WM_RBUTTONUP => menu(hwnd),
                        _ => {}
                    }
                    0
                }
                WM_TIMER => {
                    Shell_NotifyIconW(NIM_MODIFY, &nid(hwnd));
                    0
                }
                // Logoff/shutdown: stop between USB writes instead of being killed mid-transfer.
                WM_QUERYENDSESSION => 1,
                WM_ENDSESSION => {
                    QUIT.store(true, Relaxed);
                    0
                }
                m if m == TASKBAR_CREATED.load(Relaxed) => {
                    Shell_NotifyIconW(NIM_ADD, &nid(hwnd));
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }

    unsafe fn menu(hwnd: HWND) {
        unsafe {
            let m = CreatePopupMenu();
            AppendMenuW(m, MF_STRING, OPEN, wide("Open RingLeader").as_ptr());
            AppendMenuW(m, MF_STRING, RESTART, wide("Restart controller").as_ptr());
            AppendMenuW(m, MF_SEPARATOR, 0, null());
            AppendMenuW(m, MF_STRING, EXIT, wide("Quit").as_ptr());
            SetMenuDefaultItem(m, OPEN as u32, 0);
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            SetForegroundWindow(hwnd); // otherwise the menu doesn't close when clicking elsewhere
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, hwnd, null()) as usize;
            DestroyMenu(m);
            match cmd {
                OPEN => open_gui(),
                RESTART => RESET.store(true, Relaxed),
                EXIT => QUIT.store(true, Relaxed),
                _ => {}
            }
        }
    }
}

/// StatusNotifierItem over D-Bus (KDE, GNOME with AppIndicator, XFCE, ...).
#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    struct T;

    impl ksni::Tray for T {
        fn id(&self) -> String {
            "ringleader".into()
        }
        fn title(&self) -> String {
            "RingLeader".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // RGBA -> ARGB
            let data = crate::effects::icon_rgba(ICON).chunks(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            vec![ksni::Icon { width: ICON as i32, height: ICON as i32, data }]
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: "RingLeader".into(), description: TIP.lock().unwrap().clone(), ..Default::default() }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            open_gui();
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::{MenuItem, StandardItem};
            let item = |label: &str, f: fn()| StandardItem { label: label.into(), activate: Box::new(move |_: &mut T| f()), ..Default::default() }.into();
            vec![
                item("Open RingLeader", open_gui),
                item("Restart controller", || RESET.store(true, Relaxed)),
                MenuItem::Separator,
                item("Quit", || QUIT.store(true, Relaxed)),
            ]
        }
    }

    pub struct Tray(ksni::blocking::Handle<T>);

    impl Tray {
        pub fn refresh(&self) {
            self.0.update(|_| {});
        }
        pub fn close(&self) {
            self.0.shutdown().wait();
        }
    }

    /// None without a session bus / tray host (e.g. over SSH); the service runs fine without.
    pub fn start() -> Option<Tray> {
        use ksni::blocking::TrayMethods;
        T.spawn().ok().map(Tray)
    }
}

pub use imp::start;
