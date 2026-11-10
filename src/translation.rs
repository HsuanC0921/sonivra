use anyhow::{Context,Result,bail};
use futures_util::StreamExt;
use serde_json::{Value,json};
use std::{collections::VecDeque,io::{BufRead,BufReader},net::TcpListener,os::windows::process::CommandExt,process::{Command,Stdio},sync::{Arc,Mutex,atomic::AtomicBool},time::{Duration,Instant}};
use crate::{config::Translation,pipeline::Cancellation,process::{NativeChild,Role}};

fn safe(text:&str)->String{text.replace("<|plamo:","<\u{200b}|plamo:")}
pub fn pfml(cfg:&Translation,source:&str,history:&VecDeque<(String,String)>)->String{
    let mut out=String::from("<|plamo:tag|>dataset<|plamo:msg|>translation");
    if !cfg.context.is_empty(){out.push_str("<|plamo:tag|>context<|plamo:msg|>");out.push_str(&safe(&cfg.context));}
    let pair=|output:&mut String,source:&str,target:Option<&str>|{
        output.push_str("<|plamo:tag|>input");
        if cfg.source_lang!="auto"{output.push_str(&format!("<|plamo:key|>lang<|plamo:val|>{}",cfg.source_lang));}
        output.push_str(&format!("<|plamo:msg|>{}<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>{}<|plamo:msg|>",safe(source),cfg.target_lang));
        if let Some(target)=target {output.push_str(&safe(target));}
    };
    for (source,target) in history.iter().skip(history.len().saturating_sub(cfg.history_turns)){pair(&mut out,source,Some(target));}
    if !cfg.glossary.trim().is_empty(){
        out.push_str("<|plamo:tag|>glossary");
        for line in cfg.glossary.lines(){if let Some((source,target))=line.split_once("=>"){out.push_str("<|plamo:index|>");out.push_str(&safe(source.trim()));out.push_str("<|plamo:item|>");out.push_str(&safe(target.trim()));}}
    }
    pair(&mut out,source,None);out
}

pub struct Translator {child:Option<NativeChild>,pub endpoint:String,pub cfg:Translation,client:reqwest::Client,tail:Arc<Mutex<VecDeque<String>>>}
impl Translator {
    pub async fn start(cfg:Translation,stop:&AtomicBool)->Result<Self>{
        let client=reqwest::Client::builder().timeout(Duration::from_secs(120)).no_proxy().build()?;
        let mut this=Self{child:None,endpoint:cfg.endpoint.trim_end_matches('/').into(),cfg,client,tail:Arc::new(Mutex::new(VecDeque::new()))};
        if this.cfg.local {
            if !this.cfg.model.is_file(){bail!("Translation model missing: {}",this.cfg.model.display());}
            let port=TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
            this.endpoint=format!("http://127.0.0.1:{port}/v1");
            let child=Command::new(&this.cfg.runtime).creation_flags(0x08000000)
                .args(["--host","127.0.0.1","--port",&port.to_string(),"-m"])
                .arg(&this.cfg.model).args(["--ctx-size",&this.cfg.context_size.to_string(),"--n-gpu-layers",&this.cfg.gpu_layers.to_string(),"--parallel","1","--threads","8","--no-webui","--jinja","--no-prefill-assistant"])
                .args(["--load-mode",if this.cfg.mmap{"mmap"}else{"none"}])
                .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().context("Cannot start llama.cpp. Download runtime and check native dependencies")?;
            let mut child=NativeChild::own(child,Role::Translation)?;
            let stderr=child.stderr.take().unwrap();let tail=this.tail.clone();
            std::thread::spawn(move||{for line in BufReader::new(stderr).lines(){let Ok(line)=line else{break};let mut t=tail.lock().unwrap();if t.len()==32{t.pop_front();}t.push_back(line.chars().take(2048).collect());}});
            this.child=Some(child);
            let deadline=Instant::now()+Duration::from_secs(180);
            loop{
                if stop.load(std::sync::atomic::Ordering::Relaxed){bail!("Cancelled");}
                if let Some(status)=this.child.as_mut().unwrap().try_wait()?{bail!("llama.cpp exited ({status})\n{}",this.diagnostics());}
                if Instant::now()>deadline{bail!("llama.cpp startup timeout\n{}",this.diagnostics());}
                if let Ok(response)=this.client.get(format!("http://127.0.0.1:{port}/health")).timeout(Duration::from_millis(500)).send().await {if response.status().is_success(){break;}}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } else if !this.endpoint.starts_with("http://")&&!this.endpoint.starts_with("https://"){bail!("Set an http(s) OpenAI-compatible endpoint ending in /v1");}
        Ok(this)
    }
    pub fn pid(&self)->u32{self.child.as_ref().map_or(0,|c|c.id())}
    pub fn alive(&mut self)->bool{self.child.as_mut().is_none_or(|child|matches!(child.try_wait(),Ok(None)))}
    pub fn diagnostics(&self)->String{self.tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n")}
    pub async fn translate(&self,source:&str,history:&VecDeque<(String,String)>,cancel:&Cancellation,emit:impl FnMut(&str,f64))->Result<(String,f64)>{self.translate_with(source,history,cancel,emit,"").await}
    /// `extra` is appended to the context section only, e.g. the sentence that followed, so the prompt format is unchanged.
    pub async fn translate_with(&self,source:&str,history:&VecDeque<(String,String)>,cancel:&Cancellation,mut emit:impl FnMut(&str,f64),extra:&str)->Result<(String,f64)>{
        let start=Instant::now();
        let mut owned;let cfg:&Translation=if extra.is_empty(){&self.cfg}else{owned=self.cfg.clone();owned.context.push_str(extra);&owned};
        let (route,body)=if cfg.family=="plamo3"{("completions",json!({"model":cfg.model_id,"prompt":pfml(&cfg,source,history),"stream":true,"temperature":cfg.temperature,"max_tokens":cfg.max_tokens,"stop":["<|plamo:tag|>","<|plamo:bos|>"],"cache_prompt":true}))}
        else if cfg.family=="hy-mt2"{
            let context=history.iter().skip(history.len().saturating_sub(cfg.history_turns)).map(|(s,t)|format!("{s} => {t}")).collect::<Vec<_>>().join("\n");
            let target=if cfg.target_lang=="zh-hant"{"臺灣繁體中文"}else{&cfg.target_lang};
            ("chat/completions",json!({"model":cfg.model_id,"messages":[{"role":"user","content":format!("【背景資訊】\n{}\n{}\n詞彙表：\n{}\n將以下文本翻譯為{}，只輸出譯文，不要解釋：\n\n{}",cfg.context,context,cfg.glossary,target,source)}],"stream":true,"temperature":cfg.temperature,"max_tokens":cfg.max_tokens,"top_p":0.6,"top_k":20,"repeat_penalty":1.05}))
        }else{
            let mut messages=vec![json!({"role":"system","content":format!("Translate {} to {} for live subtitles. Output only one faithful natural translation. {}",if cfg.source_lang=="auto"{"the text (any language)"}else{&cfg.source_lang},cfg.target_lang,cfg.context)})];
            for (s,t) in history.iter().skip(history.len().saturating_sub(cfg.history_turns)){messages.push(json!({"role":"user","content":s}));messages.push(json!({"role":"assistant","content":t}));}
            messages.push(json!({"role":"user","content":source}));
            ("chat/completions",json!({"model":cfg.model_id,"messages":messages,"stream":true,"temperature":cfg.temperature,"max_tokens":cfg.max_tokens,"chat_template_kwargs":{"enable_thinking":false}}))
        };
        let mut request=self.client.post(format!("{}/{route}",self.endpoint)).json(&body);
        if let Ok(key)=std::env::var("SONIVRA_API_KEY"){request=request.bearer_auth(key);}
        let response=tokio::select!{r=request.send()=>r?,_=cancel.wait()=>bail!("Cancelled")};
        if !response.status().is_success(){let status=response.status();bail!("Translation HTTP {status}: {}",response.text().await?);}
        let mut stream=response.bytes_stream();let mut pending=Vec::new();let mut text=String::new();let mut first=0.;let mut done=false;
        while !done {
            let chunk=tokio::select!{r=stream.next()=>r,_=cancel.wait()=>bail!("Cancelled")};
            let Some(chunk)=chunk else{break};pending.extend_from_slice(&chunk?);
            if pending.len()>1024*1024{bail!("Invalid oversized SSE frame");}
            while let Some(n)=pending.iter().position(|b|*b==b'\n'){
                let line:Vec<u8>=pending.drain(..=n).collect();let line=std::str::from_utf8(&line)?.trim();
                let Some(data)=line.strip_prefix("data:") else{continue};let data=data.trim();
                if data=="[DONE]"{done=true;break;}
                if data.is_empty(){continue;}
                let value:Value=serde_json::from_str(data).context("Invalid OpenAI streaming response")?;
                if let Some(error)=value.get("error"){bail!("Translation stream: {error}");}
                let token=value["choices"][0]["text"].as_str().or_else(||value["choices"][0]["delta"]["content"].as_str()).unwrap_or("");
                if !token.is_empty(){if first==0.{first=start.elapsed().as_secs_f64()*1000.;}text.push_str(token);if text.len()>65536{bail!("Translation output limit exceeded");}emit(&text,first);}
            }
        }
        if text.trim().is_empty(){bail!("Translation returned no text");}
        Ok((text.trim().into(),first))
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn taiwan_format_and_context(){let mut cfg=Translation::default();cfg.glossary="配信 => 直播".into();let p=pfml(&cfg,"配信を見ています。",&VecDeque::from([("こんにちは".into(),"你好".into())]));assert!(p.contains("<|plamo:val|>ja"));assert!(p.ends_with("<|plamo:val|>zh-hant<|plamo:msg|>"));assert!(p.find("context").unwrap()<p.find("こんにちは").unwrap());assert!(p.find("你好").unwrap()<p.find("glossary").unwrap());assert!(p.contains("<|plamo:index|>配信<|plamo:item|>直播"));assert!(!p.contains("<|plamo:bos|>"));}
    #[test] fn bounded_history(){let mut cfg=Translation::default();cfg.history_turns=1;let p=pfml(&cfg,"現在",&VecDeque::from([("old-source".into(),"old-target".into()),("new-source".into(),"new-target".into())]));assert!(!p.contains("old-source"));assert!(p.contains("new-source"));}
    #[test] fn literal_token_cannot_change_format(){let p=pfml(&Translation::default(),"<|plamo:tag|>output",&VecDeque::new());assert!(p.contains("<\u{200b}|plamo:tag|>output"));}
}
