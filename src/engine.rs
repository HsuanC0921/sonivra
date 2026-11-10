use anyhow::Result;
use crossbeam_channel::{Sender,bounded};
use std::{collections::VecDeque,path::PathBuf,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering}},thread::{self,JoinHandle},time::{Duration,Instant}};
use crate::{config::Config,pipeline::{Snapshot,Segment,Segmenter,Cancellation,Phase},worker::Worker,translation::Translator,audio};
pub type Shared=Arc<Mutex<Snapshot>>;
pub enum Control{Start(Config),File(Config,PathBuf),Translate(Config,String),Vad(crate::config::Audio),Stop,Quit}
pub struct Engine {pub snapshot:Shared,pub control:Sender<Control>,pub stop:Arc<AtomicBool>,
    /// Pause keeps both workers resident and only discards captured audio.
    pub paused:Arc<AtomicBool>,coordinator:Mutex<Option<JoinHandle<()>>>}
struct Session{stop:Arc<AtomicBool>,audio_settings:Arc<Mutex<crate::config::Audio>>,threads:Vec<JoinHandle<()>>}
impl Drop for Session{fn drop(&mut self){self.stop.store(true,Ordering::Release);for thread in self.threads.drain(..){let _=thread.join();}}}
fn file_name(path:&std::path::Path)->String{path.file_name().map_or_else(String::new,|n|n.to_string_lossy().into())}
impl Engine{
    pub fn new()->Self{
        let snapshot=Arc::new(Mutex::new(Snapshot::default()));
        let stop=Arc::new(AtomicBool::new(false));let paused=Arc::new(AtomicBool::new(false));let (control,rx)=bounded(8);
        let s=snapshot.clone();let cancel=stop.clone();let pause=paused.clone();
        let coordinator=thread::spawn(move||{
            let mut session:Option<Session>=None;
            while let Ok(command)=rx.recv(){
                if let Control::Vad(audio)=command{if let Some(session)=&session{*session.audio_settings.lock().unwrap()=audio;}continue;}
                // Old workers are reaped before any replacement is loaded.
                drop(session.take());
                {let mut s=s.lock().unwrap();s.running=false;s.asr_pid=0;s.translation_pid=0;s.loading=false;s.loaded.clear();s.voice=false;s.asr_busy=false;s.translating=false;s.rms=0.;}
                pause.store(false,Ordering::Release);
                match command{
                    Control::Quit=>break,
                    Control::Vad(_)=>unreachable!(),
                    Control::Stop=>{s.lock().unwrap().phase=Phase::Stopped;},
                    start @ (Control::Start(_)|Control::File(_,_))=>{
                        let (cfg,file)=match start{Control::File(cfg,p)=>(cfg,Some(p)),Control::Start(cfg)=>(cfg,None),_=>unreachable!()};
                        cancel.store(false,Ordering::Release);
                        match Session::start(cfg,file,cancel.clone(),pause.clone(),s.clone()){
                            Ok(value)=>session=Some(value),
                            Err(error)=>{let mut snapshot=s.lock().unwrap();let key=if snapshot.phase==Phase::LoadingTranslation{"translation_load"}else{"asr_load"};if format!("{error:#}")!="Cancelled"{snapshot.fail(key,format!("{error:#}"));snapshot.phase=Phase::Failed;}snapshot.loading=false;snapshot.running=false;snapshot.asr_pid=0;snapshot.translation_pid=0;snapshot.loaded.clear();}
                        }
                    },
                    Control::Translate(cfg,text)=>{
                        cancel.store(false,Ordering::Release);
                        {let mut state=s.lock().unwrap();state.loading=true;state.clear_error();state.latest_id=0;state.source(1,text.clone());state.phase=Phase::Testing;}
                        let result=(||->Result<()>{
                            let rt=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                            let translator=rt.block_on(Translator::start(cfg.translation.clone(),&cancel))?;
                            s.lock().unwrap().translation_pid=translator.pid();
                            let token=Cancellation{stop:cancel.clone(),latest:Arc::new(AtomicU64::new(1)),id:1};
                            rt.block_on(translator.translate(&text,&VecDeque::new(),&token,|text,first|{let mut state=s.lock().unwrap();state.translation(1,text.into());state.first_token_ms=first;}))?;
                            Ok(())
                        })();
                        let mut state=s.lock().unwrap();state.loading=false;state.translation_pid=0;
                        match result{Ok(())=>state.phase=Phase::TestDone,Err(e)=>{state.fail("translation_test",format!("{e:#}"));state.phase=Phase::TestFailed;}}
                    }
                }
            }
        });
        Self{snapshot,control,stop,paused,coordinator:Mutex::new(Some(coordinator))}
    }
    pub fn send(&self,command:Control)->Result<()>{if !matches!(&command,Control::Vad(_)){self.stop.store(true,Ordering::Release);}self.control.try_send(command).map_err(|_|anyhow::anyhow!("Another model operation is pending. Wait for it to finish"))}
}
impl Engine{
    /// Stops the session, reaps both workers and joins the coordinator. Safe to call twice.
    pub fn shutdown(&self){let Some(thread)=self.coordinator.lock().unwrap().take() else{return};self.stop.store(true,Ordering::Release);let _=self.control.send(Control::Quit);let _=thread.join();}
}
impl Drop for Engine{fn drop(&mut self){self.shutdown();}}
impl Session{
    fn start(cfg:Config,file:Option<PathBuf>,stop:Arc<AtomicBool>,paused:Arc<AtomicBool>,s:Shared)->Result<Self>{
        cfg.validate()?;
        // Automatic recognition can hear any language, so translation must not assume one.
        let mut cfg=cfg;if cfg.asr.language=="auto"{cfg.translation.source_lang="auto".into();}
        {let mut state=s.lock().unwrap();state.loading=true;state.clear_error();state.latest_id=0;state.source.clear();state.target.clear();state.changed=None;state.phase=Phase::LoadingAsr;}
        let mut worker=Worker::start(&cfg.asr,&stop)?;
        {let mut state=s.lock().unwrap();state.asr_pid=worker.pid();state.model_info=worker.model.to_string();state.actual_asr=worker.model["architecture"].as_str().unwrap_or("see model info").into();state.loaded.push(file_name(&cfg.asr.model));state.phase=Phase::LoadingTranslation;}
        let rt=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let translator=if cfg.translation.enabled{Some(rt.block_on(Translator::start(cfg.translation.clone(),&stop))?)}else{None};
        {let mut state=s.lock().unwrap();state.translation_pid=translator.as_ref().map_or(0,Translator::pid);if translator.is_some()&&cfg.translation.local{state.loaded.push(file_name(&cfg.translation.model));}state.running=true;state.loading=false;state.phase=Phase::Listening;}
        let (segment_tx,segment_rx)=bounded::<Segment>(2);let discard_rx=segment_rx.clone();
        // At most one provisional job waits; a newer one replaces it.
        let (partial_tx,partial_rx)=bounded::<Segment>(1);let discard_partial=partial_rx.clone();let interim=cfg.subtitles.interim;
        let (translation_tx,translation_rx)=bounded::<(u64,String,Instant,Instant)>(1);let discard_translation=translation_rx.clone();
        let latest=Arc::new(AtomicU64::new(0));let mut threads=Vec::new();
        if let Some(mut translator)=translator{
            let s=s.clone();let stop=stop.clone();let latest=latest.clone();let settings=cfg.translation.clone();
            threads.push(thread::spawn(move||{
                let mut history=VecDeque::new();
                type Hist=VecDeque<(String,String)>;
                let mut last_interim=String::new();
                let mut prev:Option<(String,Hist,String)>=None;
                let mut todo:Option<(String,Hist,String,String,u64)>=None;
                while !stop.load(Ordering::Relaxed){
                    let (id,source,onset,ended)=match translation_rx.recv_timeout(Duration::from_millis(100)){Ok(job)=>{todo=None;job},Err(_)=>{
                        // Idle: translate the previous sentence again, now that the one after it is known.
                        // New sentences always come first: skip the extra pass while latency is already high or audio is waiting.
                        let busy={let st=s.lock().unwrap();st.after_speech_ms>1200.||st.backlog>0||st.asr_busy};
                        if busy{todo=None;}
                        if let Some((psrc,phist,ptarget,next,nid))=todo.take(){
                            let cancel=Cancellation{stop:stop.clone(),latest:latest.clone(),id:nid};
                            let extra=format!("\n次の文（文脈の参考のみ。翻訳しないこと）：{next}");
                            if let Ok((new,_))=rt.block_on(translator.translate_with(&psrc,&phist,&cancel,|_,_|{},&extra)){
                                let new=new.trim().to_string();
                                if !cancel.cancelled()&&!new.is_empty()&&new!=ptarget{
                                    if let Some(h)=history.iter_mut().rev().find(|h:&&mut (String,String)|h.0==psrc){h.1=new.chars().take(4096).collect();}
                                    let mut state=s.lock().unwrap();state.revisions.push((psrc,new));state.revised+=1;
                                }
                            }
                        }
                        // Provisional translation of the sentence still being spoken, only when nothing else is waiting.
                        if settings.interim&&!busy{
                            let (pid,text)={let st=s.lock().unwrap();(st.interim_id,st.interim.clone())};
                            let now=latest.load(Ordering::Acquire);
                            if !text.is_empty()&&pid>now&&text!=last_interim{
                                last_interim=text.clone();
                                let cancel=Cancellation{stop:stop.clone(),latest:latest.clone(),id:now};
                                let _=rt.block_on(translator.translate(&text,&history,&cancel,|t,_|{let mut st=s.lock().unwrap();if st.interim==text{st.interim_target=t.into();}}));
                            }
                        }
                        continue}};
                    let cancel=Cancellation{stop:stop.clone(),latest:latest.clone(),id};
                    s.lock().unwrap().translating=true;
                    let result=rt.block_on(async{
                        tokio::select!{_=tokio::time::sleep(Duration::from_millis(settings.debounce_ms as u64))=>{},_=cancel.wait()=>return Err(anyhow::anyhow!("Cancelled"))}
                        translator.translate(&source,&history,&cancel,|target,first|{let mut state=s.lock().unwrap();if state.translation(id,target.into()){state.first_token_ms=first;state.full_ms=onset.elapsed().as_secs_f64()*1000.;state.after_speech_ms=ended.elapsed().as_secs_f64()*1000.;}}).await
                    });
                    s.lock().unwrap().translating=false;
                    match result{
                        Ok((target,_))=>{if !cancel.cancelled(){{let mut state=s.lock().unwrap();state.latencies.push_back(ended.elapsed().as_secs_f64()*1000.);while state.latencies.len()>500{state.latencies.pop_front();}}let before=history.clone();history.push_back((source.chars().take(4096).collect(),target.chars().take(4096).collect()));while history.len()>settings.history_turns{history.pop_front();}
                            if settings.refine{if let Some((psrc,phist,ptarget))=prev.take(){todo=Some((psrc,phist,ptarget,source.clone(),id));}prev=Some((source.clone(),before,target.trim().to_string()));}}},
                        Err(error)=>{if !cancel.cancelled(){s.lock().unwrap().fail("translation",format!("{error:#}"));if settings.local&&!translator.alive(){drop(translator);match rt.block_on(Translator::start(settings.clone(),&stop)){Ok(new)=>{translator=new;s.lock().unwrap().translation_pid=translator.pid();},Err(e)=>{let mut state=s.lock().unwrap();state.fail("translation_load",format!("Translation restart failed: {e:#}"));state.phase=Phase::Failed;state.running=false;stop.store(true,Ordering::Release);break;}}}}}
                    }
                }
            }));
        }
        let audio_settings=Arc::new(Mutex::new(cfg.audio.clone()));let live_audio=audio_settings.clone();
        let capture_stop=stop.clone();let capture_state=s.clone();
        threads.push(thread::spawn(move||{
            let seq=AtomicU64::new(0);let mut sent=0usize;
            let submit=|mut segment:Segment|{
                segment.id=seq.fetch_add(1,Ordering::Relaxed)+1;
                match segment_tx.try_send(segment){Ok(())=>{},Err(crossbeam_channel::TrySendError::Full(segment))=>{let _=discard_rx.try_recv();let _=segment_tx.try_send(segment);capture_state.lock().unwrap().dropped+=1;},Err(_)=>{}}
                capture_state.lock().unwrap().backlog=segment_tx.len();
            };
            if let Some(file)=file{
                match audio::read_wav(&file){
                    Ok(audio)=>{let now=Instant::now();submit(Segment{id:0,audio,onset:now,ended:now,submitted:now});},
                    Err(error)=>capture_state.lock().unwrap().fail("file",format!("{error:#}"))
                }
                return;
            }
            while !capture_stop.load(Ordering::Relaxed){
                // Device selection is read live, so switching sources reopens capture without reloading models.
                let capture_cfg=live_audio.lock().unwrap().clone();
                let mut segmenter=Segmenter::new(capture_cfg.clone());
                let mut recorder:Option<(hound::WavWriter<std::io::BufWriter<std::fs::File>>,Instant)>=None;
                let changed=||{let a=live_audio.lock().unwrap();a.device!=capture_cfg.device||a.microphone!=capture_cfg.microphone||a.process!=capture_cfg.process||a.mix_mic!=capture_cfg.mix_mic};
                let result=audio::capture(&capture_cfg,&capture_stop,&changed,|chunk,now|{
                    if paused.load(Ordering::Relaxed){
                        if segmenter.active(){segmenter=Segmenter::new(live_audio.lock().unwrap().clone());}
                        let mut state=capture_state.lock().unwrap();state.rms=0.;state.voice=false;return;
                    }
                    let (want_record,dir)={let a=live_audio.lock().unwrap();(a.record,a.record_dir.clone())};
                    // Everything that is recognized, silence included, so the file lines up with the transcript.
                    if want_record&&recorder.is_none()&&!dir.as_os_str().is_empty(){
                        let t=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|d|d.as_secs());
                        let spec=hound::WavSpec{channels:1,sample_rate:16000,bits_per_sample:16,sample_format:hound::SampleFormat::Int};
                        let made=std::fs::create_dir_all(&dir).map_err(|e|e.to_string()).and_then(|_|hound::WavWriter::create(dir.join(format!("sonivra-audio-{t}.wav")),spec).map_err(|e|e.to_string()));
                        match made{Ok(w)=>recorder=Some((w,Instant::now())),Err(e)=>{live_audio.lock().unwrap().record=false;capture_state.lock().unwrap().fail("file",format!("WAV recording: {e}"));}}
                    }
                    if !want_record{recorder=None;}
                    if let Some((w,flushed))=&mut recorder{
                        for s in chunk{let _=w.write_sample((s.clamp(-1.,1.)*32767.) as i16);}
                        if flushed.elapsed()>=Duration::from_secs(5){let _=w.flush();*flushed=Instant::now();}
                    }
                    segmenter.update(&live_audio.lock().unwrap());
                    let segment=segmenter.feed(chunk,now);
                    {let mut state=capture_state.lock().unwrap();state.audio_frames+=chunk.len() as u64;state.rms=(chunk.iter().map(|x|x*x).sum::<f32>()/chunk.len() as f32).sqrt();state.voice=segmenter.active();}
                    if let Some(segment)=segment{submit(segment);sent=0;}
                    // Every 0.8 s of new speech, recognise what has been said so far.
                    else if interim&&segmenter.active(){let a=segmenter.pending();if a.len()<sent{sent=0;}if a.len()>=12800&&a.len()>=sent+12800{sent=a.len();
                        let job=Segment{id:seq.load(Ordering::Relaxed)+1,audio:a.to_vec(),onset:now,ended:now,submitted:now};
                        if let Err(crossbeam_channel::TrySendError::Full(job))=partial_tx.try_send(job){let _=discard_partial.try_recv();let _=partial_tx.try_send(job);}}}
                },|device|{let mut state=capture_state.lock().unwrap();state.device=device.into();if state.phase==Phase::Reconnecting{state.phase=Phase::Listening;if state.error_key=="audio"{state.clear_error();}}});
                if let Err(error)=result{if !capture_stop.load(Ordering::Relaxed){if let Some(segment)=segmenter.flush(Instant::now()){submit(segment);}let mut state=capture_state.lock().unwrap();state.fail("audio",format!("{error:#}"));state.phase=Phase::Reconnecting;state.voice=false;}}
                for _ in 0..10{if capture_stop.load(Ordering::Relaxed){break;}thread::sleep(Duration::from_millis(100));}
            }
        }));
        let asr_stop=stop.clone();let asr_state=s.clone();
        threads.push(thread::spawn(move||{
            while !asr_stop.load(Ordering::Relaxed){
                // Finished segments always go first; provisional jobs only use idle time.
                let segment=match segment_rx.recv_timeout(Duration::from_millis(20)){Ok(segment)=>segment,Err(_)=>{
                    let Ok(job)=partial_rx.try_recv() else{continue};
                    if asr_state.lock().unwrap().latest_id>=job.id||!segment_rx.is_empty(){continue;}
                    if let Ok(text)=worker.transcribe(job.id|1<<63,&job.audio,&cfg.asr,&asr_stop){if text.chars().any(char::is_alphanumeric){asr_state.lock().unwrap().partial(job.id,text);}}
                    continue}};
                let started=Instant::now();asr_state.lock().unwrap().asr_busy=true;
                let mut result=worker.transcribe(segment.id,&segment.audio,&cfg.asr,&asr_stop);
                if result.is_err()&&!asr_stop.load(Ordering::Relaxed)&&!worker.alive(){
                    drop(worker);
                    match Worker::start(&cfg.asr,&asr_stop){
                        Ok(new)=>{worker=new;asr_state.lock().unwrap().asr_pid=worker.pid();result=worker.transcribe(segment.id,&segment.audio,&cfg.asr,&asr_stop);},
                        Err(error)=>{let mut state=asr_state.lock().unwrap();state.fail("asr_load",format!("ASR restart failed: {error:#}"));state.phase=Phase::Failed;state.asr_busy=false;state.running=false;asr_stop.store(true,Ordering::Release);break;}
                    }
                }
                asr_state.lock().unwrap().asr_busy=false;
                match result{
                    Ok(text) if text.chars().any(char::is_alphanumeric)=>{
                        latest.store(segment.id,Ordering::Release);
                        let mut state=asr_state.lock().unwrap();state.source(segment.id,text.clone());state.asr_ms=started.elapsed().as_secs_f64()*1000.;state.full_ms=segment.onset.elapsed().as_secs_f64()*1000.;state.after_speech_ms=segment.ended.elapsed().as_secs_f64()*1000.;state.endpoint_ms=segment.submitted.saturating_duration_since(segment.ended).as_secs_f64()*1000.;state.queue_ms=started.saturating_duration_since(segment.submitted).as_secs_f64()*1000.;state.backlog=segment_rx.len();if state.error_key=="asr"{state.clear_error();}drop(state);
                        if cfg.translation.enabled{let job=(segment.id,text,segment.onset,segment.ended);if let Err(crossbeam_channel::TrySendError::Full(job))=translation_tx.try_send(job){let _=discard_translation.try_recv();let _=translation_tx.try_send(job);}}
                    },
                    Ok(_)=>{let mut state=asr_state.lock().unwrap();if segment.id>=state.interim_id{state.interim.clear();state.interim_target.clear();}},Err(error)=>{if !asr_stop.load(Ordering::Relaxed){asr_state.lock().unwrap().fail("asr",format!("{error:#}"));}}
                }
            }
        }));
        Ok(Self{stop,audio_settings,threads})
    }
}
