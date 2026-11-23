//! Terminals (PowerShell, cmd, bash...) via pseudo-terminal (ConPTY on Windows).

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use tao::event_loop::EventLoopProxy;

use crate::config::ShellDef;
use crate::UserEvent;

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    pub fn spawn(
        shell: &ShellDef,
        cols: u16,
        rows: u16,
        pane: u32,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let mut cmd = CommandBuilder::new(&shell.program);
        cmd.args(&shell.args);
        let cwd = shell
            .cwd
            .clone()
            .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_string_lossy().into_owned()));
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        for (k, v) in &shell.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if proxy.send_event(UserEvent::PtyOutput { pane, data: buf[..n].to_vec() }).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = proxy.send_event(UserEvent::PtyExit { pane });
        });
        Ok(PtySession { master: pair.master, writer, child })
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if cols > 0 && rows > 0 {
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
