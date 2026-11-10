//! Notification-area icon and the global lock hotkey. They are the recovery path when the
//! subtitle window is locked with mouse pass-through or hidden.
use anyhow::{Result,bail};
use std::cell::RefCell;
use windows::{core::{PCWSTR,w},Win32::{Foundation::{HWND,LPARAM,LRESULT,POINT,WPARAM},Graphics::Gdi::HBRUSH,System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::{MOD_ALT,MOD_CONTROL,MOD_NOREPEAT,RegisterHotKey,UnregisterHotKey},Shell::{NIF_ICON,NIF_MESSAGE,NIF_TIP,NIM_ADD,NIM_DELETE,NIM_MODIFY,NOTIFYICONDATAW,Shell_NotifyIconW},
    WindowsAndMessaging::*}}};

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Command{ToggleLock,ToggleOverlay,OpenSettings,ResetPosition,Quit,Pause,Back,Forward,Bigger,Smaller,Save}
const WM_TRAY:u32=WM_APP+1;
/// Global shortcuts. They work while the subtitle window lets the mouse pass through.
/// (display name, virtual key, command, UI text key)
pub const HOTKEYS:[(&str,u32,Command,&str);8]=[
    ("Ctrl+Alt+L",'L' as u32,Command::ToggleLock,"hotkey.lock"),
    ("Ctrl+Alt+P",'P' as u32,Command::Pause,"hotkey.pause"),
    ("Ctrl+Alt+H",'H' as u32,Command::ToggleOverlay,"hotkey.show"),
    ("Ctrl+Alt+↑",0x26,Command::Back,"hotkey.back"),
    ("Ctrl+Alt+↓",0x28,Command::Forward,"hotkey.forward"),
    ("Ctrl+Alt+=",0xBB,Command::Bigger,"hotkey.bigger"),
    ("Ctrl+Alt+-",0xBD,Command::Smaller,"hotkey.smaller"),
    ("Ctrl+Alt+S",'S' as u32,Command::Save,"hotkey.save"),
];
pub const HOTKEY:&str=HOTKEYS[0].0;
pub struct MenuLabels{pub transport:String,pub lock:String,pub show:String,pub settings:String,pub reset:String,pub quit:String,pub locked:bool,pub visible:bool}
thread_local!{
    static QUEUE:RefCell<Vec<Command>>=const{RefCell::new(Vec::new())};
    static MENU:RefCell<Option<MenuLabels>>=const{RefCell::new(None)};
}
pub fn take()->Vec<Command>{QUEUE.with(|q|std::mem::take(&mut *q.borrow_mut()))}
pub fn set_menu(labels:MenuLabels){MENU.with(|m|*m.borrow_mut()=Some(labels));}
fn push(command:Command){QUEUE.with(|q|q.borrow_mut().push(command));}
fn wide(text:&str)->Vec<u16>{text.encode_utf16().chain(std::iter::once(0)).collect()}

unsafe fn menu(hwnd:HWND){unsafe{
    let Ok(menu)=CreatePopupMenu() else{return};
    MENU.with(|m|{if let Some(l)=&*m.borrow(){
        let flag=|on:bool|if on{MF_STRING|MF_CHECKED}else{MF_STRING};
        let transport=wide(&l.transport);let _=AppendMenuW(menu,MF_STRING,6,PCWSTR(transport.as_ptr()));let _=AppendMenuW(menu,MF_SEPARATOR,0,PCWSTR::null());
        let items=[(1usize,&l.show,flag(l.visible)),(2,&l.lock,flag(l.locked)),(3,&l.reset,MF_STRING),(4,&l.settings,MF_STRING)];
        for (id,text,flags) in items{let text=wide(text);let _=AppendMenuW(menu,flags,id,PCWSTR(text.as_ptr()));}
        let _=AppendMenuW(menu,MF_SEPARATOR,0,PCWSTR::null());
        let quit=wide(&l.quit);let _=AppendMenuW(menu,MF_STRING,5,PCWSTR(quit.as_ptr()));
    }});
    let mut point=POINT::default();let _=GetCursorPos(&mut point);
    // Required so the menu closes when the user clicks elsewhere.
    let _=SetForegroundWindow(hwnd);
    let chosen=TrackPopupMenu(menu,TPM_RETURNCMD|TPM_NONOTIFY|TPM_RIGHTBUTTON,point.x,point.y,Some(0),hwnd,None).0;
    let _=DestroyMenu(menu);
    match chosen{1=>push(Command::ToggleOverlay),2=>push(Command::ToggleLock),3=>push(Command::ResetPosition),4=>push(Command::OpenSettings),5=>push(Command::Quit),6=>push(Command::Pause),_=>{}}
}}
unsafe extern "system" fn proc(hwnd:HWND,msg:u32,wparam:WPARAM,lparam:LPARAM)->LRESULT{unsafe{
    match msg{
        WM_TRAY=>{match (lparam.0 & 0xffff) as u32{WM_LBUTTONUP=>push(Command::OpenSettings),WM_RBUTTONUP|WM_CONTEXTMENU=>menu(hwnd),_=>{}}LRESULT(0)},
        WM_HOTKEY=>{if let Some((_,_,command,_))=HOTKEYS.get(wparam.0.wrapping_sub(1)){push(*command);}LRESULT(0)},
        _=>DefWindowProcW(hwnd,msg,wparam,lparam)
    }
}}

/// RGBA pixels of the app mark: a rounded teal tile with three sound bars.
pub fn icon_rgba(size:u32)->Vec<u8>{
    let s=size as f32;let mut out=vec![0u8;(size*size*4) as usize];
    let bars=[(0.30,0.42),(0.50,0.62),(0.70,0.36)];
    for y in 0..size{for x in 0..size{
        let (fx,fy)=((x as f32+0.5)/s,(y as f32+0.5)/s);
        let r=0.22;let (cx,cy)=(fx.clamp(r,1.-r),fy.clamp(r,1.-r));let d=((fx-cx).powi(2)+(fy-cy).powi(2)).sqrt();
        let coverage=((r-d)*s+0.5).clamp(0.,1.);
        if coverage<=0.{continue;}
        let mut c=[(20.+30.*fy) as u8,(122.-20.*fy) as u8,(110.-10.*fy) as u8];
        for (bx,h) in bars{if (fx-bx).abs()<0.065&&(fy-0.5).abs()<h/2.{c=[245,250,248];}}
        let i=((y*size+x)*4) as usize;out[i..i+4].copy_from_slice(&[c[0],c[1],c[2],(coverage*255.) as u8]);
    }}
    out
}

pub struct Tray{hwnd:HWND,icon:HICON,registered:Vec<i32>,pub failed:Vec<&'static str>}
impl Tray{
    pub fn new(tooltip:&str)->Result<Self>{unsafe{
        let instance=GetModuleHandleW(None)?;
        let class=WNDCLASSW{lpfnWndProc:Some(proc),hInstance:instance.into(),lpszClassName:w!("SonivraTray"),hbrBackground:HBRUSH::default(),..Default::default()};
        if RegisterClassW(&class)==0{bail!("RegisterClassW failed: {}",windows::core::Error::from_thread());}
        let hwnd=CreateWindowExW(WS_EX_TOOLWINDOW,w!("SonivraTray"),w!("Sonivra"),WS_POPUP,0,0,0,0,None,None,Some(instance.into()),None)?;
        let size=32;let rgba=icon_rgba(size);let mut bgra=rgba.clone();for p in bgra.chunks_exact_mut(4){p.swap(0,2);}
        let mask=vec![0u8;(size*size/8) as usize];
        let icon=CreateIcon(Some(instance.into()),size as i32,size as i32,1,32,mask.as_ptr(),bgra.as_ptr())?;
        let mut tray=Self{hwnd,icon,registered:Vec::new(),failed:Vec::new()};
        tray.notify(NIM_ADD,tooltip)?;
        for (i,(name,key,_,_)) in HOTKEYS.iter().enumerate(){let id=i as i32+1;if RegisterHotKey(Some(hwnd),id,MOD_CONTROL|MOD_ALT|MOD_NOREPEAT,*key).is_ok(){tray.registered.push(id);}else{tray.failed.push(name);}}
        Ok(tray)
    }}
    fn notify(&mut self,action:windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE,tooltip:&str)->Result<()>{unsafe{
        let mut data=NOTIFYICONDATAW{cbSize:std::mem::size_of::<NOTIFYICONDATAW>() as u32,hWnd:self.hwnd,uID:1,uFlags:NIF_ICON|NIF_MESSAGE|NIF_TIP,uCallbackMessage:WM_TRAY,hIcon:self.icon,..Default::default()};
        for (slot,unit) in data.szTip.iter_mut().zip(tooltip.encode_utf16().take(127)){*slot=unit;}
        if !Shell_NotifyIconW(action,&data).as_bool(){bail!("Shell_NotifyIconW failed");}Ok(())
    }}
    pub fn set_tooltip(&mut self,tooltip:&str){let _=self.notify(NIM_MODIFY,tooltip);}
}
impl Drop for Tray{fn drop(&mut self){unsafe{
    let data=NOTIFYICONDATAW{cbSize:std::mem::size_of::<NOTIFYICONDATAW>() as u32,hWnd:self.hwnd,uID:1,..Default::default()};
    let _=Shell_NotifyIconW(NIM_DELETE,&data);for id in &self.registered{let _=UnregisterHotKey(Some(self.hwnd),*id);}
    let _=DestroyIcon(self.icon);let _=DestroyWindow(self.hwnd);
}}}
