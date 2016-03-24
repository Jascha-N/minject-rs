use std::{ptr, mem, error};
use std::fmt::{self, Display, Formatter};
use std::io::{self, ErrorKind};
use std::sync::{Once, ONCE_INIT};
use std::path::Path;
use std::os::windows::prelude::*;
use std::io::prelude::*;

use {w, k32};
use bincode::SizeLimit;
use bincode::serde::{self, DeserializeError, SerializeResult};
use byteorder::{WriteBytesExt, NativeEndian};
use serde::Serialize;

use handle::Handle;
use init::InitError;

struct RemoteMemory<'a> {
    process: &'a Handle,
    memory: *mut u8,
    offset: usize
}

impl<'a> RemoteMemory<'a> {
    fn new(process: &Handle, size: usize, executable: bool) -> io::Result<RemoteMemory> {
        if size == 0 {
            return Err(io::Error::new(ErrorKind::InvalidInput, "size is zero"));
        }

        let protect = if executable {
            w::PAGE_EXECUTE_READWRITE
        } else {
            w::PAGE_READWRITE
        };
        let memory = unsafe {
            k32::VirtualAllocEx(process.as_inner(), ptr::null_mut(), size as w::SIZE_T,
                                w::MEM_COMMIT | w::MEM_RESERVE, protect)
        };
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }

        Ok(RemoteMemory {
            process: process,
            memory: memory as *mut _,
            offset: 0
        })
    }

    unsafe fn from_raw(process: &Handle, memory: *mut u8) -> RemoteMemory {
        RemoteMemory {
            process: process,
            memory: memory,
            offset: 0
        }
    }

    unsafe fn write_inner<T>(&mut self, value: *const T, size: usize, align: usize) -> io::Result<*mut T> {
        let offset = self.offset + (align - (self.offset % align)) % align;
        let remote_ptr = self.memory.offset(offset as isize) as *mut T;

        if size == 0 {
            return Ok(remote_ptr)
        }

        if k32::WriteProcessMemory(self.process.as_inner(),
                                   remote_ptr as w::LPVOID,
                                   value as w::LPCVOID,
                                   size as w::SIZE_T,
                                   ptr::null_mut()) == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        self.offset = offset + size;

        Ok(remote_ptr)
    }

    fn write<T: Copy>(&mut self, value: &T) -> io::Result<*mut T> {
        unsafe { self.write_inner(value, mem::size_of::<T>(), mem::align_of::<T>()) }
    }

    fn write_slice<T: Copy>(&mut self, value: &[T]) -> io::Result<*mut T> {
        unsafe { self.write_inner(value.as_ptr(), mem::size_of_val(value), mem::align_of_val(value)) }
    }

    unsafe fn read<T: Copy>(&self, remote_ptr: *const T) -> io::Result<T> {
        let mut value = mem::uninitialized::<T>();

        if k32::ReadProcessMemory(self.process.as_inner(),
                                  remote_ptr as w::LPVOID,
                                  &mut value as *mut T as w::LPVOID,
                                  mem::size_of::<T>() as w::SIZE_T,
                                  ptr::null_mut()) == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        Ok(value)
    }

    unsafe fn read_vec<T: Copy>(&self, remote_ptr: *const T, length: usize) -> io::Result<Vec<T>> {
        if length == 0 {
            return Ok(Vec::new());
        }

        let mut buffer = Vec::with_capacity(length);
        buffer.set_len(length);

        if k32::ReadProcessMemory(self.process.as_inner(),
                                  remote_ptr as w::LPVOID,
                                  buffer.as_mut_ptr() as w::LPVOID,
                                  mem::size_of_val(&buffer[..]) as w::SIZE_T,
                                  ptr::null_mut()) == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        Ok(buffer)
    }
}

impl<'a> Drop for RemoteMemory<'a> {
    fn drop(&mut self) {
        unsafe { k32::VirtualFreeEx(self.process.as_inner(), self.memory as w::LPVOID, 0, w::MEM_RELEASE); }
    }
}


/// A module builder for a module without an initialization function.
pub struct ModuleBuilder {
    path: Vec<u16>
}

/// A module builder for a module with an initialization function.
pub struct ModuleBuilderWithInit {
    path: Vec<u16>,
    init: Vec<u8>,
    args: Vec<InitArg>
}

enum InitArg {
    Serialized(Vec<u8>),
    Handle(Handle)
}

impl ModuleBuilder {
    fn new<P: AsRef<Path>>(path: P) -> ModuleBuilder {
        let path = path.as_ref().as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();

        ModuleBuilder {
            path: path
        }
    }

    /// Call the given initializer function after loading the module.
    ///
    /// Arguments can be added by calling `arg()` on the result. An initializer function
    /// can be created in the module using the `initializer!` macro.
    pub fn init<N: Into<Vec<u8>>>(self, name: N) -> ModuleBuilderWithInit {
        let mut init = name.into();
        init.push(0);

        ModuleBuilderWithInit {
            path: self.path,
            init: init,
            args: Vec::new()
        }
    }

    /// Constructs a module and consumes this builder.
    pub fn unwrap(self) -> Module {
        Module {
            path: self.path,
            init: None
        }
    }
}

impl ModuleBuilderWithInit {
    /// Adds an argument to the initializer invocation.
    ///
    /// The argument needs to be serializable with `serde`.
    pub fn arg<T: ?Sized + Serialize>(mut self, arg: &T) -> SerializeResult<ModuleBuilderWithInit> {
        let data = try!(serde::serialize(&arg, SizeLimit::Infinite));

        let mut args = mem::replace(&mut self.args, Vec::new());
        args.push(InitArg::Serialized(data));

        Ok(ModuleBuilderWithInit {
            args: args,
            ..self
        })
    }

    /// Adds a handle argument to the initializer invocation.
    ///
    /// Handles get duplicated into the target process before they are serialized. They can be
    /// accessed in the initializer function with a parameter of type `Shared`.
    pub fn handle<H: ?Sized + AsRawHandle>(mut self, handle: &H) -> io::Result<ModuleBuilderWithInit> {
        let mut args = mem::replace(&mut self.args, Vec::new());
        args.push(InitArg::Handle(try!(Handle::duplicate_from(handle.as_raw_handle(), false))));

        Ok(ModuleBuilderWithInit {
            args: args,
            ..self
        })
    }

    /// Constructs a module and consumes this builder.
    pub fn unwrap(self) -> Module {
        Module {
            path: self.path,
            init: Some((self.init, self.args))
        }
    }
}

/// A description of a module (DLL) to be injected into a process.
///
/// It contains a path to a module, the name of an optional initializer
/// function and optional arguments for said function.
pub struct Module {
    path: Vec<u16>,
    init: Option<(Vec<u8>, Vec<InitArg>)>
}

#[cfg_attr(feature = "clippy", allow(new_ret_no_self))]
impl Module {
    /// Creates a new module definition builder given the path to a module.
    pub fn new<P: AsRef<Path>>(path: P) -> ModuleBuilder {
        ModuleBuilder::new(path.as_ref())
    }

    fn copy_to_process<'a>(&self, process: &'a Handle) -> io::Result<(RemoteMemory<'a>, *mut ThreadParam)> {
        let init = match self.init {
            None => None,
            Some((ref init, ref args)) => {
                let mut serialized_args = Vec::new();
                for arg in args {
                    match *arg {
                        InitArg::Serialized(ref data) => serialized_args.extend_from_slice(data),
                        InitArg::Handle(ref handle) => {
                            let mut copied_handle = unsafe { mem::uninitialized() };
                            let current_process = unsafe { k32::GetCurrentProcess() };
                            if unsafe { k32::DuplicateHandle(current_process, handle.as_raw_handle(), process.as_raw_handle(), &mut copied_handle,
                                                             0, w::FALSE, w::DUPLICATE_SAME_ACCESS) } == w::FALSE
                            {
                                return Err(io::Error::last_os_error());
                            }

                            serde::serialize_into(&mut serialized_args, &(copied_handle as usize), SizeLimit::Infinite).expect("internal error");
                        }
                    }
                }
                Some((init, serialized_args))
            }
        };

        let mut size = mem::size_of_val(&self.path[..]) +
                       mem::size_of::<ThreadParam>();

        if let Some((init, ref args)) = init {
            size += mem::size_of_val(&init[..]) +
                    mem::size_of_val(&args[..])
        }

        let mut remote = try!(RemoteMemory::new(process, size, false));
        let module_path = try!(remote.write_slice(&self.path[..]));

        let (init_name, user_data, user_len) = if let Some((init, ref args)) = init {
            let init_name = try!(remote.write_slice(&init[..]));
            let user_data = try!(remote.write_slice(&args[..]));

            (init_name, user_data, args.len())
        } else {
            (ptr::null_mut(), ptr::null_mut(), 0)
        };

        let param = ThreadParam {
            module_path: module_path,
            init_name: init_name as *const _,
            user_data: user_data,
            user_len: user_len,
            last_error: 0
        };

        let param = try!(remote.write(&param));

        Ok((remote, param))
    }
}

impl From<ModuleBuilder> for Module {
    fn from(builder: ModuleBuilder) -> Module {
        builder.unwrap()
    }
}

impl From<ModuleBuilderWithInit> for Module {
    fn from(builder: ModuleBuilderWithInit) -> Module {
        builder.unwrap()
    }
}



#[repr(C)]
#[derive(Copy, Clone)]
struct ThreadParam {
    module_path: w::LPCWSTR,
    init_name: w::LPCSTR,
    user_data: *const u8,
    user_len: usize,
    last_error: w::DWORD
}

const SUCCESS: w::DWORD = 0;
const ERROR_LOAD_FAILED: w::DWORD = 1;
const ERROR_INIT_NOT_FOUND: w::DWORD = 2;
const ERROR_INIT_FAILED: w::DWORD = 3;

/// An error that can occur during the injection process.
#[derive(Debug)]
pub enum Error {
    /// The target process's bitness (32-bit vs 64-bit) does not match the current process's bitness.
    Bitness,
    /// The module could not be loaded into the target process.
    LoadFailed(io::Error),
    /// The module's initializer function was not found.
    InitNotFound(io::Error),
    /// An error occurred when the initializer function was called.
    InitError(Option<InitError>),
    /// An error occurred while deserializing the error message.
    Deserialize(DeserializeError),
    /// The remote injection thread returned an unexpected exit code and probably crashed.
    UnexpectedExitCode(u32),
    /// An I/O error occurred.
    Io(io::Error)
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        match *self {
            Error::Bitness => write!(formatter, "Target process's bitness does not match"),
            Error::LoadFailed(ref error) => write!(formatter, "Failed to load module: {}", error),
            Error::InitNotFound(ref error) => write!(formatter, "Failed to find initializer function: {}", error),
            Error::InitError(None) => write!(formatter, "Unspecified error during initialization"),
            Error::InitError(Some(ref error)) => write!(formatter, "Error during initialization: {}", error),
            Error::Deserialize(ref error) => write!(formatter, "Error deserializing initialization error: {}", error),
            Error::UnexpectedExitCode(code) => write!(formatter, "Remote thread returned unexpected exit code: {}", code),
            Error::Io(ref error) => write!(formatter, "An I/O error occurred: {}", error)
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Bitness => "mismatched bitness",
            Error::LoadFailed(_) => "failed to load module",
            Error::InitNotFound(_) => "initializer function not found",
            Error::InitError(_) => "initializer error",
            Error::Deserialize(_) => "deserialization error",
            Error::UnexpectedExitCode(_) => "unexpected error code",
            Error::Io(_) => "I/O error"
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::LoadFailed(ref error) | Error::InitNotFound(ref error) | Error::Io(ref error) => Some(error),
            Error::InitError(Some(ref error)) => Some(error),
            _ => None
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        Error::Io(error)
    }
}

impl From<DeserializeError> for Error {
    fn from(error: DeserializeError) -> Error {
        Error::Deserialize(error)
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> io::Error {
        match error {
            Error::Io(error) => error,
            error => io::Error::new(ErrorKind::Other, error)
        }
    }
}

pub type Result<T> = ::std::result::Result<T, Error>;

pub struct Injector<'a> {
    process: &'a Handle,
    _code: RemoteMemory<'a>,
    thread_proc: *const u8
}

impl<'a> Injector<'a> {
    pub fn new(process: &Handle) -> io::Result<Injector> {
        try!(check_same_bitness(process));

        let thunk = get_thunk();
        let mut code = try!(RemoteMemory::new(process, mem::size_of_val(thunk), true));
        let thread_proc = try!(code.write_slice(thunk));

        Ok(Injector {
             process: process,
             _code: code,
             thread_proc: thread_proc
        })
    }

    pub fn inject(&self, module: &Module) -> Result<()> {
        let (remote_data, param) = try!(module.copy_to_process(self.process));

        let thread = unsafe {
            k32::CreateRemoteThread(self.process.as_inner(), ptr::null_mut(), 0,
                                    mem::transmute(self.thread_proc), // Yikes!
                                    param as w::LPVOID, 0, ptr::null_mut())
        };
        if thread.is_null() {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        let thread = Handle::new(thread);
        try!(thread.wait());

        let mut exit_code = unsafe { mem::uninitialized() };
        if unsafe { k32::GetExitCodeThread(thread.as_inner(), &mut exit_code) } == w::FALSE {
            return Err(Error::Io(io::Error::last_os_error()));
        }

        let param = try!(unsafe { remote_data.read(param) });

        match exit_code {
            SUCCESS => Ok(()),
            ERROR_LOAD_FAILED => Err(Error::LoadFailed(io::Error::from_raw_os_error(param.last_error as i32))),
            ERROR_INIT_NOT_FOUND => Err(Error::InitNotFound(io::Error::from_raw_os_error(param.last_error as i32))),
            ERROR_INIT_FAILED => {
                let error = param.user_data;
                let error_length = param.user_len;
                if error.is_null() || error_length == 0 {
                    Err(Error::InitError(None))
                } else {
                    let remote_message = unsafe { RemoteMemory::from_raw(self.process, error as *mut _) };
                    let serialized_error = try!(unsafe { remote_message.read_vec(error, error_length) });
                    let deserialized_error = try!(serde::deserialize(&serialized_error[..]));

                    Err(Error::InitError(Some(deserialized_error)))
                }
            },
            code => Err(Error::UnexpectedExitCode(code))
        }
    }
}

#[cfg(target_arch = "x86")]
fn check_same_bitness(process: &Handle) -> Result<()> {
    let mut si = unsafe { mem::uninitialized() };
    unsafe { k32::GetNativeSystemInfo(&mut si); }
    if si.wProcessorArchitecture == 0 /* w::PROCESSOR_ARCHITECTURE_INTEL */ {
        return Ok(());
    }

    let mut wow64 = unsafe { mem::uninitialized() };
    if unsafe { k32::IsWow64Process(process.as_inner(), &mut wow64) } == w::FALSE {
        return Err(Error::Io(io::Error::last_os_error()));
    }

    if wow64 == w::TRUE {
        Ok(())
    } else {
        Err(Error::Bitness)
    }
}

#[cfg(target_arch = "x86_64")]
fn check_same_bitness(process: &Handle) -> Result<()> {
    let mut wow64 = unsafe { mem::uninitialized() };
    if unsafe { k32::IsWow64Process(process.as_inner(), &mut wow64) } == w::FALSE {
        return Err(Error::Io(io::Error::last_os_error()));
    }

    if wow64 == w::FALSE {
        Ok(())
    } else {
        Err(Error::Bitness)
    }
}

fn get_thunk() -> &'static [u8] {
    static INIT: Once = ONCE_INIT;
    static mut THUNK: *const [u8] = &[];

    INIT.call_once(|| {
        const KERNEL32_NAME: &'static [w::WCHAR] = &[0x6B, 0x65, 0x72, 0x6E, 0x65, 0x6C, 0x33, 0x32, 0x2E, 0x64, 0x6C, 0x6C, 0x0];

        static THUNK_CODE: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thunk.bin"));

        fn write_function(vec: &mut Vec<u8>, module: w::HMODULE, name: &[u8]) {
            #[cfg(target_arch = "x86")]
            fn write(vec: &mut Vec<u8>, function: w::FARPROC) {
                vec.write_u32::<NativeEndian>(function as u32).unwrap();
            }

            #[cfg(target_arch = "x86_64")]
            fn write(vec: &mut Vec<u8>, function: w::FARPROC) {
                vec.write_u64::<NativeEndian>(function as u64).unwrap();
            }

            let function = unsafe { k32::GetProcAddress(module, name.as_ptr() as *const _) };
            if function.is_null() {
                panic!("{}", io::Error::last_os_error());
            }
            write(vec, function);
        }

        let kernel32 = unsafe { k32::GetModuleHandleW(KERNEL32_NAME.as_ptr()) };
        if kernel32.is_null() {
            panic!("{}", io::Error::last_os_error());
        }

        let mut vec = Vec::with_capacity(THUNK_CODE.len() * 2);
        vec.write_all(THUNK_CODE).unwrap();
        while vec.len() % mem::size_of::<usize>() > 0 {
            vec.push(0)
        }

        write_function(&mut vec, kernel32, b"LoadLibraryW\0");
        write_function(&mut vec, kernel32, b"FreeLibrary\0");
        write_function(&mut vec, kernel32, b"GetProcAddress\0");
        write_function(&mut vec, kernel32, b"GetLastError\0");

        unsafe { THUNK = Box::into_raw(vec.into_boxed_slice()); }
    });

    unsafe { &*THUNK }
}