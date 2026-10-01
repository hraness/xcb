//! The operator's stop request: SIGINT or SIGTERM on Unix; Ctrl-C, Ctrl-Break,
//! or closing the console window on Windows. Install it before any work that
//! must settle, then await [`Stop::recv`] beside that work.

impl Stop {
    /// A phase owns its children until it returns. Record a stop request, let
    /// that same phase finish cleanup, and stop before starting the next phase.
    pub async fn settle<T>(
        &mut self,
        work: impl Future<Output = xcb_runtime::Result<T>>,
    ) -> xcb_runtime::Result<T> {
        // Do not start another phase if the stop is already queued.
        tokio::select! {
            biased;
            _ = self.recv() => return Err(interrupted()),
            _ = std::future::ready(()) => {},
        }
        settle(work, self.recv()).await
    }
}

fn interrupted() -> xcb_runtime::Error {
    xcb_runtime::Error::Unavailable("setup interrupted; running work has finished cleanup")
}

async fn settle<T>(
    work: impl Future<Output = xcb_runtime::Result<T>>,
    request: impl Future<Output = ()>,
) -> xcb_runtime::Result<T> {
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = request => {
            eprintln!("Stopping after the current setup step finishes cleanup.");
            // Preserve uncertain-cleanup errors instead of claiming settlement.
            work.await?;
            Err(interrupted())
        },
        result = &mut work => result,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stopped_phase_is_awaited_through_cleanup() {
        let mut settled = false;
        let result = settle(
            async {
                tokio::task::yield_now().await;
                settled = true;
                Ok(())
            },
            std::future::ready(()),
        )
        .await;
        assert!(result.is_err());
        assert!(settled);
    }

    #[tokio::test]
    async fn stopped_phase_preserves_unproven_cleanup() {
        let result: xcb_runtime::Result<()> = settle(
            async { Err(xcb_runtime::Error::CleanupUnproven) },
            std::future::ready(()),
        )
        .await;
        assert!(matches!(result, Err(xcb_runtime::Error::CleanupUnproven)));
    }

    #[tokio::test]
    async fn successful_phase_returns_without_waiting_for_stop() {
        assert_eq!(
            settle(async { Ok(42) }, std::future::pending())
                .await
                .unwrap(),
            42
        );
    }
}
