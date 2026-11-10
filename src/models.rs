use anyhow::{Context,Result,bail};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use std::{fs::{self,OpenOptions,File},io::{Read,Write},path::{Path,PathBuf},sync::atomic::{AtomicBool,Ordering},time::Duration};
use reqwest::header::{RANGE,CONTENT_RANGE};

#[derive(Clone,Debug,Deserialize,Serialize)]
pub struct Artifact {pub id:String,pub revision:String,pub file:String,pub size:u64,pub sha256:String,pub url:String,pub kind:String,pub architecture:String,pub license:String}
pub fn catalog()->Vec<Artifact>{serde_json::from_str(include_str!("../catalog.json").trim_start_matches('\u{feff}')).expect("checked-in model catalog")}
pub fn runtimes()->Vec<Artifact>{serde_json::from_str(include_str!("../runtimes.json").trim_start_matches('\u{feff}')).expect("checked-in runtime manifest")}
pub fn hash(path:&Path)->Result<String>{let mut file=File::open(path)?;let mut hasher=Sha256::new();let mut buf=[0u8;65536];loop{let n=file.read(&mut buf)?;if n==0{break;}hasher.update(&buf[..n]);}Ok(format!("{:x}",hasher.finalize()))}
pub fn download(asset:&Artifact,directory:&Path,cancel:&AtomicBool,mut progress:impl FnMut(u64,u64))->Result<PathBuf>{
    fs::create_dir_all(directory)?;
    let final_path=directory.join(&asset.file);
    if final_path.exists(){if hash(&final_path)?==asset.sha256{return Ok(final_path);}bail!("Existing file failed SHA-256: {}. Remove or choose another path.",final_path.display());}
    let part=directory.join(format!("{}.part",asset.file));
    let client=reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(30)).timeout(Duration::from_secs(3600)).build()?;
    let mut last_error=None;
    for _ in 0..3{
        let result=(||->Result<()>{
            let mut offset=fs::metadata(&part).map_or(0,|m|m.len());
            let mut request=client.get(&asset.url);if offset>0{request=request.header(RANGE,format!("bytes={offset}-"));}
            let mut response=request.send()?.error_for_status()?;
            if offset>0&&response.status()!=reqwest::StatusCode::PARTIAL_CONTENT{offset=0;}
            if offset>0&&!response.headers().get(CONTENT_RANGE).and_then(|h|h.to_str().ok()).unwrap_or("").starts_with(&format!("bytes {offset}-")){bail!("Unexpected resume range");}
            let mut file=OpenOptions::new().create(true).write(true).append(offset>0).truncate(offset==0).open(&part)?;
            progress(offset,asset.size);let mut buffer=[0u8;65536];
            loop{if cancel.load(Ordering::Relaxed){bail!("Cancelled");}let n=response.read(&mut buffer)?;if n==0{break;}file.write_all(&buffer[..n])?;offset+=n as u64;if offset>asset.size{bail!("Download exceeds published size");}progress(offset,asset.size);}
            file.flush()?;if offset!=asset.size{bail!("Incomplete download: {offset}/{}",asset.size);}Ok(())
        })();
        match result{Ok(())=>{last_error=None;break;},Err(e)=>{if cancel.load(Ordering::Relaxed){return Err(e);}last_error=Some(e)}}
    }
    if let Some(error)=last_error{return Err(error);}
    if hash(&part)?!=asset.sha256{bail!("Published SHA-256 mismatch: {}",part.display());}
    fs::rename(&part,&final_path)?;Ok(final_path)
}
pub fn install_runtimes(home:&Path,variant:&str,cancel:&AtomicBool,mut progress:impl FnMut(u64,u64))->Result<()> {
    for asset in runtimes().into_iter().filter(|a|a.architecture=="speech"||a.architecture==variant){
        let archive=download(&asset,&home.join("downloads"),cancel,&mut progress)?;
        let target=home.join("runtime").join(&asset.architecture);fs::create_dir_all(&target)?;
        let mut zip=zip::ZipArchive::new(File::open(&archive)?)?;
        for i in 0..zip.len(){let mut entry=zip.by_index(i)?;let path=entry.enclosed_name().context("Unsafe runtime archive path")?;let dest=target.join(path);if entry.is_dir(){fs::create_dir_all(dest)?;}else{if let Some(parent)=dest.parent(){fs::create_dir_all(parent)?;}std::io::copy(&mut entry,&mut File::create(dest)?)?;}}
    }
    Ok(())
}
pub fn validate_gguf(path:&Path)->Result<()>{let mut file=File::open(path)?;let mut header=[0u8;8];file.read_exact(&mut header)?;if &header[..4]!=b"GGUF"||u32::from_le_bytes(header[4..8].try_into().unwrap())!=3{bail!("Expected GGUF v3; actual architecture compatibility is checked when loading the runtime");}Ok(())}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn catalog_is_pinned_and_complete(){let c=catalog();assert_eq!(c.len(),14);let names:std::collections::HashSet<_>=c.iter().map(|m|&m.file).collect();assert_eq!(names.len(),c.len());for m in c{assert_eq!(m.sha256.len(),64);assert_eq!(m.revision.len(),40);assert!(m.size>0);assert!(m.url.starts_with("https://huggingface.co/"));}}
}
