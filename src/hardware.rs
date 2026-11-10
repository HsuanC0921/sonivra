use anyhow::Result;
use serde::Serialize;
use windows::Win32::{Graphics::Dxgi::{CreateDXGIFactory1,IDXGIFactory1},System::SystemInformation::{MEMORYSTATUSEX,GlobalMemoryStatusEx}};
#[derive(Clone,Debug,Serialize)]pub struct Adapter{pub name:String,pub vendor:u32,pub dedicated_bytes:u64,pub free_bytes:Option<u64>,pub software:bool}
#[derive(Clone,Debug,Serialize)]pub struct Hardware{pub adapters:Vec<Adapter>,pub ram_total:u64,pub ram_available:u64}
#[derive(Clone,Debug,Serialize)]pub struct Recommendation{pub asr_file:String,pub translation_file:String,pub asr_device:String,pub gpu_layers:i32,pub runtime_variant:String,pub estimated_model_mib:u64,pub available_budget_mib:u64,pub reason:String}
pub fn detect()->Result<Hardware>{unsafe{
    let factory:IDXGIFactory1=CreateDXGIFactory1()?;let mut adapters=Vec::new();let mut i=0;
    loop{match factory.EnumAdapters1(i){Ok(adapter)=>{let desc=adapter.GetDesc1()?;let n=desc.Description.iter().position(|v|*v==0).unwrap_or(desc.Description.len());adapters.push(Adapter{name:String::from_utf16_lossy(&desc.Description[..n]),vendor:desc.VendorId,dedicated_bytes:desc.DedicatedVideoMemory as u64,free_bytes:None,software:desc.Flags&2!=0});},Err(error) if error.code()==windows::Win32::Graphics::Dxgi::DXGI_ERROR_NOT_FOUND=>break,Err(error)=>return Err(error.into())}i+=1;}
    if let Some(gpu)=crate::metrics::Gpu::new(){if let Some(name)=gpu.name(){if let Some(adapter)=adapters.iter_mut().find(|a|a.name==name){adapter.free_bytes=gpu.free();}}}
    let mut memory=MEMORYSTATUSEX{dwLength:std::mem::size_of::<MEMORYSTATUSEX>() as u32,..Default::default()};GlobalMemoryStatusEx(&mut memory)?;
    Ok(Hardware{adapters,ram_total:memory.ullTotalPhys,ram_available:memory.ullAvailPhys})
}}
pub fn recommend(h:&Hardware,reserve_mb:u32,preference:i32)->Recommendation{
    let gpu=h.adapters.iter().filter(|a|!a.software&&a.dedicated_bytes>=2*1024*1024*1024).max_by_key(|a|a.dedicated_bytes);
    // These are conservative capacity estimates, not performance benchmarks.
    let budget=gpu.map_or(0,|a|(a.free_bytes.unwrap_or(a.dedicated_bytes).min(a.dedicated_bytes)/1048576).saturating_sub(reserve_mb as u64));
    let (asr,translation,estimate)=if preference==2&&budget>=9500&&h.ram_available>=8*1024*1024*1024{("Qwen3-ASR-1.7B-Q4_K.gguf","plamo-3-translate-8b-Q4_K_M.gguf",8500)}
        else if preference==1{("Haas-616M-ja-v1.0-Q8_0.gguf","plamo-3-translate-0.8b-Q4_K_M.gguf",2400)}
        else if preference==3||budget<4400{("Qwen3-ASR-0.6B-Q4_K.gguf","plamo-3-translate-0.8b-Q4_K_M.gguf",2200)}
        else{("Qwen3-ASR-1.7B-Q4_K.gguf","plamo-3-translate-2b-Q4_K_M.gguf",4100)};
    let fits=budget>=estimate;
    let variant=if !fits{"llama-cpu"}else if gpu.is_some_and(|g|g.vendor==0x10de&&!g.name.to_lowercase().contains("gtx")){"llama"}else{"llama-vulkan"};
    Recommendation{asr_file:asr.into(),translation_file:translation.into(),asr_device:if fits{"Vulkan0"}else{"CPU"}.into(),gpu_layers:if fits{99}else{0},runtime_variant:variant.into(),estimated_model_mib:estimate,available_budget_mib:budget,reason:if fits{"Capacity estimate only. Runtime compatibility and actual latency must be verified by loading the models."}else{"GPU budget is insufficient for this preset. CPU inference selected. The Windows speech.cpp binary still requires the Vulkan loader."}.into()}
}
#[cfg(test)]mod tests{
    use super::*;
    fn hardware(vram:u64)->Hardware{Hardware{adapters:vec![Adapter{name:"Example adapter".into(),vendor:0x1002,dedicated_bytes:vram*1048576,free_bytes:Some(vram*1048576),software:false}],ram_total:32*1024*1024*1024,ram_available:20*1024*1024*1024}}
    #[test]fn target_capacity_balanced(){let r=recommend(&hardware(12288),0,0);assert!(r.translation_file.contains("2b-"));assert!(r.estimated_model_mib<=r.available_budget_mib);assert_eq!(r.runtime_variant,"llama-vulkan");}
    #[test]fn reservation_changes_capacity(){let r=recommend(&hardware(4096),4096,0);assert_eq!(r.gpu_layers,0);assert_eq!(r.asr_device,"CPU");}
    #[test]fn large_card_quality_preset(){let r=recommend(&hardware(24576),0,2);assert!(r.translation_file.contains("8b-"));}
}
