use std::io::Cursor;

static PING: &[u8] = include_bytes!("../../../assets/sfx/ping.wav");
static JOIN: &[u8] = include_bytes!("../../../assets/sfx/join.wav");
static LEAVE: &[u8] = include_bytes!("../../../assets/sfx/leave.wav");

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

    std::thread::spawn(move || {
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            return;
        };
        let Ok(sink) = rodio::Sink::try_new(&handle) else {
            return;
        };
        let Ok(source) = rodio::Decoder::new(Cursor::new(bytes)) else {
            return;
        };
        sink.append(source);
        sink.sleep_until_end();
    });
}
