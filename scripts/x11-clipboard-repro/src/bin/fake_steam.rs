//! An X11 CLIPBOARD owner that behaves like Steam's "Copy to clipboard"
//! (ValveSoftware/steam-for-linux#11476): offers TARGETS + text/uri-list, and
//! answers text/uri-list with the property typed UTF8_STRING.
//! Mode (argv[1]) for targets it doesn't offer:
//!   ignore  - never answers (worst case)
//!   refuse  - answers with property None (ICCCM-correct)
//!   anyutf8 - answers EVERY request with the uri-list typed UTF8_STRING
//!   slow    - like refuse, but each reply takes 1.5 s (a busy owner)
//! argv[2]: the file to offer as text/uri-list; empty offers no file list.
//! FAKE_TEXT=<text>: also offer UTF8_STRING (a text copy from an owner with the same manners).
//! FAKE_TEXT_BYTES=<n>: offer n bytes of generated text instead.
//! FAKE_INCR_CHUNK=<n>: send text bigger than n bytes with the ICCCM INCR protocol, n bytes at a time.
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

/// An INCR transfer in progress: the requestor deletes the property, we write the next chunk.
struct Incr {
    requestor: Window,
    property: Atom,
    offset: usize,
    done: bool,
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ignore".into());
    let path = std::env::args().nth(2).unwrap_or_else(|| "/tmp/shot.jpg".into());
    let uri = format!("file://{path}\r\n");
    let text: Option<Vec<u8>> = match std::env::var("FAKE_TEXT_BYTES").ok().and_then(|n| n.parse::<usize>().ok()) {
        Some(n) => Some((0..n).map(|i| if i % 64 == 63 { b'\n' } else { b'a' + (i % 26) as u8 }).collect()),
        None => std::env::var("FAKE_TEXT").ok().map(String::into_bytes),
    };
    let incr_chunk: Option<usize> = std::env::var("FAKE_INCR_CHUNK").ok().and_then(|n| n.parse().ok());
    let (conn, screen) = x11rb::connect(None).unwrap();
    let root = conn.setup().roots[screen].root;
    let win = conn.generate_id().unwrap();
    conn.create_window(0, win, root, 0, 0, 1, 1, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
    let atom = |n: &str| conn.intern_atom(false, n.as_bytes()).unwrap().reply().unwrap().atom;
    let clipboard = atom("CLIPBOARD");
    let targets = atom("TARGETS");
    let uri_list = atom("text/uri-list");
    let utf8 = atom("UTF8_STRING");
    let incr_atom = atom("INCR");
    let mut offered = vec![targets];
    if !path.is_empty() {
        offered.push(uri_list);
    }
    if text.is_some() {
        offered.push(utf8);
    }
    conn.set_selection_owner(win, clipboard, x11rb::CURRENT_TIME).unwrap();
    conn.flush().unwrap();
    eprintln!("fake_steam[{mode}]: owning CLIPBOARD");
    let mut incr: Option<Incr> = None;
    loop {
        let event = conn.wait_for_event().unwrap();
        if let Event::PropertyNotify(p) = &event {
            let (Some(t), Some(body)) = (incr.as_mut(), text.as_ref()) else { continue };
            if p.window != t.requestor || p.atom != t.property || p.state != Property::DELETE || t.done {
                continue;
            }
            let chunk = incr_chunk.unwrap_or(usize::MAX);
            let end = (t.offset + chunk).min(body.len());
            conn.change_property8(PropMode::REPLACE, t.requestor, t.property, utf8, &body[t.offset..end]).unwrap();
            // The final, empty write ends the transfer.
            t.done = t.offset == end;
            t.offset = end;
            conn.flush().unwrap();
            if t.done {
                eprintln!("fake_steam: INCR done");
                incr = None;
            }
            continue;
        }
        let Event::SelectionRequest(req) = event else { continue };
        let name = String::from_utf8(conn.get_atom_name(req.target).unwrap().reply().unwrap().name).unwrap();
        eprintln!("fake_steam: request for {name}");
        if mode == "slow" {
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        let mut property = req.property;
        if req.target == targets {
            conn.change_property32(PropMode::REPLACE, req.requestor, req.property, AtomEnum::ATOM, &offered).unwrap();
        } else if req.target == utf8 && text.is_some() {
            let body = text.as_ref().unwrap();
            match incr_chunk {
                Some(chunk) if body.len() > chunk => {
                    conn.change_window_attributes(req.requestor, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE)).unwrap();
                    conn.change_property32(PropMode::REPLACE, req.requestor, req.property, incr_atom, &[body.len() as u32]).unwrap();
                    incr = Some(Incr { requestor: req.requestor, property: req.property, offset: 0, done: false });
                }
                _ => {
                    conn.change_property8(PropMode::REPLACE, req.requestor, req.property, utf8, body).unwrap();
                }
            }
        } else if (req.target == uri_list && !path.is_empty()) || mode == "anyutf8" {
            conn.change_property8(PropMode::REPLACE, req.requestor, req.property, utf8, uri.as_bytes()).unwrap();
        } else if mode == "ignore" {
            conn.flush().unwrap();
            continue;
        } else {
            property = x11rb::NONE;
        }
        let ev = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property,
        };
        conn.send_event(false, req.requestor, EventMask::NO_EVENT, ev).unwrap();
        conn.flush().unwrap();
    }
}
