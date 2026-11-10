#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod gui;
mod tray;
fn run()->anyhow::Result<()>{
    let args:Vec<String>=std::env::args().skip(1).collect();
    let value=|key:&str|args.iter().position(|s|s==key).and_then(|i|args.get(i+1));
    let home=value("--home").map(std::path::PathBuf::from).or_else(||std::env::var_os("SONIVRA_HOME").map(std::path::PathBuf::from))
        // Portable mode: a file named `portable` beside the executable keeps settings, models and runtimes in that folder.
        .or_else(||std::env::current_exe().ok()?.parent().filter(|d|d.join("portable").is_file()).map(std::path::PathBuf::from)).unwrap_or_else(||sonivra::config::default_path().parent().unwrap().into());
    let config_path=value("--config").map(std::path::PathBuf::from).unwrap_or_else(||home.join("config.toml"));
    let mut cfg=sonivra::config::Config::load(&config_path)?;cfg.resolve(&home);
    if let Some(result)=sonivra::cli::run(&args,&cfg,&home)?{
        let text=serde_json::to_string_pretty(&result)?;
        if let Some(path)=value("--output"){std::fs::write(path,&text)?;}
        println!("{text}");return Ok(());
    }
    gui::run(cfg,home,config_path,&args)
}
fn main(){if let Err(error)=run(){eprintln!("{error:#}");let path=sonivra::config::default_path().with_file_name("last-error.txt");if let Some(parent)=path.parent(){let _=std::fs::create_dir_all(parent);}let _=std::fs::write(path,format!("{error:#}"));if !std::env::args().any(|s|s.starts_with("--")){rfd::MessageDialog::new().set_title(sonivra::i18n::t(sonivra::i18n::system_language(),"dialog.startup_error")).set_description(format!("{error:#}")).set_level(rfd::MessageLevel::Error).show();}std::process::exit(1);}}
