//! An X11 CLIPBOARD owner that behaves like Steam's "Copy to clipboard"
//! (ValveSoftware/steam-for-linux#11476): offers TARGETS + text/uri-list, and
//! answers text/uri-list with the property typed UTF8_STRING.
//! Mode (argv[1]) for targets it doesn't offer:
//!   ignore  - never answers (worst case)
//!   refuse  - answers with property None (ICCCM-correct)
//!   anyutf8 - answers EVERY request with the uri-list typed UTF8_STRING
//!   slow    - like refuse, but each reply takes 1.5 s (a busy owner)
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ignore".into());
    let path = std::env::args().nth(2).unwrap_or_else(|| "/tmp/shot.jpg".into());
    let uri = format!("file://{path}\r\n");
    let (conn, screen) = x11rb::connect(None).unwrap();
    let root = conn.setup().roots[screen].root;
    let win = conn.generate_id().unwrap();
    conn.create_window(0, win, root, 0, 0, 1, 1, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
    let atom = |n: &str| conn.intern_atom(false, n.as_bytes()).unwrap().reply().unwrap().atom;
    let clipboard = atom("CLIPBOARD");
    let targets = atom("TARGETS");
    let uri_list = atom("text/uri-list");
    let utf8 = atom("UTF8_STRING");
    conn.set_selection_owner(win, clipboard, x11rb::CURRENT_TIME).unwrap();
    conn.flush().unwrap();
    eprintln!("fake_steam[{mode}]: owning CLIPBOARD");
    loop {
        let Event::SelectionRequest(req) = conn.wait_for_event().unwrap() else { continue };
        let name = String::from_utf8(conn.get_atom_name(req.target).unwrap().reply().unwrap().name).unwrap();
        eprintln!("fake_steam: request for {name}");
        if mode == "slow" {
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        let mut property = req.property;
        if req.target == targets {
            conn.change_property32(PropMode::REPLACE, req.requestor, req.property, AtomEnum::ATOM, &[targets, uri_list]).unwrap();
        } else if req.target == uri_list || mode == "anyutf8" {
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
