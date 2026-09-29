//! write 子命令 —— 单文件局部写（移植自 src/cli/write.ts）
use std::collections::HashMap;

use crate::cli::args::{int_flag, parse_args, wants_help, Flag, UsageError};
use crate::cli::io::CommandIO;
use crate::tag::read::{read_tags, Picture};
use crate::tag::write::intent::{
    diff_fields, format_diff, format_json_diff, format_json_diff_full,
    is_unset_key, merge_fields, preview_view, sniff_image_mime, WritableFields,
};

pub const WRITE_USAGE: &str = r#"用法: music-robot write <file> [字段...] [--preview|--bak|--json]

修改单个音乐文件 | 局部编辑语义：只动你点名的字段，其余帧（含未知帧/APEv2）
原样保留；想彻底清空用 blank。ID3v1 仅当原文件存在时同步（点名覆盖+原值继承）。

字段:
  --title T             --artist A（可多次）    --album ALB
  --album-artist AA     --track N               --track-total N
  --disc N              --disc-total N          --year Y
  --genre G（可多次）     --composer C（可多次）
  --comment S           --lyrics S              --lyrics-timed S
  --cover <图>          嵌入封面（JPEG/PNG）      --unset a,b,c   清空字段
  --unset-cover         删除封面

安全:
  --preview   只算差异不落盘（文件字节不变）
  --bak       写前备份 <file>.bak
  --json      输出 {diffs, audioHashOk}
  -h, --help  帮助"#;

const WRITE_FLAGS: &[&str] = &[
    "title", "artist", "album", "album-artist", "track", "track-total", "disc", "disc-total",
    "year", "genre", "composer", "comment", "lyrics", "lyrics-timed", "cover",
    "unset", "unset-cover", "preview", "bak", "json", "verbose",
];

type Flags = HashMap<String, Flag>;

/// 单值（重复出现 = 用法错误）
fn one(flags: &Flags, key: &str) -> Result<Option<String>, UsageError> {
    let Some(f) = flags.get(key) else { return Ok(None) };
    Ok(f.single(key)?)
}
fn many(flags: &Flags, key: &str) -> Option<Vec<String>> {
    flags.get(key).and_then(|f| f.multi())
}
fn num(flags: &Flags, key: &str) -> Result<Option<i64>, UsageError> {
    let Some(f) = flags.get(key) else { return Ok(None) };
    Ok(int_flag(Some(f), key)?)
}
fn b(flags: &Flags, key: &str) -> bool {
    flags.get(key).map(|f| f.as_bool()).unwrap_or(false)
}

/// flags → 原始意图。只输出「点名」的字段：给了值 → 赋值；--unset → 记入 unset。
/// 未点名的一律 None → 写层原样保留（不重编码、不触碰）。
pub fn fields_from_flags(flags: &Flags) -> Result<WritableFields, UsageError> {
    let mut f = WritableFields::default();
    f.title = one(flags, "title")?;
    f.artists = many(flags, "artist");
    if let Some(al) = one(flags, "album")? { f.albums = Some(vec![al]) }
    f.album_artist = one(flags, "album-artist")?;
    f.track = num(flags, "track")?;
    f.track_total = num(flags, "track-total")?;
    f.disc = num(flags, "disc")?;
    f.disc_total = num(flags, "disc-total")?;
    f.year = one(flags, "year")?;
    f.genres = many(flags, "genre");
    f.composers = many(flags, "composer");
    f.comment = one(flags, "comment")?;
    f.lyrics = one(flags, "lyrics")?;
    f.lyrics_timed = one(flags, "lyrics-timed")?;

    if let Some(u) = one(flags, "unset")? {
        let keys: Vec<String> = u.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        for k in &keys {
            if !is_unset_key(k) { return Err(UsageError::new(format!("--unset 未知字段: {k}"))) }
        }
        f.unset = keys;
    }
    if b(flags, "unset-cover") { f.unset_cover = true }
    if let Some(cp) = one(flags, "cover")? {
        let data = std::fs::read(&cp).map_err(|_| UsageError::new(format!("封面文件不存在: {cp}")))?;
        let Some(mime) = sniff_image_mime(&data) else {
            return Err(UsageError::new(format!("封面不是 JPEG/PNG/GIF: {cp}")))
        };
        f.replace_cover = Some(Picture { mime_type: mime, pic_type: 3, description: "front".into(), data });
    }
    Ok(f)
}

pub fn run_write(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) { io.log(WRITE_USAGE); return Ok(0) }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(WRITE_FLAGS, "见 --help 的字段清单")?;
    parsed.require(1, WRITE_USAGE)?;
    let file = parsed.positionals[0].clone();
    if !std::path::Path::new(&file).exists() { io.error(&format!("文件不存在: {file}")); return Ok(1) }

    let fields = fields_from_flags(&parsed.flags)?;
    let current = match read_tags(std::path::Path::new(&file)) {
        Ok(m) => m,
        Err(e) => { io.error(&format!("读取失败: {e}")); return Ok(1) }
    };
    let intent = merge_fields(fields);
    let view = preview_view(&current, &intent);
    let (preview, json_out, bak) = (b(&parsed.flags, "preview"), b(&parsed.flags, "json"), b(&parsed.flags, "bak"));

    if preview {
        let diffs = diff_fields(&current, &view);
        if json_out { io.log(&format_json_diff(&file, true, &diffs)) } else { io.log(&format_diff(&diffs)) }
        return Ok(0);
    }

    if bak {
        let bk = format!("{file}.bak");
        if std::fs::copy(&file, &bk).is_err() { io.error(&format!("备份失败: {bk}")); return Ok(1) }
    }
    if let Err(e) = crate::tag::write::write_tags(std::path::Path::new(&file), &intent) {
        io.error(&format!("写入失败: {e}"));
        return Ok(1);
    }
    let after = read_tags(std::path::Path::new(&file)).unwrap_or_else(|_| current.clone());
    let diffs = diff_fields(&current, &crate::tag::write::AfterView::from(&after));
    if json_out {
        io.log(&format_json_diff_full(&file, &current, &after, &diffs));
    } else {
        io.log(&format_diff(&diffs));
        io.log(&format!("已写入: {file}（音频 hash 校验通过 ✓）"));
    }
    Ok(0)
}
