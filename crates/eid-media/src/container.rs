//! Structural check of a finished MP4 before we publish it (P4 review M4).
//!
//! yt-dlp (with its FFmpeg) produces the file; we never run another tool ourselves (ADR-0003:
//! only yt-dlp is launched), so the check is pure parsing of the ISO-BMFF top-level boxes:
//!
//! * the first box is `ftyp`, a `moov` box is present;
//! * every box header is complete and every box lies inside the file; the boxes cover the file
//!   exactly (a box of size 0, "to the end of the file", is accepted only as the last one).
//!
//! A cut, empty or foreign file (for example a WebM renamed to `.mp4`, or a conversion that
//! stopped half-way) fails. This is not a codec check: it proves the container is whole.

use std::io::{Read, Seek, SeekFrom};

/// Most top-level boxes we walk (a real file has a handful; fragmented ones a few thousand).
pub const MAX_BOXES: usize = 100_000;

/// Check the file behind `f` (read from its start; the position is left unspecified).
pub fn check_mp4<R: Read + Seek>(f: &mut R) -> Result<(), String> {
    let len = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let mut pos: u64 = 0;
    let (mut first, mut moov) = (true, false);
    for _ in 0..MAX_BOXES {
        if pos == len {
            return if moov {
                Ok(())
            } else {
                Err("not a complete MP4 file (no moov box)".into())
            };
        }
        let rest = len - pos;
        if rest < 8 {
            return Err(format!("not a complete MP4 file (cut at byte {pos})"));
        }
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut h = [0u8; 8];
        f.read_exact(&mut h).map_err(|e| e.to_string())?;
        let small = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        let size = match small {
            // largesize follows the type
            1 => {
                if rest < 16 {
                    return Err(format!("not a complete MP4 file (cut at byte {pos})"));
                }
                let mut l = [0u8; 8];
                f.read_exact(&mut l).map_err(|e| e.to_string())?;
                let s = u64::from_be_bytes(l);
                if s < 16 {
                    return Err(format!("not an MP4 file (bad box size at byte {pos})"));
                }
                s
            }
            // to the end of the file
            0 => rest,
            s if s < 8 => return Err(format!("not an MP4 file (bad box size at byte {pos})")),
            s => s,
        };
        if first && &kind != b"ftyp" {
            return Err("not an MP4 file (it does not start with an ftyp box)".into());
        }
        first = false;
        if &kind == b"moov" {
            moov = true;
        }
        if size > rest {
            return Err(format!(
                "not a complete MP4 file (a box at byte {pos} ends after the end of the file)"
            ));
        }
        pos += size;
    }
    Err(format!("more than {MAX_BOXES} top-level boxes; refused"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn check(b: &[u8]) -> Result<(), String> {
        check_mp4(&mut Cursor::new(b.to_vec()))
    }

    #[test]
    fn mp4_structure_check_accepts_whole_files_and_refuses_cut_or_foreign_ones() {
        let clip = include_bytes!("../../eid-fake-ytdlp/fixtures/clip.mp4");
        check(clip).unwrap();
        let good = [
            bx(b"ftyp", b"isom"),
            bx(b"moov", &[0; 20]),
            bx(b"mdat", &[1; 50]),
        ]
        .concat();
        check(&good).unwrap();
        // a 64-bit size and a final size-0 box are valid
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"mdat");
        large.extend_from_slice(&(16u64 + 4).to_be_bytes());
        large.extend_from_slice(&[7; 4]);
        check(&[bx(b"ftyp", b"isom"), bx(b"moov", &[]), large].concat()).unwrap();
        let mut to_end = 0u32.to_be_bytes().to_vec();
        to_end.extend_from_slice(b"mdat");
        to_end.extend_from_slice(&[9; 33]);
        check(&[bx(b"ftyp", b"isom"), bx(b"moov", &[]), to_end].concat()).unwrap();
        // refused: cut anywhere, empty, no moov, not starting with ftyp, bad sizes, a WebM
        for cut in [1, 7, 9, clip.len() / 2, clip.len() - 1] {
            assert!(check(&clip[..cut]).is_err(), "cut at {cut}");
        }
        assert!(check(&[]).is_err());
        assert!(check(&[bx(b"ftyp", b"isom"), bx(b"mdat", &[1; 9])].concat()).is_err());
        assert!(check(&[bx(b"moov", &[]), bx(b"ftyp", b"isom")].concat()).is_err());
        let mut tiny = good.clone();
        tiny[3] = 4; // ftyp size 4 < 8
        assert!(check(&tiny).is_err());
        let mut long = good.clone();
        long.extend_from_slice(&[0, 0, 0]); // trailing bytes that are not a box
        assert!(check(&long).is_err());
        let webm = [&[0x1a, 0x45, 0xdf, 0xa3][..], &[0x42; 60]].concat();
        assert!(check(&webm).is_err());
        let mut huge = [bx(b"ftyp", b"isom"), bx(b"moov", &[])].concat();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(b"mdat");
        assert!(check(&huge).is_err(), "a box larger than the file");
    }
}
