//! ID3v2 帧级编辑器（局部编辑语义）—— 移植自 src/tag/write/id3v2-editor.ts
//!
//! 原则：只动「点名」的字段，其余帧（含未知帧）**payload 原样保留**，重编码为 v2.4 帧头。
//! 帧组粒度：track/trackTotal 共用 TRCK、disc/discTotal 共用 TPOS —— 这是 ID3 规范本身
//! 就把「号 / 总数」写在同一帧里。
//!
//! ## 歌词为什么必须一对一
//!
//! 早期版本把 USLT 和 SYLT 都算作 `lyrics` 字段，靠 description 区分「纯歌词 / 带时间轴」。
//! 两个后果都很严重：
//!
//!   1. `field_of_frame` 把 SYLT 也映射成 `"lyrics"`，于是**改一次纯歌词就把 SYLT 帧删了**；
//!   2. 读写两处对 description 的用法是相反的，同一个字段前后指的不是一回事。
//!
//! 现在按 ID3 规范各归各的帧：`USLT` = 无时间轴歌词 → `lyrics`，
//! `SYLT` = 有时间轴歌词 → `lyrics_timed`。两者不再共享帧，`GROUP_ALIASES` 里也就不需要歌词。
use super::super::read::{decode_sylt, decode_txxx, latin1_encode, Picture, RawFrame};

/// 编辑意图。`Option` = TS 的 `!== undefined`（未点名即 None），用于决定"替换/删除/保留"。
#[derive(Debug, Clone, Default)]
pub struct Id3EditMeta {
    pub title: Option<String>,
    pub artists: Option<Vec<String>>,
    pub albums: Option<Vec<String>>,
    pub album_artist: Option<String>,
    pub track: Option<i64>,
    pub track_total: Option<i64>,
    pub disc: Option<i64>,
    pub disc_total: Option<i64>,
    pub year: Option<String>,
    pub genres: Option<Vec<String>>,
    pub composers: Option<Vec<String>>,
    pub lyrics: Option<String>,
    pub lyrics_timed: Option<String>,
    pub comment: Option<String>,
    pub pictures: Option<Vec<Picture>>,
    pub mbid_artist: Option<String>, pub mbid_release: Option<String>, pub mbid_track: Option<String>,
    pub mbid_group: Option<String>, pub mbid_disc: Option<String>, pub isrc: Option<String>,
    /// 要删除的字段名（camelCase，与 field_of_frame 返回值同名）
    pub unset_fields: Vec<String>,
    /// blank：重建成标准空标签（ID3v2.4 零帧 + 空 ID3v1；APEv2 一并清除）。
    /// 放在编辑元数据里而非外层包装——TS 的 WriteMeta extends Id3EditMeta 是同一形态，
    /// Rust 用包装类型会让测试无法按字段赋值（DerefMut 不可得），得不偿失。
    pub blank_all: bool,
}

fn trim_nul_tail(s: &str) -> String { s.trim_end_matches('\u{0}').to_string() }
fn utf8_bytes(s: &str) -> Vec<u8> { s.as_bytes().to_vec() }

/// 通用 v2.4 帧壳：id(4) + size(syncsafe 4) + flags(2=0) + payload
fn frame(id: &str, body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(10 + body.len());
    out.extend_from_slice(&latin1_encode(id));
    while out.len() < 4 { out.push(b' ') } // pad3 升格：3 字符 ID 补空格到 4
    if out.len() > 4 { out.truncate(4) }
    out.extend_from_slice(&super::super::read::write_sync_safe(body.len() as i64));
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(body);
    out
}

/// 文本帧（统一 UTF-8, encoding=3）
pub fn text_frame(id: &str, text: &str) -> Vec<u8> {
    let mut data = vec![3u8];
    data.extend_from_slice(&utf8_bytes(&trim_nul_tail(text)));
    frame(id, &data)
}

/// COMM 帧（UTF-8, lang=eng, desc 空）
fn com_frame(text: &str) -> Vec<u8> {
    let mut b = vec![3u8];
    b.extend_from_slice(b"eng"); b.push(0);
    b.extend_from_slice(&utf8_bytes(&trim_nul_tail(text)));
    frame("COMM", &b)
}

/// USLT 帧（无时间轴歌词）。description 留空 —— 这是绝大多数打标器的做法
///（实测本项目所有下载器产物都是空 desc），自造一个 desc 只会让别的播放器认不出来。
fn uslt_frame(text: &str) -> Vec<u8> {
    let mut b = vec![3u8];
    b.extend_from_slice(b"eng");
    b.push(0); // 空 description
    b.extend_from_slice(&utf8_bytes(&trim_nul_tail(text)));
    frame("USLT", &b)
}

/// LRC 文本 → `(毫秒, 文本)` 列表。
///
/// 只认 `[mm:ss]` / `[mm:ss.xx]` / `[m:ss.xxx]` 这几种。`[ti:]` / `[offset:0]` 这类
/// LRC 元数据行解析不出数字，会自然被跳过，不需要特判。
/// **没有时间轴的行直接丢掉** —— SYLT 的每一条都必须带时间戳，留下也只是无主文本。
pub(crate) fn parse_lrc(lrc: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for line in lrc.lines() {
        let Some(close) = line.find(']') else { continue };
        if !line.starts_with('[') { continue }
        let text = line[close + 1..].trim();
        if text.is_empty() { continue }
        let Some((min, sec)) = line[1..close].split_once(':') else { continue };
        let (Ok(min), Ok(sec)) = (min.trim().parse::<u64>(), sec.trim().parse::<f64>()) else { continue };
        out.push((min * 60_000 + (sec * 1000.0).round() as u64, text.to_string()));
    }
    out
}

/// SYLT 帧（有时间轴歌词）。时间戳格式选 2（毫秒），内容类型 1（lyrics）。
///
/// 返回 `None` = 这段文本里没有一行带时间轴。**调用方要把它当错误**，
/// 不能静默跳过 —— 用户以为存上了、实际什么都没写，是最坏的一种失败。
fn sylt_frame(lrc: &str) -> Option<Vec<u8>> {
    let pairs = parse_lrc(lrc);
    if pairs.is_empty() { return None }
    let mut b = vec![3u8]; // encoding = UTF-8
    b.extend_from_slice(b"eng"); // language
    b.push(2); // 时间戳格式 = 毫秒
    b.push(1); // 内容类型 = lyrics
    b.push(0); // 空 description
    for (ms, text) in pairs {
        b.extend_from_slice(&utf8_bytes(&trim_nul_tail(&text)));
        b.push(0);
        b.extend_from_slice(&(ms as u32).to_be_bytes());
    }
    Some(frame("SYLT", &b))
}

/// APIC 帧（encoding=0, mime NUL 结尾, type, desc NUL 结尾）
fn apic_frame(pic: &Picture) -> Vec<u8> {
    let mime = if pic.mime_type.is_empty() { "image/jpeg".to_string() } else { pic.mime_type.clone() };
    let desc = trim_nul_tail(&pic.description);
    let mut head: Vec<u8> = vec![0u8];
    head.extend_from_slice(&latin1_encode(&mime)); head.push(0);
    head.push(pic.pic_type);
    head.extend_from_slice(&latin1_encode(&desc)); head.push(0);
    let mut data = head;
    data.extend_from_slice(&pic.data);
    frame("APIC", &data)
}

/// TXXX 帧（键值，统一 UTF-8）
fn txxx_frame(key: &str, value: &str) -> Vec<u8> {
    let mut b = vec![3u8];
    b.extend_from_slice(&latin1_encode(key)); b.push(0);
    b.extend_from_slice(&utf8_bytes(&trim_nul_tail(value)));
    frame("TXXX", &b)
}

/// 帧 id → 字段名（决定替换还是保留）。TXXX 按 key 细分；其它未知帧返回 None = 原样保留。
pub fn field_of_frame(f: &RawFrame) -> Option<&'static str> {
    Some(match f.frame_id.as_str() {
        "TIT2" => "title",
        "TPE1" | "TPE4" => "artists",   // TPE4 兼容旧分帧文件（新写不再产生）
        "TPE2" => "albumArtist",        // TPE2 = 专辑艺术家（ID3v2 标准字段）
        "TALB" => "albums",
        "TRCK" => "track",
        "TPOS" => "disc",
        "TDRC" | "TYER" => "year",
        "TCON" => "genres",
        "TCOM" | "TEXT" => "composers",
        "COMM" => "comment",
        // 歌词两种帧各归各的字段，严格一对一（见文件头「歌词为什么必须一对一」）。
        "USLT" => "lyrics",
        // SYLT 只在**能读成 LRC** 时才算已知字段：读不出来的（时间戳用 MPEG 帧号）
        // 必须当未知帧原样保留，否则改一次歌词就把它删了。
        "SYLT" => {
            if decode_sylt(&f.data).is_some() { "lyricsTimed" } else { return None }
        }
        "APIC" => "pictures",
        "TSRC" => "isrc",
        "TXXX" => {
            let key = decode_txxx(&f.data).map(|t| t.key).unwrap_or_default();
            match key.as_str() {
                "MusicBrainz Artist Id" => "musicBrainzArtistId",
                "MusicBrainz Release Id" => "musicBrainzReleaseId",
                "MusicBrainz Track Id" => "musicBrainzTrackId",
                "MusicBrainz Release Group Id" => "musicBrainzReleaseGroupId",
                "MusicBrainz Disc Id" => "musicBrainzDiscId",
                "ALBUM ARTIST" => "albumArtist",
                "ISRC" => "isrc",
                _ => return None, // 其它 TXXX：未知字段，保留
            }
        }
        _ => return None, // 未知帧：原样保留
    })
}

/// 字段是否被点名（给了值 或 在 unsetFields 里）
fn is_touched(m: &Id3EditMeta, field: &str) -> bool {
    if m.unset_fields.iter().any(|x| x == field) { return true }
    let present = |o: &Option<String>| o.is_some();
    let list_present = |o: &Option<Vec<String>>| o.is_some();
    let num_present = |o: &Option<i64>| o.is_some();
    match field {
        "title" => present(&m.title), "artists" => list_present(&m.artists),
        "albums" => list_present(&m.albums), "albumArtist" => present(&m.album_artist),
        "track" => num_present(&m.track), "trackTotal" => num_present(&m.track_total),
        "disc" => num_present(&m.disc), "discTotal" => num_present(&m.disc_total),
        "year" => present(&m.year), "genres" => list_present(&m.genres),
        "composers" => list_present(&m.composers), "comment" => present(&m.comment),
        "lyrics" => present(&m.lyrics), "lyricsTimed" => present(&m.lyrics_timed),
        "pictures" => m.pictures.is_some(), "isrc" => present(&m.isrc),
        "musicBrainzArtistId" => present(&m.mbid_artist),
        "musicBrainzReleaseId" => present(&m.mbid_release),
        "musicBrainzTrackId" => present(&m.mbid_track),
        "musicBrainzReleaseGroupId" => present(&m.mbid_group),
        "musicBrainzDiscId" => present(&m.mbid_disc),
        _ => false,
    }
}

/// 共享同一帧组的字段：点名其中任一个，整组按现值重写/删除。
///
/// 只剩编号这两组 —— ID3 本身就把编号与总数写在同一个帧里（`TRCK="5/12"`）。
/// 歌词**不在**此列：USLT 与 SYLT 是两个独立帧，各改各的（见文件头）。
const GROUP_ALIASES: &[(&str, &[&str])] = &[
    ("track", &["track", "trackTotal"]),
    ("disc", &["disc", "discTotal"]),
];
fn field_touched(m: &Id3EditMeta, field: &str) -> bool {
    let aliases = GROUP_ALIASES.iter().find(|(k, _)| *k == field).map(|(_, v)| *v).unwrap_or(std::slice::from_ref(&field));
    aliases.iter().any(|a| is_touched(m, a))
}

#[derive(Debug, Clone, Copy, Default)]
struct CurPair { a: Option<i64>, b: Option<i64> }

/// 文本帧 payload → 文本（支持 UTF-8/latin1 与 UTF-16 BOM；**在首个 NUL 截断**）
fn frame_text(data: &[u8]) -> String {
    let enc = data.first().copied().unwrap_or(0);
    if enc == 1 || enc == 2 {
        let mut le = true; let mut p = 2usize;
        if data.len() >= 3 && data[1] == 0xFF && data[2] == 0xFE { le = true; p = 3 }
        else if data.len() >= 3 && data[1] == 0xFE && data[2] == 0xFF { le = false; p = 3 }
        let mut units: Vec<u16> = Vec::new();
        let mut i = p;
        while i + 1 < data.len() {
            let c = if le { u16::from_le_bytes([data[i], data[i + 1]]) } else { u16::from_be_bytes([data[i], data[i + 1]]) };
            if c == 0 { break }
            units.push(c); i += 2;
        }
        return String::from_utf16_lossy(&units)
    }
    let s = super::super::read::latin1_decode(&data[1.min(data.len())..]);
    s.split('\u{0}').next().unwrap_or("").to_string()
}

/// '5/12' 或 '5' → {a:5,b:12}
fn pair_of(s: &str) -> CurPair {
    let t = s.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '/') { return CurPair::default() }
    let mut it = t.split('/');
    let parse = |x: Option<&str>| -> Option<i64> { x.filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit())).and_then(|v| v.parse::<i64>().ok()).filter(|n| *n != 0) };
    CurPair { a: parse(it.next()), b: parse(it.next()) }
}

fn cur_of(frames: Option<&[RawFrame]>, id: &str) -> CurPair {
    match frames.and_then(|fs| fs.iter().find(|x| x.frame_id == id)) { Some(f) => pair_of(&frame_text(&f.data)), None => CurPair::default() }
}

/// 该字段被替换时构建的新帧
fn build_field_frames(m: &Id3EditMeta, cur: (CurPair, CurPair)) -> Vec<Vec<u8>> {
    let (cur_track, cur_disc) = cur;
    let mut out: Vec<Vec<u8>> = Vec::new();
    if let Some(t) = &m.title { out.push(text_frame("TIT2", t)) }
    // 多值歌手全部并入单个 TPE1（NUL 分隔，ID3v2.4 允许）；TPE2 专用专辑艺术家（REVIEW §2.5）
    let artists: Vec<String> = m.artists.clone().unwrap_or_default().into_iter().filter(|a| !a.is_empty()).collect();
    if !artists.is_empty() { out.push(text_frame("TPE1", &artists.join("\u{0}"))) }
    if let Some(al) = &m.albums { if !al.is_empty() { out.push(text_frame("TALB", &al[0])) } }
    if let Some(aa) = &m.album_artist { if !aa.is_empty() { out.push(text_frame("TPE2", aa)) } }

    // track/trackTotal 任一被点名 → TRCK 整体重写（unset track-total 只去 total）
    if field_touched(m, "track") {
        let t = m.track.or(cur_track.a);
        let unset_total = m.unset_fields.iter().any(|x| x == "trackTotal");
        let tot = if unset_total { None } else { m.track_total.or(cur_track.b) };
        if let Some(v) = t { out.push(text_frame("TRCK", &if let Some(x) = tot { format!("{v}/{x}") } else { v.to_string() })) }
    }
    if field_touched(m, "disc") {
        let d = m.disc.or(cur_disc.a);
        let unset_total = m.unset_fields.iter().any(|x| x == "discTotal");
        let tot = if unset_total { None } else { m.disc_total.or(cur_disc.b) };
        if let Some(v) = d { out.push(text_frame("TPOS", &if let Some(x) = tot { format!("{v}/{x}") } else { v.to_string() })) }
    }
    if let Some(y) = &m.year { if !y.is_empty() { out.push(text_frame("TDRC", y)) } }
    for g in m.genres.clone().unwrap_or_default() { if !g.is_empty() { out.push(text_frame("TCON", &g)) } }
    let composers = m.composers.clone().unwrap_or_default();
    if !composers.is_empty() {
        out.push(text_frame("TCOM", &composers[0]));
        for c in &composers[1..] { out.push(text_frame("TEXT", c)) }
    }
    if let Some(c) = &m.comment { if !c.is_empty() { out.push(com_frame(c)) } }
    if let Some(l) = &m.lyrics { if !l.is_empty() { out.push(uslt_frame(l)) } }
    if let Some(l) = &m.lyrics_timed { if !l.is_empty() { if let Some(f) = sylt_frame(l) { out.push(f) } } }
    if let Some(pics) = &m.pictures { for p in pics { if !p.data.is_empty() { out.push(apic_frame(p)) } } }
    for (key, val) in [
        ("MusicBrainz Artist Id", &m.mbid_artist), ("MusicBrainz Release Id", &m.mbid_release),
        ("MusicBrainz Track Id", &m.mbid_track), ("MusicBrainz Release Group Id", &m.mbid_group),
        ("MusicBrainz Disc Id", &m.mbid_disc),
    ] { if let Some(v) = val { out.push(txxx_frame(key, v)) } }
    if let Some(v) = &m.isrc {
        if !v.is_empty() { out.push(txxx_frame("ISRC", v)) } else { out.push(text_frame("TSRC", "")) }
    }
    out
}

/// 帧级编辑：未点名帧原样保留（payload 不动，重挂 v2.4 帧头），点名帧删旧 + 末尾追加新帧。
pub fn edit_id3v2_frames(orig: Option<&[RawFrame]>, meta: &Id3EditMeta) -> Vec<Vec<u8>> {
    let cur = (cur_of(orig, "TRCK"), cur_of(orig, "TPOS"));
    let mut out: Vec<Vec<u8>> = Vec::new();
    if let Some(frames) = orig {
        for f in frames {
            if let Some(field) = field_of_frame(f) {
                if field_touched(meta, field) { continue } // 被点名：删旧帧
            }
            out.push(frame(&f.frame_id, &f.data));
        }
    }
    out.extend(build_field_frames(meta, cur));
    out
}
