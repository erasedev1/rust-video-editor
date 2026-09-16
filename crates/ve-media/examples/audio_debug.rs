//! Prints the format FFmpeg reports for an audio stream before and after the
//! first frame is decoded. Kept as an example because that discrepancy is
//! exactly what makes resampler setup subtle.
use ffmpeg_next as ffmpeg;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg::init()?;
    let path = std::env::args().nth(1).expect("usage: audio_debug <file>");
    let mut input = ffmpeg::format::input(&path)?;
    let stream = input.streams().best(ffmpeg::media::Type::Audio).unwrap();
    let idx = stream.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    let mut dec = ctx.decoder().audio()?;
    println!(
        "decoder before: format={:?} rate={} channels={} layout_empty={} layout_bits={:?}",
        dec.format(),
        dec.rate(),
        dec.channels(),
        dec.channel_layout().is_empty(),
        dec.channel_layout().bits()
    );

    // Feed packets until the decoder produces its first frame.
    let mut frame = ffmpeg::frame::Audio::empty();
    for (stream, packet) in input.packets() {
        if stream.index() != idx {
            continue;
        }
        dec.send_packet(&packet)?;
        if dec.receive_frame(&mut frame).is_ok() {
            break;
        }
    }
    println!(
        "frame:          format={:?} rate={} channels={} layout_empty={} layout_bits={:?} samples={}",
        frame.format(), frame.rate(), frame.channels(),
        frame.channel_layout().is_empty(), frame.channel_layout().bits(), frame.samples()
    );
    println!(
        "decoder after:  format={:?} rate={} channels={}",
        dec.format(),
        dec.rate(),
        dec.channels()
    );

    // Raw i16 peak straight from the decoded frame, before any resampling.
    // Useful for telling "the decoder is wrong" apart from "the file is quiet".
    let raw = frame.data(0);
    let mut peak_i16 = 0i32;
    for c in raw.as_chunks::<2>().0.iter().take(frame.samples() * frame.channels() as usize) {
        peak_i16 = peak_i16.max((i16::from_ne_bytes([c[0], c[1]]) as i32).abs());
    }
    println!("raw i16 peak:   {peak_i16} (full scale = 32767)");

    Ok(())
}
