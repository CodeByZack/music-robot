//! blank 子命令 —— 重建成标准空标签（移植自 src/cli/blank.ts）
use crate::cli::args::{parse_args, wants_help, UsageError};
use crate::cli::io::CommandIO;
use crate::tag::read::read_tags;
use crate::tag::write::intent::{blank_view, diff_fields, format_diff, format_json_diff, format_json_diff_full, WriteMeta};
use crate::tag::write::write_tags;

pub const BLANK_USAGE: &str = r#"用法: music-robot blank <file> [--preview|--bak|--json]

把文件重建成标准空标签：ID3v2.4（0 帧）/ 空 Vorbis Comment / 空 ID3v1，
所有标签信息（标题/歌手/封面/歌词/MBID/未知帧/APEv2）全部清空，音频字节不动。
比局部编辑的 write 更彻底：write 只动点名，blank 就是全清。

安全:
  --preview   只算差异不落盘
  --bak       写前备份 <file>.bak
  --json      输出 {diffs, audioHashOk}
  -h, --help  帮助"#;

pub fn run_blank(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) { io.log(BLANK_USAGE); return Ok(0) }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(&["preview", "json", "bak"], "blank 支持: --preview --json --bak")?;
    parsed.require(1, BLANK_USAGE)?;
    let file = parsed.positionals[0].clone();

    let current = match read_tags(std::path::Path::new(&file)) {
        Ok(m) => m,
        Err(e) => { io.error(&format!("读取失败: {e}")); return Ok(1) }
    };
    let preview = parsed.flags.get("preview").map(|f| f.as_bool()).unwrap_or(false);
    let json_out = parsed.flags.get("json").map(|f| f.as_bool()).unwrap_or(false);
    let bak = parsed.flags.get("bak").map(|f| f.as_bool()).unwrap_or(false);

    if preview {
        let diffs = diff_fields(&current, &blank_view());
        if json_out { io.log(&format_json_diff(&file, true, &diffs)) } else { io.log(&format_diff(&diffs)) }
        return Ok(0);
    }

    if bak {
        let b = format!("{file}.bak");
        if std::fs::copy(&file, &b).is_err() { io.error(&format!("备份失败: {b}")); return Ok(1) }
    }
    let intent = WriteMeta { blank_all: true, ..Default::default() };
    if let Err(e) = write_tags(std::path::Path::new(&file), &intent) {
        io.error(&format!("写入失败: {e}"));
        return Ok(1);
    }
    let after = read_tags(std::path::Path::new(&file)).unwrap_or_else(|_| current.clone());
    let diffs = diff_fields(&current, &crate::tag::write::AfterView::from(&after));
    if json_out {
        io.log(&format_json_diff_full(&file, &current, &after, &diffs));
    } else {
        io.log(&format_diff(&diffs));
        io.log(&format!("已写入: {file}（音频 hash 校验通过 ✓，标签已清空）"));
    }
    Ok(0)
}
