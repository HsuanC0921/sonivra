use anyhow::{Context,Result};
use sonivra::{audio::{self,AudioDevice},config::Config,engine::{Control,Engine},hardware,i18n::{self,t,tf},languages::{self,Language},metrics,models::{self,Artifact},pipeline::{Phase,Snapshot}};
use slint::{ComponentHandle,ModelRc,VecModel,winit_030::WinitWindowAccessor};
use std::{cell::RefCell,collections::VecDeque,path::{Path,PathBuf},rc::{Rc,Weak},sync::{Arc,atomic::{AtomicBool,Ordering}},time::{Duration,Instant}};
use crate::tray::{self,Tray};
slint::include_modules!();

/// Fonts shipped with Windows 11 (plus Noto if the user installed it) and the file that proves they exist.
const FONTS:[(&str,&str);7]=[("Yu Gothic UI","YuGothR.ttc"),("Microsoft JhengHei UI","msjh.ttc"),("Meiryo UI","meiryo.ttc"),("BIZ UDPGothic","BIZ-UDGothicR.ttc"),("Noto Sans JP","NotoSansJP-VF.ttf"),("Noto Sans TC","NotoSansTC-VF.ttf"),("Segoe UI","segoeui.ttf")];
const DEMO_SOURCE:&str="これは字幕のプレビューです。位置と文字の大きさを調整してください。";
const DEMO_TARGET:&str="這是字幕預覽。請調整位置與文字大小。";
const DEMO_PREVIOUS:(&str,&str)=("前の字幕は小さく上に残ります。","上一句會縮小留在上方。");
/// Retention choices for the in-memory transcript.
const KEEP_CHOICES:[u32;4]=[500,2000,10000,50000];
// ponytail: TextEdit reshapes its whole text on every update, so the log page lists only the newest matches. Export covers everything kept.
const SWATCHES:[&str;6]=["#FFF3CF","#FFFFFF","#FFE15A","#8FE3FF","#B6F0A8","#FFB8D2"];
const LOG_VIEW_LINES:usize=1500;
/// Scrollback on the subtitle window returns to live after this much idle time.
const SCROLL_IDLE:Duration=Duration::from_secs(20);
const RUNTIME_VARIANTS:[&str;3]=["llama","llama-vulkan","llama-cpu"];
// VAD threshold <-> 1..100 sensitivity on a log scale (100 = most sensitive).
fn threshold(sensitivity:f32)->f32{0.05*0.01f32.powf((sensitivity-1.)/99.)}
fn sensitivity(threshold:f32)->f32{(1.+99.*(threshold/0.05).ln()/0.01f32.ln()).clamp(1.,100.)}

#[derive(Clone)]enum Job{Model(usize),Runtimes(String)}
/// One finished subtitle. `start` and `end` count from app start, `clock` is the local time it appeared.
#[derive(Clone,Default)]struct Line{clock:String,start:Duration,end:Duration,source:String,target:String}
fn now_local()->windows::Win32::Foundation::SYSTEMTIME{unsafe{windows::Win32::System::SystemInformation::GetLocalTime()}}
fn clock()->String{let t=now_local();format!("{:02}:{:02}:{:02}",t.wHour,t.wMinute,t.wSecond)}
fn txt(line:&Line)->String{if line.target.is_empty(){format!("[{}] {}\r\n\r\n",line.clock,line.source)}else{format!("[{}] {}\r\n{}\r\n\r\n",line.clock,line.source,line.target)}}
fn srt_time(d:Duration,sep:char)->String{let ms=d.as_millis();format!("{:02}:{:02}:{:02}{sep}{:03}",ms/3_600_000,ms/60_000%60,ms/1000%60,ms%1000)}
/// SRT, or WebVTT when `vtt` (header, no cue numbers, dot before milliseconds).
fn srt<'a>(lines:impl Iterator<Item=&'a Line>,vtt:bool)->String{
    let sep=if vtt{'.'}else{','};
    let cues:String=lines.enumerate().map(|(i,l)|{let end=l.end.max(l.start+Duration::from_millis(500));let text=if l.target.is_empty(){l.source.clone()}else{format!("{}\r\n{}",l.source,l.target)};
        let index=if vtt{String::new()}else{format!("{}\r\n",i+1)};format!("{index}{} --> {}\r\n{text}\r\n\r\n",srt_time(l.start,sep),srt_time(end,sep))}).collect();
    if vtt{format!("WEBVTT\r\n\r\n{cues}")}else{cues}
}
struct State{
    cfg:Config,path:PathBuf,home:PathBuf,devices:Vec<AudioDevice>,apps:Vec<String>,asr_devices:Vec<String>,fonts:Vec<String>,
    asr_langs:Vec<Language>,tr_langs:Vec<Language>,catalog:Vec<Artifact>,
    preview:bool,lock_hint:Option<Instant>,bar_hint:Option<Instant>,toast:Option<(Instant,String)>,live_text:String,reload_pending:bool,hardware:Option<hardware::Hardware>,recommendation:Option<hardware::Recommendation>,
    download_cancel:Arc<AtomicBool>,last_job:Option<Job>,auto_use:Option<String>,geometry_dirty:Option<Instant>,hittest:Option<bool>,placed:bool,transcript:VecDeque<Line>,current:Line,shown_history:Vec<(String,String)>,
    begin:Instant,log_file:Option<PathBuf>,log_version:u64,log_shown:Option<(u64,String)>,scroll:usize,scroll_at:Instant,last_sound:Instant,voice_since:Option<Instant>,
    last_metrics:Instant,last_slow:Instant,last_screen_check:Instant,tooltip:String,
}
struct Ctx{overlay:Overlay,settings:Settings,engine:Engine,st:RefCell<State>,gpu:Option<metrics::Gpu>,tray:RefCell<Option<Tray>>}
fn color(hex:&str)->slint::Color{let [r,g,b]=sonivra::config::parse_color(hex).unwrap_or([255,255,255]);slint::Color::from_rgb_u8(r,g,b)}
fn strings(v:impl IntoIterator<Item=String>)->ModelRc<slint::SharedString>{Rc::new(VecModel::from(v.into_iter().map(Into::into).collect::<Vec<_>>())).into()}
fn lang(c:&Ctx)->i32{i18n::index(&c.st.borrow().cfg.ui_language).unwrap_or(0)}
fn file_name(path:&Path)->String{path.file_name().map_or_else(String::new,|n|n.to_string_lossy().into())}
fn gb(bytes:u64)->String{if bytes>=1_000_000_000{format!("{:.2} GB",bytes as f64/1e9)}else{format!("{:.0} MB",bytes as f64/1e6)}}
fn fail(c:&Ctx,key:&str,detail:impl Into<String>){c.engine.snapshot.lock().unwrap().fail(key,detail.into());}
/// Finished lines plus the one on screen, oldest first.
fn all_lines(st:&State)->impl Iterator<Item=&Line>{st.transcript.iter().chain(std::iter::once(&st.current)).filter(|l|!l.source.is_empty())}
fn transcripts_dir(c:&Ctx)->PathBuf{c.st.borrow().home.join("transcripts")}
/// Stores a finished line, trims to the retention limit and appends it to the session file when enabled.
fn keep(c:&Ctx,line:Line){
    let mut st=c.st.borrow_mut();let save=st.cfg.transcript.save_files;let text=txt(&line);
    st.transcript.push_back(line);while st.transcript.len()>st.cfg.transcript.keep_lines as usize{st.transcript.pop_front();}
    if st.scroll>0{st.scroll=(st.scroll+1).min(st.transcript.len());}
    drop(st);if !save{return;}
    let path=c.st.borrow().log_file.clone().unwrap_or_else(||{let t=now_local();transcripts_dir(c).join(format!("sonivra-{:04}{:02}{:02}-{:02}{:02}{:02}.txt",t.wYear,t.wMonth,t.wDay,t.wHour,t.wMinute,t.wSecond))});
    let written=std::fs::create_dir_all(transcripts_dir(c)).and_then(|_|{use std::io::Write;std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(text.as_bytes())});
    match written{Ok(())=>c.st.borrow_mut().log_file=Some(path),Err(e)=>{c.st.borrow_mut().cfg.transcript.save_files=false;c.settings.set_save_files(false);fail(c,"file",format!("{}: {e}",path.display()));}}
}
/// Moves the subtitle window through earlier lines. Positive goes back in time, 0 is live.
fn scroll(c:&Ctx,delta:i64){let mut st=c.st.borrow_mut();let max=st.transcript.len() as i64;st.scroll=(st.scroll as i64+delta).clamp(0,max) as usize;st.scroll_at=Instant::now();}
fn confirm(lang:i32,text:&str)->bool{rfd::MessageDialog::new().set_title(t(lang,"dialog.title")).set_description(text).set_level(rfd::MessageLevel::Info).set_buttons(rfd::MessageButtons::YesNo).show()==rfd::MessageDialogResult::Yes}
/// Friendly model label: "Qwen3 ASR 1.7B · Q4_K".
fn model_name(file:&str)->String{let stem=file.trim_end_matches(".gguf");match stem.rsplit_once('-'){Some((name,quant)) if quant.starts_with('Q')=>format!("{} · {quant}",name.replace('-'," ")),_=>stem.replace('-'," ")}}
fn purpose_key(file:&str)->&'static str{let f=file.to_lowercase();if f.starts_with("qwen3-asr"){"purpose.qwen_asr"}else if f.contains("0.6b-v3"){"purpose.eu_asr"}else if f.starts_with("haas")||f.starts_with("reazon")||f.starts_with("parakeet"){"purpose.ja_asr"}else if f.starts_with("plamo"){"purpose.plamo"}else{"purpose.hymt"}}
fn needs_setup(cfg:&Config)->bool{!cfg.asr.runtime.is_file()||!cfg.asr.model.is_file()||(cfg.translation.enabled&&cfg.translation.local&&(!cfg.translation.runtime.is_file()||!cfg.translation.model.is_file()))}
fn runtime_family(cfg:&Config)->i32{let p=cfg.translation.runtime.to_string_lossy();if p.contains("llama-vulkan"){1}else if p.contains("llama-cpu"){2}else{0}}
fn runtime_size(variant:&str)->u64{models::runtimes().iter().filter(|a|a.architecture=="speech"||a.architecture==variant).map(|a|a.size).sum()}

/// Program list for "capture only this program", rebuilt when the language or the running programs change.
fn set_apps(c:&Ctx){
    let s=&c.settings;let st=c.st.borrow();let cfg=&st.cfg;
    let mut apps=vec![t(lang(c),"audio.whole_device")];apps.extend(st.apps.iter().cloned());
    if !cfg.audio.process.is_empty()&&!st.apps.iter().any(|a|a.eq_ignore_ascii_case(&cfg.audio.process)){apps.push(cfg.audio.process.clone());}
    s.set_app_index(if cfg.audio.process.is_empty(){0}else{apps.iter().position(|a|a.eq_ignore_ascii_case(&cfg.audio.process)).unwrap_or(0) as i32});
    s.set_app_names(strings(apps));
}
fn set_lang(c:&Ctx){let l=lang(c);c.overlay.global::<I18n>().set_lang(l);c.settings.global::<I18n>().set_lang(l);
    let st=c.st.borrow();
    c.settings.set_runtime_names(strings(RUNTIME_VARIANTS.iter().zip(["runtime.cuda","runtime.vulkan","runtime.cpu"]).map(|(v,k)|tf(l,k,&[&gb(runtime_size(v))]))));
    let mut audio=vec![t(l,"audio.default_output"),t(l,"audio.default_mic")];audio.extend(st.devices.iter().map(|d|tf(l,if d.microphone{"audio.mic_named"}else{"audio.output_named"},&[&d.name])));c.settings.set_audio_names(strings(audio));set_apps(c);
    c.settings.set_hotkey_text(format!("{}\n{}",t(l,"hotkey.title"),tray::HOTKEYS.iter().map(|(k,_,_,key)|format!("{k}  {}",t(l,key))).collect::<Vec<_>>().join("\n")).into());
    c.settings.set_keep_names(strings(KEEP_CHOICES.iter().map(|n|tf(l,"log.lines",&[n]))));
    drop(st);refresh_hardware(c);
}
fn load(c:&Ctx){
    let s=&c.settings;let st=c.st.borrow();let cfg=&st.cfg;
    s.set_language(lang(c));
    let sub=&cfg.subtitles;s.set_overlay_visible(sub.enabled);s.set_overlay_locked(sub.locked);s.set_click_through(sub.click_through);s.set_preview(st.preview);
    s.set_font_names(strings(st.fonts.clone()));s.set_source_font_index(st.fonts.iter().position(|f|*f==sub.source_font).unwrap_or(0) as i32);s.set_target_font_index(st.fonts.iter().position(|f|*f==sub.font).unwrap_or(0) as i32);
    s.set_font_size(sub.font_size as f32);s.set_line_gap(sub.spacing as f32);s.set_bg_opacity(sub.opacity as f32);s.set_hold_s(sub.hold_ms as f32/1000.);s.set_effect(sub.effect as i32);s.set_show_mode(if !sub.show_source{1}else if !sub.show_translation{2}else{0});s.set_history_lines(sub.history.min(4) as i32);s.set_history_mode(sub.history_mode as i32);s.set_interim(sub.interim);s.set_source_color(sub.source_color.as_str().into());s.set_target_color(sub.target_color.as_str().into());s.set_panel_color_text(sub.background_color.as_str().into());s.set_source_scale(sub.source_scale as f32);s.set_align(sub.align as i32);s.set_outline(sub.outline as f32);s.set_source_bold(sub.source_bold);s.set_target_bold(sub.target_bold);s.set_fit_text(sub.fit_text);s.set_radius(sub.radius as f32);s.set_translation_first(sub.translation_first);s.set_bar_mode(sub.bar as i32);s.set_anchor_top(sub.anchor_top);s.set_text_opacity(sub.text_opacity as f32);s.set_outline_color(sub.outline_color.as_str().into());s.set_show_status(sub.show_status);
    s.set_keep_index(KEEP_CHOICES.iter().position(|n|*n==cfg.transcript.keep_lines).unwrap_or(1) as i32);s.set_save_files(cfg.transcript.save_files);s.set_mix_mic(cfg.audio.mix_mic);s.set_record_audio(cfg.audio.record);s.set_live_file(cfg.transcript.live_file);s.set_autostart(cfg.autostart);
    set_apps(c);
    let a=&cfg.audio;s.set_audio_index(if a.device.is_empty(){i32::from(a.microphone)}else{st.devices.iter().position(|d|d.id==a.device).map_or(0,|i|i as i32+2)});
    s.set_sensitivity(sensitivity(a.threshold).round());s.set_silence_ms(a.silence_ms as f32);s.set_min_speech_ms(a.min_speech_ms as f32);s.set_max_segment_s(a.max_segment_ms as f32/1000.);
    s.set_asr_lang_names(strings(st.asr_langs.iter().map(languages::display)));s.set_asr_lang_index(st.asr_langs.iter().position(|l|l.code==cfg.asr.language).unwrap_or(1) as i32);
    s.set_asr_device_names(strings(st.asr_devices.clone()));s.set_asr_device_index(st.asr_devices.iter().position(|d|d.split_whitespace().next()==Some(cfg.asr.device.as_str())).unwrap_or(0) as i32);
    s.set_asr_prompt(cfg.asr.prompt.as_str().into());s.set_asr_threads(cfg.asr.threads as i32);
    let tr=&cfg.translation;s.set_translate_enabled(tr.enabled);s.set_translation_lang_names(strings(st.tr_langs.iter().map(languages::display)));
    s.set_source_lang_index(st.tr_langs.iter().position(|l|l.code==tr.source_lang).unwrap_or(0) as i32);s.set_target_lang_index(st.tr_langs.iter().position(|l|l.code==tr.target_lang).unwrap_or(0) as i32);
    s.set_translation_family(match tr.family.as_str(){"plamo3"=>0,"hy-mt2"=>1,_=>2});s.set_translation_context(tr.context.as_str().into());s.set_glossary(tr.glossary.as_str().into());
    s.set_local_translation(tr.local);s.set_refine(tr.refine);s.set_interim_translate(tr.interim);s.set_endpoint(tr.endpoint.as_str().into());s.set_model_id(tr.model_id.as_str().into());s.set_gpu_layers(tr.gpu_layers);s.set_context_size(tr.context_size as i32);s.set_max_tokens(tr.max_tokens as i32);s.set_temperature(tr.temperature);s.set_debounce_ms(tr.debounce_ms as i32);s.set_history_turns(tr.history_turns as i32);s.set_mmap(tr.mmap);
    s.set_asr_runtime(cfg.asr.runtime.to_string_lossy().as_ref().into());s.set_translation_runtime(tr.runtime.to_string_lossy().as_ref().into());
    s.set_asr_path(cfg.asr.model.to_string_lossy().as_ref().into());s.set_translation_path(tr.model.to_string_lossy().as_ref().into());
    s.set_gpu_reserve_mb(cfg.gpu_reserve_mb as i32);s.set_runtime_family(runtime_family(cfg));
}
fn collect(c:&Ctx)->Result<Config>{
    let s=&c.settings;let st=c.st.borrow();let mut cfg=st.cfg.clone();
    cfg.ui_language=i18n::LANGS[s.get_language().clamp(0,2) as usize].into();cfg.english=s.get_language()==1;cfg.gpu_reserve_mb=s.get_gpu_reserve_mb().max(0) as u32;
    {let i=s.get_app_index();let names=s.get_app_names();cfg.audio.process=if i<=0{String::new()}else{{use slint::Model;names.row_data(i as usize).map(|n|n.to_string()).unwrap_or_default()}};}
    let i=s.get_audio_index();if i<2{cfg.audio.device.clear();cfg.audio.microphone=i==1;}else if let Some(d)=st.devices.get(i as usize-2){cfg.audio.device=d.id.clone();cfg.audio.microphone=d.microphone;}
    if (s.get_sensitivity()-sensitivity(cfg.audio.threshold).round()).abs()>0.01{cfg.audio.threshold=threshold(s.get_sensitivity());}
    cfg.audio.silence_ms=s.get_silence_ms().round() as u32;cfg.audio.min_speech_ms=s.get_min_speech_ms().round() as u32;cfg.audio.max_segment_ms=(s.get_max_segment_s().round()*1000.) as u32;
    if let Some(l)=st.asr_langs.get(s.get_asr_lang_index() as usize){cfg.asr.language=l.code.clone();}
    if let Some(d)=st.asr_devices.get(s.get_asr_device_index() as usize).and_then(|d|d.split_whitespace().next()){cfg.asr.device=d.into();}
    cfg.asr.prompt=s.get_asr_prompt().to_string();cfg.asr.threads=s.get_asr_threads() as u32;cfg.asr.runtime=s.get_asr_runtime().as_str().into();
    let tr=&mut cfg.translation;tr.enabled=s.get_translate_enabled();
    if let Some(l)=st.tr_langs.get(s.get_source_lang_index() as usize){tr.source_lang=l.code.clone();}
    if let Some(l)=st.tr_langs.get(s.get_target_lang_index() as usize){tr.target_lang=l.code.clone();}
    tr.family=["plamo3","hy-mt2","chat"][s.get_translation_family().clamp(0,2) as usize].into();tr.context=s.get_translation_context().to_string();tr.glossary=s.get_glossary().to_string();
    tr.local=s.get_local_translation();tr.endpoint=s.get_endpoint().to_string();tr.model_id=s.get_model_id().to_string();tr.gpu_layers=s.get_gpu_layers();tr.context_size=s.get_context_size() as u32;tr.max_tokens=s.get_max_tokens() as u32;tr.temperature=(s.get_temperature()*10.).round()/10.;tr.debounce_ms=s.get_debounce_ms() as u32;tr.history_turns=s.get_history_turns() as usize;tr.refine=s.get_refine();tr.interim=s.get_interim_translate();tr.mmap=s.get_mmap();tr.runtime=s.get_translation_runtime().as_str().into();
    let sub=&mut cfg.subtitles;sub.enabled=s.get_overlay_visible();sub.locked=s.get_overlay_locked();sub.click_through=s.get_click_through();
    if let Some(f)=st.fonts.get(s.get_source_font_index() as usize){sub.source_font=f.clone();}if let Some(f)=st.fonts.get(s.get_target_font_index() as usize){sub.font=f.clone();}
    sub.font_size=s.get_font_size().round() as u32;sub.spacing=s.get_line_gap().round() as u32;sub.opacity=s.get_bg_opacity().round() as u32;sub.hold_ms=(s.get_hold_s().round()*1000.) as u32;sub.effect=s.get_effect().clamp(0,3) as u32;
    let mode=s.get_show_mode();sub.show_source=mode!=1;sub.show_translation=mode!=2;sub.history=match s.get_history_lines(){4=>sonivra::config::FILL_HISTORY,n=>n.clamp(0,3) as u32};sub.interim=s.get_interim();sub.history_mode=s.get_history_mode().clamp(0,2) as u32;sub.translation_first=s.get_translation_first();
    // A colour field being typed stays at the last valid value.
    for (slot,text) in [(&mut sub.source_color,s.get_source_color()),(&mut sub.target_color,s.get_target_color()),(&mut sub.background_color,s.get_panel_color_text()),(&mut sub.outline_color,s.get_outline_color())]{if sonivra::config::parse_color(&text).is_some(){*slot=text.to_uppercase();}}
    sub.source_scale=s.get_source_scale().round() as u32;sub.align=s.get_align().clamp(0,2) as u32;sub.outline=s.get_outline().round().clamp(1.,4.) as u32;sub.source_bold=s.get_source_bold();sub.target_bold=s.get_target_bold();sub.fit_text=s.get_fit_text();sub.radius=s.get_radius().round() as u32;sub.bar=s.get_bar_mode().clamp(0,2) as u32;sub.anchor_top=s.get_anchor_top();sub.text_opacity=s.get_text_opacity().round().clamp(20.,100.) as u32;sub.show_status=s.get_show_status();
    cfg.transcript.keep_lines=KEEP_CHOICES[s.get_keep_index().clamp(0,3) as usize];cfg.transcript.save_files=s.get_save_files();cfg.audio.mix_mic=s.get_mix_mic();cfg.audio.record=s.get_record_audio();cfg.transcript.live_file=s.get_live_file();cfg.autostart=s.get_autostart();
    cfg.validate()?;Ok(cfg)
}
/// Saves a new configuration and applies only what changed: look and VAD immediately, model settings on reload.
fn commit_with(c:&Ctx,new:Config){
    let old=c.st.borrow().cfg.clone();
    if new==old{return;}
    if let Err(e)=new.save(&c.st.borrow().path){fail(c,"settings",format!("{e:#}"));return;}
    let preview=c.settings.get_preview();
    {let mut st=c.st.borrow_mut();st.cfg=new.clone();if st.preview!=preview{st.preview=preview;}if new.subtitles.locked&&!old.subtitles.locked{st.lock_hint=Some(Instant::now());}if new.subtitles.bar==1&&old.subtitles.bar==0{st.bar_hint=Some(Instant::now());}}
    if new.ui_language!=old.ui_language{set_lang(c);}
    let (running,loading)={let s=c.engine.snapshot.lock().unwrap();(s.running,s.loading)};
    if running&&new.audio!=old.audio{let mut audio=new.audio.clone();audio.record_dir=transcripts_dir(c);if let Err(e)=c.engine.send(Control::Vad(audio)){fail(c,"settings",format!("{e:#}"));}}
    if (running||loading)&&(new.asr!=old.asr||new.translation!=old.translation){c.st.borrow_mut().reload_pending=true;}
    if new.gpu_reserve_mb!=old.gpu_reserve_mb{refresh_hardware(c);}
    if new.transcript.keep_lines<old.transcript.keep_lines{let mut st=c.st.borrow_mut();while st.transcript.len()>new.transcript.keep_lines as usize{st.transcript.pop_front();}st.scroll=st.scroll.min(st.transcript.len());st.log_version+=1;}
    apply_overlay(c);
}
fn commit(c:&Ctx){
    {let preview=c.settings.get_preview();c.st.borrow_mut().preview=preview;}
    match collect(c){Ok(cfg)=>commit_with(c,cfg),Err(e)=>fail(c,"settings",format!("{e:#}"))}
    apply_overlay(c);
}
fn edit(c:&Ctx,change:impl FnOnce(&mut Config)){let mut cfg=c.st.borrow().cfg.clone();change(&mut cfg);if let Err(e)=cfg.validate(){fail(c,"settings",format!("{e:#}"));return;}commit_with(c,cfg);load(c);}

fn apply_overlay(c:&Ctx){
    let o=&c.overlay;let st=c.st.borrow();let s=&st.cfg.subtitles;
    o.set_source_face(s.source_font.as_str().into());o.set_target_face(s.font.as_str().into());o.set_size(s.font_size as i32);o.set_gap(s.spacing as i32);o.set_effect(s.effect as i32);o.set_bg_opacity(s.opacity as f32/100.);
    o.set_show_source(s.show_source);
    let (st_,tt,pc)=(color(&s.source_color),color(&s.target_color),color(&s.background_color));
    o.set_source_tint(st_);o.set_target_tint(tt);o.set_panel_color(pc);c.settings.set_source_tint(st_);c.settings.set_target_tint(tt);c.settings.set_panel_color(pc);
    o.set_source_scale(s.source_scale as f32/100.);o.set_align(s.align as i32);o.set_outline(s.outline as f32);o.set_source_bold(s.source_bold);o.set_target_bold(s.target_bold);o.set_fit(s.fit_text);o.set_radius(s.radius as f32);o.set_history_mode(s.history_mode as i32);o.set_translation_first(s.translation_first);o.set_bar_mode(s.bar as i32);o.set_anchor_top(s.anchor_top);o.set_show_status(s.show_status);o.set_text_alpha(s.text_opacity as f32/100.);let ec=color(&s.outline_color);o.set_edge_color(ec);c.settings.set_edge_color(ec);o.set_show_target(s.show_translation&&st.cfg.translation.enabled);o.set_locked(s.locked);o.set_pass_through(s.locked&&s.click_through);o.set_preview(st.preview);
    let want=s.enabled||st.preview;let pass=s.locked&&s.click_through;drop(st);
    if want&&!o.window().is_visible(){if let Err(e)=o.show(){fail(c,"overlay",format!("{e:#}"));return;}c.st.borrow_mut().hittest=None;ensure_on_screen(c,false);}
    if !want&&o.window().is_visible(){if let Err(e)=o.hide(){fail(c,"overlay",format!("{e:#}"));}return;}
    if want{sync_hittest(c,pass);}
    menu_labels(c);
}
/// Tray menu text, refreshed when the language, lock state or phase changes.
fn menu_labels(c:&Ctx){
    let (running,loading)={let s=c.engine.snapshot.lock().unwrap();(s.running,s.loading)};let paused=c.engine.paused.load(Ordering::Relaxed);
    let l=lang(c);let st=c.st.borrow();let want=st.cfg.subtitles.enabled||st.preview;
    tray::set_menu(tray::MenuLabels{transport:t(l,if !running&&!loading{"action.start"}else if paused{"action.resume"}else{"action.pause"}),lock:t(l,"overlay.lock"),show:t(l,"overlay.show"),settings:t(l,"action.open_settings"),reset:t(l,"action.reset_position"),quit:t(l,"action.quit"),locked:st.cfg.subtitles.locked,visible:want});
}
/// Click-through. The winit window only exists once the event loop runs, so the timer retries until it does.
fn sync_hittest(c:&Ctx,pass:bool){
    if c.st.borrow().hittest==Some(!pass){return;}
    match c.overlay.window().with_winit_window(|w|w.set_cursor_hittest(!pass).map_err(|e|e.to_string())){Some(Ok(()))=>c.st.borrow_mut().hittest=Some(!pass),Some(Err(e))=>{c.st.borrow_mut().hittest=Some(!pass);fail(c,"overlay",e)},None=>{}}
}
/// Moves the subtitle window to the primary monitor when it is mostly off every monitor (unplugged screen, old coordinates).
fn ensure_on_screen(c:&Ctx,force:bool)->bool{
    let w=c.overlay.window();let pos=w.position();let size=w.size();
    let Some(target)=w.with_winit_window(|win|{
        let monitors:Vec<_>=win.available_monitors().collect();
        let visible=|m:&slint::winit_030::winit::monitor::MonitorHandle|{let p=m.position();let s=m.size();let dx=(pos.x+size.width as i32).min(p.x+s.width as i32)-pos.x.max(p.x);let dy=(pos.y+size.height as i32).min(p.y+s.height as i32)-pos.y.max(p.y);dx>=96&&dy>=48};
        if !force&&c.st.borrow().cfg.subtitles.x!=i32::MIN&&monitors.iter().any(visible){return None;}
        let m=win.primary_monitor().or_else(||monitors.first().cloned())?;let p=m.position();let s=m.size();
        Some((p.x+(s.width as i32-size.width as i32)/2,p.y+s.height as i32*86/100-size.height as i32))
    })else{return false};
    if let Some((x,y))=target{w.set_position(slint::PhysicalPosition::new(x,y));}
    true
}
/// Docks the subtitle window to the top or bottom edge of the monitor it is on.
fn snap(c:&Ctx,top:bool){
    let w=c.overlay.window();let size=w.size();
    let target=w.with_winit_window(|win|{let m=win.current_monitor().or_else(||win.primary_monitor())?;let p=m.position();let s=m.size();
        Some((p.x+(s.width as i32-size.width as i32)/2,if top{p.y+s.height as i32*3/100}else{p.y+s.height as i32*86/100-size.height as i32}))}).flatten();
    match target{Some((x,y))=>w.set_position(slint::PhysicalPosition::new(x,y)),None=>fail(c,"overlay","no monitor found")}
}
/// Exports every kept line. kind 0 is plain text with clock times, 1 SRT and 2 WebVTT timed from app start.
fn save_transcript(c:&Ctx,kind:i32){
    let l=lang(c);let (name,filter,ext)=match kind{1=>("sonivra-transcript.srt","dialog.srt","srt"),2=>("sonivra-transcript.vtt","dialog.vtt","vtt"),_=>("sonivra-transcript.txt","dialog.text","txt")};
    let Some(path)=rfd::FileDialog::new().set_title(t(l,"dialog.save_transcript")).set_file_name(name).add_filter(t(l,filter),&[ext]).save_file()else{return};
    let text={let mut st=c.st.borrow_mut();st.current.end=st.begin.elapsed();let st=&*st;if kind>0{srt(all_lines(st),kind==2)}else{all_lines(st).map(txt).collect::<String>()}};
    if let Err(e)=std::fs::write(&path,format!("\u{feff}{text}")){fail(c,"file",format!("{}: {e}",path.display()));}
}
/// Writes the whole kept transcript to a new file in `transcripts` without a dialog, for the button and the hotkey.
fn quick_save(c:&Ctx){
    let l=lang(c);let t0=now_local();
    let text={let mut st=c.st.borrow_mut();st.current.end=st.begin.elapsed();all_lines(&st).map(txt).collect::<String>()};
    if text.is_empty(){c.st.borrow_mut().toast=Some((Instant::now(),t(l,"toast.nothing_to_save")));return;}
    let path=transcripts_dir(c).join(format!("sonivra-saved-{:04}{:02}{:02}-{:02}{:02}{:02}.txt",t0.wYear,t0.wMonth,t0.wDay,t0.wHour,t0.wMinute,t0.wSecond));
    match std::fs::create_dir_all(transcripts_dir(c)).and_then(|_|std::fs::write(&path,format!("\u{feff}{text}"))){
        Ok(())=>c.st.borrow_mut().toast=Some((Instant::now(),tf(l,"toast.saved",&[&path.display()]))),
        Err(e)=>fail(c,"file",format!("{}: {e}",path.display())),
    }
}
/// A look-and-context preset a user can keep per programme and share as a file.
#[derive(serde::Serialize,serde::Deserialize)]
struct Profile{subtitles:sonivra::config::Subtitles,context:String,glossary:String,prompt:String}
fn export_profile(c:&Ctx){
    let l=lang(c);let dir=c.st.borrow().home.join("profiles");let _=std::fs::create_dir_all(&dir);
    let Some(path)=rfd::FileDialog::new().set_title(t(l,"dialog.profile_save")).set_directory(&dir).set_file_name("profile.toml").add_filter(t(l,"dialog.profile"),&["toml"]).save_file()else{return};
    let p={let cfg=&c.st.borrow().cfg;Profile{subtitles:cfg.subtitles.clone(),context:cfg.translation.context.clone(),glossary:cfg.translation.glossary.clone(),prompt:cfg.asr.prompt.clone()}};
    if let Err(e)=toml::to_string_pretty(&p).map_err(|e|e.to_string()).and_then(|s|std::fs::write(&path,s).map_err(|e|e.to_string())){fail(c,"file",format!("{}: {e}",path.display()));}
}
fn import_profile(c:&Ctx){
    let l=lang(c);let dir=c.st.borrow().home.join("profiles");let _=std::fs::create_dir_all(&dir);
    let Some(path)=rfd::FileDialog::new().set_title(t(l,"dialog.profile_open")).set_directory(&dir).add_filter(t(l,"dialog.profile"),&["toml"]).pick_file()else{return};
    let p:Profile=match std::fs::read_to_string(&path).map_err(|e|e.to_string()).and_then(|s|toml::from_str(&s).map_err(|e|e.to_string())){Ok(p)=>p,Err(e)=>{fail(c,"file",format!("{}: {e}",path.display()));return}};
    // Window position, size and visibility belong to this screen, not to the preset.
    edit(c,|cfg|{let k=cfg.subtitles.clone();cfg.subtitles=p.subtitles;let s=&mut cfg.subtitles;s.enabled=k.enabled;s.locked=k.locked;s.click_through=k.click_through;s.width=k.width;s.height=k.height;s.x=k.x;s.y=k.y;cfg.translation.context=p.context;cfg.translation.glossary=p.glossary;cfg.asr.prompt=p.prompt;});
}
fn clear_transcript(c:&Ctx){
    let l=lang(c);let n=all_lines(&c.st.borrow()).count();if n==0||!confirm(l,&tf(l,"confirm.clear_log",&[&n])){return;}
    let mut st=c.st.borrow_mut();st.transcript.clear();st.current=Line::default();st.scroll=0;st.log_version+=1;
}
fn refresh_hardware(c:&Ctx){
    let l=lang(c);let mut st=c.st.borrow_mut();let preference=c.settings.get_preference();
    let Some(h)=st.hardware.clone() else{c.settings.set_hardware_text(t(l,"hw.unavailable").into());return};
    let vendor=|v:u32|match v{0x10de=>"NVIDIA",0x1002=>"AMD",0x8086=>"Intel",_=>""};
    let gpus:Vec<String>=h.adapters.iter().filter(|a|!a.software).map(|a|tf(l,"hw.gpu",&[&format!("{} {}",vendor(a.vendor),a.name).trim().replace("NVIDIA NVIDIA","NVIDIA").replace("AMD AMD","AMD").replace("Intel Intel","Intel"),&format!("{:.1}",a.dedicated_bytes as f64/1073741824.)])).collect();
    c.settings.set_hardware_text(format!("{}\n{}",if gpus.is_empty(){t(l,"hw.no_gpu")}else{gpus.join("\n")},tf(l,"hw.ram",&[&format!("{:.1}",h.ram_total as f64/1073741824.),&format!("{:.1}",h.ram_available as f64/1073741824.)])).into());
    let r=hardware::recommend(&h,st.cfg.gpu_reserve_mb,preference);
    c.settings.set_recommendation(format!("{}\n{}\n{}",tf(l,"rec.pair",&[&model_name(&r.asr_file),&model_name(&r.translation_file)]),tf(l,"rec.estimate",&[&r.estimated_model_mib,&r.available_budget_mib]),if r.gpu_layers>0{t(l,"rec.capacity_only")}else{t(l,"rec.cpu")}).into());
    st.recommendation=Some(r);
}

fn start(c:&Ctx){
    let cfg=c.st.borrow().cfg.clone();
    if needs_setup(&cfg){c.settings.set_page(0);show_settings(c);return;}
    {let mut st=c.st.borrow_mut();st.reload_pending=false;st.preview=false;}c.settings.set_preview(false);apply_overlay(c);
    let mut cfg=cfg;cfg.audio.record_dir=transcripts_dir(c);
    if let Err(e)=c.engine.send(Control::Start(cfg)){fail(c,"asr_load",format!("{e:#}"));}
}
fn show_settings(c:&Ctx){if let Err(e)=c.settings.show(){fail(c,"overlay",format!("{e:#}"));}c.settings.window().with_winit_window(|w|{w.set_minimized(false);w.focus_window();});}
fn toggle_lock(c:&Ctx){edit(c,|cfg|{cfg.subtitles.locked=!cfg.subtitles.locked;cfg.subtitles.enabled=true;});}
fn quit(c:&Ctx){save_geometry(c);let _=slint::quit_event_loop();}
fn save_geometry(c:&Ctx){let st=c.st.borrow();if let Err(e)=st.cfg.save(&st.path){drop(st);fail(c,"settings",format!("{e:#}"));}}

fn download(c:&Ctx,job:Job,ask:bool){
    let l=lang(c);let st=c.st.borrow();if c.engine.snapshot.lock().unwrap().download_busy{return;}
    let (assets,label):(Vec<Artifact>,String)=match &job{
        Job::Model(i)=>{let Some(m)=st.catalog.get(*i)else{return};(vec![m.clone()],m.file.clone())},
        Job::Runtimes(v)=>(models::runtimes().into_iter().filter(|a|a.architecture=="speech"||a.architecture==*v).collect(),"runtimes".into()),
    };
    if ask{
        let text=match &job{
            Job::Model(_)=>{let m=&assets[0];tf(l,"confirm.model",&[&model_name(&m.file),&gb(m.size),&format!("huggingface.co/{} @ {}",m.id,&m.revision[..8]),&m.license,&t(l,purpose_key(&m.file))])},
            Job::Runtimes(_)=>tf(l,"confirm.runtimes",&[&gb(assets.iter().map(|a|a.size).sum()),&assets.iter().map(|a|format!("{} {} ({})",a.id,a.revision,a.license)).collect::<Vec<_>>().join("\n")]),
        };
        drop(st);if !confirm(l,&text){return;}
    }else{drop(st);}
    let mut st=c.st.borrow_mut();st.last_job=Some(job.clone());st.download_cancel.store(false,Ordering::Release);
    let cancel=st.download_cancel.clone();let home=st.home.clone();let state=c.engine.snapshot.clone();
    {let mut s=state.lock().unwrap();s.download_busy=true;s.download_file=label.clone();s.downloaded=0;s.download_total=assets.iter().map(|a|a.size).sum();if s.error_key=="download"{s.clear_error();}}
    std::thread::spawn(move||{
        let progress=|done,total|{let mut s=state.lock().unwrap();s.downloaded=done;s.download_total=total;};
        let result=match &job{Job::Model(_)=>models::download(&assets[0],&home.join("models"),&cancel,progress).map(|_|()),Job::Runtimes(v)=>models::install_runtimes(&home,v,&cancel,progress)};
        let mut s=state.lock().unwrap();s.download_busy=false;
        match result{Ok(())=>s.download_done=Some(label),Err(e)=>{if !cancel.load(Ordering::Relaxed){s.fail("download",format!("{e:#}"));}}}
    });
}
fn finish_download(c:&Ctx,done:String){
    let job=c.st.borrow().last_job.clone();
    match job{
        Some(Job::Runtimes(v))=>{let home=c.st.borrow().home.clone();edit(c,|cfg|{cfg.asr.runtime=home.join("runtime/speech/speech.exe");cfg.translation.runtime=home.join("runtime").join(&v).join("llama-server.exe");});refresh_asr_devices(c);load(c);},
        Some(Job::Model(i))=>{let auto=c.st.borrow_mut().auto_use.take();if auto.as_deref()==Some(done.as_str()){use_model(c,i);}},
        None=>{}
    }
}
fn use_model(c:&Ctx,i:usize){
    let Some(m)=c.st.borrow().catalog.get(i).cloned()else{return};let path=c.st.borrow().home.join("models").join(&m.file);
    if !path.is_file(){download(c,Job::Model(i),true);c.st.borrow_mut().auto_use=Some(m.file);return;}
    edit(c,|cfg|if m.kind=="asr"{cfg.asr.model=path;}else{cfg.translation.model=path;cfg.translation.local=true;cfg.translation.family=match m.architecture.as_str(){"plamo3"=>"plamo3","hy-mt2"=>"hy-mt2",_=>"chat"}.into();});
}
fn choose_model(c:&Ctx,kind:i32){
    let l=lang(c);let Some(file)=rfd::FileDialog::new().set_title(t(l,"dialog.choose_model")).add_filter(t(l,"dialog.gguf"),&["gguf"]).pick_file()else{return};
    if let Err(e)=models::validate_gguf(&file){fail(c,"file",format!("{}: {e:#}",file.display()));return;}
    let lower=file_name(&file).to_lowercase();
    edit(c,|cfg|if kind==0{cfg.asr.model=file;}else{cfg.translation.model=file;cfg.translation.local=true;cfg.translation.family=if lower.contains("plamo"){"plamo3"}else if lower.contains("hy-mt"){"hy-mt2"}else{"chat"}.into();});
}
fn refresh_asr_devices(c:&Ctx){
    let runtime=c.st.borrow().cfg.asr.runtime.clone();let mut found=Vec::new();
    if runtime.is_file(){use std::os::windows::process::CommandExt;if let Ok(output)=std::process::Command::new(&runtime).arg("devices").creation_flags(0x08000000).output(){if output.status.success(){found=String::from_utf8_lossy(&output.stdout).lines().filter(|l|{let code=l.split_whitespace().next().unwrap_or("");code.starts_with("Vulkan")||code=="CPU"}).map(|l|l.trim().to_string()).collect();}}}
    if found.is_empty(){found=vec!["Vulkan0".into(),"CPU".into()];}
    c.st.borrow_mut().asr_devices=found;
}
fn refresh_audio(c:&Ctx){c.st.borrow_mut().apps=audio::app_names();match audio::devices(){Ok(found)=>{c.st.borrow_mut().devices=found;set_lang(c);load(c);},Err(e)=>fail(c,"audio",format!("{e:#}"))}}

fn model_rows(c:&Ctx,snap:&Snapshot)->Vec<ModelRow>{
    let l=lang(c);let st=c.st.borrow();let rec=st.recommendation.as_ref();
    st.catalog.iter().map(|m|{
        let path=st.home.join("models").join(&m.file);let size=std::fs::metadata(&path).map_or(0,|x|x.len());
        let partial=std::fs::metadata(st.home.join("models").join(format!("{}.part",m.file))).map_or(0,|x|x.len());
        let selected=file_name(if m.kind=="asr"{&st.cfg.asr.model}else{&st.cfg.translation.model})==m.file;
        let downloading=snap.download_busy&&snap.download_file==m.file;let failed=!snap.download_busy&&snap.error_key=="download"&&snap.download_file==m.file;
        let installed=size==m.size;let loaded=snap.loaded.contains(&m.file);let verified=st.cfg.verified_models.contains(&m.file);
        let (kind,state)=if downloading{(1,tf(l,"model.downloading",&[&format!("{:.0}",snap.downloaded as f64*100./m.size.max(1) as f64)]))}
            else if failed{(5,t(l,"model.failed"))}
            else if loaded{(4,t(l,"model.loaded"))}
            else if installed&&selected{(3,t(l,if verified{"model.selected_verified"}else{"model.selected"}))}
            else if installed{(2,t(l,if verified{"model.downloaded_verified"}else{"model.downloaded"}))}
            else if partial>0{(0,tf(l,"model.partial",&[&format!("{:.0}",partial as f64*100./m.size as f64)]))}
            else{(0,t(l,"model.not_downloaded"))};
        ModelRow{name:model_name(&m.file).into(),purpose:t(l,purpose_key(&m.file)).into(),size:gb(m.size).into(),license:m.license.as_str().into(),state:state.into(),state_kind:kind,
            progress:if downloading{snap.downloaded as f32/m.size.max(1) as f32}else{0.},path:path.to_string_lossy().as_ref().into(),kind:i32::from(m.kind!="asr"),
            recommended:rec.is_some_and(|r|r.asr_file==m.file||r.translation_file==m.file),verified,can_download:!installed&&!downloading,can_use:!selected&&!downloading,can_remove:installed&&!loaded}
    }).collect()
}
fn phase_key(s:&Snapshot,paused:bool)->&'static str{match s.phase{Phase::Idle=>"phase.idle",Phase::LoadingAsr=>"phase.loading_asr",Phase::LoadingTranslation=>"phase.loading_translation",Phase::Listening=>if paused{"phase.paused"}else{"phase.listening"},Phase::Reconnecting=>"phase.reconnecting",Phase::Stopped=>"phase.stopped",Phase::Failed=>"phase.failed",Phase::Testing=>"phase.testing",Phase::TestDone=>"phase.test_done",Phase::TestFailed=>"phase.test_failed"}}

fn tick(c:&Ctx){
    let snap=c.engine.snapshot.lock().unwrap().clone();let l=lang(c);let paused=c.engine.paused.load(Ordering::Relaxed);
    if let Some(done)=c.engine.snapshot.lock().unwrap().download_done.take(){finish_download(c,done);}
    // Sentences translated again with the following sentence as context replace the earlier wording.
    {let revised:Vec<(String,String)>=c.engine.snapshot.lock().unwrap().revisions.drain(..).collect();
     if !revised.is_empty(){let mut st=c.st.borrow_mut();for (source,target) in revised{let hit=st.transcript.iter_mut().rev().find(|l|l.source==source).map(|l|{let same=l.target==target;l.target=target.clone();!same});if hit==Some(true){st.log_version+=1;}}}}
    // Remember models that actually loaded on this machine.
    if snap.running&&snap.loaded.iter().any(|f|!c.st.borrow().cfg.verified_models.contains(f)){let loaded=snap.loaded.clone();let mut cfg=c.st.borrow().cfg.clone();for f in loaded{if !cfg.verified_models.contains(&f){cfg.verified_models.push(f);}}commit_with(c,cfg);}
    for command in tray::take(){match command{
        tray::Command::ToggleLock=>toggle_lock(c),tray::Command::ToggleOverlay=>edit(c,|cfg|cfg.subtitles.enabled=!cfg.subtitles.enabled),tray::Command::OpenSettings=>show_settings(c),
        tray::Command::ResetPosition=>{edit(c,|cfg|{cfg.subtitles.enabled=true;cfg.subtitles.locked=false;});ensure_on_screen(c,true);},tray::Command::Quit=>quit(c),
        tray::Command::Pause=>if snap.running{let p=&c.engine.paused;p.store(!p.load(Ordering::Relaxed),Ordering::Relaxed);}else if !snap.loading{start(c);},
        tray::Command::Save=>quick_save(c),tray::Command::Back=>scroll(c,1),tray::Command::Forward=>scroll(c,-1),
        tray::Command::Bigger=>edit(c,|cfg|cfg.subtitles.font_size=(cfg.subtitles.font_size+2).min(72)),tray::Command::Smaller=>edit(c,|cfg|cfg.subtitles.font_size=cfg.subtitles.font_size.saturating_sub(2).max(12))}}
    if c.st.borrow().scroll>0&&c.st.borrow().scroll_at.elapsed()>SCROLL_IDLE{c.st.borrow_mut().scroll=0;}
    let st=c.st.borrow();let cfg=&st.cfg;let o=&c.overlay;
    let fresh=cfg.subtitles.hold_ms==0||snap.changed.is_some_and(|at|at.elapsed()<Duration::from_millis(cfg.subtitles.hold_ms as u64));
    drop(st);
    let done={let mut st=c.st.borrow_mut();
        if snap.source!=st.current.source{let now=st.begin.elapsed();let mut done=std::mem::replace(&mut st.current,Line{clock:clock(),start:now,end:now,source:snap.source.clone(),target:snap.target.clone()});st.log_version+=1;
            done.end=now.min(done.start+Duration::from_secs(10));Some(done).filter(|d|!d.source.is_empty())}
        else{if snap.target!=st.current.target{st.current.target=snap.target.clone();st.log_version+=1;}None}};
    if let Some(done)=done{keep(c,done);}
    // Smart hints: silence on the chosen source, or sound that never turns into text (often music).
    {let mut st=c.st.borrow_mut();if !snap.running||paused||snap.rms>0.0005{st.last_sound=Instant::now();}if snap.running&&snap.voice{st.voice_since.get_or_insert_with(Instant::now);}else{st.voice_since=None;}}
    let st=c.st.borrow();let cfg=&st.cfg;
    let back=st.scroll.min(st.transcript.len());let viewing=(back>0).then(||st.transcript[st.transcript.len()-back].clone());
    let history:Vec<(String,String)>=if st.preview{if cfg.subtitles.history>0{vec![(DEMO_PREVIOUS.0.into(),DEMO_PREVIOUS.1.into())]}else{Vec::new()}}
        else if viewing.is_some(){let end=st.transcript.len()-back;st.transcript.range(end.saturating_sub(cfg.subtitles.history as usize)..end).map(|l|(l.source.clone(),l.target.clone())).collect()}
        else if fresh{st.transcript.iter().rev().take(cfg.subtitles.history as usize).rev().map(|l|(l.source.clone(),l.target.clone())).collect()}else{Vec::new()};
    if history!=st.shown_history{o.set_history(Rc::new(VecModel::from(history.iter().map(|(s,t)|HistoryLine{source:s.into(),target:t.into()}).collect::<Vec<_>>())).into());drop(st);c.st.borrow_mut().shown_history=history;}else{drop(st);}
    let st=c.st.borrow();let cfg=&st.cfg;
    o.set_scroll_badge(if viewing.is_some()&&!st.preview{tf(l,"overlay.scrollback",&[&back]).into()}else{"".into()});
    let (source,target)=if st.preview{(DEMO_SOURCE.to_string(),DEMO_TARGET.to_string())}else if let Some(v)=&viewing{(v.source.clone(),v.target.clone())}else if fresh{(snap.source.clone(),snap.target.clone())}else{(String::new(),String::new())};
    let translate=cfg.translation.enabled;let setup=needs_setup(cfg);
    let problem=!snap.error_key.is_empty();
    let (status,level)=if problem{(t(l,&format!("err.{}",snap.error_key)),2)}
        else if snap.loading{(t(l,phase_key(&snap,paused)),1)}
        else if snap.running&&paused{(t(l,"status.paused"),1)}
        else if snap.running&&st.last_sound.elapsed()>Duration::from_secs(20){(t(l,"status.no_audio"),1)}
        else if snap.running&&st.voice_since.is_some_and(|v|v.elapsed()>Duration::from_secs(15))&&snap.changed.is_none_or(|at|at.elapsed()>Duration::from_secs(15)){(t(l,"status.music_hint"),1)}
        else if snap.running{(t(l,if snap.voice{"status.hearing"}else{"status.waiting"}),0)}
        else if setup{(t(l,"status.needs_setup"),1)}
        else{(t(l,"status.idle"),0)};
    o.set_status_line(status.as_str().into());o.set_status_level(level);
    o.set_pending(if !st.preview&&translate&&target.is_empty()&&!source.is_empty()&&snap.translating{t(l,"status.translating").into()}else{"".into()});
    o.set_activity(if !st.preview&&snap.asr_busy&&!source.is_empty(){t(l,"status.recognizing_next").into()}else{"".into()});
    // Provisional text only on the live view, and only while that segment is still pending.
    let interim=!st.preview&&viewing.is_none()&&cfg.subtitles.interim&&(snap.voice||snap.asr_busy||snap.backlog>0);
    o.set_interim(if interim{snap.interim.as_str().into()}else{"".into()});
    o.set_interim_target(if interim&&cfg.translation.enabled&&cfg.subtitles.show_translation{snap.interim_target.as_str().into()}else{"".into()});
    o.set_source(source.as_str().into());o.set_target(target.as_str().into());
    let mut live_write:Option<(PathBuf,String)>=None;
    // OBS text source: the same text the overlay shows, rewritten only when it changes.
    if cfg.transcript.live_file&&!st.preview{
        let (a,b)=if cfg.subtitles.translation_first{(&target,&source)}else{(&source,&target)};
        let (show_a,show_b)=if cfg.subtitles.translation_first{(cfg.subtitles.show_translation,cfg.subtitles.show_source)}else{(cfg.subtitles.show_source,cfg.subtitles.show_translation)};
        let text=[(show_a,a),(show_b,b)].iter().filter(|(on,t)|*on&&!t.is_empty()).map(|(_,t)|t.as_str()).collect::<Vec<_>>().join("\n");
        if text!=st.live_text{live_write=Some((st.home.join("transcripts").join("live.txt"),text));}
    }
    o.set_running(snap.running);o.set_loading(snap.loading);o.set_paused(paused);o.set_needs_setup(setup);
    let hint=st.lock_hint.filter(|t|t.elapsed()<Duration::from_secs(5)).map(|_|if cfg.subtitles.click_through{tf(l,"overlay.locked_hint_pass",&[&tray::HOTKEY])}else{t(l,"overlay.locked_hint")});
    let hint=st.toast.as_ref().filter(|(at,_)|at.elapsed()<Duration::from_secs(6)).map(|(_,m)|m.clone()).or(hint);
    let hint=hint.or_else(||st.bar_hint.filter(|t|t.elapsed()<Duration::from_secs(5)).map(|_|t(l,"overlay.bar_hint")));
    o.set_lock_hint(hint.unwrap_or_default().into());
    let tooltip=format!("Sonivra · {}",t(l,phase_key(&snap,paused)));
    drop(st);
    if let Some((path,text))=live_write{
        match std::fs::create_dir_all(path.parent().unwrap()).and_then(|_|std::fs::write(&path,&text)){Ok(())=>c.st.borrow_mut().live_text=text,Err(e)=>{c.st.borrow_mut().cfg.transcript.live_file=false;c.settings.set_live_file(false);fail(c,"file",format!("{}: {e}",path.display()));}}
    }
    if c.st.borrow().tooltip!=tooltip{if let Some(tray)=c.tray.borrow_mut().as_mut(){tray.set_tooltip(&tooltip);}c.st.borrow_mut().tooltip=tooltip;menu_labels(c);}
    // Persist drag and resize; keep the window reachable when monitors change.
    if o.window().is_visible()&&!c.st.borrow().placed{let auto=c.st.borrow().cfg.subtitles.x==i32::MIN;if ensure_on_screen(c,auto){c.st.borrow_mut().placed=true;}}
    if o.window().is_visible()&&c.st.borrow().placed{
        let pass={let st=c.st.borrow();st.cfg.subtitles.locked&&st.cfg.subtitles.click_through};sync_hittest(c,pass);
        let pos=o.window().position();let size=o.window().size().to_logical(o.window().scale_factor());
        let mut st=c.st.borrow_mut();let sub=&mut st.cfg.subtitles;
        let (w,h)=(size.width.round() as u32,size.height.round() as u32);
        if (sub.x,sub.y,sub.width,sub.height)!=(pos.x,pos.y,w.clamp(320,3840),h.clamp(80,1600)){sub.x=pos.x;sub.y=pos.y;sub.width=w.clamp(320,3840);sub.height=h.clamp(80,1600);st.geometry_dirty=Some(Instant::now());}
        if st.geometry_dirty.is_some_and(|t|t.elapsed()>Duration::from_millis(800)){st.geometry_dirty=None;drop(st);save_geometry(c);}else{drop(st);}
        if c.st.borrow().last_screen_check.elapsed()>Duration::from_secs(2){c.st.borrow_mut().last_screen_check=Instant::now();ensure_on_screen(c,false);}
    }
    if !c.settings.window().is_visible(){return;}
    let s=&c.settings;let st=c.st.borrow();let cfg=&st.cfg;
    s.set_running(snap.running);s.set_loading(snap.loading);s.set_paused(paused);s.set_reload_pending(st.reload_pending&&(snap.running||snap.loading));
    s.set_phase_text(t(l,phase_key(&snap,paused)).into());s.set_device_text(if snap.device.is_empty(){"".into()}else{tf(l,"audio.device_prefix",&[&snap.device]).into()});
    s.set_error_summary(if problem{t(l,&format!("err.{}",snap.error_key)).into()}else{"".into()});s.set_error_fix(if problem{t(l,&format!("fix.{}",snap.error_key)).into()}else{"".into()});s.set_error_detail(snap.error.as_str().into());
    s.set_error_action(match snap.error_key.as_str(){"asr_load"|"translation_load"=>1,"asr"|"translation"=>2,"audio"=>3,"download"=>4,_=>0});
    s.set_threshold_text(tf(l,"audio.threshold",&[&format!("{:.4}",threshold(s.get_sensitivity()))]).into());
    s.set_rms(snap.rms);s.set_voice(snap.voice);s.set_source(snap.source.as_str().into());s.set_target(snap.target.as_str().into());
    s.set_test_result(if matches!(snap.phase,Phase::Testing|Phase::TestDone){snap.target.as_str().into()}else{"".into()});
    s.set_download_busy(snap.download_busy);s.set_download_progress(if snap.download_total>0{snap.downloaded as f32/snap.download_total as f32}else{0.});
    s.set_download_text(tf(l,"download.progress",&[&gb(snap.downloaded),&gb(snap.download_total)]).into());
    if st.last_slow.elapsed()>=Duration::from_millis(500){
        let runtime_ok=cfg.asr.runtime.is_file()&&(!cfg.translation.local||cfg.translation.runtime.is_file());let variant=RUNTIME_VARIANTS[runtime_family(cfg) as usize];
        s.set_runtime_ok(runtime_ok);s.set_runtime_detail(if runtime_ok{tf(l,"setup.runtime_ready",&[&cfg.asr.runtime.parent().and_then(Path::parent).unwrap_or(Path::new("")).display()])}else{tf(l,"setup.runtime_missing",&[&gb(runtime_size(variant))])}.into());
        s.set_runtime_action(tf(l,"action.download_runtimes",&[&gb(runtime_size(variant))]).into());
        let describe=|path:&PathBuf|if path.is_file(){tf(l,"setup.model_ready",&[&model_name(&file_name(path)),&gb(std::fs::metadata(path).map_or(0,|m|m.len()))])}else{t(l,"setup.model_missing")};
        s.set_asr_ok(cfg.asr.model.is_file());s.set_asr_detail(describe(&cfg.asr.model).into());s.set_translation_ok(cfg.translation.model.is_file()||!cfg.translation.local);s.set_translation_detail(if cfg.translation.local{describe(&cfg.translation.model)}else{tf(l,"setup.endpoint",&[&cfg.translation.endpoint])}.into());
        let named=|file:Option<&String>|file.and_then(|f|st.catalog.iter().find(|m|&m.file==f)).map_or_else(||t(l,"action.download"),|m|tf(l,"action.download_named",&[&model_name(&m.file),&gb(m.size)]));
        s.set_rec_asr_action(named(st.recommendation.as_ref().map(|r|&r.asr_file)).into());s.set_rec_translation_action(named(st.recommendation.as_ref().map(|r|&r.translation_file)).into());
        let lines:Vec<_>=all_lines(&st).collect();s.set_transcript(lines[lines.len().saturating_sub(6)..].iter().map(|l|if l.target.is_empty(){l.source.clone()}else{format!("{}\n{}",l.source,l.target)}).collect::<Vec<_>>().join("\n\n").into());
        drop(st);s.set_models(Rc::new(VecModel::from(model_rows(c,&snap))).into());c.st.borrow_mut().last_slow=Instant::now();
    }else{drop(st);}
    if s.get_page()==6{
        let query=s.get_log_query().trim().to_lowercase();let st=c.st.borrow();let key=(st.log_version,query.clone());
        if st.log_shown.as_ref()!=Some(&key)&&(s.get_log_follow()||st.log_shown.as_ref().is_none_or(|(_,q)|*q!=query)){
            let matched:Vec<&Line>=all_lines(&st).filter(|l|query.is_empty()||l.source.to_lowercase().contains(&query)||l.target.to_lowercase().contains(&query)).collect();
            let total=all_lines(&st).count();
            let text:String=matched[matched.len().saturating_sub(LOG_VIEW_LINES)..].iter().map(|l|txt(l).replace("\r\n","\n")).collect();
            s.set_log_text(if text.is_empty(){t(l,if total==0{"log.empty"}else{"log.no_match"})}else{text.trim_end().to_string()}.into());
            s.set_log_count(tf(l,"log.count",&[&matched.len(),&total]).into());s.set_log_limited(if matched.len()>LOG_VIEW_LINES{tf(l,"log.view_limit",&[&LOG_VIEW_LINES])}else{String::new()}.into());
            drop(st);c.st.borrow_mut().log_shown=Some(key);
        }
    }else{c.st.borrow_mut().log_shown=None;}
    if c.st.borrow().last_metrics.elapsed()>=Duration::from_secs(1){
        c.st.borrow_mut().last_metrics=Instant::now();let m=metrics::sample(snap.asr_pid,snap.translation_pid,c.gpu.as_ref());let mib=|v:u64|format!("{:.0}",v as f64/1048576.);
        s.set_performance(tf(l,"perf.text",&[&mib(m.main_rss),&mib(m.asr_rss),&mib(m.translation_rss),&mib(m.combined_rss),&m.gpu_device_used.map_or_else(||t(l,"perf.unavailable"),|v|format!("{} MiB",mib(v))),&format!("{:.0}",snap.asr_ms),&format!("{:.0}",snap.first_token_ms),&format!("{:.0}",snap.full_ms),&snap.backlog,&snap.dropped,&format!("{:.1}",snap.audio_frames as f64/16000.)]).into());
        s.set_model_info(snap.model_info.as_str().into());
    }
}

/// Flattens the transparent overlay onto a solid backdrop so evidence images are fully opaque.
fn opaque(mut b:slint::SharedPixelBuffer<slint::Rgba8Pixel>)->slint::SharedPixelBuffer<slint::Rgba8Pixel>{
    for p in b.make_mut_slice(){let a=p.a as u32;let mix=|c:u8,bg:u32|((c as u32*a+bg*(255-a))/255) as u8;*p=slint::Rgba8Pixel{r:mix(p.r,0x2a),g:mix(p.g,0x2c),b:mix(p.b,0x30),a:255};}
    b
}
fn save_png(path:&Path,buffer:&slint::SharedPixelBuffer<slint::Rgba8Pixel>)->Result<()>{
    let file=std::io::BufWriter::new(std::fs::File::create(path)?);let mut encoder=png::Encoder::new(file,buffer.width(),buffer.height());encoder.set_color(png::ColorType::Rgba);encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(buffer.as_bytes())?;Ok(())
}

#[allow(unused_braces)]
pub fn run(config:Config,home:PathBuf,config_path:PathBuf,args:&[String])->Result<()>{
    slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()?;
    let mut config=config;if i18n::index(&config.ui_language).is_none(){config.ui_language=i18n::LANGS[if config.english{1}else{i18n::system_language()} as usize].into();}
    let overlay=Overlay::new()?;let settings=Settings::new()?;
    let icon=slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(&tray::icon_rgba(64),64,64));overlay.set_app_icon(icon.clone());settings.set_app_icon(icon);
    for g in [overlay.global::<I18n>(),settings.global::<I18n>()]{g.on_t(|l,key|t(l,&key).into());}
    let mut fonts:Vec<String>=FONTS.iter().filter(|(_,file)|Path::new(r"C:\Windows\Fonts").join(file).is_file()||std::env::var_os("LOCALAPPDATA").is_some_and(|d|Path::new(&d).join(r"Microsoft\Windows\Fonts").join(file).is_file())).map(|(n,_)|n.to_string()).collect();
    for f in [&config.subtitles.source_font,&config.subtitles.font]{if !fonts.contains(f){fonts.push(f.clone());}}
    let devices=audio::devices();
    let st=State{cfg:config,path:config_path,home:home.clone(),apps:Vec::new(),devices:devices.as_ref().map_or_else(|_|Vec::new(),Clone::clone),asr_devices:Vec::new(),fonts,asr_langs:languages::asr(),tr_langs:languages::translation(),catalog:models::catalog(),
        preview:false,lock_hint:None,bar_hint:None,toast:None,live_text:String::new(),reload_pending:false,hardware:hardware::detect().ok(),recommendation:None,download_cancel:Arc::new(AtomicBool::new(false)),last_job:None,auto_use:None,geometry_dirty:None,hittest:None,placed:false,transcript:VecDeque::new(),current:Line::default(),shown_history:Vec::new(),
        begin:Instant::now(),log_file:None,log_version:0,log_shown:None,scroll:0,scroll_at:Instant::now(),last_sound:Instant::now(),voice_since:None,
        last_metrics:Instant::now()-Duration::from_secs(5),last_slow:Instant::now()-Duration::from_secs(5),last_screen_check:Instant::now(),tooltip:String::new()};
    let c=Rc::new(Ctx{overlay,settings,engine:Engine::new(),st:RefCell::new(st),gpu:metrics::Gpu::new(),tray:RefCell::new(None)});
    if let Err(e)=devices{fail(&c,"audio",format!("{e:#}"));}
    refresh_asr_devices(&c);set_lang(&c);load(&c);
    {let sub=c.st.borrow().cfg.subtitles.clone();c.overlay.window().set_size(slint::LogicalSize::new(sub.width as f32,sub.height as f32));if sub.x!=i32::MIN{c.overlay.window().set_position(slint::PhysicalPosition::new(sub.x,sub.y));}}
    macro_rules! on{($win:ident.$set:ident,|$cx:ident $(,$arg:ident)*| $body:expr)=>{{let w:Weak<Ctx>=Rc::downgrade(&c);c.$win.$set(move|$($arg),*|{if let Some($cx)=w.upgrade(){let $cx=&*$cx;$body}});}}}
    on!(overlay.on_drag,|c|{if let Some(Err(e))=c.overlay.window().with_winit_window(|w|w.drag_window().map_err(|e|e.to_string())){fail(c,"overlay",e);}});
    on!(overlay.on_resize,|c,dir|{use slint::winit_030::winit::window::ResizeDirection as R;
        let dir=[R::East,R::North,R::NorthEast,R::NorthWest,R::South,R::SouthEast,R::SouthWest,R::West][dir.clamp(0,7) as usize];
        if let Some(Err(e))=c.overlay.window().with_winit_window(|w|w.drag_resize_window(dir).map_err(|e|e.to_string())){fail(c,"overlay",e);}});
    on!(overlay.on_start,|c|start(c));on!(settings.on_start,|c|start(c));
    on!(overlay.on_pause,|c|{let p=&c.engine.paused;p.store(!p.load(Ordering::Relaxed),Ordering::Relaxed);});on!(settings.on_pause,|c|{let p=&c.engine.paused;p.store(!p.load(Ordering::Relaxed),Ordering::Relaxed);});
    on!(overlay.on_stop,|c|{if let Err(e)=c.engine.send(Control::Stop){fail(c,"settings",format!("{e:#}"));}});on!(settings.on_stop,|c|{if let Err(e)=c.engine.send(Control::Stop){fail(c,"settings",format!("{e:#}"));}});
    on!(settings.on_reload,|c|start(c));
    on!(overlay.on_toggle_lock,|c|toggle_lock(c));on!(overlay.on_open_settings,|c|show_settings(c));on!(overlay.on_quit,|c|quit(c));
    on!(overlay.on_save_log,|c|quick_save(c));
    on!(settings.on_export_profile,|c|export_profile(c));
    on!(settings.on_import_profile,|c|import_profile(c));
    on!(overlay.on_toggle_bar,|c|edit(c,|cfg|cfg.subtitles.bar=if cfg.subtitles.bar==0{1}else{0}));
    // Quick style strip on the overlay. Colours match `swatches` in app.slint.
    on!(overlay.on_quick_style,|c,kind,value|match kind{
        5=>{c.settings.set_page(1);show_settings(c);}
        4=>{let fonts=c.st.borrow().fonts.clone();edit(c,|cfg|{let s=&mut cfg.subtitles;let i=fonts.iter().position(|f|*f==s.font).map_or(0,|i|(i+1)%fonts.len());s.font=fonts[i].clone();});}
        _=>edit(c,|cfg|{let s=&mut cfg.subtitles;match kind{0=>s.font_size=(s.font_size as i32+value*2).clamp(12,72) as u32,1=>s.target_color=SWATCHES[value as usize].into(),2=>s.opacity=[0,35,60,85][value as usize],6=>s.opacity=(s.opacity as i32+value*5).clamp(0,100) as u32,3=>s.effect=(s.effect+1)%4,_=>{}}}),
    });
    on!(overlay.on_end_preview,|c|{c.settings.set_preview(false);commit(c);});
    on!(settings.on_commit,|c|commit(c));
    on!(settings.on_dismiss_error,|c|c.engine.snapshot.lock().unwrap().clear_error());
    on!(settings.on_error_action_clicked,|c,action|match action{1=>c.settings.set_page(3),2=>start(c),3=>c.settings.set_page(2),4=>{let job=c.st.borrow().last_job.clone();if let Some(job)=job{download(c,job,false);}},_=>{}});
    on!(settings.on_reset_overlay,|c|{edit(c,|cfg|{let d=sonivra::config::Subtitles::default();cfg.subtitles.width=d.width;cfg.subtitles.height=d.height;cfg.subtitles.enabled=true;cfg.subtitles.locked=false;});c.overlay.window().set_size(slint::LogicalSize::new(960.,190.));ensure_on_screen(c,true);});
    on!(settings.on_snap,|c,edge|snap(c,edge==0));
    on!(settings.on_apply_preset,|c,preset|{if preset==3{let l=lang(c);if !confirm(l,&t(l,"confirm.reset_look")){return;}}edit(c,|cfg|{let s=&mut cfg.subtitles;match preset{3=>{let k=s.clone();*s=sonivra::config::Subtitles{enabled:k.enabled,locked:k.locked,click_through:k.click_through,width:k.width,height:k.height,x:k.x,y:k.y,bar:k.bar,anchor_top:k.anchor_top,show_status:k.show_status,hold_ms:k.hold_ms,history:k.history,..Default::default()};},0=>{s.opacity=0;s.effect=1;s.font_size=32;},1=>{s.opacity=60;s.effect=2;s.font_size=30;},_=>{s.opacity=45;s.effect=1;s.font_size=44;}}s.enabled=true;})});
    on!(settings.on_save_transcript,|c,kind|save_transcript(c,kind));on!(settings.on_clear_transcript,|c|clear_transcript(c));
    on!(settings.on_open_transcripts,|c|{let dir=transcripts_dir(c);if let Err(e)=std::fs::create_dir_all(&dir).and_then(|_|std::process::Command::new("explorer.exe").arg(&dir).spawn().map(|_|())){fail(c,"file",format!("{}: {e}",dir.display()));}});
    on!(overlay.on_scroll_by,|c,delta|scroll(c,delta as i64));on!(overlay.on_back_to_live,|c|scroll(c,-(1<<30)));
    on!(settings.on_refresh_audio,|c|refresh_audio(c));
    on!(settings.on_install_runtimes,|c|{let v=RUNTIME_VARIANTS[c.settings.get_runtime_family().clamp(0,2) as usize].to_string();download(c,Job::Runtimes(v),true);});
    on!(settings.on_download_recommended,|c,kind|{let file=c.st.borrow().recommendation.as_ref().map(|r|if kind==0{r.asr_file.clone()}else{r.translation_file.clone()});let index=file.and_then(|f|c.st.borrow().catalog.iter().position(|m|m.file==f));if let Some(i)=index{use_model(c,i);}});
    on!(settings.on_cancel_download,|c|c.st.borrow().download_cancel.store(true,Ordering::Release));
    on!(settings.on_choose_model,|c,kind|choose_model(c,kind));
    on!(settings.on_choose_runtime,|c,kind|{let l=lang(c);if let Some(file)=rfd::FileDialog::new().set_title(t(l,"dialog.choose_runtime")).add_filter(t(l,"dialog.exe"),&["exe"]).pick_file(){edit(c,|cfg|if kind==0{cfg.asr.runtime=file;}else{cfg.translation.runtime=file;});refresh_asr_devices(c);load(c);}});
    on!(settings.on_download_model,|c,i|download(c,Job::Model(i as usize),true));
    on!(settings.on_use_model,|c,i|use_model(c,i as usize));
    on!(settings.on_remove_model,|c,i|{let l=lang(c);let Some(m)=c.st.borrow().catalog.get(i as usize).cloned()else{return};let path=c.st.borrow().home.join("models").join(&m.file);if path.exists()&&confirm(l,&tf(l,"confirm.remove",&[&model_name(&m.file),&path.display()])){if let Err(e)=std::fs::remove_file(&path){fail(c,"file",format!("{}: {e}",path.display()));}}});
    on!(settings.on_apply_recommendation,|c|{let Some(r)=c.st.borrow().recommendation.clone()else{return};let home=c.st.borrow().home.clone();edit(c,|cfg|{cfg.asr.model=home.join("models").join(&r.asr_file);cfg.translation.model=home.join("models").join(&r.translation_file);cfg.asr.device=r.asr_device.clone();cfg.translation.gpu_layers=r.gpu_layers;cfg.translation.family="plamo3".into();cfg.translation.local=true;cfg.translation.runtime=home.join("runtime").join(&r.runtime_variant).join("llama-server.exe");});});
    on!(settings.on_test_translation,|c|{let cfg=c.st.borrow().cfg.clone();if let Err(e)=c.engine.send(Control::Translate(cfg,c.settings.get_test_text().to_string())){fail(c,"translation_test",format!("{e:#}"));}});
    on!(settings.on_test_asr,|c|{let l=lang(c);if let Some(file)=rfd::FileDialog::new().set_title(t(l,"dialog.choose_wav")).add_filter(t(l,"dialog.wav"),&["wav"]).pick_file(){let cfg=c.st.borrow().cfg.clone();if needs_setup(&cfg){c.settings.set_page(0);return;}if let Err(e)=c.engine.send(Control::File(cfg,file)){fail(c,"file",format!("{e:#}"));}}});
    on!(settings.on_open_data_folder,|c|{let home=c.st.borrow().home.clone();if let Err(e)=std::process::Command::new("explorer.exe").arg(&home).spawn(){fail(c,"file",format!("{e:#}"));}});
    {let w=Rc::downgrade(&c);c.overlay.window().on_close_requested(move||{if let Some(c)=w.upgrade(){quit(&c);}slint::CloseRequestResponse::KeepWindowShown});}
    c.settings.set_demo_source(DEMO_SOURCE.into());c.settings.set_demo_target(DEMO_TARGET.into());
    c.settings.set_test_text("今日は少し寒いですね。配信を見ながら、温かいお茶を飲んでいます。".into());
    match Tray::new("Sonivra"){Ok(tray)=>{if !tray.failed.is_empty(){let l=lang(&c);fail(&c,"overlay",tf(l,"hotkey.failed",&[&tray.failed.join(", ")]));}*c.tray.borrow_mut()=Some(tray);},Err(e)=>fail(&c,"overlay",format!("{e:#}"))}
    apply_overlay(&c);
    if needs_setup(&c.st.borrow().cfg)||args.iter().any(|a|a=="--settings"){show_settings(&c);}
    if let Some(i)=args.iter().position(|a|a=="--page"){if let Some(p)=args.get(i+1).and_then(|p|p.parse().ok()){c.settings.set_page(p);show_settings(&c);}}
    if args.iter().any(|a|a=="--preview"){c.settings.set_preview(true);commit(&c);}
    let auto=c.st.borrow().cfg.autostart&&!needs_setup(&c.st.borrow().cfg);
    if auto||args.iter().any(|a|a=="--autostart"){start(&c);}
    let begin=Instant::now();let limit=args.iter().position(|s|s=="--seconds").and_then(|i|args.get(i+1)).and_then(|s|s.parse::<u64>().ok());let smoke=args.iter().any(|s|s=="--gui-smoke");
    let output=args.iter().position(|s|s=="--output").and_then(|i|args.get(i+1)).map(PathBuf::from);
    let snapshot_dir=args.iter().position(|s|s=="--snapshot").and_then(|i|args.get(i+1)).map(PathBuf::from);let mut shot=0usize;let mut last_shot=Instant::now();
    // Live-test evidence: the overlay rendered by Slint itself (transparent PNG), so nothing behind the window is captured.
    let overlay_shots=args.iter().position(|s|s=="--overlay-shots").and_then(|i|args.get(i+1)).map(PathBuf::from);let mut overlay_shot=Instant::now();
    let timer=slint::Timer::default();let w=Rc::downgrade(&c);
    timer.start(slint::TimerMode::Repeated,Duration::from_millis(75),move||{
        let Some(c)=w.upgrade()else{return};tick(&c);
        // Developer evidence: render every settings page in all three languages, plus the overlay preview.
        if let Some(dir)=&overlay_shots{if overlay_shot.elapsed()>=Duration::from_secs(4){overlay_shot=Instant::now();match c.overlay.window().take_snapshot().map(opaque){Ok(b)=>{if let Err(e)=save_png(&dir.join(format!("overlay-{:04}.png",begin.elapsed().as_secs())),&b){eprintln!("{e:#}");}},Err(e)=>eprintln!("{e}")}}}
        if let Some(dir)=&snapshot_dir{
            // One frame per 700 ms: save what is on screen, then switch to the next language × page.
            if begin.elapsed()>Duration::from_millis(1500)&&last_shot.elapsed()>Duration::from_millis(700){
                last_shot=Instant::now();
                if let Some(prev)=shot.checked_sub(1){let code=i18n::LANGS[prev/7];
                    match c.settings.window().take_snapshot(){Ok(b)=>{if let Err(e)=save_png(&dir.join(format!("settings-{code}-{}.png",prev%7)),&b){eprintln!("{e:#}");}},Err(e)=>eprintln!("{e}")}
                    if prev%7==6{match c.overlay.window().take_snapshot().map(opaque){Ok(b)=>{if let Err(e)=save_png(&dir.join(format!("overlay-{code}.png")),&b){eprintln!("{e:#}");}},Err(e)=>eprintln!("{e}")}c.overlay.set_style_open(code=="zh-TW");}}
                if shot<21{c.settings.set_language((shot/7) as i32);commit(&c);c.settings.set_page((shot%7) as i32);shot+=1;}else{let _=slint::quit_event_loop();}
            }
        }
        if (smoke&&begin.elapsed()>Duration::from_secs(3))||limit.is_some_and(|limit|begin.elapsed()>Duration::from_secs(limit)){
            let mut snapshot=c.engine.snapshot.lock().unwrap().clone();let o=c.overlay.window();
            if let Some(path)=&output{let value=serde_json::json!({"gui_startup_smoke":true,"elapsed_s":begin.elapsed().as_secs_f64(),"overlay_visible":o.is_visible(),"overlay_position":[o.position().x,o.position().y],"overlay_size":[o.size().width,o.size().height],"scale_factor":o.scale_factor(),"overlay_hit_test":c.st.borrow().hittest,"settings_visible":c.settings.window().is_visible(),"phase":format!("{:?}",snapshot.phase),"source":snapshot.source,"translation":snapshot.target,"error_key":snapshot.error_key,"error":snapshot.error,"audio_samples":snapshot.audio_frames,"asr_ms":snapshot.asr_ms,"ttft_ms":snapshot.first_token_ms,"live_end_to_end_ms":snapshot.full_ms,"after_speech_ms":snapshot.after_speech_ms,"after_speech_p50_ms":metrics::percentile(snapshot.latencies.make_contiguous(),0.5),"after_speech_p95_ms":metrics::percentile(snapshot.latencies.make_contiguous(),0.95),"translated_segments":snapshot.latencies.len(),"revised":snapshot.revised,"endpoint_ms":snapshot.endpoint_ms,"backlog":snapshot.backlog,"dropped":snapshot.dropped,"memory":metrics::sample(snapshot.asr_pid,snapshot.translation_pid,c.gpu.as_ref())});if let Err(e)=std::fs::write(path,serde_json::to_string_pretty(&value).unwrap()){eprintln!("{e}");}}
            quit(&c);
        }
    });
    slint::run_event_loop_until_quit().context("Slint event loop")?;
    timer.stop();let _=c.overlay.hide();let _=c.settings.hide();c.tray.borrow_mut().take();c.engine.shutdown();Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn srt_numbers_lines_and_formats_times(){
        let a=Line{clock:"10:00:00".into(),start:Duration::from_millis(3_723_045),end:Duration::from_millis(3_725_000),source:"こんにちは".into(),target:"你好".into()};
        let b=Line{clock:"10:00:02".into(),start:Duration::from_secs(5),end:Duration::from_secs(5),source:"はい".into(),target:String::new()};
        assert_eq!(srt([a.clone(),b.clone()].iter(),true),"WEBVTT\r\n\r\n01:02:03.045 --> 01:02:05.000\r\nこんにちは\r\n你好\r\n\r\n00:00:05.000 --> 00:00:05.500\r\nはい\r\n\r\n");
        assert_eq!(srt([a.clone(),b].iter(),false),"1\r\n01:02:03,045 --> 01:02:05,000\r\nこんにちは\r\n你好\r\n\r\n2\r\n00:00:05,000 --> 00:00:05,500\r\nはい\r\n\r\n");
        assert_eq!(txt(&a),"[10:00:00] こんにちは\r\n你好\r\n\r\n");
    }
}
