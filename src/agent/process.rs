//! Foreground shell ownership: dropping a tool future also kills its shell group.
use std::io;
use tokio::process::{Child, Command};

pub(crate) struct ProcessGroup {
    #[cfg(unix)]
    id: u32,
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the child was spawned in its own process group; a negative
        // pid targets that group, never the application's terminal group.
        unsafe {
            libc::kill(-(self.id as i32), libc::SIGKILL);
        }
    }
}

pub(crate) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessGroup)> {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn()?;
    let group = ProcessGroup {
        #[cfg(unix)]
        id: child.id().expect("a newly spawned child has a pid"),
    };
    Ok((child, group))
}
