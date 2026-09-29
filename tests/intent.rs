//! 意图层（mergeFields / diffFields / sniffImageMime）—— 移植自 tests/cli/write-cmd.test.ts 的 1-5 用例
//! 这层是纯逻辑（不碰文件），先把它锁死，写侧实现才有地基。
use music_robot::tag::write::intent::{diff_fields, format_diff, merge_fields, sniff_image_mime, WritableFields};
use std::collections::BTreeSet;

fn unset(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

#[test]
fn i01_merge_only_named_fields() {
    // TS 1/13：局部编辑意图——只输出点名，其余不碰
    let mut f = WritableFields::default();
    f.title = Some("新标题".into());
    let m = merge_fields(f);
    assert_eq!(m.title.as_deref(), Some("新标题"));
    assert!(m.artists.is_none() && m.albums.is_none() && m.year.is_none(), "未点名字段不得出现");
    assert!(m.unset_fields.is_empty(), "没给 --unset 就不应有删除标记");
}

#[test]
fn i02_unset_marks_deletion_and_supersedes_value() {
    // TS 2/13：--unset → unsetFields 标记删除；且 unset 优先于同名赋值
    let mut f = WritableFields::default();
    f.title = Some("会被忽略".into());
    f.unset = unset(&["title"]);
    let m = merge_fields(f);
    assert!(m.title.is_none(), "--unset title 与 --title 同时给时，unset 优先");
    assert_eq!(m.unset_fields, vec!["title".to_string()]);
}

#[test]
fn i03_cover_semantics_three_cases() {
    // TS 3/13：--cover 替换 / --unset-cover 清空 / 不给则不输出
    let mut f = WritableFields::default();
    let m0 = merge_fields(f.clone());
    assert!(m0.pictures.is_none() && !m0.unset_fields.iter().any(|x| x == "pictures"));
    f.replace_cover = Some(music_robot::tag::read::Picture {
        mime_type: "image/jpeg".into(), pic_type: 3, description: String::new(), data: vec![0xFF, 0xD8],
    });
    assert!(merge_fields(f.clone()).pictures.is_some());
    f.replace_cover = None;
    f.unset_cover = true;
    let m = merge_fields(f);
    assert!(m.unset_fields.iter().any(|x| x == "pictures") && m.pictures.is_none());
}

#[test]
fn i04_sniff_image_magic() {
    // TS 4/13：JPEG/PNG/GIF magic 识别
    assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0x00]).as_deref(), Some("image/jpeg"));
    assert_eq!(sniff_image_mime(&[0x89, 0x50, 0x4E, 0x47, 0x0D]).as_deref(), Some("image/png"));
    assert_eq!(sniff_image_mime(b"GIF89a").as_deref(), Some("image/gif"));
    assert_eq!(sniff_image_mime(b"RIFFxxxxWAVE"), None, "非图片必须返回 None 而非猜测");
    assert_eq!(sniff_image_mime(&[0xFF, 0xD8]), None, "长度不足不得越界");
}

#[test]
fn i05_diff_shows_only_changed_lines() {
    // TS 5/13：变化行 + 无变化
    let mut before = music_robot::tag::read::AudioMetadata::default();
    before.title = Some("旧标题".into());
    before.artists = vec!["旧歌手".into()];
    let mut after = music_robot::tag::read::AudioMetadata::default();
    after.title = Some("新标题".into());
    after.artists = vec!["旧歌手".into()];
    let av = music_robot::tag::write::intent::AfterView::from(&after);
    let d = diff_fields(&before, &av);
    assert_eq!(d.iter().map(|x| x.key.clone()).collect::<BTreeSet<_>>(), BTreeSet::from(["title".to_string()]));
    assert_eq!(d[0].before, "旧标题");
    assert_eq!(d[0].after, "新标题");
    assert!(format_diff(&d).contains("旧标题") && format_diff(&d).contains("新标题"));
    let same = diff_fields(&before, &music_robot::tag::write::intent::AfterView::from(&before));
    assert!(same.is_empty(), "无变化应零行");
}

#[test]
fn i06_unset_key_whitelist_rejects_unknown() {
    // ARCHITECTURE §11.4：未知 --unset 键必须明确报错（wash/write 都靠这个白名单）
    use music_robot::tag::write::intent::is_unset_key;
    assert!(is_unset_key("track-total") && is_unset_key("lyrics-timed"), "复查 §3.2 补的两个键必须在册");
    assert!(!is_unset_key("titel"), "拼错的键不得放行");
}

#[test]
fn i07_multi_artist_maps_to_single_tpe1() {
    // TS write.test 10/10 的意图层部分：artists 多值全部进 TPE1，TPE2 专用 albumArtist
    let mut f = WritableFields::default();
    f.artists = Some(vec!["甲".into(), "乙".into(), "丙".into()]);
    f.album_artist = Some("专辑艺术家".into());
    let m = merge_fields(f);
    assert_eq!(m.artists.as_deref().unwrap_or(&[]), ["甲", "乙", "丙"]);
    assert_eq!(m.album_artist.as_deref(), Some("专辑艺术家"));
}
