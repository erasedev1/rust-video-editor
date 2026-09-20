//! Which matrix converts between RGB and YUV, and telling swscale about it.
//!
//! Video is stored as luma and chroma; everything above the decoder works in
//! RGB. The matrix between them is **not** a constant: standard definition uses
//! BT.601 and high definition uses BT.709, and converting with the wrong one
//! shifts saturated colour by more than twenty 8-bit levels — enough to see on
//! a flat graphic and far more than any codec's rounding.
//!
//! swscale's own default is BT.601 whatever the picture's size, which is right
//! for the fixtures this crate is tested against and wrong for every HD file.
//! So both directions — the decoder reading a file, and the exporter writing one
//! — say explicitly which matrix they mean, and say the same thing.
//!
//! This is the one place in `ve-media` that hands an FFmpeg type across a crate
//! boundary. That is deliberate: the exporter drives swscale itself, and a
//! second copy of this reasoning living over there is exactly how the two would
//! come to disagree.

use std::os::raw::c_int;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::software::scaling;

/// The RGB-to-YUV matrix a picture is to be interpreted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    /// BT.601, standard definition and everything below 720 lines.
    Bt601,
    /// BT.709, high definition.
    Bt709,
}

impl Matrix {
    /// What a player assumes for an untagged picture: 709 from 720 lines up.
    pub fn for_height(height: u32) -> Matrix {
        if height >= 720 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        }
    }

    /// What a stream *says* it is, falling back on its size when it says
    /// nothing — which most camera and screen-capture files do.
    pub fn from_space(space: ffmpeg::color::Space, height: u32) -> Matrix {
        match space {
            ffmpeg::color::Space::BT709 => Matrix::Bt709,
            ffmpeg::color::Space::BT470BG
            | ffmpeg::color::Space::SMPTE170M
            | ffmpeg::color::Space::SMPTE240M => Matrix::Bt601,
            _ => Matrix::for_height(height),
        }
    }

    /// The colour space to tag an encoded stream with.
    pub fn space(self) -> ffmpeg::color::Space {
        match self {
            Matrix::Bt601 => ffmpeg::color::Space::SMPTE170M,
            Matrix::Bt709 => ffmpeg::color::Space::BT709,
        }
    }

    pub fn primaries(self) -> ffmpeg::color::Primaries {
        match self {
            Matrix::Bt601 => ffmpeg::color::Primaries::SMPTE170M,
            Matrix::Bt709 => ffmpeg::color::Primaries::BT709,
        }
    }

    /// The transfer characteristic that goes with the matrix.
    ///
    /// BT.709's curve for both: 601 and 709 share it in practice, and tagging
    /// the 601 original (`gamma 2.2`-ish) differently would change nothing any
    /// player does and confuse everything that reads the tag.
    pub fn transfer(self) -> ffmpeg::color::TransferCharacteristic {
        match self {
            Matrix::Bt601 => ffmpeg::color::TransferCharacteristic::SMPTE170M,
            Matrix::Bt709 => ffmpeg::color::TransferCharacteristic::BT709,
        }
    }

    fn sws_id(self) -> c_int {
        match self {
            Matrix::Bt601 => ffmpeg::ffi::SWS_CS_ITU601,
            Matrix::Bt709 => ffmpeg::ffi::SWS_CS_ITU709,
        }
    }
}

/// Whether the RGB side of a conversion covers the full 0–255 range.
///
/// Editing happens in full-range RGB; the YUV side of a delivery file is
/// conventionally limited range (16–235), and saying so is what stops black
/// from coming back as 16 after a round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Limited,
    Full,
}

impl Range {
    fn flag(self) -> c_int {
        match self {
            Range::Limited => 0,
            Range::Full => 1,
        }
    }
}

/// Tells a scaler which matrix and ranges to convert with.
///
/// Returns whether swscale accepted it: some conversions — YUV to the same YUV
/// without scaling, for one — have no matrix to set and refuse. That is not an
/// error worth failing an export over, so it is reported rather than raised.
pub fn set_matrix(
    scaler: &mut scaling::Context,
    matrix: Matrix,
    source: Range,
    destination: Range,
) -> bool {
    // SAFETY: `scaler` is a live context owned by the caller, and
    // `sws_getCoefficients` returns a pointer to static tables for any input —
    // it cannot fail or be freed. The call only reads them.
    unsafe {
        let table = ffmpeg::ffi::sws_getCoefficients(matrix.sws_id());
        ffmpeg::ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            table,
            source.flag(),
            table,
            destination.flag(),
            0,
            1 << 16, // Unity contrast and saturation, in swscale's 16.16 fixed
            1 << 16, // point: this call is about the matrix, not about grading.
        ) >= 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untagged_picture_is_read_by_its_size() {
        assert_eq!(Matrix::for_height(480), Matrix::Bt601);
        assert_eq!(Matrix::for_height(719), Matrix::Bt601);
        assert_eq!(Matrix::for_height(720), Matrix::Bt709);
        assert_eq!(Matrix::for_height(2160), Matrix::Bt709);
    }

    #[test]
    fn a_tagged_picture_is_read_by_its_tag_whatever_its_size() {
        // An HD-sized file that says 601 means 601.
        assert_eq!(Matrix::from_space(ffmpeg::color::Space::SMPTE170M, 1080), Matrix::Bt601);
        // ...and an SD-sized one that says 709 means 709.
        assert_eq!(Matrix::from_space(ffmpeg::color::Space::BT709, 480), Matrix::Bt709);
        // Unspecified falls back to the size.
        assert_eq!(Matrix::from_space(ffmpeg::color::Space::Unspecified, 1080), Matrix::Bt709);
    }

    #[test]
    fn a_scaler_accepts_a_matrix_for_a_conversion_that_has_one() {
        crate::ffmpeg_init().unwrap();
        let mut scaler = scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            320,
            240,
            ffmpeg::format::Pixel::YUV420P,
            320,
            240,
            scaling::Flags::BILINEAR,
        )
        .unwrap();
        assert!(set_matrix(&mut scaler, Matrix::Bt709, Range::Full, Range::Limited));
    }
}
