//! Interface text. Keys are semantic (`area.name`) and every language catalog must define the same set.
use std::{collections::HashMap,fmt::Display,sync::OnceLock};
pub const LANGS:[&str;3]=["zh-TW","en","ja"];
/// Error categories set by the engine and GUI. Each has `err.<key>` and `fix.<key>` text.
pub const ERROR_KEYS:[&str;10]=["settings","asr_load","translation_load","audio","asr","translation","translation_test","file","download","overlay"];
type Catalog=HashMap<String,String>;
fn parse(text:&str)->Catalog{serde_json::from_str(text.trim_start_matches('\u{feff}')).expect("UI catalog")}
fn catalogs()->&'static [Catalog;3]{static C:OnceLock<[Catalog;3]>=OnceLock::new();C.get_or_init(||[parse(include_str!("../locales/zh-TW.json")),parse(include_str!("../locales/en.json")),parse(include_str!("../locales/ja.json"))])}
pub fn t(lang:i32,key:&str)->String{catalogs()[lang.clamp(0,2) as usize].get(key).cloned().unwrap_or_else(||key.into())}
/// Replaces `{0}`, `{1}`… with the given values.
pub fn tf(lang:i32,key:&str,args:&[&dyn Display])->String{let mut text=t(lang,key);for (i,arg) in args.iter().enumerate(){text=text.replace(&format!("{{{i}}}"),&arg.to_string());}text}
pub fn index(code:&str)->Option<i32>{LANGS.iter().position(|l|*l==code).map(|i|i as i32)}
/// Interface language from the Windows display language: Japanese, Chinese or English.
pub fn system_language()->i32{match unsafe{windows::Win32::Globalization::GetUserDefaultUILanguage()}&0x3ff{0x11=>2,0x04=>0,_=>1}}

#[cfg(test)]mod tests{
    use super::*;
    fn keys(c:&Catalog)->std::collections::BTreeSet<&str>{c.keys().map(String::as_str).collect()}
    fn placeholders(v:&str)->Vec<String>{let mut out:Vec<String>=(0..10).map(|i|format!("{{{i}}}")).filter(|p|v.contains(p.as_str())).collect();out.sort();out}
    fn cjk(c:char)->bool{('\u{3040}'..='\u{30ff}').contains(&c)||('\u{4e00}'..='\u{9fff}').contains(&c)}
    #[test]fn catalogs_define_identical_keys(){let c=catalogs();for other in &c[1..]{let (a,b)=(keys(&c[0]),keys(other));assert_eq!(a.difference(&b).collect::<Vec<_>>(),Vec::<&&str>::new(),"missing in a translation");assert_eq!(b.difference(&a).collect::<Vec<_>>(),Vec::<&&str>::new(),"missing in zh-TW");}}
    #[test]fn values_are_nonempty_and_keep_placeholders(){let c=catalogs();for (key,zh) in &c[0]{for cat in c{let v=&cat[key];assert!(!v.trim().is_empty(),"empty {key}");assert_eq!(placeholders(zh),placeholders(v),"placeholders differ for {key}");}}}
    #[test]fn interface_copy_has_no_semicolons(){for cat in catalogs(){for (key,value) in cat{assert!(!value.contains([';','；']),"semicolon in {key}: {value}");}}}
    #[test]fn traditional_chinese_has_no_simplified_forms(){const SIMPLIFIED:&str="这们时设载统选择简网络帮显实现应语录间关开发启务资讯数运历过错误响";for (key,value) in &catalogs()[0]{for c in SIMPLIFIED.chars(){assert!(!value.contains(c),"simplified {c} in {key}: {value}");}}}
    #[test]fn japanese_reads_as_japanese(){for (key,value) in &catalogs()[2]{if value.chars().any(cjk){assert!(value.chars().any(|c|('\u{3040}'..='\u{30ff}').contains(&c))||value.chars().count()<=6,"ja text without kana for {key}: {value}");}}}
    #[test]fn english_has_no_cjk(){for (key,value) in &catalogs()[1]{assert!(!value.chars().any(cjk),"CJK in English {key}: {value}");}}
    #[test]fn every_slint_key_exists(){let source=include_str!("../ui/app.slint");let mut count=0;for part in source.split("I18n.tr(\"").skip(1){let key=part.split('"').next().unwrap();count+=1;assert!(catalogs()[0].contains_key(key),"Missing key from app.slint: {key}");}assert!(count>150,"only {count} keys found");}
    #[test]fn every_rust_key_exists(){
        let prefixes:std::collections::HashSet<&str>=catalogs()[0].keys().filter_map(|k|k.split('.').next()).collect();
        for source in [include_str!("gui.rs"),include_str!("main.rs")]{for literal in source.split('"').skip(1).step_by(2){
            let Some((prefix,rest))=literal.split_once('.')else{continue};
            if !prefixes.contains(prefix)||rest.is_empty()||!literal.chars().all(|c|c.is_ascii_lowercase()||c.is_ascii_digit()||c=='_'||c=='.'){continue;}
            assert!(catalogs()[0].contains_key(literal),"Missing key from Rust: {literal}");
        }}
        for key in ERROR_KEYS{for prefix in ["err","fix"]{assert!(catalogs()[0].contains_key(&format!("{prefix}.{key}")),"Missing {prefix}.{key}");}}
    }
    #[test]fn slint_has_no_hardcoded_copy(){
        let source=include_str!("../ui/app.slint");
        for part in source.split("text: \"").skip(1){let literal=part.split('"').next().unwrap();assert!(literal=="Sonivra"||literal.starts_with("\\u{"),"Hard-coded text in app.slint: {literal}");}
        for part in source.split("title: \"").skip(1){let literal=part.split('"').next().unwrap();assert_eq!(literal,"Sonivra");}
    }
    #[test]fn format_replaces_arguments(){assert_eq!(tf(1,"unit.mb",&[&"12.5"]),"12.5 MB");}
}
