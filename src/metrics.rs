use serde::Serialize;
use windows::Win32::{Foundation::CloseHandle,System::{Threading::{OpenProcess,PROCESS_QUERY_INFORMATION,PROCESS_VM_READ},ProcessStatus::{K32GetProcessMemoryInfo,PROCESS_MEMORY_COUNTERS}}};
#[derive(Clone,Debug,Default,Serialize)]
pub struct Memory {pub main_rss:u64,pub asr_rss:u64,pub translation_rss:u64,pub combined_rss:u64,pub gpu_device_used:Option<u64>}
pub fn rss(pid:u32)->u64{
    if pid==0{return 0;}
    unsafe{
        let Ok(handle)=OpenProcess(PROCESS_QUERY_INFORMATION|PROCESS_VM_READ,false,pid)else{return 0};
        let mut counters=PROCESS_MEMORY_COUNTERS::default();
        let ok=K32GetProcessMemoryInfo(handle,&mut counters,std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32).as_bool();
        let _=CloseHandle(handle);if ok{counters.WorkingSetSize as u64}else{0}
    }
}
#[repr(C)]struct NvmlMemory{total:u64,free:u64,used:u64}
pub struct Gpu{library:libloading::Library,device:*mut std::ffi::c_void}
impl Gpu{
    pub fn new()->Option<Self>{unsafe{
        let library=libloading::Library::new("nvml.dll").ok()?;
        let init=library.get::<unsafe extern "C" fn()->i32>(b"nvmlInit_v2\0").ok()?;
        if init()!=0{return None;}
        let get=library.get::<unsafe extern "C" fn(u32,*mut *mut std::ffi::c_void)->i32>(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
        let mut device=std::ptr::null_mut();if get(0,&mut device)!=0{return None;}
        Some(Self{library,device})
    }}
    pub fn used(&self)->Option<u64>{unsafe{let get=self.library.get::<unsafe extern "C" fn(*mut std::ffi::c_void,*mut NvmlMemory)->i32>(b"nvmlDeviceGetMemoryInfo\0").ok()?;let mut memory=NvmlMemory{total:0,free:0,used:0};if get(self.device,&mut memory)==0{Some(memory.used)}else{None}}}
    pub fn free(&self)->Option<u64>{unsafe{let get=self.library.get::<unsafe extern "C" fn(*mut std::ffi::c_void,*mut NvmlMemory)->i32>(b"nvmlDeviceGetMemoryInfo\0").ok()?;let mut memory=NvmlMemory{total:0,free:0,used:0};if get(self.device,&mut memory)==0{Some(memory.free)}else{None}}}
    pub fn name(&self)->Option<String>{unsafe{let get=self.library.get::<unsafe extern "C" fn(*mut std::ffi::c_void,*mut std::ffi::c_char,u32)->i32>(b"nvmlDeviceGetName\0").ok()?;let mut buffer=[0u8;128];if get(self.device,buffer.as_mut_ptr().cast(),128)!=0{return None;}let n=buffer.iter().position(|v|*v==0).unwrap_or(128);Some(String::from_utf8_lossy(&buffer[..n]).into())}}
}
impl Drop for Gpu{fn drop(&mut self){unsafe{if let Ok(shutdown)=self.library.get::<unsafe extern "C" fn()->i32>(b"nvmlShutdown\0"){shutdown();}}}}
pub fn sample(asr:u32,translation:u32,gpu:Option<&Gpu>)->Memory{let active=crate::process::active();let asr=if asr>0{asr}else{active.iter().find(|(_,r)|*r==crate::process::Role::Asr).map_or(0,|(id,_)|*id)};let translation=if translation>0{translation}else{active.iter().find(|(_,r)|*r==crate::process::Role::Translation).map_or(0,|(id,_)|*id)};let main=rss(std::process::id());let a=rss(asr);let t=rss(translation);Memory{main_rss:main,asr_rss:a,translation_rss:t,combined_rss:main+a+t,gpu_device_used:gpu.and_then(Gpu::used)}}
pub fn percentile(values:&[f64],p:f64)->Option<f64>{if values.is_empty(){return None;}let mut sorted=values.to_vec();sorted.sort_by(f64::total_cmp);Some(sorted[((sorted.len()-1) as f64*p).round() as usize])}
pub struct CpuSampler{previous:std::collections::HashMap<u32,u64>,at:std::time::Instant}
impl Default for CpuSampler{fn default()->Self{Self{previous:Default::default(),at:std::time::Instant::now()}}}
fn cpu_ticks(pid:u32)->Option<u64>{unsafe{
    use windows::Win32::{Foundation::FILETIME,System::Threading::{GetProcessTimes,PROCESS_QUERY_LIMITED_INFORMATION}};
    let handle=OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,false,pid).ok()?;
    let mut created=FILETIME::default();let mut exited=FILETIME::default();let mut kernel=FILETIME::default();let mut user=FILETIME::default();
    let ok=GetProcessTimes(handle,&mut created,&mut exited,&mut kernel,&mut user);let _=CloseHandle(handle);ok.ok()?;
    let ticks=|t:FILETIME|((t.dwHighDateTime as u64)<<32)|t.dwLowDateTime as u64;Some(ticks(kernel)+ticks(user))
}}
impl CpuSampler{pub fn sample(&mut self)->f64{
    let mut current=std::collections::HashMap::new();let mut delta=0u64;
    for pid in std::iter::once(std::process::id()).chain(crate::process::active().into_iter().map(|(pid,_)|pid)){if let Some(ticks)=cpu_ticks(pid){if let Some(last)=self.previous.get(&pid){delta+=ticks.saturating_sub(*last);}current.insert(pid,ticks);}}
    let elapsed=self.at.elapsed().as_secs_f64();self.at=std::time::Instant::now();self.previous=current;
    if elapsed>0.{delta as f64/10000000./elapsed/std::thread::available_parallelism().map_or(1,|n|n.get()) as f64*100.}else{0.}
}}
#[derive(Default,Serialize)]pub struct PeakReport{pub memory:Memory,pub cpu_average_percent:f64,pub cpu_peak_percent:f64,pub sample_count:u64,pub interval_ms:u64}
pub struct Sampler{stop:std::sync::Arc<std::sync::atomic::AtomicBool>,thread:Option<std::thread::JoinHandle<PeakReport>>}
impl Sampler{
    pub fn start()->Self{let stop=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));let cancelled=stop.clone();let thread=std::thread::spawn(move||{
        let gpu=Gpu::new();let mut cpu=CpuSampler::default();let mut report=PeakReport{interval_ms:20,..Default::default()};let mut sum=0.;
        while !cancelled.load(std::sync::atomic::Ordering::Relaxed){let memory=sample(0,0,gpu.as_ref());report.memory.main_rss=report.memory.main_rss.max(memory.main_rss);report.memory.asr_rss=report.memory.asr_rss.max(memory.asr_rss);report.memory.translation_rss=report.memory.translation_rss.max(memory.translation_rss);report.memory.combined_rss=report.memory.combined_rss.max(memory.combined_rss);report.memory.gpu_device_used=report.memory.gpu_device_used.max(memory.gpu_device_used);let percent=cpu.sample();sum+=percent;report.cpu_peak_percent=report.cpu_peak_percent.max(percent);report.sample_count+=1;std::thread::sleep(std::time::Duration::from_millis(20));}
        if report.sample_count>0{report.cpu_average_percent=sum/report.sample_count as f64;}report
    });Self{stop,thread:Some(thread)}}
    pub fn finish(mut self)->PeakReport{self.stop.store(true,std::sync::atomic::Ordering::Relaxed);self.thread.take().unwrap().join().expect("performance sampler")}
}
impl Drop for Sampler{fn drop(&mut self){self.stop.store(true,std::sync::atomic::Ordering::Relaxed);if let Some(thread)=self.thread.take(){let _=thread.join();}}}
#[cfg(test)]mod tests{use super::*;#[test]fn measured_percentiles(){assert_eq!(percentile(&[8.,2.,4.,6.],0.5),Some(6.));assert_eq!(percentile(&[],0.95),None);}}
