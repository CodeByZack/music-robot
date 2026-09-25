//! 参数解析测试 —— 移植自 tests/cli/args.test.ts（8 例 + 1 辅助）
use music_tag::cli::args::{parse_args, int_flag, UsageError};

fn v(args: &[&str]) -> music_tag::cli::args::ParsedArgs { parse_args(args).unwrap() }
fn f(args: &[&str], k: &str) -> music_tag::cli::args::Flag { v(args).flags.get(k).cloned().unwrap_or_default() }

#[test] fn a01_inline_equals() {
    let p = v(&["--title=新标题", "x.mp3"]);
    assert_eq!(p.flags["title"].values, vec!["新标题".to_string()]);
    assert!(p.flags.get("title-total").is_none(), "不存在的键不得有默认值");
    assert_eq!(p.positionals, vec!["x.mp3".to_string()]);
}

#[test] fn a02_space_form_equivalent() {
    let a = v(&["--title", "新标题"]);
    let b = v(&["--title=新标题"]);
    assert_eq!(a.flags["title"].values, b.flags["title"].values, "--key value 与 --key=value 必须等价");
    assert!(!a.flags["title"].bare && !b.flags["title"].bare);
    assert_eq!(a.positionals, b.positionals);
}

#[test] fn a03_repeated_flag_collects() {
    let f = f(&["--artist", "歌手A", "--artist", "歌手B"], "artist");
    assert_eq!(f.multi(), Some(vec!["歌手A".to_string(), "歌手B".to_string()]), "重复 flag 必须收集成数组");
}

#[test] fn a04_mixed_positionals_and_flags() {
    let p = v(&["a.mp3", "--title", "T", "--track", "5", "b.mp3"]);
    assert_eq!(p.positionals, vec!["a.mp3".to_string(), "b.mp3".to_string()]);
    assert_eq!(p.flags["title"].values, vec!["T".to_string()]);
    assert_eq!(p.flags["track"].values, vec!["5".to_string()]);
}

#[test] fn a05_empty_inline_value_is_empty_string() {
    let f = f(&["--title="], "title");
    assert_eq!(f.values, vec![String::from("")], "--key= 必须是空串而不是被吞掉");
    assert!(!f.bare);
}

#[test] fn a06_bare_boolean_flag() {
    let f = f(&["--preview"], "preview");
    assert!(f.bare && f.as_bool());
    // 紧随其后的 flag 不得被当成值
    let p = v(&["--preview", "--json"]);
    assert!(p.flags["preview"].bare && p.flags["json"].bare);
    assert!(p.flags["json"].values.is_empty(), "--json 不得把 --preview 的值吃进来");
}

#[test] fn a07_unknown_flag_not_an_error() {
    let f = f(&["--whatever", "x"], "whatever");
    assert_eq!(f.values, vec!["x".to_string()], "未知 flag 不在解析层报错，合法性由命令层校验");
}

#[test] fn a08_kebab_case_preserved() {
    let f = f(&["--track-total", "12"], "track-total");
    assert_eq!(f.values, vec!["12".to_string()]);
}

#[test] fn a09_extra_bare_flag_rejected_by_single() {
    let f = f(&["--title", "A", "--title", "B"], "title");
    assert!(matches!(f.single("title"), Err(UsageError(m)) if m.contains("只能出现一次")));
}

#[test] fn a10_int_flag_js_semantics() {
    assert_eq!(int_flag(Some(&f(&["--track", "5"], "track")), "track").unwrap(), Some(5));
    assert_eq!(int_flag(Some(&f(&["--track", "5/12"], "track")), "track").unwrap(), Some(5), "JS parseInt 取前缀数字");
    assert_eq!(int_flag(Some(&f(&["--track", ""], "track")), "track").unwrap(), None, "空串 = 未给");
    assert!(int_flag(Some(&f(&["--track", "abc"], "track")), "track").is_err(), "非数字必须报用法错误");
    assert_eq!(int_flag(Some(&f(&["--track"], "track")), "track").unwrap(), None, "裸 flag 视为未给");
    assert_eq!(int_flag(None, "track").unwrap(), None);
}

#[test] fn a11_empty_flag_is_usage_error() {
    assert!(parse_args(&["--"]).is_err(), "-- 单独出现 = 空 flag");
}

#[test] fn a12_bool_from_inline_string() {
    assert!(f(&["--json=true"], "json").as_bool());
    assert!(!f(&["--json=false"], "json").as_bool());
    assert!(f(&["--json"], "json").as_bool());
}

#[test] fn a13_reject_unknown_via_parsed_args() {
    let p = v(&["--whatever", "x"]);
    let e = p.reject_unknown(&["title"], "支持: --title").err().unwrap();
    assert!(e.0.contains("未知选项") && e.0.contains("whatever"), "应指明是哪个 flag：{}", e.0);
}

#[test] fn a14_require_positionals() {
    assert!(v(&["x.mp3"]).require(1, "U").is_ok());
    assert!(v(&[]).require(1, "U").is_err(), "缺位置参数必须报用法错误");
    assert!(v(&["a", "b"]).require(1, "U").is_err(), "多给也必须报错");
}
