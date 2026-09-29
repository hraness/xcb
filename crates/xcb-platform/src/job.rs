#![allow(unsafe_code)]
//! Job Objects: the Windows counterpart of a Unix process group.
//!
//! A child is started suspended (`CREATE_SUSPENDED`), assigned to a new job
//! whose limit is `KILL_ON_JOB_CLOSE`, and only then resumed, so every
//! process it creates is born inside the job. Terminating the job kills all
//! of them; closing the last job handle (including when xcb itself dies)
//! does the same. "The group is absent" is "the job has no active process".

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

/// Creation flags for a child that [`Job::adopt`] will assign and resume.
pub const SPAWN_SUSPENDED: u32 = CREATE_SUSPENDED;

/// A kill-on-close job that holds one child and all its descendants.
#[derive(Debug)]
pub struct Job(OwnedHandle);

impl Job {
    pub fn new() -> io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    /// Assign a child started with [`SPAWN_SUSPENDED`] to this job, then
    /// resume its one thread. On error the child stays suspended; the
    /// caller kills it.
    pub fn adopt(&self, process: RawHandle, pid: u32) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        resume_threads(pid)
    }

    /// Kill every process in the job (`killpg(SIGKILL)`).
    pub fn kill(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The number of processes still running in the job.
    pub fn active(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        if unsafe {
            QueryInformationJobObject(
                self.handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses)
    }
}

/// Resume every thread of a process created suspended (it has exactly one).
fn resume_threads(pid: u32) -> io::Result<()> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut resumed = 0;
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed += 1;
        }
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    if resumed == 0 {
        return Err(io::Error::other("the suspended child has no thread"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    #[test]
    fn a_job_holds_the_child_and_its_descendants_until_killed() {
        let job = Job::new().unwrap();
        // cmd starts a grandchild (ping) that outlives cmd's own wait.
        let mut child = std::process::Command::new("cmd")
            .args([
                "/C",
                "start /B ping -n 30 127.0.0.1 >NUL & ping -n 30 127.0.0.1 >NUL",
            ])
            .creation_flags(SPAWN_SUSPENDED)
            .spawn()
            .unwrap();
        job.adopt(child.as_raw_handle(), child.id()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while job.active().unwrap() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "descendants never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        job.kill().unwrap();
        child.wait().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while job.active().unwrap() != 0 {
            assert!(std::time::Instant::now() < deadline, "job never emptied");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
