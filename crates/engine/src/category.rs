//! Built-in categories by file extension (`whoosh_proto::category`).

use whoosh_proto::category;

use crate::names::split_extension;

/// The built-in category for a file name. A user-supplied category always
/// overrides this.
pub fn category_for(filename: &str) -> &'static str {
    let (_, ext) = split_extension(filename);
    // For double extensions (`.tar.gz`) the last part decides.
    let ext = ext.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "3gp"
        | "ogv" | "ts" | "m2ts" => category::VIDEO,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "oga" | "m4a" | "opus" | "wma" | "aiff"
        | "alac" | "mid" | "midi" => category::MUSIC,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp"
        | "txt" | "rtf" | "epub" | "mobi" | "csv" | "md" | "djvu" => category::DOCUMENTS,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz" | "lzma"
        | "z" | "cab" | "arj" => category::COMPRESSED,
        "exe" | "msi" | "msix" | "appx" | "deb" | "rpm" | "apk" | "appimage" | "dmg" | "pkg"
        | "iso" | "img" | "flatpak" | "snap" | "jar" => category::PROGRAMS,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" | "heic"
        | "heif" | "avif" | "ico" | "raw" => category::IMAGES,
        _ => category::OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(category_for("movie.MKV"), category::VIDEO);
        assert_eq!(category_for("song.mp3"), category::MUSIC);
        assert_eq!(category_for("paper.pdf"), category::DOCUMENTS);
        assert_eq!(category_for("src.tar.gz"), category::COMPRESSED);
        assert_eq!(category_for("setup.exe"), category::PROGRAMS);
        assert_eq!(category_for("ubuntu.iso"), category::PROGRAMS);
        assert_eq!(category_for("photo.jpeg"), category::IMAGES);
        assert_eq!(category_for("data.bin"), category::OTHER);
        assert_eq!(category_for("README"), category::OTHER);
        assert_eq!(category_for(".mp4"), category::OTHER);
    }
}
