use std::{io, ptr, mem};
use std::sync::{Once, ONCE_INIT};
use std::path::Path;
use std::os::windows::prelude::*;
use std::io::prelude::*;

use {w, k32};
use bincode::SizeLimit;
use bincode::serde::{self, SerializeResult};
use byteorder::{WriteBytesExt, NativeEndian};
use serde::Serialize;

use handle::Handle;

struct RemoteMemory<'a> {
    process: &'a Handle,
    memory: *mut u8,
    offset: usize
}

impl<'a> RemoteMemory<'a> {
    fn new(process: &Handle, size: usize, executable: bool) -> io::Result<RemoteMemory> {
        if size == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "size is zero"));
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

    unsafe fn read_string(&self, remote_ptr: *const u8, length: usize) -> io::Result<String> {
        if length == 0 {
            return Ok(String::new())
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

        Ok(String::from_utf8_unchecked(buffer))
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
    args: Vec<u8>
}

impl ModuleBuilder {
    fn new(path: &Path) -> ModuleBuilder {
        let path = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();

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
        let mut args = mem::replace(&mut self.args, Vec::new());
        try!(serde::serialize_into(&mut args, &arg, SizeLimit::Infinite));
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
    init: Option<(Vec<u8>, Vec<u8>)>
}

impl Module {
    /// Creates a new module definition builder given the path to a module.
    pub fn new<P: AsRef<Path>>(path: P) -> ModuleBuilder {
        ModuleBuilder::new(path.as_ref())
    }

    fn copy_to_process<'a>(&self, process: &'a Handle) -> io::Result<(RemoteMemory<'a>, *mut ThreadParam)> {
        let mut size = mem::size_of_val(&self.path[..]) +
                       mem::size_of::<ThreadParam>();

        if let &Some((ref init, ref args)) = &self.init {
            size += mem::size_of_val(&init[..]) +
                    mem::size_of_val(&args[..])
        }

        let mut remote = try!(RemoteMemory::new(process, size, false));
        let module_path = try!(remote.write_slice(&self.path[..]));

        let (init_name, user_data, user_len) = if let &Some((ref init, ref args)) = &self.init {
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

pub struct Injector<'a> {
    process: &'a Handle,
    _code: RemoteMemory<'a>,
    thread_proc: *const u8
}

impl<'a> Injector<'a> {
    pub fn new(process: &Handle) -> io::Result<Injector> {
        try!(check_same_architecture(process));

        let thunk = get_thunk();
        let mut code = try!(RemoteMemory::new(process, mem::size_of_val(thunk), true));
        let thread_proc = try!(code.write_slice(thunk));

        Ok(Injector {
             process: process,
             _code: code,
             thread_proc: thread_proc
        })
    }

    pub fn inject(&self, module: &Module) -> io::Result<()> {
        let (remote_data, param) = try!(module.copy_to_process(self.process));

        let thread = unsafe {
            k32::CreateRemoteThread(self.process.as_inner(), ptr::null_mut(), 0,
                                    mem::transmute(self.thread_proc), // Yikes!
                                    param as w::LPVOID, 0, ptr::null_mut())
        };
        if thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        let thread = Handle::new(thread);
        try!(thread.wait());

        let mut exit_code = unsafe { mem::uninitialized() };
        if unsafe { k32::GetExitCodeThread(thread.as_inner(), &mut exit_code) } == w::FALSE {
            return Err(io::Error::last_os_error());
        }

        let param = try!(unsafe { remote_data.read(param) });

        match exit_code {
            SUCCESS => Ok(()),
            ERROR_LOAD_FAILED => Err(io::Error::from_raw_os_error(param.last_error as i32)),
            ERROR_INIT_NOT_FOUND => Err(io::Error::from_raw_os_error(param.last_error as i32)),
            ERROR_INIT_FAILED => {
                let error_message = param.user_data;
                let error_length = param.user_len;
                if error_message.is_null() || error_length == 0 {
                    Err(io::Error::new(io::ErrorKind::Other, "initialization routine panicked"))
                } else {
                    let remote_message = unsafe { RemoteMemory::from_raw(self.process, error_message as *mut _) };
                    let error_string = try!(unsafe { remote_message.read_string(error_message, error_length) });

                    Err(io::Error::new(io::ErrorKind::Other, format!("initialization routine panicked: {}", error_string)))
                }
            },
            code => panic!("an unexpected exit code was returned: {}", code),
        }
    }
}

#[cfg(target_arch = "x86")]
fn check_same_architecture(process: &Handle) -> io::Result<()> {
    let mut si = unsafe { mem::uninitialized() };
    unsafe { k32::GetNativeSystemInfo(&mut si); }
    if si.wProcessorArchitecture == 0 /* w::PROCESSOR_ARCHITECTURE_INTEL */ {
        return Ok(());
    }

    let mut wow64 = unsafe { mem::uninitialized() };
    if unsafe { k32::IsWow64Process(process.as_inner(), &mut wow64) } == w::FALSE {
        return Err(io::Error::last_os_error());
    }

    if wow64 == w::TRUE {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other,
                           "target process is 64-bit while current process is 32-bit"))
    }
}

#[cfg(target_arch = "x86_64")]
fn check_same_architecture(process: &Handle) -> io::Result<()> {
    let mut wow64 = unsafe { mem::uninitialized() };
    if unsafe { k32::IsWow64Process(process.as_inner(), &mut wow64) } == w::FALSE {
        return Err(io::Error::last_os_error());
    }

    if wow64 == w::FALSE {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other,
                           "target process is 32-bit while current process is 64-bit"))
    }
}

fn get_thunk() -> &'static [u8] {
    static INIT: Once = ONCE_INIT;
    static mut THUNK: *const [u8] = &[];

    INIT.call_once(|| {
        const KERNEL32_NAME: &'static [w::WCHAR] = &[0x6B, 0x65, 0x72, 0x6E, 0x65, 0x6C, 0x33, 0x32, 0x2E, 0x64, 0x6C, 0x6C, 0x0];

        #[cfg(target_arch = "x86")]
        static THUNK_CODE: &'static [u8] = include_bytes!("thunk32.bin");

        #[cfg(target_arch = "x86_64")]
        static THUNK_CODE: &'static [u8] = include_bytes!("thunk64.bin");

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