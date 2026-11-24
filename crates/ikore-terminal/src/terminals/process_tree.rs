//! Windows child startup and process-tree ownership.
//! Spawn through std/Tokio, assign the still-suspended process, then release its
//! sole initial thread. This is process-tree control, not an OS access sandbox.

use std::fmt;
use std::io;

use tokio::process::{Child, Command};

#[derive(Debug)]
pub enum ProcessTreeSpawnError {
    Spawn(io::Error),
    Control(io::Error),
}

impl ProcessTreeSpawnError {
    /// The underlying Win32 code survives operation-context wrapping.
    pub fn raw_os_error(&self) -> Option<i32> {
        let error = match self {
            Self::Spawn(error) | Self::Control(error) => error,
        };
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(error) = source {
            if let Some(code) = error
                .downcast_ref::<io::Error>()
                .and_then(io::Error::raw_os_error)
            {
                return Some(code);
            }
            source = error.source();
        }
        None
    }
}

impl fmt::Display for ProcessTreeSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "Process creation failed: {error}"),
            Self::Control(error) => write!(f, "Process-tree control failed: {error}"),
        }
    }
}

impl std::error::Error for ProcessTreeSpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Spawn(error) | Self::Control(error) => error,
        })
    }
}

impl From<ProcessTreeSpawnError> for io::Error {
    fn from(error: ProcessTreeSpawnError) -> Self {
        let kind = match &error {
            ProcessTreeSpawnError::Spawn(cause) | ProcessTreeSpawnError::Control(cause) => {
                cause.kind()
            }
        };
        // CLI callers may use `?`; preserve the typed distinction in get_ref().
        // The composition runner must match the original enum before conversion.
        io::Error::new(kind, error)
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct OperationError {
    operation: &'static str,
    cause: io::Error,
}

#[cfg(windows)]
impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.operation, self.cause)
    }
}

#[cfg(windows)]
impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

#[cfg(windows)]
fn operation(operation: &'static str, cause: io::Error) -> io::Error {
    io::Error::new(cause.kind(), OperationError { operation, cause })
}

#[cfg(windows)]
fn last_error(name: &'static str) -> io::Error {
    // Capture GetLastError before RAII closes another handle.
    let cause = io::Error::last_os_error();
    operation(name, cause)
}

#[cfg(windows)]
#[must_use = "keep the job until the complete child process tree has exited"]
pub struct ProcessTreeJob {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(not(windows))]
#[must_use = "keep this guard until the child process has exited"]
pub struct ProcessTreeJob;

#[cfg(windows)]
mod win {
    use super::{
        Child, Command, ProcessTreeJob, ProcessTreeSpawnError, fmt, io, last_error, operation,
    };
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{
        ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessId, GetProcessIdOfThread, OpenThread,
        ResumeThread, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME, TerminateProcess,
        WaitForSingleObject,
    };

    const CLEANUP_WAIT_MS: u32 = 5_000;

    fn raw(handle: &OwnedHandle) -> HANDLE {
        handle.as_raw_handle().cast()
    }

    /// Keeps a newly spawned child owned across every fallible pre-resume step.
    /// On panic, direct termination is the pre-assignment fallback; the job is
    /// still owned by the outer frame and closes after this guard is dropped.
    struct PendingChild(Option<Child>);

    impl PendingChild {
        fn child(&self) -> &Child {
            self.0.as_ref().expect("pending child is owned")
        }

        fn terminate_and_wait(&mut self) -> io::Result<()> {
            let Some(mut child) = self.0.take() else {
                return Ok(());
            };
            let Some(handle) = child.raw_handle() else {
                return child.try_wait().and_then(|status| {
                    status
                        .map(|_| ())
                        .ok_or_else(|| io::Error::other("child handle lost before cleanup"))
                });
            };
            let process = handle.cast();
            // SAFETY: Child owns the process handle throughout these synchronous
            // calls. No wait/try_wait consumes it until the raw calls finish.
            let initial = unsafe { WaitForSingleObject(process, 0) };
            match initial {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => {
                    // SAFETY: the freshly created child handle has terminate access;
                    // only this helper's child is targeted, never a numeric PID.
                    if unsafe { TerminateProcess(process, 1) } == 0 {
                        let failure = last_error("TerminateProcess(cleanup)");
                        // Termination can race a separate termination. Accept only
                        // an actually signalled process, never ERROR_ACCESS_DENIED alone.
                        // SAFETY: Child still owns the same valid process handle.
                        if unsafe { WaitForSingleObject(process, 0) } != WAIT_OBJECT_0 {
                            let _ = child.start_kill();
                            return Err(failure);
                        }
                    }
                }
                WAIT_FAILED => return Err(last_error("WaitForSingleObject(cleanup initial)")),
                other => {
                    return Err(io::Error::other(format!(
                        "unexpected initial cleanup wait {other}"
                    )));
                }
            }
            // SAFETY: same live process handle, bounded wait; no other code has
            // consumed Child and the initial primary thread may still be suspended.
            match unsafe { WaitForSingleObject(process, CLEANUP_WAIT_MS) } {
                WAIT_OBJECT_0 => {
                    // Let Tokio settle its own child state after Win32 confirmed exit.
                    match child
                        .try_wait()
                        .map_err(|e| operation("Child::try_wait(cleanup)", e))?
                    {
                        Some(_) => Ok(()),
                        None => Err(io::Error::other(
                            "Child::try_wait did not confirm signalled child exit",
                        )),
                    }
                }
                WAIT_FAILED => Err(last_error("WaitForSingleObject(cleanup)")),
                WAIT_TIMEOUT => {
                    let _ = child.start_kill();
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "cleanup did not confirm process exit",
                    ))
                }
                other => Err(io::Error::other(format!("unexpected cleanup wait {other}"))),
            }
            // Child's kill_on_drop remains a final fallback. Normal error paths
            // return cleanup failures, rather than reporting successful containment.
        }

        fn release(mut self) -> Child {
            self.0.take().expect("validated child is owned")
        }
    }

    impl Drop for PendingChild {
        fn drop(&mut self) {
            if self.0.is_some() {
                // Drop cannot return an error. Every ordinary error path below
                // calls this explicitly and preserves any cleanup failure.
                let _ = self.terminate_and_wait();
            }
        }
    }

    fn process_handle(child: &Child) -> io::Result<HANDLE> {
        child
            .raw_handle()
            .map(|h| h.cast())
            .ok_or_else(|| io::Error::other("suspended child process handle is unavailable"))
    }

    fn ensure_alive(process: HANDLE) -> io::Result<()> {
        // SAFETY: borrowed from Child; wait with zero timeout never consumes it.
        match unsafe { WaitForSingleObject(process, 0) } {
            WAIT_TIMEOUT => Ok(()),
            WAIT_OBJECT_0 => Err(io::Error::other(
                "suspended process terminated before release",
            )),
            WAIT_FAILED => Err(last_error("WaitForSingleObject(pre-resume)")),
            other => Err(io::Error::other(format!(
                "unexpected pre-resume wait {other}"
            ))),
        }
    }

    fn sole_initial_thread(process: HANDLE) -> io::Result<OwnedHandle> {
        ensure_alive(process)?;
        // SAFETY: borrowed live process handle; required query access comes from
        // std's CreateProcess handle. A zero return is captured immediately.
        let pid = unsafe { GetProcessId(process) };
        if pid == 0 {
            return Err(last_error("GetProcessId"));
        }
        // Only thread snapshot: module/heap snapshots are inappropriate before
        // the suspended child's loader has initialized. PID parameter is ignored
        // by TH32CS_SNAPTHREAD; explicitly filter every entry by owner PID below.
        // SAFETY: thread-only snapshot has no input pointers or child-loader dependency.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateToolhelp32Snapshot"));
        }
        // SAFETY: successful API returned a fresh owned snapshot handle, not null
        // or INVALID_HANDLE_VALUE. No other owner closes it.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.cast()) };
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        // SAFETY: initialized full-size THREADENTRY32 and live snapshot handle.
        let mut found = unsafe { Thread32First(raw(&snapshot), &mut entry) };
        let mut thread_id = None;
        if found == 0 {
            let failure = io::Error::last_os_error();
            if failure.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(operation("Thread32First", failure));
            }
        }
        while found != 0 {
            if entry.dwSize < size_of::<THREADENTRY32>() as u32 {
                return Err(io::Error::other(
                    "short THREADENTRY32 cannot establish ownership",
                ));
            }
            if entry.th32OwnerProcessID == pid && thread_id.replace(entry.th32ThreadID).is_some() {
                return Err(io::Error::other(
                    "suspended child has more than one thread; primary is ambiguous",
                ));
            }
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            // SAFETY: same live snapshot and reset full-size output structure.
            found = unsafe { Thread32Next(raw(&snapshot), &mut entry) };
            if found == 0 {
                let failure = io::Error::last_os_error();
                if failure.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                    return Err(operation("Thread32Next", failure));
                }
            }
        }
        let tid = thread_id
            .ok_or_else(|| io::Error::other("suspended child's initial thread was not found"))?;
        // SAFETY: OpenThread receives a snapshot identifier, with minimum rights
        // for ownership validation and resume; no debug privilege or inheritance.
        let thread = unsafe {
            OpenThread(
                THREAD_QUERY_LIMITED_INFORMATION | THREAD_SUSPEND_RESUME,
                0,
                tid,
            )
        };
        if thread.is_null() {
            return Err(last_error("OpenThread"));
        }
        // SAFETY: newly opened non-null handle; exactly one owning wrapper.
        let thread = unsafe { OwnedHandle::from_raw_handle(thread.cast()) };
        // Validate the handle, not only the possibly stale snapshot identifier.
        // SAFETY: thread is an owned handle opened with query-limited access.
        let actual_owner = unsafe { GetProcessIdOfThread(raw(&thread)) };
        if actual_owner == 0 {
            return Err(last_error("GetProcessIdOfThread"));
        }
        if actual_owner != pid {
            return Err(io::Error::other("thread handle belongs to another process"));
        }
        ensure_alive(process)?;
        Ok(thread)
    }

    impl ProcessTreeJob {
        fn create() -> io::Result<Self> {
            // SAFETY: unnamed job, default security; returned handle is checked
            // before transferring unique ownership to OwnedHandle.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(last_error("CreateJobObjectW"));
            }
            let job = Self {
                // SAFETY: CreateJobObjectW returned a fresh non-null handle;
                // this wrapper is its sole owner and closes it on every path.
                handle: unsafe { OwnedHandle::from_raw_handle(handle.cast()) },
            };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: exact initialized structure and size, with owned valid job.
            if unsafe {
                SetInformationJobObject(
                    raw(&job.handle),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(last_error("SetInformationJobObject"));
            }
            Ok(job)
        }

        pub fn spawn(command: &mut Command) -> Result<(Child, Self), ProcessTreeSpawnError> {
            Self::spawn_inner(command, |_, _, _| Ok(()))
        }

        // Private synchronous seam: tests can inspect the actual suspended thread
        // and inject a pre-resume failure without adding authority to callers.
        fn spawn_inner(
            command: &mut Command,
            before_resume: impl FnOnce(&Child, &Self, &OwnedHandle) -> io::Result<()>,
        ) -> Result<(Child, Self), ProcessTreeSpawnError> {
            // Create/configure the job first; failure cannot leave a child behind.
            let job = Self::create().map_err(ProcessTreeSpawnError::Control)?;
            command.kill_on_drop(true);
            // Private host policy. Do not reconstruct argv, environment or stdio,
            // and do not preserve caller-provided breakaway/debug flags.
            command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
            let child = command.spawn().map_err(ProcessTreeSpawnError::Spawn)?;
            let mut pending = PendingChild(Some(child));
            let controlled = (|| {
                let process = process_handle(pending.child())?;
                ensure_alive(process)?;
                // SAFETY: owned job, suspended child process handle with the access
                // rights supplied by CreateProcess. Any failure remains fatal.
                if unsafe { AssignProcessToJobObject(raw(&job.handle), process) } == 0 {
                    return Err(last_error("AssignProcessToJobObject"));
                }
                let primary = sole_initial_thread(process)?;
                before_resume(pending.child(), &job, &primary)
                    .map_err(|e| operation("before_resume", e))?;
                ensure_alive(process)?;
                // SAFETY: sole thread of the still-live child was validated by
                // handle. The job is already assigned and remains owned across
                // this call; the production hook cannot run arbitrary code.
                let previous = unsafe { ResumeThread(raw(&primary)) };
                if previous == u32::MAX {
                    return Err(last_error("ResumeThread"));
                }
                if previous != 1 {
                    return Err(io::Error::other(format!(
                        "ResumeThread returned suspend count {previous}, expected 1"
                    )));
                }
                Ok(())
            })();
            if let Err(primary) = controlled {
                let error = match pending.terminate_and_wait() {
                    Ok(()) => primary,
                    Err(cleanup) => {
                        io::Error::new(primary.kind(), CleanupError { primary, cleanup })
                    }
                };
                return Err(ProcessTreeSpawnError::Control(error));
            }
            Ok((pending.release(), job))
        }

        pub fn terminate(&self) {
            // Preserve the established cancellation API. Drop remains the job's
            // final KILL_ON_JOB_CLOSE fallback; spawn failures use checked cleanup.
            let _ = self.terminate_checked();
        }

        fn terminate_checked(&self) -> io::Result<()> {
            // SAFETY: valid owned job handle, no handle/thread affinity.
            if unsafe { TerminateJobObject(raw(&self.handle), 1) } == 0 {
                Err(last_error("TerminateJobObject"))
            } else {
                Ok(())
            }
        }

        pub(crate) fn observation(&self) -> io::Result<(u64, u64, u64, u64)> {
            use windows_sys::Win32::System::JobObjects::{
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
                QueryInformationJobObject,
            };
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            // SAFETY: exact initialized output structure, size and owned job.
            if unsafe {
                QueryInformationJobObject(
                    raw(&self.handle),
                    JobObjectBasicAccountingInformation,
                    (&raw mut accounting).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_error("QueryInformationJobObject(accounting)"));
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // SAFETY: same owned job with correctly sized initialized output.
            if unsafe {
                QueryInformationJobObject(
                    raw(&self.handle),
                    JobObjectExtendedLimitInformation,
                    (&raw mut limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_error("QueryInformationJobObject(limits)"));
            }
            Ok((
                accounting.TotalUserTime.max(0) as u64,
                accounting.TotalKernelTime.max(0) as u64,
                limits.PeakProcessMemoryUsed as u64,
                limits.PeakJobMemoryUsed as u64,
            ))
        }
    }

    #[derive(Debug)]
    struct CleanupError {
        primary: io::Error,
        cleanup: io::Error,
    }
    impl fmt::Display for CleanupError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "{}; cleanup could not confirm containment: {}",
                self.primary, self.cleanup
            )
        }
    }
    impl std::error::Error for CleanupError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.primary)
        }
    }

    // OwnedHandle closes the non-inheritable job once. KILL_ON_JOB_CLOSE is the
    // final tree safety net; no manual unsafe Send/Sync or duplicate raw owner.

    #[cfg(test)]
    mod tests {
        include!("process_tree_tests.rs");
    }
}

#[cfg(not(windows))]
impl ProcessTreeJob {
    pub fn spawn(command: &mut Command) -> Result<(Child, Self), ProcessTreeSpawnError> {
        command.kill_on_drop(true);
        Ok((command.spawn().map_err(ProcessTreeSpawnError::Spawn)?, Self))
    }
    pub fn terminate(&self) {}
    pub(crate) fn observation(&self) -> io::Result<(u64, u64, u64, u64)> {
        Ok((0, 0, 0, 0))
    }
}
