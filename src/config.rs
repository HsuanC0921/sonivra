use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub english: bool,
    pub ui_language: String,
    pub gpu_reserve_mb: u32,
    /// Model files that loaded successfully on this machine.
    pub verified_models: Vec<String>,
    pub audio: Audio,
    pub asr: Asr,
    pub translation: Translation,
    pub subtitles: Subtitles,
    /// Start listening as soon as Sonivra opens (when setup is complete).
    pub autostart: bool,
    pub transcript: Transcript,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    /// Lines kept in memory for scrollback and the log page.
    pub keep_lines: u32,
    /// Append every finished line to a text file under `<home>/transcripts`.
    pub save_files: bool,
    /// Keep `<home>/transcripts/live.txt` equal to the subtitle on screen, for an OBS text source.
    pub live_file: bool,
}
impl Default for Transcript { fn default() -> Self { Self { keep_lines:2000, save_files:false, live_file:false } } }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub device: String,
    /// Capture only this program (executable name, e.g. `chrome.exe`) instead of the whole output device. Empty means the device.
    pub process: String,
    /// Also listen to the default microphone and mix it into the captured sound.
    pub mix_mic: bool,
    /// Write the sound that is recognized to a WAV file in `transcripts`.
    pub record: bool,
    /// Where recordings go. Set by the app at start, never stored.
    #[serde(skip)]
    pub record_dir: PathBuf,
    pub microphone: bool,
    pub threshold: f32,
    pub silence_ms: u32,
    pub min_speech_ms: u32,
    pub max_segment_ms: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asr {
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub device: String,
    pub language: String,
    pub prompt: String,
    pub threads: u32,
    pub timeout_s: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Translation {
    pub enabled: bool,
    pub local: bool,
    pub family: String,
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub endpoint: String,
    pub model_id: String,
    pub source_lang: String,
    pub target_lang: String,
    pub gpu_layers: i32,
    pub mmap: bool,
    pub context_size: u32,
    pub history_turns: usize,
    pub max_tokens: u32,
    pub temperature: f32,
    pub debounce_ms: u32,
    /// After the next sentence arrives and the translator is idle, translate the previous sentence again with it as context.
    pub refine: bool,
    /// Also translate the provisional text while a sentence is being spoken (uses idle translator time only).
    pub interim: bool,
    pub context: String,
    pub glossary: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subtitles {
    pub enabled: bool,
    pub locked: bool,
    pub click_through: bool,
    pub source_font: String,
    pub font: String,
    pub font_size: u32,
    pub opacity: u32,
    pub hold_ms: u32,
    pub width: u32,
    pub height: u32,
    /// Physical screen position. Off-screen values are corrected at startup and when monitors change.
    pub x: i32,
    pub y: i32,
    pub spacing: u32,
    /// 0 none, 1 outline, 2 shadow, 3 band behind the text
    pub effect: u32,
    pub show_source: bool,
    pub show_translation: bool,
    /// Earlier lines kept above the current one, older ones dimmer (0..=3, or FILL_HISTORY to fill the window).
    pub history: u32,
    /// Earlier lines show 0 translation, 1 original, 2 both.
    pub history_mode: u32,
    /// Show provisional recognition while a sentence is still being spoken.
    pub interim: bool,
    /// Show the translation above the original.
    pub translation_first: bool,
    /// `#RRGGBB` colours.
    pub source_color: String,
    pub target_color: String,
    pub background_color: String,
    /// Original text size relative to the translation, in percent (40..=100).
    pub source_scale: u32,
    /// 0 left, 1 center, 2 right
    pub align: u32,
    /// Outline thickness 1..=4
    pub outline: u32,
    pub source_bold: bool,
    pub target_bold: bool,
    /// Background hugs the text instead of spanning the window.
    pub fit_text: bool,
    pub radius: u32,
    /// Top bar while unlocked: 0 always, 1 on hover, 2 hidden.
    pub bar: u32,
    /// Text sits at the top of the window instead of the bottom.
    pub anchor_top: bool,
    /// Text opacity in percent (20..=100).
    pub text_opacity: u32,
    /// Outline and shadow colour.
    pub outline_color: String,
    /// Info messages (paused, no sound) on the locked window. Errors always show.
    pub show_status: bool,
}
/// History setting that sends this many lines and lets the window height decide how many show.
pub const FILL_HISTORY:u32=300;
/// Parses `#RRGGBB`.
pub fn parse_color(text:&str)->Option<[u8;3]>{
    let hex=text.strip_prefix('#')?;if hex.len()!=6{return None;}
    let v=u32::from_str_radix(hex,16).ok()?;Some([(v>>16) as u8,(v>>8) as u8,v as u8])
}
impl Default for Audio { fn default() -> Self { Self { device: String::new(), process: String::new(), mix_mic:false, record:false, record_dir:PathBuf::new(), microphone:false, threshold:0.003, silence_ms:450, min_speech_ms:100, max_segment_ms:8000 } } }
impl Default for Asr { fn default() -> Self { Self { runtime:"runtime/speech/speech.exe".into(), model:"models/Qwen3-ASR-1.7B-Q4_K.gguf".into(), device:"Vulkan0".into(), language:"ja".into(), prompt:String::new(), threads:8, timeout_s:120 } } }
impl Default for Translation { fn default() -> Self { Self { enabled:true, local:true, family:"plamo3".into(), runtime:"runtime/llama/llama-server.exe".into(), model:"models/plamo-3-translate-2b-Q4_K_M.gguf".into(), endpoint:String::new(), model_id:"sonivra-translate".into(), source_lang:"ja".into(), target_lang:"zh-hant".into(), gpu_layers:99, mmap:true, context_size:4096, history_turns:4, max_tokens:256, temperature:0.0, debounce_ms:80,refine:false,interim:false, context:"即時對話字幕。使用臺灣自然口語，保留原意、語氣、猶豫、重複與未完句，避免文言與過度壓縮。".into(), glossary:String::new() } } }
impl Default for Subtitles { fn default() -> Self { Self { enabled:true, locked:false, click_through:true, source_font:"Yu Gothic UI".into(), font:"Microsoft JhengHei UI".into(), font_size:30, opacity:45, hold_ms:8000, width:960, height:190, x:i32::MIN, y:i32::MIN, spacing:6, effect:1, show_source:true, show_translation:true, history:FILL_HISTORY, history_mode:0, interim:true, translation_first:false, source_color:"#E9EEF4".into(), target_color:"#FFF3CF".into(), background_color:"#05080C".into(), source_scale:78, align:1, outline:2, source_bold:false, target_bold:true, fit_text:false, radius:8,bar:0,anchor_top:false,text_opacity:100,outline_color:"#000000".into(),show_status:true } } }
impl Default for Config { fn default() -> Self { Self {english:false,ui_language:String::new(),gpu_reserve_mb:0,verified_models:Vec::new(),audio:Audio::default(),asr:Asr::default(),translation:Translation::default(),subtitles:Subtitles::default(),autostart:false,transcript:Transcript::default()} } }
impl Config {
    pub fn load(path:&Path) -> Result<Self> {
        if !path.exists() { return Ok(Self::default()); }
        let value:Self = toml::from_str(&fs::read_to_string(path)?)?;
        value.validate()?; Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        if self.audio.process.len()>260||self.audio.process.contains(['\\','/']) { bail!("Program name must be an executable file name"); }
        if !self.audio.threshold.is_finite() || !(0.0001..=0.5).contains(&self.audio.threshold) { bail!("VAD threshold must be between 0.0001 and 0.5"); }
        if !(50..=3000).contains(&self.audio.silence_ms) || !(50..=5000).contains(&self.audio.min_speech_ms) || !(1000..=30000).contains(&self.audio.max_segment_ms) || self.audio.min_speech_ms > self.audio.max_segment_ms { bail!("Invalid speech segmentation durations"); }
        if self.translation.history_turns>16 || !(512..=32768).contains(&self.translation.context_size) || !(1..=2048).contains(&self.translation.max_tokens) { bail!("Context/history/token limits exceeded"); }
        if !self.translation.temperature.is_finite()||!(0.0..=2.0).contains(&self.translation.temperature)||self.translation.debounce_ms>1000{bail!("Invalid temperature or translation debounce");}
        if self.translation.family=="plamo3"&&self.translation.target_lang=="auto"{bail!("PLaMo requires a concrete target language, for example zh-hant");}
        if self.translation.context.chars().count()>8192||self.translation.glossary.chars().count()>8192||self.asr.prompt.chars().count()>4096{bail!("Context, glossary or vocabulary hints exceed the bounded text budget");}
        for lang in [&self.translation.source_lang,&self.translation.target_lang] { if lang.is_empty() || !lang.chars().all(|c|c.is_ascii_alphanumeric()||c=='-') { bail!("Invalid language tag"); } }
        if !(1..=32).contains(&self.asr.threads) || !(5..=600).contains(&self.asr.timeout_s) { bail!("Invalid ASR threads or timeout"); }
        if !(12..=72).contains(&self.subtitles.font_size) || self.subtitles.opacity>100 || !(320..=3840).contains(&self.subtitles.width) || !(80..=1600).contains(&self.subtitles.height) || self.subtitles.spacing>48 || self.subtitles.effect>3 || self.subtitles.history>FILL_HISTORY || self.subtitles.history_mode>2 || self.subtitles.bar>2 || !(20..=100).contains(&self.subtitles.text_opacity) || parse_color(&self.subtitles.outline_color).is_none() || !(100..=100_000).contains(&self.transcript.keep_lines) || !(40..=100).contains(&self.subtitles.source_scale) || self.subtitles.align>2 || !(1..=4).contains(&self.subtitles.outline) || self.subtitles.radius>24 || [&self.subtitles.source_color,&self.subtitles.target_color,&self.subtitles.background_color].iter().any(|c|parse_color(c).is_none()) { bail!("Invalid subtitle font, opacity or size"); }
        Ok(())
    }
    pub fn save(&self,path:&Path) -> Result<()> {
        self.validate()?;
        if let Some(parent)=path.parent() {fs::create_dir_all(parent)?;}
        fs::write(path,toml::to_string_pretty(self)?)?; Ok(())
    }
    pub fn resolve(&mut self, base:&Path) {
        for path in [&mut self.asr.runtime,&mut self.asr.model,&mut self.translation.runtime,&mut self.translation.model] { if path.is_relative() {*path=base.join(&*path);} }
    }
}
pub fn default_path()->PathBuf { std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(||PathBuf::from(".")).join("Sonivra/config.toml") }

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn defaults_round_trip() {let c=Config::default(); let parsed:Config=toml::from_str(&toml::to_string(&c).unwrap()).unwrap(); parsed.validate().unwrap(); assert_eq!(parsed.translation.target_lang,"zh-hant");}
    #[test] fn reject_invalid_range() {let mut c=Config::default();c.audio.max_segment_ms=0;assert!(c.validate().is_err());}
    #[test] fn preserve_partial_config() {let c:Config=toml::from_str("english = true\n[translation]\ntarget_lang = 'en'").unwrap();assert!(c.english);assert_eq!(c.translation.target_lang,"en");assert_eq!(c.audio.silence_ms,450);}
}
