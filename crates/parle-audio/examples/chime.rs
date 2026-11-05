//! Play the 15-minute limit chime through the default output device:
//! `cargo run -p parle-audio --example chime`
fn main() {
    match parle_audio::chime::play_blocking() {
        Ok(()) => println!("chime played"),
        Err(e) => eprintln!("chime failed: {e}"),
    }
}
