use anyhow::{Context,Result,bail};
use serde::Serialize;
use std::{collections::VecDeque,path::Path,sync::{Mutex,atomic::{AtomicBool,Ordering}},time::{Instant,Duration}};
use wasapi::*;
use crate::config::Audio;

#[derive(Clone,Debug,Serialize)]
pub struct AudioDevice {pub id:String,pub name:String,pub microphone:bool}
pub fn devices()->Result<Vec<AudioDevice>>{
    std::thread::spawn(||->Result<Vec<AudioDevice>>{
        initialize_mta().ok()?;
        let e=DeviceEnumerator::new()?;let mut out=Vec::new();
        for (direction,microphone) in [(Direction::Render,false),(Direction::Capture,true)]{
            let collection=e.get_device_collection(direction)?;
            for i in 0..collection.get_nbr_devices()?{let device=collection.get_device_at_index(i)?;out.push(AudioDevice{id:device.get_id()?,name:device.get_friendlyname()?,microphone});}
        }
        Ok(out)
    }).join().map_err(|_|anyhow::anyhow!("Audio enumeration thread failed"))?
}
/// Executable names of programs that currently own a visible window, sorted and without duplicates.
pub fn app_names()->Vec<String>{
    let mut names:Vec<String>=windows_pids().into_iter().filter_map(|(_,n)|Some(n)).collect();
    names.sort_by_key(|n|n.to_lowercase());names.dedup_by(|a,b|a.eq_ignore_ascii_case(b));names
}
fn windows_pids()->Vec<(u32,String)>{
    use windows::{Win32::{Foundation::*,System::Threading::*,UI::WindowsAndMessaging::*},core::{BOOL,PWSTR}};
    unsafe extern "system" fn each(window:HWND,out:LPARAM)->BOOL{unsafe{
        if IsWindowVisible(window).as_bool()&&GetWindowTextLengthW(window)>0&&GetWindow(window,GW_OWNER).is_err(){
            let mut pid=0u32;GetWindowThreadProcessId(window,Some(&mut pid));
            if pid!=0{if let Ok(h)=OpenProcessW_(pid){
                let mut buf=[0u16;520];let mut len=buf.len() as u32;
                if QueryFullProcessImageNameW(h,PROCESS_NAME_WIN32,PWSTR(buf.as_mut_ptr()),&mut len).is_ok(){
                    let path=String::from_utf16_lossy(&buf[..len as usize]);
                    if let Some(name)=path.rsplit('\\').next(){(*(out.0 as *mut Vec<(u32,String)>)).push((pid,name.to_string()));}
                }
                let _=CloseHandle(h);
            }}
        }
        BOOL(1)
    }}
    #[allow(non_snake_case)]
    unsafe fn OpenProcessW_(pid:u32)->windows::core::Result<HANDLE>{unsafe{OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,false,pid)}}
    let mut found:Vec<(u32,String)>=Vec::new();
    unsafe{let _=EnumWindows(Some(each),LPARAM(&mut found as *mut _ as isize));}
    found
}
/// Feeds the default microphone into `queue` as 16 kHz mono until `stop` or `done`.
fn mic_into(queue:&Mutex<VecDeque<f32>>,stop:&AtomicBool,done:&AtomicBool)->Result<()>{
    initialize_mta().ok()?;
    let device=DeviceEnumerator::new()?.get_default_device(Direction::Capture)?;
    let mut client=device.get_iaudioclient()?;
    let format=WaveFormat::new(32,32,SampleType::Float,16000,1,None);
    client.initialize_client(&format,Direction::Capture,StreamMode::EventsShared{autoconvert:true,buffer_duration_hns:200000},None)?;
    let event=client.set_get_eventhandle()?;let capture=client.get_audiocaptureclient()?;
    let mut bytes:VecDeque<u8>=VecDeque::new();
    client.start_stream()?;
    while !stop.load(Ordering::Relaxed)&&!done.load(Ordering::Relaxed){
        let _=event.wait_for_event(100);
        while capture.get_next_packet_size()?.unwrap_or(0)>0{capture.read_from_device_to_deque(&mut bytes)?;}
        let mut q=queue.lock().unwrap();
        while bytes.len()>=4{let b=[bytes.pop_front().unwrap(),bytes.pop_front().unwrap(),bytes.pop_front().unwrap(),bytes.pop_front().unwrap()];q.push_back(f32::from_le_bytes(b));}
        // The two clocks drift; never let the microphone fall more than a second behind.
        while q.len()>16000{q.pop_front();}
    }
    client.stop_stream()?;Ok(())
}
/// Returns Ok(()) when `changed` reports a new device selection so the caller can reopen.
pub fn capture(cfg:&Audio,stop:&AtomicBool,changed:&dyn Fn()->bool,receive:impl FnMut(&[f32],Instant),report:impl FnMut(&str))->Result<()> {
    let mix=cfg.mix_mic&&!cfg.microphone;
    let queue=Mutex::new(VecDeque::new());let done=AtomicBool::new(false);
    std::thread::scope(|scope|{
        if mix{scope.spawn(||{let _=mic_into(&queue,stop,&done);});}
        let result=capture_main(cfg,stop,changed,receive,report,mix.then_some(&queue));
        done.store(true,Ordering::Release);result
    })
}
fn capture_main(cfg:&Audio,stop:&AtomicBool,changed:&dyn Fn()->bool,mut receive:impl FnMut(&[f32],Instant),mut report:impl FnMut(&str),mic:Option<&Mutex<VecDeque<f32>>>)->Result<()> {
    initialize_mta().ok()?;
    // Adds the microphone to a frame of the captured sound.
    let mut give=|frame:&mut [f32],at:Instant|{if let Some(mic)=mic{let mut q=mic.lock().unwrap();for s in frame.iter_mut(){if let Some(m)=q.pop_front(){*s=(*s+m).clamp(-1.,1.);}}}receive(frame,at);};
    let enumerator=DeviceEnumerator::new()?;
    let direction=if cfg.microphone{Direction::Capture}else{Direction::Render};
    let app=!cfg.process.is_empty();
    let (mut client,device_id)=if app{
        // One program's sound only (Windows process loopback), including the processes it started.
        let pid=windows_pids().into_iter().find(|(_,n)|n.eq_ignore_ascii_case(&cfg.process)).map(|(p,_)|p).with_context(||format!("{} is not running",cfg.process))?;
        report(&cfg.process);
        (AudioClient::new_application_loopback_client(pid,true)?,String::new())
    }else{
        let device=if cfg.device.is_empty(){enumerator.get_default_device(direction)?}else{enumerator.get_device(&cfg.device)?};
        let id=device.get_id()?;report(&device.get_friendlyname()?);
        (device.get_iaudioclient()?,id)
    };
    // WASAPI's shared-mode converter performs downmix and resampling once.
    let format=WaveFormat::new(32,32,SampleType::Float,16000,1,None);
    client.initialize_client(&format,Direction::Capture,StreamMode::EventsShared{autoconvert:true,buffer_duration_hns:200000},None)?;
    let event=client.set_get_eventhandle()?;
    let capture=client.get_audiocaptureclient()?;
    let mut queue:VecDeque<u8>=VecDeque::new();
    let mut frame=Vec::with_capacity(320);let mut checked=Instant::now();let mut last_packet=Instant::now();
    client.start_stream()?;
    let result=(||->Result<()>{while !stop.load(Ordering::Relaxed)&&!changed(){
        let _=event.wait_for_event(100);
        loop{
            if capture.get_next_packet_size()?.unwrap_or(0)==0{break;}
            capture.read_from_device_to_deque(&mut queue)?;
            last_packet=Instant::now();
            while queue.len()>=4{
                let sample=[queue.pop_front().unwrap(),queue.pop_front().unwrap(),queue.pop_front().unwrap(),queue.pop_front().unwrap()];
                frame.push(f32::from_le_bytes(sample));
                if frame.len()==320{give(&mut frame,Instant::now());frame.clear();}
            }
        }
        // Some loopback devices emit no packets during silence; advance VAD
        // with real elapsed silence so the final utterance is still flushed.
        if last_packet.elapsed()>=Duration::from_millis(100){let frames=(last_packet.elapsed().as_millis()/20).min(50);for _ in 0..frames{last_packet+=Duration::from_millis(20);give(&mut [0.;320],last_packet);}}
        if checked.elapsed()>=Duration::from_secs(2){
            checked=Instant::now();
            if !app&&cfg.device.is_empty()&&enumerator.get_default_device(direction)?.get_id()?!=device_id{bail!("Default audio device changed; reconnecting");}
        }
    }Ok(())})();
    let stopped=client.stop_stream();result?;stopped?;Ok(())
}
pub fn read_wav(path:&Path)->Result<Vec<f32>>{
    let mut reader=hound::WavReader::open(path).context("Open PCM/float WAV audio")?;
    let spec=reader.spec();
    if spec.channels==0||reader.duration()>120*spec.sample_rate{bail!("Use a WAV sample of at most 120 seconds");}
    let raw:Vec<f32>=match spec.sample_format{hound::SampleFormat::Float=>reader.samples::<f32>().collect::<std::result::Result<Vec<_>,_>>()?,hound::SampleFormat::Int=>{let scale=2f32.powi(spec.bits_per_sample as i32-1);reader.samples::<i32>().map(|s|s.map(|v|v as f32/scale)).collect::<std::result::Result<Vec<_>,_>>()?}};
    let mono:Vec<f32>=raw.chunks_exact(spec.channels as usize).map(|f|f.iter().sum::<f32>()/spec.channels as f32).collect();
    if spec.sample_rate==16000{return Ok(mono);}
    if mono.is_empty(){return Ok(Vec::new());}
    let count=(mono.len() as u64*16000/spec.sample_rate as u64) as usize;
    Ok((0..count).map(|i|{let p=i as f64*spec.sample_rate as f64/16000.;let n=p as usize;let f=(p-n as f64) as f32;mono[n]*(1.-f)+mono[(n+1).min(mono.len()-1)]*f}).collect())
}
