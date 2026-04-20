use std::io::Cursor;

static PING: &[u8] = include_bytes!("../../../assets/sfx/ping_fx.wav");
static JOIN: &[u8] = include_bytes!("../../../assets/sfx/join_fx.wav");
static LEAVE: &[u8] = include_bytes!("../../../assets/sfx/leave-fx.wav");

/// Play a named sound effect on the host audio device.
///
/// Runs on a detached thread so it never blocks the Tauri command thread.
/// All errors are silently ignored — a missing audio device should never
/// crash the app.
#[tauri::command]
pub fn play_sfx(sound: &str) {
    let bytes: &'static [u8] = match sound {
        "ping" => PING,
        "join" => JOIN,
        "leave" => LEAVE,
        _ => return,
    };

    let sound_name = sound.to_string();
    std::thread::spawn(move || {
        let (_stream, handle) = match rodio::OutputStream::try_default() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] OutputStream::try_default failed for {sound_name}: {e}");
                return;
            }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] Sink::try_new failed for {sound_name}: {e}");
                return;
            }
        };
        let source = match rodio::Decoder::new(Cursor::new(bytes)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sfx] Decoder::new failed for {sound_name}: {e}");
                return;
            }
        };
        sink.append(source);
        sink.sleep_until_end();
    });
}
