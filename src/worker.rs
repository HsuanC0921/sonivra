use anyhow::{Context,Result,bail};
use base64::{Engine,engine::general_purpose::STANDARD};
use crossbeam_channel::{Receiver,bounded};
use serde_json::{Value,json};
use std::{collections::VecDeque,io::{BufRead,BufReader,Write},os::windows::process::CommandExt,process::{ChildStdin,Command,Stdio},sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}},thread,time::{Instant,Duration}};
use crate::{config::Asr,process::{NativeChild,Role}};

pub struct Worker {child:NativeChild,input:ChildStdin,rx:Receiver<Result<Value,String>>,stderr:Arc<Mutex<VecDeque<String>>>,pub model:Value}
impl Worker {
    pub fn start(cfg:&Asr,stop:&AtomicBool)->Result<Self>{
        if !cfg.model.is_file(){bail!("ASR model missing: {}. Import or download a supported speech.cpp GGUF.",cfg.model.display());}
        let mut cmd=Command::new(&cfg.runtime);
        cmd.arg("worker").arg(&cfg.model).args(["--device",&cfg.device,"--threads",&cfg.threads.to_string()]);
        Self::spawn(cmd,stop,Duration::from_secs(cfg.timeout_s))
    }
    fn spawn(mut cmd:Command,stop:&AtomicBool,timeout:Duration)->Result<Self>{
        let child=cmd.creation_flags(0x08000000).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("Cannot start speech.cpp. Install runtime and check Vulkan/VC++ dependencies")?;
        let mut child=NativeChild::own(child,Role::Asr)?;
        let input=child.stdin.take().unwrap();let stdout=child.stdout.take().unwrap();let err=child.stderr.take().unwrap();
        let (tx,rx)=bounded(8);let stderr=Arc::new(Mutex::new(VecDeque::with_capacity(32)));let tail=stderr.clone();
        thread::spawn(move||{let mut reader=BufReader::new(stdout);loop{let mut line=String::new();match reader.read_line(&mut line){Ok(0)=>break,Ok(_)=>{if line.len()>1024*1024{let _=tx.send(Err("Oversized protocol line".into()));break;}let v=serde_json::from_str(&line).map_err(|e|format!("Invalid speech.cpp JSON: {e}"));if tx.send(v).is_err(){break;}},Err(e)=>{let _=tx.send(Err(e.to_string()));break;}}}});
        thread::spawn(move||{for line in BufReader::new(err).lines(){let Ok(line)=line else{break};let mut lines=tail.lock().unwrap();if lines.len()==32{lines.pop_front();}lines.push_back(line.chars().take(2048).collect());}});
        let mut worker=Self{child,input,rx,stderr,model:Value::Null};
        let message=worker.receive(stop,Instant::now()+timeout)?;
        if message["type"]!="ready"{bail!("ASR startup: {message}\n{}",worker.diagnostics());}
        if message["protocol"]!=4{bail!("speech.cpp protocol 4 required; received {}",message["protocol"]);}
        worker.model=message["model"].clone();
        if worker.model["task"]!="recognition"{bail!("Select a speech recognition model, not a synthesis or enhancement model");}
        Ok(worker)
    }
    pub fn pid(&self)->u32{self.child.id()}
    pub fn alive(&mut self)->bool{matches!(self.child.try_wait(),Ok(None))}
    pub fn diagnostics(&self)->String{self.stderr.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n")}
    fn send(&mut self,value:Value)->Result<()>{serde_json::to_writer(&mut self.input,&value)?;self.input.write_all(b"\n")?;self.input.flush()?;Ok(())}
    fn receive(&mut self,stop:&AtomicBool,deadline:Instant)->Result<Value>{loop{
        if stop.load(Ordering::Relaxed){bail!("Cancelled");}
        if Instant::now()>=deadline{bail!("speech.cpp timed out\n{}",self.diagnostics());}
        match self.rx.recv_timeout(Duration::from_millis(50)){Ok(Ok(v))=>return Ok(v),Ok(Err(e))=>bail!("{e}"),Err(crossbeam_channel::RecvTimeoutError::Disconnected)=>bail!("speech.cpp exited\n{}",self.diagnostics()),Err(_)=>{}}
    }}
    pub fn transcribe(&mut self,id:u64,audio:&[f32],cfg:&Asr,stop:&AtomicBool)->Result<String>{
        let id=id.to_string();
        let mut request=json!({"type":"transcribe","id":id,"sample_rate":16000});
        let supports=|name:&str|self.model["options"].as_array().is_some_and(|options|options.iter().any(|v|v["name"]==name));
        if !cfg.language.is_empty()&&cfg.language!="auto" {request["language"]=json!(cfg.language);}
        if !cfg.prompt.is_empty(){
            if supports("prompt"){request["prompt"]=json!(cfg.prompt);}
            else if supports("keywords"){request["keywords"]=json!(cfg.prompt);}
            else{bail!("This model does not support vocabulary hints. Clear hints or choose Qwen3-ASR / Haas.");}
        }
        for (seq,chunk) in audio.chunks(16384).enumerate(){
            let mut pcm=Vec::with_capacity(chunk.len()*2);
            for value in chunk{pcm.extend_from_slice(&((value.clamp(-1.,1.)*32767.).round() as i16).to_le_bytes());}
            self.send(json!({"type":"chunk","id":id,"seq":seq,"pcm":STANDARD.encode(pcm)}))?;
        }
        self.send(request)?;
        let deadline=Instant::now()+Duration::from_secs(cfg.timeout_s);
        loop{let message=self.receive(stop,deadline)?;if message["id"]!=id{bail!("Unexpected worker request id: {message}");}match message["type"].as_str(){Some("end")=>return Ok(message["text"].as_str().context("Recognition result has no text; choose a recognition model")?.trim().into()),Some("error"|"fatal"|"cancelled")=>bail!("ASR request: {message}"),Some("progress")=>{},_=>bail!("Unexpected ASR response: {message}")}}
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn reject_malformed_protocol_and_reap_process(){let mut cmd=Command::new("pwsh");cmd.args(["-NoProfile","-Command","Write-Output 'broken-json'"]);let stop=AtomicBool::new(false);assert!(Worker::spawn(cmd,&stop,Duration::from_secs(10)).is_err());}
    #[test] fn worker_crash_can_be_restarted(){let stop=AtomicBool::new(false);for _ in 0..2{let mut cmd=Command::new("pwsh");cmd.args(["-NoProfile","-Command","Write-Output '{\"type\":\"ready\",\"protocol\":4,\"model\":{\"architecture\":\"qwen3-asr\",\"task\":\"recognition\"}}'; exit 0"]);let mut w=Worker::spawn(cmd,&stop,Duration::from_secs(10)).unwrap();assert!(w.receive(&stop,Instant::now()+Duration::from_secs(5)).is_err());}}
    #[test] fn refuse_incompatible_protocol(){let mut cmd=Command::new("pwsh");cmd.args(["-NoProfile","-Command","Write-Output '{\"type\":\"ready\",\"protocol\":99}'"]);assert!(Worker::spawn(cmd,&AtomicBool::new(false),Duration::from_secs(10)).is_err());}
}
