//! The analysis decoders behind one interface.

use streamlens_types::{Codec, FrameAnalysis, PlanarImage};

use crate::Error;

/// One decoded picture with its per-block analysis.
pub struct Picture {
    /// The tag passed with the access unit this picture was decoded from.
    pub tag: u32,
    pub image: PlanarImage,
    pub analysis: FrameAnalysis,
}

/// An analysis decoder, fed Annex B access units in decode order. Pictures
/// come out in display order; a picture the decoder cannot produce (e.g. a
/// leading picture whose references precede the first keyframe fed) is
/// simply never output.
pub trait Decoder: Send {
    /// Decodes one access unit and appends every picture that became ready.
    fn decode(&mut self, au: &[u8], tag: u32, out: &mut Vec<Picture>) -> Result<(), Error>;

    /// Ends the input and appends every picture still held back. The
    /// decoder takes no more input afterwards.
    fn finish(&mut self, out: &mut Vec<Picture>) -> Result<(), Error>;
}

/// Opens the analysis decoder for `codec`. `stages` enables the costly
/// reconstruction-stage planes (predicted, unfiltered, residual).
pub fn open_decoder(codec: Codec, stages: bool) -> Result<Box<dyn Decoder>, Error> {
    Ok(match codec {
        Codec::H264 => Box::new(crate::h264::H264Decoder::open(stages)?),
        Codec::H265 => Box::new(crate::h265::H265Decoder::open(stages)?),
    })
}
