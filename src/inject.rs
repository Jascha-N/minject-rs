use std::{io, ptr, mem, slice};
use std::sync::{Once, ONCE_INIT};
use std::path::Path;
use std::ffi::OsString;
use std::marker::PhantomData;
use std::os::windows::prelude::*;
use std::io::prelude::*;
use std::os::raw::c_void;

use {w, k32};
use byteorder::{WriteBytesExt, NativeEndian};

use handle::Handle;

struct RemoteMemory<'a, T: ?Sized> {
    process: &'a Handle,
    memory: w::LPVOID,
    data: PhantomData<T>
}

impl<'a, T: ?Sized> RemoteMemory<'a, T> {
    fn new(process: &Handle, size: usize, executable: bool) -> io::Result<RemoteMemory<T>> {
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
            memory: memory,
            data: PhantomData
        })
    }

    fn write(&self, data: &T) -> io::Result<()> {
        if unsafe { k32::WriteProcessMemory(self.process.as_inner(), self.memory,
                                            data as *const T as *const _,
                                            mem::size_of_val(data) as w::SIZE_T,
                                            ptr::null_mut()) } == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    fn read(&self, data: &mut T) -> io::Result<()> {
        if unsafe { k32::ReadProcessMemory(self.process.as_inner(), self.memory,
                                            data as *mut T as *mut _,
                                            mem::size_of_val(data) as w::SIZE_T,
                                            ptr::null_mut()) } == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    fn as_raw_ptr(&self) -> *mut c_void { self.memory }
}

impl<'a, T> RemoteMemory<'a, T> {
    fn new_sized(process: &Handle, executable: bool) -> io::Result<RemoteMemory<T>> {
        RemoteMemory::new(process, mem::size_of::<T>(), executable)
    }

    fn as_ptr(&self) -> *mut T { self.memory as *mut _ }
}

impl<'a, T> RemoteMemory<'a, [T]> {
    fn as_ptr(&self) -> *mut T { self.memory as *mut _ }
}

impl<'a, T: ?Sized> Drop for RemoteMemory<'a, T> {
    fn drop(&mut self) {
        unsafe { k32::VirtualFreeEx(self.process.as_inner(), self.memory, 0, w::MEM_RELEASE); }
    }
}



pub struct Module<'a> {
    path: OsString,
    initializer: Option<(&'a [u8], Option<(&'a mut [u8], usize)>)>
}

impl<'a> Module<'a> {
    pub fn new<P: AsRef<Path>>(path: P) -> Module<'a> {
        Module {
            path: path.as_ref().into(),
            initializer: None,
        }
    }

    pub unsafe fn with_init<P: AsRef<Path>>(path: P, name: &'a [u8]) -> Module<'a> {
        Module {
            path: path.as_ref().into(),
            initializer: Some((name, None))
        }
    }

    pub unsafe fn with_init_param<P: AsRef<Path>, T: Copy + 'static>(path: P, name: &'a [u8], param: &'a mut T) -> Module<'a> {
        Module::with_init_param_slice(path.as_ref(), name, slice::from_raw_parts_mut(param, 1))
    }

    pub unsafe fn with_init_param_slice<P: AsRef<Path>, T: Copy + 'static>(path: P, name: &'a [u8], param: &'a mut [T]) -> Module<'a> {
        Module::with_init_param_raw(path.as_ref(),
                                    name,
                                    slice::from_raw_parts_mut(param.as_mut_ptr() as *mut _,
                                                              mem::size_of_val(param)),
                                    param.len())
    }

    pub unsafe fn with_init_param_raw(path: &Path, name: &'a [u8], param: &'a mut [u8], length: usize) -> Module<'a> {
        Module {
            path: path.into(),
            initializer: Some((name, Some((param, length))))
        }
    }
}



#[repr(C)]
struct ThreadParam {
    module_path: w::LPCWSTR,
    init_name: w::LPCSTR,
    user_data: *mut c_void,
    user_size: usize,
    last_error: w::DWORD
}

const SUCCESS: w::DWORD = 0;
const ERROR_LOAD_FAILED: w::DWORD = 1;
const ERROR_INIT_NOT_FOUND: w::DWORD = 2;
const ERROR_INIT_FAILED: w::DWORD = 3;

pub struct Injector<'a> {
    process: &'a Handle,
    code: RemoteMemory<'a, [u8]>,
    data: RemoteMemory<'a, ThreadParam>
}

impl<'a> Injector<'a> {
    pub fn new(process: &Handle) -> io::Result<Injector> {
        try!(check_same_architecture(process));

        let stub = get_stub();
        let code = try!(RemoteMemory::new(process, stub.len(), true));
        try!(code.write(stub));

        let data = try!(RemoteMemory::new_sized(process, false));

        Ok(Injector {
             process: process,
             code: code,
             data: data
        })
    }

    pub fn inject(&self, module: &mut Module) -> io::Result<()> {
        let local_path = module.path.encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let remote_path = try!(RemoteMemory::new(self.process, local_path.len(), false));
        try!(remote_path.write(&local_path[..]));

        let initializer = match module.initializer {
            Some((ref local_init, ref mut local_ud)) => {
                let mut local_init = local_init.to_vec();
                local_init.push(0);
                let remote_init = try!(RemoteMemory::new(self.process, local_init.len(), false));
                try!(remote_init.write(&local_init[..]));

                let user_data = match *local_ud {
                    Some((ref mut local_ud, ud_len)) => {
                        let remote_ud = try!(RemoteMemory::new(self.process, mem::size_of_val(*local_ud), false));
                        try!(remote_ud.write(*local_ud));
                        Some((remote_ud, local_ud, ud_len))
                    },
                    None => None
                };
                Some((remote_init, user_data))
            },
            None => None
        };

        let mut param = match initializer {
            Some((ref remote_init, ref user_data)) => ThreadParam {
                module_path: remote_path.as_ptr(),
                init_name: remote_init.as_ptr() as *const _,
                user_data: user_data.as_ref().map(|&(ref remote_ud, _, _)| remote_ud.as_raw_ptr())
                                             .unwrap_or(ptr::null_mut()),
                user_size: user_data.as_ref().map(|&(_, _, ud_len)| ud_len)
                                             .unwrap_or(0),
                last_error: 0
            },
            None => ThreadParam {
                module_path: remote_path.as_ptr(),
                init_name: ptr::null(),
                user_data: ptr::null_mut(),
                user_size: 0,
                last_error: 0
            }
        };

        try!(self.data.write(&param));

        let thread = unsafe {
            k32::CreateRemoteThread(self.process.as_inner(), ptr::null_mut(), 0,
                                    mem::transmute(self.code.as_ptr()), // Yikes!
                                    self.data.as_ptr() as *mut _, 0, ptr::null_mut())
        };
        if thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        let thread = Handle::new(thread);
        try!(thread.wait());

        // Make sure the remote memory has not been freed before this point.
        mem::drop(remote_path);

        let mut exit_code = unsafe { mem::uninitialized() };
        if unsafe { k32::GetExitCodeThread(thread.as_inner(), &mut exit_code) } == w::FALSE {
            return Err(io::Error::last_os_error());
        }

        try!(self.data.read(&mut param));

        match exit_code {
            SUCCESS => initializer.and_then(|(_, user_data)| user_data)
                                  .map(|(remote_ud, local_ud, _)| remote_ud.read(*local_ud))
                                  .unwrap_or(Ok(())),
            ERROR_LOAD_FAILED => Err(io::Error::from_raw_os_error(param.last_error as i32)),
            ERROR_INIT_NOT_FOUND => Err(io::Error::from_raw_os_error(param.last_error as i32)),
            ERROR_INIT_FAILED => Err(io::Error::new(io::ErrorKind::Other, "initialization routine failed")),
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

fn get_stub() -> &'static [u8] {
    static INIT: Once = ONCE_INIT;
    static mut STUB: *const [u8] = &[];

    INIT.call_once(|| {
        const KERNEL32_NAME: &'static [w::WCHAR] = &[0x6B, 0x65, 0x72, 0x6E, 0x65, 0x6C, 0x33, 0x32, 0x2E, 0x64, 0x6C, 0x6C, 0x0];

        #[cfg(target_arch = "x86")]
        static STUB_CODE: &'static [u8] = include_bytes!("stub32.bin");

        #[cfg(target_arch = "x86_64")]
        static STUB_CODE: &'static [u8] = include_bytes!("stub64.bin");

        #[cfg(target_arch = "x86")]
        fn write_function(vec: &mut Vec<u8>, module: w::HMODULE, name: &[u8]) {
            let function = unsafe { k32::GetProcAddress(module, name.as_ptr() as *const _) };
            if function.is_null() {
                panic!("{}", io::Error::last_os_error());
            }
            vec.write_u32::<NativeEndian>(function as u32).unwrap();
        }

        #[cfg(target_arch = "x86_64")]
        fn write_function(vec: &mut Vec<u8>, module: w::HMODULE, name: &[u8]) {
            let function = unsafe { k32::GetProcAddress(module, name.as_ptr() as *const _) };
            if function.is_null() {
                panic!("{}", io::Error::last_os_error());
            }
            vec.write_u64::<NativeEndian>(function as u64).unwrap();
        }

        let kernel32 = unsafe { k32::GetModuleHandleW(KERNEL32_NAME.as_ptr()) };
        if kernel32.is_null() {
            panic!("{}", io::Error::last_os_error());
        }

        let mut vec = Vec::with_capacity(STUB_CODE.len() * 2);
        vec.write_all(STUB_CODE).unwrap();
        while vec.len() % mem::size_of::<usize>() > 0 {
            vec.push(0)
        }

        write_function(&mut vec, kernel32, b"LoadLibraryW\0");
        write_function(&mut vec, kernel32, b"FreeLibrary\0");
        write_function(&mut vec, kernel32, b"GetProcAddress\0");
        write_function(&mut vec, kernel32, b"GetLastError\0");

        unsafe { STUB = Box::into_raw(vec.into_boxed_slice()); }
    });

    unsafe { &*STUB }
}