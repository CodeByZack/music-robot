//! S20 · 封面：从专辑表 / 音频文件里取出一张可以返回的封面。
//!
//! 与 [crate::audio::stream] 一个路子，本模块只放**不碰 IO** 的纯逻辑，方便逐条单测：
//!
//! * [front_cover] —— 从 read_tags 读出的 [Picture] 列表里挑一张；
//! * [sniff_image_mime] / [mime_for] —— 定响应用的 Content-Type；
//! * [check_size] —— 上限检查；
//! * [extract] / [from_stored] —— 把上面几步串成「拿到一张可返回的封面」。
//!
//! 真正的读库、读盘、回填与响应组装在 crate::server::routes::cover。
//!
//! # 为什么接口必须「先读专辑表，空了再回退到文件内嵌封面」
//!
//! 画布给 GET /api/songs/:id/cover 标的流程是「读 albums.cover_data」。那一列由
//! **刮削插件**写入（S23 起插件注册表已接通，见 crate::server::state），但刮削是
//! **按需触发**的：新扫描进来的歌、用户没点过刮削的库，albums.cover_data 依然是空的。
//!
//! 如果只按画布字面实现，这些歌会**每一首都 404** —— 一个在刚导入的曲库上取不到封面的
//! 接口，等于没做。所以正确做法是两步：
//!
//! 1. 歌曲挂了专辑、且 albums.cover_data 非空 → 直接返回（连同 cover_mime）；
//! 2. 否则**回退**到文件内嵌封面：read_tags(file_path) → 从 pictures 里挑 front
//!    cover，把字节与 MIME 返回给客户端，并**顺手回填** albums.cover_data /
//!    cover_mime，下次同专辑的歌就不必再读盘、再解标签。
//!
//! **这一步回退不是多余代码**：对「还没刮削过的专辑」它就是封面唯一的真实来源；
//! 刮削过的专辑走第 1 步先命中。不要删。
//!
//! # 上限（为什么必须有，且为什么不能静默截断）
//!
//! 内嵌封面来自第三方文件，字节数完全不可信；而整张封面要被读进内存、再写进响应体。
//! 没有上限时，一个几 GB 的畸形 APIC / PICTURE 块就能把服务打爆。本步定
//! [MAX_COVER_BYTES] = 10 MiB：真实封面（几百 KB ~ 一两 MB 的 JPEG/PNG）远在
//! 限额之内，而远超 10 MiB 的「封面」基本只可能是畸形数据。超限一律**明确拒绝**，
//! 既不返回、也不回填 —— **绝不静默截断**：截出来的图是坏的，客户端会当成成功却
//! 渲染失败，比直接报错更难排查。

use crate::tag::read::Picture;

/// 单张封面允许的最大字节数（10 MiB）。
///
/// 理由见模块文档「上限」。取整 10 * 1024 * 1024：真实专辑封面通常 < 2 MiB，10 MiB
/// 已留足余量；再大基本只有畸形文件或攻击负载。常量放这里，路由层只引用、不另写一个。
pub const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;

/// 封面提取 / 组装的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverError {
    /// 封面字节数超过 [MAX_COVER_BYTES]，明确拒绝（不截断）
    TooLarge {
        /// 实际字节数，用于给出一条能定位问题的错误信息
        len: usize,
    },
}

impl std::fmt::Display for CoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoverError::TooLarge { len } => write!(
                f,
                "封面 {len} 字节，超过上限 {MAX_COVER_BYTES} 字节，拒绝返回（不做截断）"
            ),
        }
    }
}

impl std::error::Error for CoverError {}

/// 一张可以返回给客户端的封面：字节 + 已经定好的 MIME。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverImage {
    /// 封面原始字节
    pub data: Vec<u8>,
    /// 响应 Content-Type 用的 MIME
    pub mime: String,
}

/// 检查字节数是否在上限内。超限返回 [CoverError::TooLarge]。
pub fn check_size(len: usize) -> Result<(), CoverError> {
    if len > MAX_COVER_BYTES {
        return Err(CoverError::TooLarge { len });
    }
    Ok(())
}

/// 从标签图片里挑一张可用的封面。
///
/// 优先 pic_type == 3（ID3 / FLAC 规范的 FrontCover）；没有就取第一张。
/// 空数据的图片一律跳过 —— 返回空字节等于给客户端一张坏图，不如当作没有封面。
pub fn front_cover(pictures: &[Picture]) -> Option<&Picture> {
    let front = pictures
        .iter()
        .find(|picture| picture.pic_type == 3 && !picture.data.is_empty());
    front.or_else(|| pictures.iter().find(|picture| !picture.data.is_empty()))
}

/// 按魔数嗅探图片类型；认不出返回 None。
///
/// 只认最常见的几种（JPEG / PNG / GIF / WebP / BMP）：标签里声明的 MIME 常常为空或
/// 写错，字节魔数才是「图片的真实 MIME」，而这里又不想为了嗅探去引一个图片库。
pub fn sniff_image_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
        return Some("image/webp");
    }
    if data.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}

/// 定响应 Content-Type。
///
/// 顺序：① 字节魔数（最可信，即「图片的真实 MIME」）；② 标签 / 库里声明的 MIME；
/// ③ 都没有就给 application/octet-stream —— 宁可给通用二进制，也不瞎猜一个图片类型。
pub fn mime_for(declared: Option<&str>, data: &[u8]) -> String {
    if let Some(mime) = sniff_image_mime(data) {
        return mime.to_string();
    }
    if let Some(declared) = declared {
        let trimmed = declared.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "application/octet-stream".to_string()
}

/// 从标签图片里提取一张封面：挑图 → 上限检查 → 定 MIME。
///
/// 没有任何可用图片返回 Ok(None)（路由层据此 404）；有图但超限返回 Err（明确拒绝）。
pub fn extract(pictures: &[Picture]) -> Result<Option<CoverImage>, CoverError> {
    let Some(picture) = front_cover(pictures) else {
        return Ok(None);
    };
    check_size(picture.data.len())?;
    Ok(Some(CoverImage {
        mime: mime_for(Some(&picture.mime_type), &picture.data),
        data: picture.data.clone(),
    }))
}

/// 用专辑表里已经存好的封面字节组装一张可返回的封面（同样过上限）。
///
/// 库里存过封面也可能超限（本接口的回填已经拦了一道，但库是外部可写的），
/// 所以这里再检查一次；MIME 同样优先按字节嗅探，cover_mime 只是兜底。
pub fn from_stored(data: Vec<u8>, mime: Option<&str>) -> Result<CoverImage, CoverError> {
    check_size(data.len())?;
    let mime = mime_for(mime, &data);
    Ok(CoverImage { data, mime })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张只有元数据、字节随便填的图片。
    fn picture(pic_type: u8, mime_type: &str, data: Vec<u8>) -> Picture {
        Picture {
            mime_type: mime_type.to_string(),
            pic_type,
            description: String::new(),
            data,
        }
    }

    fn jpeg() -> Vec<u8> {
        // JPEG SOI + 少量内容，够 sniff 认出即可。
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.extend_from_slice(&[0u8; 16]);
        bytes
    }

    // ─────────────────────── 挑图 ───────────────────────

    /// 优先 front cover（type = 3），没有才退回第一张。
    #[test]
    fn front_cover_prefers_type_three_then_falls_back() {
        let only_other = vec![picture(0, "image/png", vec![1, 2, 3])];
        assert_eq!(
            front_cover(&only_other).map(|p| p.pic_type),
            Some(0),
            "没有 type=3 时取第一张"
        );

        let mixed = vec![
            picture(0, "image/png", vec![9]),
            picture(3, "image/jpeg", jpeg()),
            picture(5, "image/png", vec![8]),
        ];
        let picked = front_cover(&mixed).expect("应有封面");
        assert_eq!(picked.pic_type, 3, "有 type=3 必须优先");
        assert_eq!(picked.mime_type, "image/jpeg");

        assert!(front_cover(&[]).is_none(), "空列表没有封面");
    }

    /// 空数据的图片不算封面（返回空字节等于发坏图）。
    #[test]
    fn front_cover_skips_empty_pictures() {
        let with_empty_front = vec![
            picture(3, "image/jpeg", Vec::new()),
            picture(0, "image/png", vec![1, 2]),
        ];
        let picked = front_cover(&with_empty_front).expect("应跳过空数据的那张");
        assert_eq!(picked.pic_type, 0);
        assert_eq!(picked.data, vec![1, 2]);

        let all_empty = vec![picture(3, "image/jpeg", Vec::new())];
        assert!(front_cover(&all_empty).is_none(), "全空数据视为没有封面");
    }

    // ─────────────────────── MIME ───────────────────────

    /// 嗅探认得出常见图片魔数；认不出返回 None。
    #[test]
    fn sniff_recognizes_common_image_magic() {
        assert_eq!(sniff_image_mime(&jpeg()), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image_mime(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"GIF87a...."), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(&[0u8; 8]);
        assert_eq!(sniff_image_mime(&webp), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"BM......"), Some("image/bmp"));
        // 认不出
        assert_eq!(sniff_image_mime(b"not an image"), None);
        assert_eq!(sniff_image_mime(&[]), None);
    }

    /// MIME 优先级：魔数 > 声明的 MIME > octet-stream。
    #[test]
    fn mime_prefers_sniffed_over_declared_and_never_guesses() {
        // 声明为空 → 用嗅探结果
        assert_eq!(mime_for(Some(""), &jpeg()), "image/jpeg");
        // 声明写错 → 仍以字节为准
        assert_eq!(mime_for(Some("image/png"), &jpeg()), "image/jpeg");
        // 嗅不出 → 用声明
        assert_eq!(mime_for(Some("image/x-icon"), b"unknown-bytes"), "image/x-icon");
        // 都没有 → 通用二进制，不瞎猜
        assert_eq!(mime_for(None, b"unknown-bytes"), "application/octet-stream");
        assert_eq!(mime_for(Some("   "), b"unknown-bytes"), "application/octet-stream");
    }

    // ─────────────────────── 上限 ───────────────────────

    /// 边界：正好上限放行，多 1 字节明确拒绝。
    #[test]
    fn check_size_is_inclusive_at_the_limit() {
        assert!(check_size(0).is_ok());
        assert!(check_size(MAX_COVER_BYTES).is_ok());
        assert_eq!(
            check_size(MAX_COVER_BYTES + 1),
            Err(CoverError::TooLarge {
                len: MAX_COVER_BYTES + 1
            })
        );
    }

    /// extract 的三个分支：没图 → None；正常图 → 带 MIME 的字节；超大图 → 明确报错。
    #[test]
    fn extract_handles_missing_normal_and_oversized_pictures() {
        assert!(extract(&[]).expect("没图不是错误").is_none());

        let normal = vec![picture(3, "", jpeg())];
        let image = extract(&normal).expect("正常封面").expect("应有封面");
        assert_eq!(image.data, jpeg());
        assert_eq!(image.mime, "image/jpeg", "声明为空时靠字节嗅探出 MIME");

        let oversized = vec![picture(3, "image/jpeg", vec![0u8; MAX_COVER_BYTES + 1])];
        assert_eq!(
            extract(&oversized),
            Err(CoverError::TooLarge {
                len: MAX_COVER_BYTES + 1
            }),
            "超限必须明确拒绝，绝不截断"
        );
    }

    /// from_stored：库里存的封面同样过上限，MIME 用字节嗅探 + 声明兜底。
    #[test]
    fn from_stored_enforces_the_same_cap() {
        let ok = from_stored(jpeg(), Some("image/jpeg")).expect("正常封面");
        assert_eq!(ok.mime, "image/jpeg");

        let no_mime = from_stored(jpeg(), None).expect("没声明 MIME 也要能返回");
        assert_eq!(no_mime.mime, "image/jpeg", "没声明时按字节嗅探");

        assert_eq!(
            from_stored(vec![0u8; MAX_COVER_BYTES + 1], Some("image/jpeg")),
            Err(CoverError::TooLarge {
                len: MAX_COVER_BYTES + 1
            })
        );
    }

    /// 每个错误都有一句中文说明，且不 panic。
    #[test]
    fn every_error_has_a_chinese_message() {
        let text = CoverError::TooLarge { len: 42 }.to_string();
        assert!(
            text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "错误文案必须是中文：{text}"
        );
    }
}
