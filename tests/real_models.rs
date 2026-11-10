use sonivra::{config::Config,worker::Worker,audio,translation::Translator,pipeline::Cancellation};
use std::{collections::VecDeque,path::PathBuf,sync::{Arc,atomic::{AtomicBool,AtomicU64}}};
fn config()->Config{let home=PathBuf::from(std::env::var("SONIVRA_TEST_HOME").expect("Set SONIVRA_TEST_HOME to installed runtime/models"));let mut cfg=Config::default();cfg.resolve(&home);cfg}
fn sample()->PathBuf{PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audio/test/12677001980660723842.wav")}
#[test]
#[ignore="Requires real native runtimes and downloaded model weights"]
fn japanese_file_and_traditional_chinese_translation(){
    let cfg=config();let stop=Arc::new(AtomicBool::new(false));let mut worker=Worker::start(&cfg.asr,&stop).unwrap();let audio=audio::read_wav(&sample()).unwrap();let text=worker.transcribe(1,&audio,&cfg.asr,&stop).unwrap();assert!(text.contains("ヨット"),"ASR: {text}");
    let rt=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();let translator=rt.block_on(Translator::start(cfg.translation,&stop)).unwrap();let token=Cancellation{stop,latest:Arc::new(AtomicU64::new(1)),id:1};let (target,_)=rt.block_on(translator.translate(&text,&VecDeque::new(),&token,|_,_|{})).unwrap();assert!(target.contains("遊艇"),"Translation: {target}");assert!(!target.contains(['岛','并','须','须']));
}
#[test]
#[ignore="Requires real native runtimes and downloaded model weights"]
fn actual_worker_crash_restart_and_model_device_switch(){
    let mut cfg=config();let stop=AtomicBool::new(false);let audio=audio::read_wav(&sample()).unwrap();
    let mut worker=Worker::start(&cfg.asr,&stop).unwrap();let pid=worker.pid();
    let status=std::process::Command::new("taskkill.exe").args(["/PID",&pid.to_string(),"/F"]).status().unwrap();assert!(status.success());assert!(worker.transcribe(1,&audio,&cfg.asr,&stop).is_err());drop(worker);
    for device in ["CPU","Vulkan0"]{cfg.asr.device=device.into();let mut worker=Worker::start(&cfg.asr,&stop).unwrap();let pid=worker.pid();assert!(worker.transcribe(2,&audio,&cfg.asr,&stop).unwrap().contains("ヨット"));drop(worker);assert_eq!(sonivra::metrics::rss(pid),0,"Old worker still resident");}
}
#[test]
#[ignore="Requires a real translation runtime and downloaded model weights"]
fn real_stream_cancellation_and_recovery(){
    let cfg=config();let stop=Arc::new(AtomicBool::new(false));let latest=Arc::new(AtomicU64::new(1));let token=Cancellation{stop:stop.clone(),latest:latest.clone(),id:1};
    let rt=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();let translator=rt.block_on(Translator::start(cfg.translation,&stop)).unwrap();
    let result=rt.block_on(translator.translate(&"今日は晴れです。".repeat(100),&VecDeque::new(),&token,|_,_|{latest.store(2,std::sync::atomic::Ordering::Release);}));assert!(result.is_err());
    let token=Cancellation{stop,latest,id:2};let (text,_)=rt.block_on(translator.translate("こんにちは。",&VecDeque::new(),&token,|_,_|{})).unwrap();assert!(text.contains("你好"),"{text}");
}
