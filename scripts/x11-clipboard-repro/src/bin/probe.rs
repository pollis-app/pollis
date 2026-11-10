//! Times the old and new paste reads against whatever owns CLIPBOARD.
use std::time::{Duration, Instant};
use x11repro::clipboard;

fn main() {
    // Old read_clipboard_files: plugin read_text() == arboard get_text(), on the main thread.
    let mut cb = arboard::Clipboard::new().unwrap();
    let t = Instant::now();
    let text = cb.get_text();
    println!("OLD files  arboard get_text      {:>6.2}s -> {:?}", t.elapsed().as_secs_f64(), text.as_ref().map(|s| s.trim()).map_err(|e| e.to_string()));

    let t = Instant::now();
    let list = cb.get().file_list();
    println!("ALT files  arboard file_list     {:>6.2}s -> {:?}", t.elapsed().as_secs_f64(), list.map_err(|e| e.to_string()));

    // Old read_clipboard_image: arboard get_image.
    let t = Instant::now();
    let img = cb.get_image();
    println!("OLD image  arboard get_image     {:>6.2}s -> {:?}", t.elapsed().as_secs_f64(), img.map(|i| (i.width, i.height)).map_err(|e| e.to_string()));

    // New: bounded reader.
    let t = Instant::now();
    let offer = clipboard::x11::read_offer(Duration::from_secs(3), true);
    let paths = offer.as_ref().and_then(|o| o.uri_list.clone()).map(|b| clipboard::paths_from_uri_list(&b));
    println!(
        "NEW files  x11::read_offer       {:>6.2}s -> paths {:?}, has_png {:?}",
        t.elapsed().as_secs_f64(),
        paths,
        offer.as_ref().map(|o| o.has_png)
    );
    // New: bounded text read (the app's own paste on X11).
    let t = Instant::now();
    let text = clipboard::x11::read_text(Duration::from_secs(5));
    println!(
        "NEW text   x11::read_text        {:>6.2}s -> {:?}",
        t.elapsed().as_secs_f64(),
        text.map(|s| if s.len() > 40 { format!("{} bytes", s.len()) } else { s })
    );
    println!("x11 session detected: {}", clipboard::is_x11_session());
}
