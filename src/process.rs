//! This module provides a drop-in replacement for most of the functionality
//! inside the `std::process` module.
//!
//! In addition, the `Command` type in this module provides the possibility
//! to inject code into the child process after it is spawned.

use std::{env, fs, ops, io, mem, ptr, thread};
use std::fmt::{self, Formatter};
use std::ascii::AsciiExt;
use std::sync::mpsc::{self, Receiver};
use std::path::Path;
use std::ffi::{OsStr, OsString};
use std::collections::HashMap;
use std::os::windows::prelude::*;
use std::os::raw::c_void;

use {k32, w};
use miow::pipe::{self, AnonRead, AnonWrite};

use handle::Handle;
use inject::{Module, Injector};

struct ProcessGuard(Option<Handle>);

impl ProcessGuard {
    fn new(process: Handle) -> ProcessGuard { ProcessGuard(Some(process)) }

    fn release(mut self) -> Handle {
        let result = self.0.take().unwrap();
        mem::forget(self);
        result
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        unsafe { k32::TerminateProcess(self.0.as_ref().unwrap().as_inner(), 1); }
    }
}

impl ops::Deref for ProcessGuard {
    type Target = Handle;

    fn deref(&self) -> &Handle { self.0.as_ref().unwrap() }
}


/// Representation of a running or exited child process.
///
/// This structure is used to represent and manage child processes. A child
/// process is created via the `Command` struct, which configures the spawning
/// process and can itself be constructed using a builder-style interface.
pub struct Child {
    process: Handle,
    id: w::DWORD,

    status: Option<ExitStatus>,

    /// The handle for writing to the child's stdin, if it has been captured
    pub stdin: Option<ChildStdin>,
    /// The handle for reading from the child's stdout, if it has been captured
    pub stdout: Option<ChildStdout>,
    /// The handle for reading from the child's stderr, if it has been captured
    pub stderr: Option<ChildStderr>
}

impl Child {
    /// Forces the child to exit.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid argument: can't kill an exited process"));
        }

        if unsafe { k32::TerminateProcess(self.process.as_inner(), 1) } == w::FALSE {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Returns the OS-assigned process identifier associated with this child.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Waits for the child to exit completely, returning the status that it
    /// exited with. This function will continue to have the same return value
    /// after it has been called at least once.
    ///
    /// The stdin handle to the child process, if any, will be closed
    /// before waiting. This helps avoid deadlock: it ensures that the
    /// child does not block waiting for input from the parent, while
    /// the parent waits for the child to exit.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        mem::drop(self.stdin.take());

        try!(self.process.wait());

        let mut exit_code: w::DWORD = unsafe { mem::uninitialized() };
        if unsafe { k32::GetExitCodeProcess(self.process.as_inner(), &mut exit_code) } == w::FALSE {
            return Err(io::Error::last_os_error());
        }
        let status = ExitStatus(exit_code);
        self.status = Some(status);

        Ok(status)
    }

    /// Simultaneously waits for the child to exit and collect all remaining
    /// output on the stdout/stderr handles, returning a `Output`
    /// instance.
    ///
    /// The stdin handle to the child process, if any, will be closed
    /// before waiting. This helps avoid deadlock: it ensures that the
    /// child does not block waiting for input from the parent, while
    /// the parent waits for the child to exit.
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        fn read<T: io::Read + Send + 'static>(stream: Option<T>) -> Receiver<io::Result<Vec<u8>>> {
            let (tx, rx) = mpsc::channel();
            match stream {
                Some(stream) => {
                    thread::spawn(move || {
                        let mut stream = stream;
                        let mut vec = Vec::new();
                        let res = stream.read_to_end(&mut vec);
                        tx.send(res.map(|_| vec)).unwrap();
                    });
                }
                None => tx.send(Ok(Vec::new())).unwrap()
            }
            rx
        }

        mem::drop(self.stdin.take());

        let stdout = read(self.stdout.take());
        let stderr = read(self.stderr.take());
        let status = try!(self.wait());

        Ok(Output {
            status: status,
            stdout: stdout.recv().unwrap().unwrap_or_else(|_| Vec::new()),
            stderr: stderr.recv().unwrap().unwrap_or_else(|_| Vec::new())
        })
    }
}

impl AsRawHandle for Child {
    fn as_raw_handle(&self) -> RawHandle {
        self.process.as_inner()
    }
}

impl IntoRawHandle for Child {
    fn into_raw_handle(self) -> RawHandle {
        self.process.as_inner()
    }
}



/// A handle to a child process's stdin.
pub struct ChildStdin(AnonWrite);

impl io::Write for ChildStdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl AsRawHandle for ChildStdin {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}

impl IntoRawHandle for ChildStdin {
    fn into_raw_handle(self) -> RawHandle {
        self.0.into_raw_handle()
    }
}



/// A handle to a child process's stdout.
pub struct ChildStdout(AnonRead);

impl io::Read for ChildStdout {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl AsRawHandle for ChildStdout {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}

impl IntoRawHandle for ChildStdout {
    fn into_raw_handle(self) -> RawHandle {
        self.0.into_raw_handle()
    }
}



/// A handle to a child process's stderr.
pub struct ChildStderr(AnonRead);

impl io::Read for ChildStderr {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl AsRawHandle for ChildStderr {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}

impl IntoRawHandle for ChildStderr {
    fn into_raw_handle(self) -> RawHandle {
        self.0.into_raw_handle()
    }
}



struct ProcThreadAtributeList {
    _buffer: Vec<u8>,
    attributes: w::LPPROC_THREAD_ATTRIBUTE_LIST
}

impl ProcThreadAtributeList {
    fn new(max_attributes: u32) -> io::Result<ProcThreadAtributeList> {
        let mut size = unsafe { mem::uninitialized() };
        if unsafe { k32::InitializeProcThreadAttributeList(ptr::null_mut(), max_attributes, 0, &mut size) } == w::FALSE {
            let last_error = io::Error::last_os_error();
            if last_error.raw_os_error().unwrap() as w::DWORD != w::ERROR_INSUFFICIENT_BUFFER {
                return Err(last_error);
            }
        }

        let mut buffer = Vec::with_capacity(size as usize);
        unsafe { buffer.set_len(size as usize); }
        let attributes = buffer.as_mut_ptr() as *mut _;

        if unsafe { k32::InitializeProcThreadAttributeList(attributes, max_attributes, 0, &mut size) } == w::FALSE {
            return Err(io::Error::last_os_error());
        }

        Ok(ProcThreadAtributeList {
            _buffer: buffer,
            attributes: attributes
        })
    }

    fn handles(&self, handles: &mut [RawHandle]) -> io::Result<()> {
        if unsafe { k32::UpdateProcThreadAttribute(self.attributes, 0,
                                                   0x00020002 /* w::PROC_THREAD_ATTRIBUTE_HANDLE_LIST */,
                                                   handles.as_mut_ptr() as *mut _, mem::size_of_val(&handles) as w::SIZE_T,
                                                   ptr::null_mut(), ptr::null_mut()) } == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    fn as_inner(&self) -> &mut w::PROC_THREAD_ATTRIBUTE_LIST { unsafe { &mut *self.attributes } }
}

impl Drop for ProcThreadAtributeList {
    fn drop(&mut self) {
        unsafe { k32::DeleteProcThreadAttributeList(self.attributes); }
    }
}


#[repr(C)]
#[allow(non_snake_case)]
struct STARTUPINFOEXW {
    StartupInfo: w::STARTUPINFOW,
    lpAttributeList: w::PPROC_THREAD_ATTRIBUTE_LIST
}

/// The `Command` type acts as a process builder, providing fine-grained control
/// over how a new process should be spawned.
///
/// A default configuration can be generated using `Command::new(program)`,
/// where `program` gives a path to the program to be executed. Additional
/// builder methods allow the configuration to be changed (for example,
/// by adding arguments) prior to spawning.
///
/// In addition to providing the fuctionality from `std::process::Command`,
/// this type allows injection of modules (DLLs) through the `inject()`
/// function. These modules are injected before the main thread of the
/// spawned process starts.
///
/// Another difference from `std::process::Command` is that this version
/// uses a lock-free method for spawning the child process.
pub struct Command {
    program: OsString,
    args: Vec<OsString>,
    env: Option<HashMap<OsString, OsString>>,
    cwd: Option<OsString>,
    modules: Vec<Module>,

    stdin: Option<StdioImp>,
    stdout: Option<StdioImp>,
    stderr: Option<StdioImp>
}

impl Command {
    /// Constructs a new `Command` for launching the program at
    /// path `program`, with the following default configuration:
    ///
    /// * No arguments to the program
    /// * Inherit the current process's environment
    /// * Inherit the current process's working directory
    /// * Inherit stdin/stdout/stderr for `spawn` or `status`, but create pipes for `output`
    ///
    /// Builder methods are provided to change these defaults and
    /// otherwise configure the process.
    pub fn new<S: AsRef<OsStr>>(program: S) -> Command {
        Command {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: None,
            cwd: None,
            modules: Vec::new(),
            stdin: None,
            stdout: None,
            stderr: None
        }
    }

    /// Add an argument to pass to the program.
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Command {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Add multiple arguments to pass to the program.
    pub fn args<S: AsRef<OsStr>>(&mut self, args: &[S]) -> &mut Command {
        self.args.extend(args.iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    fn init_env(&mut self){
        if self.env.is_none() {
            self.env = Some(env::vars_os().map(|(key, val)| {
                (make_key(&key), val)
            }).collect());
        }
    }

    /// Inserts or updates an environment variable mapping.
    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Command
    where K: AsRef<OsStr>, V: AsRef<OsStr> {
        self.init_env();
        self.env.as_mut().unwrap().insert(make_key(key.as_ref()), val.as_ref().to_owned());
        self
    }

    /// Removes an environment variable mapping.
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Command {
        self.init_env();
        self.env.as_mut().unwrap().remove(&make_key(key.as_ref()));
        self
    }

    /// Clears the entire environment map for the child process.
    pub fn env_clear(&mut self) -> &mut Command {
        self.env = Some(HashMap::new());
        self
    }

    /// Sets the working directory for the child process.
    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Command {
        self.cwd = Some(dir.as_ref().into());
        self
    }

    /// Injects a module (DLL) before the child process's main thread starts.
    pub fn inject<M: Into<Module>>(&mut self, module: M) -> &mut Command {
        self.modules.push(module.into());
        self
    }

    /// Configuration for the child process's stdin handle (file descriptor 0).
    pub fn stdin(&mut self, cfg: Stdio) -> &mut Command {
        self.stdin = Some(cfg.0);
        self
    }

    /// Configuration for the child process's stdout handle (file descriptor 1).
    pub fn stdout(&mut self, cfg: Stdio) -> &mut Command {
        self.stdout = Some(cfg.0);
        self
    }

    /// Configuration for the child process's stderr handle (file descriptor 2).
    pub fn stderr(&mut self, cfg: Stdio) -> &mut Command {
        self.stderr = Some(cfg.0);
        self
    }

    fn spawn_inner(&mut self, default_io: StdioImp) -> io::Result<Child> {
        // To have the spawning semantics of unix/windows stay the same, we need
        // to read the *child's* PATH if one is provided. See #15149 for more
        // details.
        let program = self.env.as_ref().and_then(|env| {
            for (key, v) in env {
                if OsStr::new("PATH") != &**key { continue }

                // Split the value and test each path to see if the
                // program exists.
                for path in env::split_paths(&v) {
                    let path = path.join(self.program.to_str().unwrap())
                                   .with_extension(env::consts::EXE_EXTENSION);
                    if fs::metadata(&path).is_ok() {
                        return Some(path.into_os_string())
                    }
                }
                break
            }
            None
        });

        let mut si = unsafe { mem::zeroed::<STARTUPINFOEXW>() };
        si.StartupInfo.cb = mem::size_of_val(&si) as w::DWORD;
        si.StartupInfo.dwFlags = w::STARTF_USESTDHANDLES;

        let in_handle = self.stdin.unwrap_or(default_io);
        let out_handle = self.stdout.unwrap_or(default_io);
        let err_handle = self.stderr.unwrap_or(default_io);

        let (stdin_pipe, stdin) = try!(in_handle.setup(w::STD_INPUT_HANDLE));
        let (stdout_pipe, stdout) = try!(out_handle.setup(w::STD_OUTPUT_HANDLE));
        let (stderr_pipe, stderr) = try!(err_handle.setup(w::STD_ERROR_HANDLE));

        si.StartupInfo.hStdInput = stdin.as_inner();
        si.StartupInfo.hStdOutput = stdout.as_inner();
        si.StartupInfo.hStdError = stderr.as_inner();

        let program = program.as_ref().unwrap_or(&self.program);
        let mut cmd_str = make_command_line(program, &self.args);

        let flags = w::CREATE_UNICODE_ENVIRONMENT | w::CREATE_SUSPENDED | w::EXTENDED_STARTUPINFO_PRESENT;

        let (envp, _data) = make_envp(self.env.as_ref());
        let (dirp, _data) = make_dirp(self.cwd.as_ref());

        let attributes = try!(ProcThreadAtributeList::new(1));
        try!(attributes.handles(&mut [stdin.as_inner(), stdout.as_inner(), stderr.as_inner()]));
        si.lpAttributeList = attributes.as_inner();

        let mut pi = unsafe { mem::uninitialized() };
        if unsafe { k32::CreateProcessW(ptr::null(), cmd_str.as_mut_ptr(), ptr::null_mut(),
                                        ptr::null_mut(), w::TRUE, flags, envp, dirp,
                                        &mut si.StartupInfo, &mut pi) } == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        mem::drop(attributes);

        let process = ProcessGuard::new(Handle::new(pi.hProcess));
        let thread = Handle::new(pi.hThread);

        if !self.modules.is_empty() {
            let injector = try!(Injector::new(&process));
            for module in &self.modules {
                try!(injector.inject(module));
            }
        }

        if unsafe { k32::ResumeThread(thread.as_inner()) } == -1i32 as w::DWORD {
            return Err(io::Error::last_os_error());
        }

        mem::drop(thread);

        Ok(Child {
            process: process.release(),
            id: pi.dwProcessId,
            status: None,
            stdin: stdin_pipe.map(|(_, write)| ChildStdin(write)),
            stdout: stdout_pipe.map(|(read, _)| ChildStdout(read)),
            stderr: stderr_pipe.map(|(read, _)| ChildStderr(read))
        })
    }

    /// Executes the command as a child process, returning a handle to it.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent.
    pub fn spawn(&mut self) -> io::Result<Child> {
        self.spawn_inner(StdioImp::Inherit)
    }

    /// Executes the command as a child process, waiting for it to finish and
    /// collecting all of its output.
    ///
    /// By default, stdin, stdout and stderr are captured (and used to
    /// provide the resulting output).
    pub fn output(&mut self) -> io::Result<Output> {
        self.spawn_inner(StdioImp::MakePipe).and_then(|p| p.wait_with_output())
    }

    /// Executes a command as a child process, waiting for it to finish and
    /// collecting its exit status.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn().and_then(|mut p| p.wait())
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        try!(write!(f, "{:?}", self.program));
        for arg in &self.args {
            try!(write!(f, " {:?}", arg));
        }
        Ok(())
    }
}


/// The output of a finished process.
#[derive(PartialEq, Eq, Clone)]
pub struct Output {
    /// The status (exit code) of the process.
    pub status: ExitStatus,
    /// The data that the process wrote to stdout.
    pub stdout: Vec<u8>,
    /// The data that the process wrote to stderr.
    pub stderr: Vec<u8>
}


/// Describes the result of a process after it has terminated.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitStatus(w::DWORD);

impl ExitStatus {
    /// Was termination successful? Success is defined as a zero exit status.
    pub fn success(&self) -> bool {
        self.0 == 0
    }

    /// Returns the exit code of the process as an `Option`.
    ///
    /// This will always return the code in the form of `Some(code)`.
    pub fn code(&self) -> Option<i32> {
        Some(self.code_direct())
    }

    /// Returns the exit code of the process.
    pub fn code_direct(&self) -> i32 {
        self.0 as i32
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "exit code: {}", self.0)
    }
}



/// Describes what to do with a standard I/O stream for a child process.
pub struct Stdio(StdioImp);

impl Stdio {
    /// A new pipe should be arranged to connect the parent and child processes.
    pub fn piped() -> Stdio {
        Stdio(StdioImp::MakePipe)
    }

    /// The child inherits from the corresponding parent descriptor.
    pub fn inherit() -> Stdio {
        Stdio(StdioImp::Inherit)
    }

    /// This stream will be ignored. This is the equivalent of attaching the
    /// stream to `NUL`.
    pub fn null() -> Stdio {
        Stdio(StdioImp::None)
    }
}

impl FromRawHandle for Stdio {
    unsafe fn from_raw_handle(handle: RawHandle) -> Stdio {
        Stdio(StdioImp::Raw(handle))
    }
}



#[derive(Clone, Copy)]
enum StdioImp {
    Raw(RawHandle),
    MakePipe,
    Inherit,
    None
}

impl StdioImp {
    fn setup(&self, stdio_id: w::DWORD) -> io::Result<(Option<(AnonRead, AnonWrite)>, Handle)> {
        match *self {
            StdioImp::Raw(handle) => {
                Ok((None, try!(Handle::duplicate_from(handle, true))))
            }
            StdioImp::MakePipe => {
                let (read, write): (AnonRead, AnonWrite) = try!(pipe::anonymous(0));
                let handle = try!(if stdio_id == w::STD_INPUT_HANDLE {
                    Handle::duplicate_from(read.as_raw_handle(), true)
                } else {
                    Handle::duplicate_from(write.as_raw_handle(), true)
                });

                Ok((Some((read, write)), handle))
            }
            StdioImp::Inherit => {
                let handle = unsafe { k32::GetStdHandle(stdio_id) };
                if handle == w::INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                } else if handle.is_null() {
                    return Err(io::Error::new(io::ErrorKind::Other,
                                              "no stdio handle available for this process"));
                }
                let handle = try!(Handle::duplicate_from(handle, true));

                Ok((None, handle))
            }
            StdioImp::None => {
                let name = OsStr::new("NUL").encode_wide().chain(Some(0)).collect::<Vec<_>>();
                let access = if stdio_id == w::STD_INPUT_HANDLE {
                    w::FILE_GENERIC_READ
                } else {
                    w::FILE_GENERIC_WRITE
                };
                let mut security = w::SECURITY_ATTRIBUTES {
                    nLength: mem::size_of::<w::SECURITY_ATTRIBUTES>() as w::DWORD,
                    lpSecurityDescriptor: ptr::null_mut(),
                    bInheritHandle: 1,
                };

                let handle = unsafe {
                    k32::CreateFileW(name.as_ptr(), access,
                                     w::FILE_SHARE_READ | w::FILE_SHARE_WRITE,
                                     &mut security, w::OPEN_EXISTING,
                                     w::FILE_ATTRIBUTE_NORMAL, ptr::null_mut())
                };
                if handle == w::INVALID_HANDLE_VALUE {
                     return Err(io::Error::last_os_error());
                }

                Ok((None, Handle::new(handle)))
            }
        }
    }
}



fn make_key(s: &OsStr) -> OsString {
    // Yuck
    let upper = s.to_string_lossy().to_ascii_uppercase();
    <String as AsRef<OsStr>>::as_ref(&upper).to_owned()
}

fn make_command_line(prog: &OsStr, args: &[OsString]) -> Vec<u16> {
    fn append_arg(cmd: &mut Vec<u16>, arg: &OsStr) {
        cmd.push('"' as u16);

        let mut backslashes = 0usize;
        for x in arg.encode_wide() {
            if x == '\\' as u16 {
                backslashes += 1;
            } else {
                if x == '"' as u16 {
                    for _ in 0..(backslashes + 1) {
                        cmd.push('\\' as u16);
                    }
                }
                backslashes = 0;
            }
            cmd.push(x);
        }

        for _ in 0..backslashes {
            cmd.push('\\' as u16);
        }
        cmd.push('"' as u16);
    }

    let mut cmd: Vec<u16> = Vec::new();
    append_arg(&mut cmd, prog);
    for arg in args {
        cmd.push(' ' as u16);
        append_arg(&mut cmd, arg);
    }
    cmd.push(0);
    cmd
}

fn make_envp(env: Option<&HashMap<OsString, OsString>>) -> (*mut c_void, Vec<u16>) {
    match env {
        Some(env) => {
            let mut blk = Vec::new();

            for pair in env {
                blk.extend(pair.0.encode_wide());
                blk.push('=' as u16);
                blk.extend(pair.1.encode_wide());
                blk.push(0);
            }
            blk.push(0);
            (blk.as_mut_ptr() as *mut _, blk)
        }
        None => (ptr::null_mut(), Vec::new())
    }
}

fn make_dirp(d: Option<&OsString>) -> (*const u16, Vec<u16>) {
    match d {
        Some(dir) => {
            let mut dir_str = dir.encode_wide().collect::<Vec<_>>();
            dir_str.push(0);
            (dir_str.as_ptr(), dir_str)
        },
        None => (ptr::null(), Vec::new())
    }
}
