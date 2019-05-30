#[allow(dead_code)]
#[repr(u32)]
enum X11WmState {
    Withdrawn = 0,
    Normal    = 1,
    //             = 2,
    Iconic    = 3,
}

//Getting a handle for /proc and the pid of the focused window seem like orthogonal tasks - and they are
//however, we want to avoid having our view of /proc being inconsistent with the window's pid.
//If we open /proc before we get the pid, but the program is started after we got our /proc-handle,
//we won't find the process with our handle (or potentially worse, scan a program that just exited
//and happened to have the same pid).
//If we open /proc after we get the pid, the program might be closed before we get our proc-handle,
//resulting in a similar unfortunate scenario.
//Instead we open /proc after getting the window handle, but before getting its pid. I'm not sure
//however if getting the pid actually does fail if we try it with a stale window handle.
fn get_proc_and_focused_window_pid() -> Result<(openat::Dir, u32), String> {
    //FIXME: X11 endianess?
    use byteorder::{LittleEndian, ReadBytesExt};

    let (conn, screen_num) = xcb::Connection::connect(None).map_err(|error| format!("Unable to open X11 connection: {}.", error))?;

    let active_window_atom_cookie = xcb::intern_atom(&conn, false, "_NET_ACTIVE_WINDOW");
    let pid_atom_cookie           = xcb::intern_atom(&conn, false, "_NET_WM_PID");
    let state_atom_cookie         = xcb::intern_atom(&conn, false, "WM_STATE");

    //get window
    let root = conn.get_setup().roots().nth(screen_num as usize)
                   .ok_or_else(|| "Unable to select current screen.".to_string())?.root();

    let active_window_atom = active_window_atom_cookie.get_reply().map_err(|error| format!("Unable to retrieve _NET_ACTIVE_WINDOW atom: {}.", error))?.atom();

    let reply = xcb::get_property(&conn, false, root, active_window_atom, xcb::ATOM_WINDOW, 0, 1)
                    .get_reply()
                    .map_err(|error| format!("Unable to retrieve _NET_ACTIVE_WINDOW property from root: {}.", error))?;
    if reply.value_len() == 0 {
        return Err("Unable to retrieve _NET_ACTIVE_WINDOW property from root.".to_string());
    }
    assert_eq!(reply.value_len(), 1);
    let mut raw = reply.value();
    assert_eq!(raw.len(), 4, "_NET_ACTIVE_WINDOW property is expected to be at least 4 bytes.");
    let window = raw.read_u32::<LittleEndian>().unwrap() as xcb::Window;
    if window == xcb::WINDOW_NONE {
        return Err("No window is focused".to_string());
    }

    //open proc
    let proc = openat::Dir::open("/proc").map_err(|error| format!("Unable to open /proc: {}", error))?;

    //check withdrawn state
    let state_atom = state_atom_cookie.get_reply().map_err(|error| format!("Unable to retrieve WM_STATE atom: {}.", error))?.atom();

    match xcb::get_property(&conn, false, window, state_atom, state_atom, 0, 1).get_reply() {
        Ok(reply) => {
            if reply.value_len() == 0 {
                eprintln!("Unable to retrieve WM_STATE from focused window {}.", window);
            }
            else {
                assert_eq!(reply.value_len(), 1);
                let mut raw = reply.value();
                assert_eq!(raw.len(), 4, "WM_STATE property is expected to be at least 4 bytes.");
                let state = raw.read_u32::<LittleEndian>().unwrap();
                if state != X11WmState::Normal as u32 {
                    return Err(format!("Focused window {} is not in normal (visible) state ({} != {}); Ignoring.", window, state, X11WmState::Normal as u32));
                }
            }
        }
        Err(error) => {
            eprintln!("Unable to retrieve WM_STATE from focused window {}: {}", window, error);
        }
    };

    //get pid
    let pid_atom = pid_atom_cookie.get_reply().map_err(|error| format!("Unable to retrieve _NET_WM_PID: {}.", error))?.atom();

    let reply = xcb::get_property(&conn, false, window, pid_atom, xcb::ATOM_CARDINAL, 0, 1)
                    .get_reply()
                    .unwrap_or_else(|error| panic!("Unable to retrieve _NET_WM_PID from focused window {}: {}", window, error));
    if reply.value_len() == 0 {
        eprintln!("Unable to retrieve _NET_WM_PID from focused window {}; trying WM_CLASS.", window);
        //TODO: what's a good size here?
        let reply = xcb::get_property(&conn, false, window, xcb::ATOM_WM_CLASS, xcb::ATOM_STRING, 0, 64)
                        .get_reply()
                        .unwrap_or_else(|error| panic!("Unable to retrieve WM_CLASS from focused window {}: {}", window, error));
        let class = String::from_utf8(reply.value().iter().cloned().take_while(|c| *c != 0u8).collect::<Vec<_>>())
                           .unwrap_or_else(|error| panic!("Unable to decode {:#?}: {}", reply.value() as &[u8], error));
        //TODO: find processes named 'class', compare cwds
        return Err(format!("Unimplemented: Find processes named {}", class));
    }
    assert_eq!(reply.value_len(), 1);
    let mut raw = reply.value();
    assert_eq!(raw.len(), 4, "_NET_WM_PID property is expected to be at least 4 bytes");
    Ok((proc, raw.read_u32::<LittleEndian>().unwrap()))
}

enum Cwd {
    Regular(String),
    Priority(String),
}

impl Cwd {
    fn new<Str: PartialEq<str>>(cwd: String, exe: &str, priority_commands: &[Str]) -> Self {
        if priority_commands.iter().any(|elem| elem == exe) { Cwd::Priority(cwd) } else { Cwd::Regular(cwd) }
    }
}

impl<'a> Into<String> for Cwd {
    fn into(self) -> String {
        match self {
            Cwd::Regular(cwd) => cwd,
            Cwd::Priority(cwd) => cwd,
        }
    }
}

fn get_child_cwd<Str: PartialEq<str>>(proc: &openat::Dir, pid: u32, priority_commands: &[Str]) -> Result<Cwd, String> {
    use std::io::Read;

    //find children...
    //get cwd
    let exe = proc.read_link(format!("{}/exe", pid))
                  .map_err(|error| format!("Unable to read /proc/{}/exe: {}.", pid, error))?
                  .to_str().unwrap().to_owned();
    let cwd = proc.read_link(format!("{}/cwd", pid))
                  .map_err(|error| format!("Unable to read /proc/{}/cwd: {}.", pid, error))?
                  .to_str().unwrap().to_owned();
    let cwd = Cwd::new(cwd, &exe, priority_commands);
    //FIXME: tid
    let tid = pid;
    let mut children = String::new();
    proc.open_file(format!("{}/task/{}/children", pid, tid))
        .map_err(|error| format!("Unable to open /proc/{}/task/{}/children: {}.", pid, tid, error))?
        .read_to_string(&mut children)
        .map_err(|error| format!("Unable to read from /proc/{}/task/{}/children: {}.", pid, tid, error))?;
    if children.is_empty() {
        //no children
        return Ok(cwd);
    }

    //get child cwd
    let mut children = children.split(' ');
    let child = children.next().unwrap();
    if children.next().is_some() {
        //TODO: this isn't a problem if all children have the same cwd
        eprintln!("Warning: Process {} has multiple children. Following {}.", pid, child);
    }
    Ok(match get_child_cwd(proc, child.parse().unwrap(), priority_commands) {
        Ok(child_cwd) => {
            match (&cwd, &child_cwd) {
                //prefer child cwd
                (_, Cwd::Priority(_)) => child_cwd,
                (Cwd::Regular(_), Cwd::Regular(_)) => child_cwd,
                //unless cwd is prioritized
                (Cwd::Priority(_), Cwd::Regular(_)) => cwd,
            }
        },
        Err(_) => cwd,
    })
}

fn main() {
    let cwd = get_focused_window_pid().and_then(|(proc, pid)| {
        get_child_cwd(&proc, pid, &std::env::args().skip(1).collect::<Vec<_>>()).and_then(|cwd| cwd.into())
    }).unwrap_or_else(|error| {
        eprintln!("{}", error);
        dirs::home_dir().unwrap().into_os_string().into_string().unwrap()
    });
    println!("{}", cwd);
}
