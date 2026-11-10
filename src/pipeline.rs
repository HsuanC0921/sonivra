use crate::config::Audio;
use std::{collections::VecDeque, sync::{Arc, atomic::{AtomicBool,AtomicU64,Ordering}},time::{Instant,Duration}};

pub const SAMPLE_RATE:u32=16000;
pub struct Segment {pub id:u64,pub audio:Vec<f32>,pub onset:Instant,pub ended:Instant,pub submitted:Instant}
pub struct Segmenter { cfg:Audio, pre:VecDeque<f32>, audio:Vec<f32>, quiet:usize, voiced:usize, onset:Option<Instant>, since_end:usize }
impl Segmenter {
    pub fn new(cfg:Audio)->Self {Self{cfg,pre:VecDeque::with_capacity(3200),audio:Vec::new(),quiet:0,voiced:0,onset:None,since_end:usize::MAX}}
    pub fn update(&mut self,cfg:&Audio){self.cfg.threshold=cfg.threshold;self.cfg.silence_ms=cfg.silence_ms;self.cfg.min_speech_ms=cfg.min_speech_ms;self.cfg.max_segment_ms=cfg.max_segment_ms;}
    pub fn feed(&mut self,chunk:&[f32],now:Instant)->Option<Segment> {
        if chunk.is_empty(){return None;}
        let rms=(chunk.iter().map(|v|v*v).sum::<f32>()/chunk.len() as f32).sqrt();
        // Speech that resumes within 1.5 s of a segment end is a continuation, so it gets the lower in-speech threshold.
        let threshold=if self.onset.is_some()||self.since_end<24000{self.cfg.threshold*0.65}else{self.cfg.threshold};
        if self.onset.is_none(){self.since_end=self.since_end.saturating_add(chunk.len());}
        if rms>=threshold {
            if self.onset.is_none(){
                self.onset=Some(now-Duration::from_secs_f32((self.pre.len()+chunk.len()) as f32/SAMPLE_RATE as f32));
                self.audio.extend(self.pre.drain(..));
            }
            self.quiet=0;self.voiced+=chunk.len();
        } else if self.onset.is_some(){self.quiet+=chunk.len();}
        if self.onset.is_some(){self.audio.extend_from_slice(chunk);}
        else {self.pre.extend(chunk.iter().copied());while self.pre.len()>2560{self.pre.pop_front();}}
        let maximum=self.cfg.max_segment_ms as usize*16;
        if self.onset.is_none(){return None;}
        if self.audio.len()>=maximum{return self.cut(now);}
        if self.quiet>=self.needed_silence(){return self.flush(now);}
        None
    }
    /// Continuous speech rarely leaves the full pause, so the pause that ends a segment shrinks
    /// from `silence_ms` after 1.5 s to 150 ms at the length limit. Short phrases keep the full pause.
    fn needed_silence(&self)->usize{
        let base=self.cfg.silence_ms as usize*16;let floor=base.min(150*16);
        let (start,end)=(24000,self.cfg.max_segment_ms as usize*16);
        if end<=start||self.audio.len()<=start{return base;}
        base-(base-floor)*(self.audio.len()-start).min(end-start)/(end-start)
    }
    /// At the length limit, split at the quietest 20 ms of the last 1.5 s instead of mid-word,
    /// and keep the remainder as the start of the next segment.
    fn cut(&mut self,now:Instant)->Option<Segment>{
        let n=self.audio.len();let from=n.saturating_sub(24000).max(n/2);
        let (mut best,mut low)=(n,f32::MAX);
        for i in (from..n.saturating_sub(320)).step_by(320){let e:f32=self.audio[i..i+320].iter().map(|v|v*v).sum();if e<low{low=e;best=i+160;}}
        let tail=self.audio.split_off(best);let tail_time=Duration::from_secs_f64(tail.len() as f64/16000.);
        self.quiet=0;
        let segment=self.flush(now-tail_time).map(|mut s|{s.submitted=now;s});
        self.onset=Some(now-tail_time);self.voiced=tail.len();self.audio=tail;
        segment
    }
    pub fn active(&self)->bool{self.onset.is_some()}
    /// Audio of the segment still in progress, for provisional recognition.
    pub fn pending(&self)->&[f32]{&self.audio}
    pub fn flush(&mut self,now:Instant)->Option<Segment>{
        let onset=self.onset.take()?;self.since_end=0;
        let voiced=std::mem::take(&mut self.voiced);
        let ended=now-Duration::from_secs_f64(self.quiet as f64/16000.);self.quiet=0;
        let audio=std::mem::take(&mut self.audio);
        if voiced<self.cfg.min_speech_ms as usize*16{return None;}
        Some(Segment{id:0,audio,onset,ended,submitted:now})
    }
}
#[derive(Clone)]
pub struct Cancellation {pub stop:Arc<AtomicBool>,pub latest:Arc<AtomicU64>,pub id:u64}
impl Cancellation {
    pub fn cancelled(&self)->bool {self.stop.load(Ordering::Relaxed)||self.latest.load(Ordering::Acquire)!=self.id}
    pub async fn wait(&self){while !self.cancelled(){tokio::time::sleep(Duration::from_millis(25)).await;}}
}
/// Engine lifecycle state. The UI maps each phase to localized text.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum Phase{#[default]Idle,LoadingAsr,LoadingTranslation,Listening,Reconnecting,Stopped,Failed,Testing,TestDone,TestFailed}
#[derive(Default,Clone,Debug)]
pub struct Snapshot {
    pub phase:Phase,pub device:String,pub source:String,pub target:String,
    /// Provisional text of the segment still being spoken; replaced by the final result.
    pub interim:String,pub interim_id:u64,pub interim_target:String,
    /// Corrected translations of earlier sentences (original, new translation), taken by the UI.
    pub revisions:Vec<(String,String)>,pub revised:u64,
    /// Semantic error category (localized in the UI) and the raw backend detail.
    pub error_key:String,pub error:String,
    pub voice:bool,pub asr_busy:bool,pub translating:bool,
    /// File names of models currently resident in workers.
    pub loaded:Vec<String>,pub download_file:String,
    /// Set by a finished download (artifact file name, or "runtimes"); consumed by the UI.
    pub download_done:Option<String>,
    pub running:bool,pub loading:bool,pub latest_id:u64,pub rms:f32,
    pub asr_ms:f64,pub first_token_ms:f64,pub full_ms:f64,pub endpoint_ms:f64,
    /// Speech end to finished translation, per segment (newest 500).
    pub latencies:VecDeque<f64>,
    pub after_speech_ms:f64,pub queue_ms:f64,
    pub asr_pid:u32,pub translation_pid:u32,pub backlog:usize,pub dropped:u64,
    pub audio_frames:u64,pub actual_asr:String,pub model_info:String,
    pub downloaded:u64,pub download_total:u64,pub download_busy:bool,
    pub changed:Option<Instant>,
}
impl Snapshot {
    pub fn fail(&mut self,key:&str,detail:String){self.error_key=key.into();self.error=detail;}
    pub fn clear_error(&mut self){self.error_key.clear();self.error.clear();}
    pub fn source(&mut self,id:u64,text:String){if id>=self.interim_id{self.interim.clear();self.interim_target.clear();}if id>=self.latest_id{self.latest_id=id;self.source=text;self.target.clear();self.changed=Some(Instant::now());}}
    pub fn partial(&mut self,id:u64,text:String){if id>self.latest_id{self.interim_id=id;self.interim=text;}}
    pub fn translation(&mut self,id:u64,text:String)->bool {if id!=self.latest_id{return false;}self.target=text;self.changed=Some(Instant::now());true}
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn soft_speech_and_hangover(){let mut s=Segmenter::new(Audio::default());let t=Instant::now();for _ in 0..10{assert!(s.feed(&vec![0.004;320],t).is_none());}let mut out=None;for _ in 0..24{if let Some(x)=s.feed(&vec![0.0;320],t){out=Some(x)}}assert!(out.unwrap().audio.len()<=16000);}
    #[test] fn bounded_segments(){let mut cfg=Audio::default();cfg.max_segment_ms=1000;let mut s=Segmenter::new(cfg);let t=Instant::now();let mut longest=0;for _ in 0..300{if let Some(x)=s.feed(&vec![0.01;320],t){longest=longest.max(x.audio.len())}}assert!(longest<=16000);assert!(longest>0);}
    #[test] fn long_speech_ends_on_short_pause(){let mut s=Segmenter::new(Audio::default());let t=Instant::now();for _ in 0..300{assert!(s.feed(&vec![0.01;320],t).is_none());}let mut cut=None;for i in 0..20{if s.feed(&vec![0.0;320],t).is_some(){cut=Some(i);break;}}assert!(cut.unwrap()<15,"6 s of speech should end on a pause shorter than 300 ms");}
    #[test] fn short_phrase_keeps_full_pause(){let mut s=Segmenter::new(Audio::default());let t=Instant::now();for _ in 0..25{s.feed(&vec![0.01;320],t);}for _ in 0..21{assert!(s.feed(&vec![0.0;320],t).is_none());}}
    #[test] fn length_limit_cuts_at_quietest_point(){let mut cfg=Audio::default();cfg.max_segment_ms=2000;let mut s=Segmenter::new(cfg);let t=Instant::now();let mut out=None;for i in 0..100{let level=if i==80{0.0}else{0.01};if let Some(x)=s.feed(&vec![level;320],t){out=Some(x);break;}}assert_eq!(out.unwrap().audio.len(),80*320+160);assert!(s.active());}
    #[test] fn quiet_continuation_after_segment(){let mut s=Segmenter::new(Audio::default());let t=Instant::now();for _ in 0..50{s.feed(&vec![0.01;320],t);}let mut ended=false;for _ in 0..30{ended|=s.feed(&vec![0.0;320],t).is_some();}assert!(ended);s.feed(&vec![0.0025;320],t);assert!(s.active(),"0.0025 is below the onset threshold but continues speech");let mut fresh=Segmenter::new(Audio::default());fresh.feed(&vec![0.0025;320],t);assert!(!fresh.active());}
    #[test] fn interim_replaced_by_final(){let mut s=Snapshot::default();s.source(1,"一".into());s.partial(2,"に".into());assert_eq!(s.interim,"に");s.partial(1,"舊".into());assert_eq!(s.interim,"に");s.source(2,"二".into());assert!(s.interim.is_empty());}
    #[test] fn reject_click(){let mut s=Segmenter::new(Audio::default());let t=Instant::now();s.feed(&vec![0.05;320],t);for _ in 0..30{assert!(s.feed(&vec![0.;320],t).is_none());}}
    #[test] fn stale_translation_cannot_overwrite(){let mut s=Snapshot::default();s.source(1,"一".into());s.source(2,"二".into());assert!(!s.translation(1,"舊".into()));assert!(s.translation(2,"新".into()));assert_eq!(s.target,"新");}
    #[test] fn translation_cancelled_on_switch(){let c=Cancellation{stop:Arc::new(AtomicBool::new(false)),latest:Arc::new(AtomicU64::new(1)),id:1};assert!(!c.cancelled());c.latest.store(2,Ordering::Release);assert!(c.cancelled());}
    #[test] fn repeated_utterances_are_not_deleted(){let mut s=Snapshot::default();s.source(1,"はい".into());s.source(2,"はい".into());assert_eq!(s.latest_id,2);assert_eq!(s.source,"はい");}
}
