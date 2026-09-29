//! The operator's stop request: SIGINT or SIGTERM on Unix; Ctrl-C, Ctrl-Break,
//! or closing the console window on Windows. Install it before any work that
//! must settle, then await [`Stop::recv`] beside that work.

#[cfg(unix)]
pub struct Stop {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Stop {
    pub fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
    }
}

#[cfg(windows)]
pub struct Stop {
    interrupt: tokio::signal::windows::CtrlC,
    brk: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
}

#[cfg(windows)]
impl Stop {
    pub fn install() -> std::io::Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
        Ok(Self {
            interrupt: ctrl_c()?,
            brk: ctrl_break()?,
            close: ctrl_close()?,
        })
    }

    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.brk.recv() => {},
            _ = self.close.recv() => {},
        }
    }
}
