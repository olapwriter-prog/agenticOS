//! Still photos only. This OS takes pictures; it does not record, stream or play video.
//! Every image is checked by its first bytes (not its file name), stripped of location and
//! camera metadata, and stored under a name that reveals nothing.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Png,
    Heic,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    Video,
    Animated,
    Unknown,
    TooBig,
    Malformed,
    /// A real image, but one whose location data this crate cannot remove yet.
    CannotStrip,
}

pub const MAX_BYTES: usize = 25 * 1024 * 1024;

/// Decide what a file is from its first bytes.
pub fn sniff(b: &[u8]) -> Result<Kind, Reject> {
    if b.len() > MAX_BYTES {
        return Err(Reject::TooBig);
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(Kind::Jpeg);
    }
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Ok(Kind::Png);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        // MP4, MOV and HEIC all share the "ftyp" box. The brand tells them apart.
        return match &b[8..12] {
            b"heic" | b"heix" | b"mif1" | b"msf1" => Ok(Kind::Heic),
            _ => Err(Reject::Video),
        };
    }
    if b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Err(Reject::Video); // Matroska / WebM
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"AVI " {
        return Err(Reject::Video);
    }
    if b.starts_with(b"GIF8") {
        return Err(Reject::Animated);
    }
    Err(Reject::Unknown)
}

/// Remove APP1 (EXIF, XMP) and APP13 (IPTC) segments from a JPEG. Everything else is copied
/// byte for byte, including the image data.
pub fn strip_jpeg_metadata(b: &[u8]) -> Result<Vec<u8>, Reject> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return Err(Reject::Malformed);
    }
    let mut out = vec![0xFF, 0xD8];
    let mut i = 2;
    while i + 1 < b.len() {
        if b[i] != 0xFF {
            return Err(Reject::Malformed);
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            // start of scan or end of image: the rest is image data, copy it as is
            out.extend_from_slice(&b[i..]);
            return Ok(out);
        }
        if i + 3 >= b.len() {
            return Err(Reject::Malformed);
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 || i + 2 + len > b.len() {
            return Err(Reject::Malformed);
        }
        let seg = &b[i..i + 2 + len];
        if marker != 0xE1 && marker != 0xED {
            out.extend_from_slice(seg);
        }
        i += 2 + len;
    }
    Err(Reject::Malformed)
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's civil-from-days algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn file_name(epoch_secs: u64, seq: u32, kind: Kind) -> String {
    let (y, m, d) = civil((epoch_secs / 86_400) as i64);
    let s = epoch_secs % 86_400;
    let ext = match kind {
        Kind::Jpeg => "jpg",
        Kind::Png => "png",
        Kind::Heic => "heic",
    };
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{seq:03}.{ext}",
        s / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

pub struct PhotoStore {
    pub dir: PathBuf,
}

impl PhotoStore {
    pub fn new(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Check, strip and save. Returns the path written.
    pub fn save(&self, bytes: &[u8], epoch_secs: u64, seq: u32) -> Result<PathBuf, Reject> {
        let kind = sniff(bytes)?;
        // Only JPEG metadata can be stripped so far. PNG can carry an eXIf chunk and HEIC carries
        // EXIF, and storing them untouched could leak a location, so they are refused, not saved.
        let clean = match kind {
            Kind::Jpeg => strip_jpeg_metadata(bytes)?,
            Kind::Png | Kind::Heic => return Err(Reject::CannotStrip),
        };
        let path = self.dir.join(file_name(epoch_secs, seq, kind));
        fs::write(&path, clean).map_err(|_| Reject::Malformed)?;
        Ok(path)
    }

    /// Keep only the newest `keep` photos (names sort by time). Returns how many were removed.
    pub fn prune(&self, keep: usize) -> io::Result<usize> {
        let mut names: Vec<_> = fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        names.sort();
        let remove = names.len().saturating_sub(keep);
        for p in &names[..remove] {
            fs::remove_file(p)?;
        }
        Ok(remove)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny synthetic JPEG: SOI, JFIF, an EXIF segment holding a fake GPS string, a table, SOS, data, EOI.
    fn jpeg() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        let exif = b"Exif\0\0GPSLatitude=12.97;GPSLongitude=77.59";
        v.extend_from_slice(&[0xFF, 0xE1]);
        v.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(exif);
        v.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x04, 0x01, 0x02]);
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x03, 0x00]);
        v.extend_from_slice(&[0x11, 0x22, 0xFF, 0x00, 0x33]); // image data containing an escaped FF
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("photos-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn jpeg_png_and_heic_are_accepted() {
        assert_eq!(sniff(&jpeg()), Ok(Kind::Jpeg));
        assert_eq!(
            sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]),
            Ok(Kind::Png)
        );
        let mut heic = vec![0, 0, 0, 0x18];
        heic.extend_from_slice(b"ftypheic\0\0\0\0");
        assert_eq!(sniff(&heic), Ok(Kind::Heic));
    }
    #[test]
    fn mp4_and_mov_are_video_even_though_they_look_like_heic() {
        for brand in [&b"isom"[..], b"mp42", b"qt  ", b"avc1"] {
            let mut f = vec![0, 0, 0, 0x18];
            f.extend_from_slice(b"ftyp");
            f.extend_from_slice(brand);
            f.extend_from_slice(&[0; 8]);
            assert_eq!(sniff(&f), Err(Reject::Video), "brand {:?}", brand);
        }
    }
    #[test]
    fn webm_and_avi_are_video() {
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0]), Err(Reject::Video));
        let mut avi = b"RIFF\0\0\0\0AVI LIST".to_vec();
        avi.extend_from_slice(&[0; 4]);
        assert_eq!(sniff(&avi), Err(Reject::Video));
    }
    #[test]
    fn gif_is_refused_as_animated_and_junk_as_unknown() {
        assert_eq!(sniff(b"GIF89a....."), Err(Reject::Animated));
        assert_eq!(sniff(b"hello world, not a picture"), Err(Reject::Unknown));
    }
    #[test]
    fn a_renamed_video_is_still_a_video() {
        // the check never looks at a file name, so "holiday.jpg" holding MP4 bytes is refused
        let mut f = vec![0, 0, 0, 0x18];
        f.extend_from_slice(b"ftypisom\0\0\0\0");
        assert_eq!(sniff(&f), Err(Reject::Video));
    }
    #[test]
    fn oversize_files_are_refused() {
        let big = vec![0xFF; MAX_BYTES + 1];
        assert_eq!(sniff(&big), Err(Reject::TooBig));
    }
    #[test]
    fn stripping_removes_gps_and_keeps_the_picture() {
        let out = strip_jpeg_metadata(&jpeg()).unwrap();
        assert!(!contains(&out, b"GPS"));
        assert!(!contains(&out, b"Exif"));
        assert!(contains(&out, b"JFIF"));
        assert!(out.ends_with(&[0x11, 0x22, 0xFF, 0x00, 0x33, 0xFF, 0xD9]));
        assert!(out.len() < jpeg().len());
    }
    #[test]
    fn a_jpeg_without_metadata_comes_out_identical() {
        let mut v = vec![
            0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x04, 0x01, 0x02, 0xFF, 0xDA, 0x00, 0x03, 0x00, 0x55,
            0xFF, 0xD9,
        ];
        assert_eq!(strip_jpeg_metadata(&v).unwrap(), v);
        v.truncate(6);
        assert_eq!(strip_jpeg_metadata(&v), Err(Reject::Malformed));
    }
    #[test]
    fn a_lying_segment_length_is_malformed_not_a_panic() {
        let v = vec![0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, 0x00];
        assert_eq!(strip_jpeg_metadata(&v), Err(Reject::Malformed));
    }
    #[test]
    fn file_names_hide_everything_but_the_time() {
        // 2026-10-30 09:41:07 UTC
        assert_eq!(
            file_name(1_793_353_267, 3, Kind::Jpeg),
            "20261030-094107-003.jpg"
        );
        assert_eq!(file_name(0, 0, Kind::Png), "19700101-000000-000.png");
    }
    #[test]
    fn leap_day_is_handled() {
        // 2024-02-29 00:00:00 UTC
        assert_eq!(
            file_name(1_709_164_800, 1, Kind::Heic),
            "20240229-000000-001.heic"
        );
    }
    #[test]
    fn saving_strips_metadata_on_disk() {
        let dir = tmp("save");
        let s = PhotoStore::new(&dir).unwrap();
        let p = s.save(&jpeg(), 1_793_353_267, 1).unwrap();
        assert!(!contains(&fs::read(&p).unwrap(), b"GPS"));
        fs::remove_dir_all(dir).ok();
    }
    #[test]
    fn png_and_heic_are_refused_until_they_can_be_stripped() {
        let dir = tmp("png");
        let s = PhotoStore::new(&dir).unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(s.save(&png, 1, 1), Err(Reject::CannotStrip));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).ok();
    }
    #[test]
    fn saving_a_video_writes_nothing() {
        let dir = tmp("video");
        let s = PhotoStore::new(&dir).unwrap();
        let mut f = vec![0, 0, 0, 0x18];
        f.extend_from_slice(b"ftypmp42\0\0\0\0");
        assert_eq!(s.save(&f, 1, 1), Err(Reject::Video));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).ok();
    }
    #[test]
    fn prune_keeps_the_newest() {
        let dir = tmp("prune");
        let s = PhotoStore::new(&dir).unwrap();
        for i in 0..5u64 {
            s.save(&jpeg(), 1_793_353_267 + i * 60, i as u32).unwrap();
        }
        assert_eq!(s.prune(2).unwrap(), 3);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert!(left[0].contains("094307") || left[0].contains("094407"));
        assert_eq!(left.len(), 2);
        fs::remove_dir_all(dir).ok();
    }
}
