//! readTags 统一入口 —— 移植自 src/tag/read/index.ts 的分派与兜底顺序。
use super::metadata::{AudioMetadata, RawFrame, ReadError, Result};
use super::{apev2, flac, id3v1, id3v2, native_probe, wav};
use super::probe::{Format, Probe, probe_format};
use std::path::Path;

/// 走路径沙箱的读取入口（服务端专用）。
///
/// 做法是在**边界处解析一次**：resolve 已把 symlink 全部跟随、并确认目标落在库根内，
/// 之后内部所有 `std::fs::read` 命中的都是那个已验证的规范路径，不可能再逃逸。
/// 代价是不做逐次调用级校验——好处是内部函数签名不变、迁移面最小。
pub fn read_tags_fs(fs: &crate::fs::PathSandbox, path: &Path) -> Result<AudioMetadata> {
    let target = fs.resolve(path).map_err(|e| ReadError::Escape(e.to_string()))?;
    read_tags(&target)
}

pub fn read_tags(path: &Path) -> Result<AudioMetadata> {
    let p = probe_format(path);
    match &p {
        // ID3+FLAC/WAV：拒绝假读（解析不了前缀后的 Vorbis，blank 会静默失败）
        Probe::Id3PrefixedReal(fmt) => return Err(ReadError::Id3PrefixedReal(fmt.ext())),
        Probe::Id3ChainUnresolved => return Err(ReadError::Id3PrefixedUnknown),
        Probe::Plain(Format::Mp3) => return read_mp3(path),
        Probe::Plain(Format::Flac) => return read_flac(path),
        Probe::Plain(Format::Wav) => return read_wav(path),
        Probe::Unknown => {}
    }
    // magic 无信：按扩展名容错（破损/无 sync 的旧文件）
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "mp3" => read_mp3(path),
        "flac" => read_flac(path),
        "wav" => read_wav(path),
        _ => Err(ReadError::Unrecognized),
    }
}

/// ffprobe 可用则用其精确值，否则本地估算（TS probeAudio :192-198）。
fn probe_audio(path: &Path, buf: &[u8], fmt: Format, audio_start: usize) -> native_probe::NativeProbe {
    if let Some(ff) = ffprobe_binary() {
        if let Some(np) = ffprobe_probe(&ff, path) { return np }
    }
    match fmt {
        Format::Flac => native_probe::flac_native_probe(buf).unwrap_or_else(native_probe::NativeProbe::zeroed),
        Format::Wav => native_probe::wav_native_probe(buf),
        Format::Mp3 => native_probe::mp3_native_probe(buf, audio_start),
    }
}

fn ffprobe_binary() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    CACHE.get_or_init(|| {
        if std::env::var_os("TAGWASH_NO_FFPROBE").is_some() { return None }
        for cand in ["/usr/bin/ffprobe", "/usr/local/bin/ffprobe"] {
            let ok = std::process::Command::new(cand).args(["-version"])
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
            if ok { return Some(std::path::PathBuf::from(cand)) }
        }
        None
    }).clone()
}

fn ffprobe_probe(bin: &std::path::Path, path: &Path) -> Option<native_probe::NativeProbe> {
    let out = std::process::Command::new(bin)
        .args(["-v", "quiet", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path).output().ok()?;
    if !out.status.success() { return None }
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    // 零依赖约束下不引 JSON 库到产品代码：ffprobe 输出格式稳定，只需抽 4 个标量。
    // ⚠️ 不能靠 key 出现顺序切区间——带封面的 MP3 里 "streams" 排在 format.duration **之前**
    //   （差分测试实测抓到），且 audio/video 两个 stream 都有 duration。
    //   改为按对象边界精确取：format 段 = 末尾的 "format": {...}；audio 段 = codec_type:audio 所在对象。
    let fmt_seg = json_object_containing(&txt, "\"format\":").unwrap_or_else(|| txt.clone());
    let dur = json_scalar(&fmt_seg, "duration")?.parse::<f64>().ok()?;
    let bit_rate = json_scalar(&fmt_seg, "bit_rate").and_then(|v| v.parse::<i64>().ok());
    let audio_seg = json_object_containing(&txt, "\"codec_type\": \"audio\"")
        .or_else(|| json_object_containing(&txt, "\"codec_type\":\"audio\""))?;
    Some(native_probe::NativeProbe {
        duration_ms: (dur * 1000.0).round() as i64,
        sample_rate: json_scalar(&audio_seg, "sample_rate").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0),
        bits_per_sample: json_scalar(&audio_seg, "bits_per_sample").and_then(|v| v.parse::<u32>().ok()).filter(|b| *b > 0),
        bitrate_bps: bit_rate,
    })
}

/// 返回包含 `anchor` 的那个最内层 JSON 对象的文本（花括号配平）。
/// ⚠️ 全程按 **字节** 索引 &str，且切点只落在 ASCII 字符边界上——
///    ffprobe 输出含中文标签，用 char 计数索引会 panic（差分测试实测抓到过）。
fn json_object_containing(hay: &str, anchor: &str) -> Option<String> {
    let at = hay.find(anchor)?;
    let b = hay.as_bytes();
    // 向后扫到该对象的闭合 '}'：先跳过 anchor 之后可能出现的 '{'，做深度配平
    let mut depth = 0i32;
    let mut end = hay.len();
    let mut i = at + anchor.len();
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => { if depth == 0 { end = i + 1; break } depth -= 1 }
            _ => {}
        }
        i += 1;
    }
    let start = hay[..at].rfind('{')?;
    Some(hay[start..end].to_string())
}

/// 从 `"key": "value"` 或 `"key": number` 里取值（首个命中）。
fn json_scalar(hay: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let at = hay.find(&pat)? + pat.len();
    let rest = hay[at..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        let end = r.find('"')?;
        Some(r[..end].to_string())
    } else {
        let end = rest.find(|c: char| c == ',' || c == '}' || c == '\n').unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

/// 从 ID3v2 帧构造元数据（对应 TS metaFromFrames :66-160）
fn meta_from_frames(frames: &[RawFrame]) -> FramesMeta {
    let mut o = FramesMeta::default();
    let mut tpe1_seen = false;
    for f in frames {
        let dt = || id3v2::trim_trailing_nuls(&id3v2::decode_text(&f.data));
        match f.frame_id.as_str() {
            "TIT2" => { let t = dt(); if o.title.is_none() && !t.is_empty() { o.title = Some(t) } }
            "TPE1" => { if !tpe1_seen {
                    let t = dt();
                    if !t.is_empty() { let mut parts: Vec<String> = t.split('\u{0}').filter(|x| !x.is_empty()).map(str::to_string).collect(); parts.extend(o.artists.drain(..)); o.artists = parts; }
                    tpe1_seen = true; } }
            "TPE2" => { let t = dt(); if !t.is_empty() && o.album_artist.is_none() { o.album_artist = Some(t) } }
            "TPE4" => { let t = dt(); if !t.is_empty() { o.artists.push(t) } }
            "TALB" | "TCOM" | "TEXT" | "TCON" => {
                let t = dt(); if t.is_empty() { continue }
                match f.frame_id.as_str() {
                    "TALB" => o.albums.push(t),
                    "TCOM" | "TEXT" => o.composers.push(t),
                    _ => o.genres.push(id3v2::parse_genre(&t)),
                }
            }
            "TRCK" => { let r = id3v2::parse_number_pair(&dt()); o.track = r.num; o.track_total = r.total }
            "TPOS" => { let r = id3v2::parse_number_pair(&dt()); o.disc = r.num; o.disc_total = r.total }
            "TDRC" | "TYER" | "TDOR" => { let t = dt(); if o.year.is_none() { o.year = (!t.is_empty()).then_some(t) } }
            "USLT" => { if let Some(u) = id3v2::decode_uslt(&f.data) {
                    if !u.text.is_empty() {
                        if u.description == "LYRICS" { if o.lyrics.is_none() { o.lyrics = Some(u.text) } }
                        else if o.lyrics_timed.is_none() { o.lyrics_timed = Some(u.text) } } } }
            "LYRICS" => { let t = dt(); if o.lyrics.is_none() && !t.is_empty() { o.lyrics = Some(t) } }
            "APIC" => { if let Some(pic) = id3v2::decode_apic(&f.data) { o.pictures.push(pic) } }
            "COMM" => { if let Some(c) = id3v2::decode_uslt(&f.data) { if !c.text.is_empty() && o.comment.is_none() { o.comment = Some(c.text) } } }
            "TXXX" => { if let Some(t) = id3v2::decode_txxx(&f.data) {
                    let k = t.key.trim().to_uppercase();
                    if k == "ALBUM ARTIST" && !t.value.is_empty() { o.album_artist = Some(t.value.clone()) }
                    let mk = t.key.trim();
                    if !t.value.is_empty() {
                        match mk {
                            "MusicBrainz Artist Id" => o.mbid_artist = Some(t.value.clone()),
                            "MusicBrainz Release Id" => o.mbid_release = Some(t.value.clone()),
                            "MusicBrainz Track Id" => o.mbid_track = Some(t.value.clone()),
                            "MusicBrainz Release Group Id" => o.mbid_group = Some(t.value.clone()),
                            "MusicBrainz Disc Id" => o.mbid_disc = Some(t.value.clone()),
                            "ISRC" => o.isrc = Some(t.value.clone()),
                            _ => {}
                        } } } }
            _ => {}
        }
    }
    o
}
#[derive(Debug, Default)]
pub struct FramesMeta {
    pub title: Option<String>, pub artists: Vec<String>, pub albums: Vec<String>,
    pub album_artist: Option<String>, pub track: Option<i64>, pub track_total: Option<i64>,
    pub disc: Option<i64>, pub disc_total: Option<i64>, pub year: Option<String>,
    pub genres: Vec<String>, pub composers: Vec<String>, pub lyrics: Option<String>,
    pub lyrics_timed: Option<String>, pub comment: Option<String>,
    pub pictures: Vec<super::metadata::Picture>,
    pub mbid_artist: Option<String>, pub mbid_release: Option<String>, pub mbid_track: Option<String>,
    pub mbid_group: Option<String>, pub mbid_disc: Option<String>, pub isrc: Option<String>,
}

fn strip_ts(s: &str) -> String { s.lines().filter_map(|l| { let i = l.find(']'); i.map(|j| l[j + 1..].to_string()) }).collect::<Vec<_>>().join("\n") }

pub fn read_mp3(path: &Path) -> Result<AudioMetadata> {
    let buf = std::fs::read(path)?;
    let mut m = AudioMetadata::default();
    m.detected_format = Some("mp3".into());
    let id3v2h = id3v2::parse_id3v2(&buf, 0);
    let v1 = id3v1::id3v1_parse(&buf);
    let ape = apev2::parse_ape_tag(&buf);
    let audio_start = id3v2h.as_ref().map(|h| 10 + h.tag_size as usize).unwrap_or(0);
    let np = probe_audio(path, &buf, Format::Mp3, audio_start);
    (m.duration_ms, m.sample_rate, m.bits_per_sample, m.bitrate_bps) = (np.duration_ms, np.sample_rate, np.bits_per_sample, np.bitrate_bps);

    let frames = id3v2h.map(|h| h.frames).unwrap_or_default();
    m.raw_frames = frames.clone();
    m.source = if !frames.is_empty() || id3v2::parse_id3v2(&buf, 0).is_some() { "id3v2".into() }
               else if ape.is_some() { "apev2".into() } else if v1.is_some() { "id3v1".into() } else { "id3v2".into() };
    let fm = meta_from_frames(&frames);
    apply_frames(&mut m, fm);
    if let Some(v1) = &v1 {
        if m.title.as_deref().unwrap_or("").is_empty() && !v1.title.is_empty() { m.title = Some(v1.title.clone()) }
        if m.artists.is_empty() && !v1.artist.is_empty() { m.artists = vec![v1.artist.clone()] }
        if m.albums.is_empty() && !v1.album.is_empty() { m.albums = vec![v1.album.clone()] }
        if m.year.is_none() && !v1.year.is_empty() { m.year = Some(v1.year.clone()) }
        if m.track.is_none() && v1.track_present { m.track = v1.track }
        if m.comment.as_deref().unwrap_or("").is_empty() && !v1.comment.is_empty() { m.comment = Some(v1.comment.clone()) }
        if m.genres.is_empty() { if let Some(g) = v1.genre { let gs = id3v1::id3v1_genres(); if (g as usize) < gs.len() { m.genres = vec![gs[g as usize].to_string()] } } }
    }
    Ok(m)
}

fn apply_frames(m: &mut AudioMetadata, fm: FramesMeta) {
    m.title = fm.title; m.artists = fm.artists; m.albums = fm.albums;
    if fm.album_artist.is_some() { m.album_artist = fm.album_artist }
    m.track = fm.track; m.track_total = fm.track_total; m.disc = fm.disc; m.disc_total = fm.disc_total;
    m.year = fm.year; m.genres = fm.genres; m.composers = fm.composers;
    m.pictures = fm.pictures; m.lyrics_timed = fm.lyrics_timed.clone(); m.comment = fm.comment;
    m.lyrics = fm.lyrics.clone();
    if m.lyrics_timed.is_some() && m.lyrics.is_none() { m.lyrics = Some(strip_ts(m.lyrics_timed.as_deref().unwrap())); }
    m.musicbrainz_artist_id = fm.mbid_artist; m.musicbrainz_release_id = fm.mbid_release;
    m.musicbrainz_track_id = fm.mbid_track; m.musicbrainz_release_group_id = fm.mbid_group;
    m.musicbrainz_disc_id = fm.mbid_disc; m.isrc = fm.isrc;
}

pub fn read_flac(path: &Path) -> Result<AudioMetadata> {
    let buf = std::fs::read(path)?;
    let mut m = AudioMetadata::default();
    m.source = "vorbis".into();
    m.detected_format = Some("flac".into());
    let np = probe_audio(path, &buf, Format::Flac, 0);
    if np.duration_ms == 0 { if let Some(x) = native_probe::flac_native_probe(&buf) { m.sample_rate = x.sample_rate; m.bits_per_sample = x.bits_per_sample; m.duration_ms = x.duration_ms; } }
    else { (m.duration_ms, m.sample_rate, m.bits_per_sample, m.bitrate_bps) = (np.duration_ms, np.sample_rate, np.bits_per_sample, np.bitrate_bps); }
    // ⚠️ fLaC 魔数成立但块序列解析失败 = 文件损坏，必须报错。
    //   早期版本这里 `None => return Ok(m)` 静默降级成「空但合法」的元数据，
    //   导致扫描把损坏 FLAC 判成 ok、blank 还"成功"——TS 参照实现是直接抛
    //   "Attempt to access memory outside buffer bounds"。
    let blocks = match flac::parse_flac_metadata(&buf) {
        Some(b) => b,
        None => return Err(crate::tag::read::ReadError::Unrecognized),
    };
    if let Some(si) = blocks.iter().find(|b| b.ty == 0) {
        let (sr, bits, _) = flac::parse_stream_info(&si.payload);
        if sr != 0 { m.sample_rate = sr } if bits != 0 { m.bits_per_sample = Some(bits) }
    }
    if let Some(vc) = blocks.iter().find(|b| b.ty == 4) {
        let (pairs, _vendor) = flac::parse_vorbis_comment(&vc.payload);
        for p in pairs {
            let k = p.key.to_uppercase();
            match k.as_str() {
                "TITLE" => { if m.title.is_none() { m.title = Some(p.value) } }
                "ARTIST" => { if !p.value.is_empty() { m.artists.push(p.value) } }
                "ALBUM" => { if !p.value.is_empty() { m.albums.push(p.value) } }
                "ALBUMARTIST" => { if m.album_artist.is_none() { m.album_artist = Some(p.value) } }
                "TRACKNUMBER" => { if m.track.is_none() { m.track = id3v2::parse_number_pair(&p.value).num } }
                "TRACKTOTAL" => { if m.track_total.is_none() { m.track_total = id3v2::parse_number_pair(&p.value).num } }
                "DISCNUMBER" => { if m.disc.is_none() { m.disc = id3v2::parse_number_pair(&p.value).num } }
                "DISCTOTAL" => { if m.disc_total.is_none() { m.disc_total = id3v2::parse_number_pair(&p.value).num } }
                "DATE" => { if m.year.is_none() { m.year = Some(p.value) } }
                "GENRE" => { if !p.value.is_empty() { m.genres.push(p.value) } }
                "COMPOSER" => { if !p.value.is_empty() { m.composers.push(p.value) } }
                "COMMENT" => { if m.comment.is_none() && !p.value.is_empty() { m.comment = Some(p.value) } }
                "LYRICS" | "UNSYNCEDLYRICS" => { if m.lyrics_timed.is_none() && !p.value.is_empty() { m.lyrics_timed = Some(p.value) } }
                _ => { m.raw_frames.push(RawFrame { frame_id: k, size: p.value.len(), data: p.value.into_bytes() }); }
            }
        }
        if m.lyrics_timed.is_some() && m.lyrics.is_none() { m.lyrics = Some(strip_ts(m.lyrics_timed.as_deref().unwrap())); }
    }
    for b in blocks.iter().filter(|b| b.ty == 6) { if let Some(pic) = flac::flac_picture(&b.payload) { m.pictures.push(pic) } }
    Ok(m)
}

pub fn read_wav(path: &Path) -> Result<AudioMetadata> {
    let buf = std::fs::read(path)?;
    let mut m = AudioMetadata::default();
    m.source = "riff".into();
    m.detected_format = Some("wav".into());
    let np = probe_audio(path, &buf, Format::Wav, 0);
    (m.duration_ms, m.sample_rate, m.bits_per_sample, m.bitrate_bps) =
        (np.duration_ms, np.sample_rate, np.bits_per_sample, np.bitrate_bps);
    let parsed = match wav::parse_wav(&buf) { Some(x) => x, None => return Ok(m) };
    // LIST INFO → 统一字段映射（与 TS mapWavInfo / readWavTags 顺序一致）
    let g = |k: &str| parsed.get(k).map(str::to_string);
    if let Some(v) = g("INAM") { m.title = Some(v) }
    if let Some(v) = g("IART") { m.artists = vec![v] }
    if let Some(v) = g("IPRD") { m.albums = vec![v] }
    if let Some(v) = g("ICRD") { m.year = Some(v) }
    if let Some(v) = g("ITRK") { let r = id3v2::parse_number_pair(&v); m.track = r.num; m.track_total = r.total }
    if let Some(v) = g("IGNR") { m.genres = vec![v] }
    if let Some(v) = g("ICMT") { m.comment = Some(v) }
    // 内嵌 "id3 " chunk 优先于 LIST INFO（TS 中后赋值覆盖前值）
    if let Some(h) = &parsed.id3_frames {
        let fm = meta_from_frames(&h.frames);
        m.raw_frames = h.frames.clone();
        apply_frames(&mut m, fm);
        m.source = "id3v2".into();
    }
    Ok(m)
}