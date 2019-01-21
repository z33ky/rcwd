#[allow(dead_code)]
enum X11WmState {
    Withdrawn = 0,
    Normal    = 1,
    //             = 2,
    Iconic    = 3,
}

fn get_focused_window_pid() -> std::option::Option<u32> {
    //FIXME: X11 endianess?
    use byteorder::{LittleEndian, ReadBytesExt};

    let (conn, screen_num) = {
        match xcb::Connection::connect(None) {
            Err(error) => {
                eprintln!("Unable to open X11 connection: {}", error);
                return None;
            }
            Ok(result) => result,
        }
    };

    let active_window_atom_cookie = xcb::intern_atom(&conn, false, "_NET_ACTIVE_WINDOW");
    let pid_atom_cookie           = xcb::intern_atom(&conn, false, "_NET_WM_PID");
    let state_atom_cookie         = xcb::intern_atom(&conn, false, "WM_STATE");

    //get window
    let root = conn.get_setup().roots().nth(screen_num as usize).expect("Unable to select current screen").root();

    let active_window_atom = active_window_atom_cookie.get_reply().expect("Unable to retrieve _NET_ACTIVE_WINDOW atom").atom();

    let reply = xcb::get_property(&conn, false, root, active_window_atom, xcb::ATOM_WINDOW, 0, 1).get_reply().expect("Unable to retrieve _NET_ACTIVE_WINDOW property from root");
    if reply.value_len() == 0 {
        eprintln!("Unable to retrieve _NET_ACTIVE_WINDOW property from root");
        return None;
    }
    assert_eq!(reply.value_len(), 1);
    let mut raw = reply.value();
    assert_eq!(raw.len(), 4, "_NET_ACTIVE_WINDOW property is expected to be at least 4 bytes");
    let window = raw.read_u32::<LittleEndian>().unwrap() as xcb::Window;
    if window == xcb::WINDOW_NONE {
        eprintln!("No window is focused.");
        return None;
    }

    //check withdrawn state
    let state_atom = state_atom_cookie.get_reply().expect("Unable to retrieve WM_STATE atom").atom();

    match xcb::get_property(&conn, false, window, state_atom, state_atom, 0, 1).get_reply() {
        Ok(reply) => {
            if reply.value_len() == 0 {
                eprintln!("Unable to retrieve WM_STATE from focused window {}", window);
            }
            else {
                assert_eq!(reply.value_len(), 1);
                let mut raw = reply.value();
                assert_eq!(raw.len(), 4, "WM_STATE property is expected to be at least 4 bytes");
                let state = raw.read_u32::<LittleEndian>().unwrap();
                if state != X11WmState::Normal as u32 {
                    eprintln!("Focused window {} is not in normal (visible) state ({} != {}); Ignoring.", window, state, X11WmState::Normal as u32);
                    return None;
                }
            }
        }
        Err(error) => {
            eprintln!("Unable to retrieve WM_STATE from focused window {}: {}", window, error);
        }
    };

    //get pid
    let pid_atom = pid_atom_cookie.get_reply().expect("Unable to retrieve _NET_WM_PID").atom();

    let reply = xcb::get_property(&conn, false, window, pid_atom, xcb::ATOM_CARDINAL, 0, 1).get_reply().unwrap_or_else(|error| panic!("Unable to retrieve _NET_WM_PID from focused window {}: {}", window, error));
    if reply.value_len() == 0 {
        eprintln!("Unable to retrieve _NET_WM_PID from focused window {}; trying WM_CLASS.", window);
        //TODO: what's a good size here?
        let reply = xcb::get_property(&conn, false, window, xcb::ATOM_WM_CLASS, xcb::ATOM_STRING, 0, 64).get_reply().unwrap_or_else(|error| panic!("Unable to retrieve WM_CLASS from focused window {}: {}", window, error));
        let class = std::string::String::from_utf8(reply.value().iter().cloned().take_while(|c| *c != 0u8).collect::<std::vec::Vec<_>>()).unwrap_or_else(|error| panic!("Unable to decode {:#?}: {}", reply.value() as &[u8], error));
        //TODO: find processes named 'class', compare cwds
        eprintln!("Unimplemented: Find processes named {}", class);
        return None;
    }
    assert_eq!(reply.value_len(), 1);
    let mut raw = reply.value();
    assert_eq!(raw.len(), 4, "_NET_WM_PID property is expected to be at least 4 bytes");
    Some(raw.read_u32::<LittleEndian>().unwrap())
}

enum Cwd {
    Regular(std::string::String),
    Priority(std::string::String),
}

impl Cwd {
    fn new<Str: std::convert::AsRef<str> + std::cmp::PartialEq<str>>(cwd: std::string::String, exe: &str, priority_commands: &[Str]) -> Self {
        if priority_commands.iter().any(|elem| elem == exe) { Cwd::Priority(cwd) } else { Cwd::Regular(cwd) }
    }
}

impl<'a> std::convert::Into<&'a str> for &'a Cwd {
    fn into(self) -> &'a str {
        match self {
            Cwd::Regular(cwd) => cwd,
            Cwd::Priority(cwd) => cwd,
        }
    }
}

impl std::fmt::Display for Cwd {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.into())
    }
}

fn get_child_cwd<Str: std::convert::AsRef<str> + std::cmp::PartialEq<str>>(pid: u32, priority_commands: &[Str]) -> Cwd {
    use std::io::Read;

    //find children
    let mut children = std::string::String::new();
    //get cwd
    //FIXME: potential race
    let exe = std::fs::read_link(format!("/proc/{}/exe", pid)).expect("Unable to read /proc/$PID/exe").to_str().unwrap().to_owned();
    let cwd = Cwd::new(std::fs::read_link(format!("/proc/{}/cwd", pid)).expect("Unable to read /proc/$PID/cwd").to_str().unwrap().into(), &exe, priority_commands);
    //FIXME: tid
    let tid = pid;
    std::fs::File::open(format!("/proc/{}/task/{}/children", pid, tid)).expect("Unable to read /proc/$PID/task/$PID/children").read_to_string(&mut children).unwrap();
    if children.is_empty() {
        //no children, read cwd
        return cwd;
    }
    //get child cwd
    let mut children = children.split(' ');
    let child = children.next().unwrap();
    if children.next().is_some() {
        //TODO: this isn't a problem if all children have the same cwd
        eprintln!("Warning: Process {} has multiple children. Following {}.", pid, child);
    }
    let child_cwd = get_child_cwd(child.parse().unwrap(), priority_commands);
    match child_cwd {
        Cwd::Regular(_) => match cwd {
            Cwd::Regular(_) => child_cwd,
            Cwd::Priority(_) => cwd,
        }
        Cwd::Priority(_) => child_cwd,
    }
}

fn main() {
    match get_focused_window_pid() {
        std::option::Option::Some(pid) => println!("{}", get_child_cwd(pid, &std::env::args().skip(1).collect::<std::vec::Vec<_>>())),
        std::option::Option::None => println!("{}", dirs::home_dir().unwrap().to_str().unwrap()),
    }
}