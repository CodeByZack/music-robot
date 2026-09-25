//! ID3v1 尾部 128 字节解析 —— 移植自 src/tag/read/id3v1.ts
//! 布局：TAG(3) + title(30) + artist(30) + album(30) + year(4) + comment(28/30) + [track(1)] + genre(1)

/// ID3v1 的 148 个标准流派（ID3v1.1）
pub fn id3v1_genres() -> &'static [&'static str] {
    const G: [&str; 148] = [
    "Blues", "Classic Rock", "Country", "Dance",
    "Disco", "Funk", "Grunge", "Hip-Hop",
    "Jazz", "Metal", "New Age", "Oldies",
    "Other", "Pop", "R&B", "Rap",
    "Reggae", "Rock", "Techno", "Industrial",
    "Alternative", "Ska", "Death Metal", "Pranks",
    "Soundtrack", "Euro-Techno", "Ambient", "Trip-Hop",
    "Vocal", "Jazz+Funk", "Fusion", "Trance",
    "Classical", "Instrumental", "Acid", "House",
    "Game", "Sound Clip", "Gospel", "Noise",
    "AlternRock", "Bass", "Soul", "Punk",
    "Space", "Meditative", "Instrumental Pop", "Instrumental Rock",
    "Ethnic", "Gothic", "Darkwave", "Techno-Industrial",
    "Electronic", "Pop-Folk", "Eurodance", "Dream",
    "Southern Rock", "Comedy", "Cult", "Gangsta",
    "Top 40", "Christian Rap", "Pop/Funk", "Jungle",
    "Native American", "Cabaret", "New Wave", "Psychedelic",
    "Rave", "Showtunes", "Trailer", "Lo-Fi",
    "Tribal", "Acid Punk", "Acid Jazz", "Polka",
    "Retro", "Musical", "Rock & Roll", "Hard Rock",
    "Folk", "Folk-Rock", "National Folk", "Swing",
    "Fast Fusion", "Bebob", "Latin", "Revival",
    "Celtic", "Bluegrass", "Avantgarde", "Gothic Rock",
    "Progressive Rock", "Psychedelic Rock", "Symphonic Rock", "Slow Rock",
    "Big Band", "Chorus", "Easy Listening", "Acoustic",
    "Humour", "Speech", "Chanson", "Opera",
    "Chamber Music", "Sonata", "Symphony", "Booty Bass",
    "Primus", "Porn Groove", "Satire", "Slow Jam",
    "Club", "Tango", "Samba", "Folklore",
    "Ballad", "Power Ballad", "Rhythmic Soul", "Freestyle",
    "Duet", "Punk Rock", "Drum Solo", "A capella",
    "Euro-House", "Dance Hall", "Goa", "Drum & Bass",
    "Club-House", "Hardcore", "Terror", "Indie",
    "BritPop", "Negerpunk", "Polsk Punk", "Beat",
    "Christian Gangsta Rap", "Heavy Metal", "Black Metal", "Crossover",
    "Contemporary Christian", "Christian Rock", "Merengue", "Salsa",
    "Thrash Metal", "Anime", "JPop", "Synthpop",
    ];
    &G
}

/// 有效 track 字节范围：1-254（0/255 视为无效垃圾数据）
pub fn parse_track_byte(b: u8) -> Option<i64> { if (1..=254).contains(&b) { Some(b as i64) } else { None } }

#[derive(Debug, Clone)]
pub struct Id3v1Tag {
    pub title: String, pub artist: String, pub album: String,
    pub year: String, pub comment: String,
    pub track: Option<i64>, pub track_present: bool,
    pub genre: Option<u8>, // 原始字节，0-147；255=unknown → None
}

/// 去除控制字符与尾部填充（\0 / 空格），与 TS clean() 的正则等价
fn clean(s: &str) -> String {
    let no_ctrl: String = s.chars().filter(|c| !('\u{0}'..'\u{1f}').contains(c)).collect();
    no_ctrl.trim_end_matches([' ', '\u{0}']).to_string()
}

/// 解析最后 128 字节。magic 不是 "TAG" 返回 None（如全 0x55 垃圾）。
pub fn id3v1_parse(buf: &[u8]) -> Option<Id3v1Tag> {
    if buf.len() < 128 { return None }
    let tag = &buf[buf.len() - 128..];
    if &tag[0..3] != b"TAG" { return None }
    let l = |a: usize, b: usize| super::id3v2::latin1_decode(&tag[a..b]);
    let title = clean(&l(3, 33));
    let artist = clean(&l(33, 63));
    let album = clean(&l(63, 93));
    let year = clean(&l(93, 97));
    let comment_raw = l(97, 125);

    // ID3v1.1 判定：comment[28]==0 且 comment[29]!=0 → 有 track 字节
    let (byte125, byte126) = (tag[125], tag[126]);
    let track_present = byte125 == 0 && byte126 != 0;
    let track = if track_present { parse_track_byte(byte126) } else { None };
    let comment = clean(if track_present { &comment_raw[..28.min(comment_raw.len())] } else { &comment_raw });
    // ⚠️ genre 恒在 offset 127（ID3v1 布局固定 128B：125 恒 0x00，126 是 v1.1 track 字节，
    //    127 永远是 genre）。旧实现 `track_present ? tag[127] : tag[126]` 在非 v1.1 时
    //    错读 126（padding 位，恒 0）→ genre=0 被当成 "Blues"，导致 blank 后 genres 残留。
    //    本 bug 在 TS 参照实现里同样存在（已复现，见下方回归用例），此处按 ID3v1 规格修正。
    let genre_byte = tag[127];
    let genre = if genre_byte <= 147 { Some(genre_byte) } else { None };
    Some(Id3v1Tag { title, artist, album, year, comment, track, track_present, genre })
}
