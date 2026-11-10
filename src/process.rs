use anyhow::{Result,anyhow};
use std::{collections::HashMap,process::Child,ops::{Deref,DerefMut},os::windows::io::AsRawHandle,sync::{OnceLock,Mutex}};
use windows::{core::PCWSTR,Win32::{Foundation::{HANDLE,CloseHandle},System::JobObjects::*}};
#[derive(Clone,Copy,PartialEq,Eq)]pub enum Role{Asr,Translation}
fn registry()->&'static Mutex<HashMap<u32,Role>>{static IDS:OnceLock<Mutex<HashMap<u32,Role>>>=OnceLock::new();IDS.get_or_init(||Mutex::new(HashMap::new()))}
pub fn active()->Vec<(u32,Role)>{registry().lock().unwrap().iter().map(|(pid,role)|(*pid,*role)).collect()}
pub struct NativeChild{child:Child}
impl NativeChild{
    pub fn own(mut child:Child,role:Role)->Result<Self>{
        static JOB:OnceLock<Result<isize,String>>=OnceLock::new();
        let result=(||->Result<()>{
            let handle=JOB.get_or_init(||unsafe{
                let handle=CreateJobObjectW(None,PCWSTR::null()).map_err(|e|e.to_string())?;
                let mut info=JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();info.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if let Err(error)=SetInformationJobObject(handle,JobObjectExtendedLimitInformation,(&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),std::mem::size_of_val(&info) as u32){let _=CloseHandle(handle);return Err(error.to_string());}
                Ok(handle.0 as isize)
            }).as_ref().map_err(|e|anyhow!("Cannot create worker lifetime job: {e}"))?;
            unsafe{AssignProcessToJobObject(HANDLE(*handle as *mut std::ffi::c_void),HANDLE(child.as_raw_handle().cast()))?;}
            Ok(())
        })();
        if let Err(error)=result{let _=child.kill();let _=child.wait();return Err(error);}
        registry().lock().unwrap().insert(child.id(),role);Ok(Self{child})
    }
}
impl Deref for NativeChild{type Target=Child;fn deref(&self)->&Child{&self.child}}
impl DerefMut for NativeChild{fn deref_mut(&mut self)->&mut Child{&mut self.child}}
impl Drop for NativeChild{fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();registry().lock().unwrap().remove(&self.child.id());}}
