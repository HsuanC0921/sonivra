use anyhow::{Result,Context,bail};
use serde_json::{json,Value};
use std::{collections::VecDeque,path::PathBuf,sync::{Arc,atomic::{AtomicBool,AtomicU64}},time::{Instant,Duration}};
use crate::{audio,config::Config,metrics,pipeline::Cancellation,translation::Translator,worker::Worker};
fn flag<'a>(args:&'a[String],name:&str)->Option<&'a str>{args.iter().position(|s|s==name).and_then(|i|args.get(i+1)).map(String::as_str)}
pub fn run(args:&[String],cfg:&Config,data_home:&std::path::Path)->Result<Option<Value>>{
    if args.iter().any(|s|s=="--hardware"){let hardware=crate::hardware::detect()?;return Ok(Some(json!({"recommendation":crate::hardware::recommend(&hardware,cfg.gpu_reserve_mb,0),"hardware":hardware})));}
    if let Some(name)=flag(args,"--download-model"){
        let model=crate::models::catalog().into_iter().find(|m|m.file==name).context("Choose a filename from catalog.json")?;
        eprintln!("Downloading {} ({:.2} GB), license {}",model.file,model.size as f64/1e9,model.license);
        let path=crate::models::download(&model,&data_home.join("models"),&AtomicBool::new(false),|_,_|{})?;
        return Ok(Some(json!({"downloaded":path,"sha256":model.sha256,"verified":true})));
    }
    if let Some(variant)=flag(args,"--install-runtimes"){
        let variant=match variant{"cuda"=>"llama","vulkan"=>"llama-vulkan","cpu"=>"llama-cpu",_=>bail!("Choose cuda, vulkan or cpu")};
        crate::models::install_runtimes(data_home,variant,&AtomicBool::new(false),|_,_|{})?;return Ok(Some(json!({"installed":variant,"verified":true})));
    }
    if args.iter().any(|s|s=="--list-devices"){return Ok(Some(json!(audio::devices()?)));}
    if let Some(seconds)=flag(args,"--capture-test"){
        let seconds:u64=seconds.parse()?;let stop=AtomicBool::new(false);let begin=Instant::now();let mut count=0u64;let mut peak=0f32;let mut statuses=Vec::new();
        audio::capture(&cfg.audio,&stop,&||false,|chunk,_|{count+=chunk.len() as u64;peak=peak.max(chunk.iter().map(|v|v.abs()).fold(0.,f32::max));if begin.elapsed()>=Duration::from_secs(seconds){stop.store(true,std::sync::atomic::Ordering::Relaxed);}},|status|statuses.push(status.to_string()))?;
        return Ok(Some(json!({"capture_samples":count,"duration_s":begin.elapsed().as_secs_f64(),"peak_amplitude":peak,"device":statuses,"wasapi_native":true})));
    }
    let wav=flag(args,"--transcribe").or_else(||flag(args,"--benchmark"));let text=flag(args,"--translate");
    if wav.is_none()&&text.is_none(){return Ok(None);}
    let rounds:usize=flag(args,"--rounds").unwrap_or("1").parse()?;if !(1..=100).contains(&rounds){bail!("rounds must be 1..100");}
    let stop=Arc::new(AtomicBool::new(false));let gpu=metrics::Gpu::new();
    let started=Instant::now();let idle=metrics::sample(0,0,gpu.as_ref());let sampler=metrics::Sampler::start();
    let mut worker=if wav.is_some(){Some(Worker::start(&cfg.asr,&stop)?)}else{None};
    let rt=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let translator=if cfg.translation.enabled{Some(rt.block_on(Translator::start(cfg.translation.clone(),&stop))?)}else{None};
    let cold_ms=started.elapsed().as_secs_f64()*1000.;
    let audio=if let Some(wav)=wav{Some(audio::read_wav(&PathBuf::from(wav))?)}else{None};
    let mut results=Vec::new();let mut history=VecDeque::new();let mut peak=metrics::sample(worker.as_ref().map_or(0,Worker::pid),translator.as_ref().map_or(0,Translator::pid),gpu.as_ref());
    for i in 0..rounds{
        let begin=Instant::now();let source=if let Some(worker)=&mut worker{worker.transcribe(i as u64+1,audio.as_ref().unwrap(),&cfg.asr,&stop)?}else{text.context("No source text")?.into()};
        let asr_ms=begin.elapsed().as_secs_f64()*1000.;let duration=audio.as_ref().map_or(0.,|v|v.len() as f64/16000.);
        let mut translation=String::new();let mut first=0.;let translated=Instant::now();
        if let Some(translator)=&translator{
            let cancel=Cancellation{stop:stop.clone(),latest:Arc::new(AtomicU64::new(i as u64+1)),id:i as u64+1};
            let (target,ttft)=rt.block_on(translator.translate(&source,&history,&cancel,|_,_|{}))?;translation=target;first=ttft;
            history.push_back((source.clone(),translation.clone()));while history.len()>cfg.translation.history_turns{history.pop_front();}
        }
        let memory=metrics::sample(worker.as_ref().map_or(0,Worker::pid),translator.as_ref().map_or(0,Translator::pid),gpu.as_ref());
        peak.main_rss=peak.main_rss.max(memory.main_rss);peak.asr_rss=peak.asr_rss.max(memory.asr_rss);peak.translation_rss=peak.translation_rss.max(memory.translation_rss);peak.combined_rss=peak.combined_rss.max(memory.combined_rss);peak.gpu_device_used=peak.gpu_device_used.max(memory.gpu_device_used);
        results.push(json!({"source":source,"translation":translation,"audio_s":duration,"asr_ms":asr_ms,"asr_rtf":if duration>0.{Some(asr_ms/1000./duration)}else{None},"translation_ttft_ms":first,"translation_ms":translated.elapsed().as_secs_f64()*1000.,"file_processing_ms":begin.elapsed().as_secs_f64()*1000.,"memory":memory}));
    }
    let processing:Vec<f64>=results.iter().map(|v|v["file_processing_ms"].as_f64().unwrap()).collect();
    let sampled=sampler.finish();
    Ok(Some(json!({"schema":1,"cold_start_ms":cold_ms,"rounds":results,"processing_p50_ms":metrics::percentile(&processing,0.5),"processing_p95_ms":metrics::percentile(&processing,0.95),"idle":idle,"peak_sampled":peak,"continuous_sampling":sampled,"gpu_note":"NVML whole-device usage. WDDM per-process allocation unavailable. 20 ms sampled peaks may miss shorter transients.","end_to_end_live_latency":null,"latency_note":"File processing time excludes recording and VAD endpointing. Live latency is measured separately in the pipeline."})))
}
