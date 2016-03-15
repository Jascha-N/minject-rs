use std::{io, mem};
use std::os::windows::io::{RawHandle, FromRawHandle, AsRawHandle};

use {w, k32};

#[derive(Debug)]
pub struct Handle(RawHandle);

impl Handle {
    pub fn new(handle: RawHandle) -> Handle {
        Handle(handle)
    }

    pub fn as_inner(&self) -> RawHandle {
        self.0
    }

    pub fn wait(&self) -> io::Result<()> {
        if unsafe { k32::WaitForSingleObject(self.0, w::INFINITE) } == w::WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            k32::CloseHandle(self.0);
        }
    }
}

impl AsRawHandle for Handle {
    fn as_raw_handle(&self) -> RawHandle {
        self.as_inner()
    }
}

impl FromRawHandle for Handle {
    unsafe fn from_raw_handle(handle: RawHandle) -> Handle {
        Handle::new(handle)
    }
}

pub fn duplicate(handle: RawHandle, inherit: bool) -> io::Result<Handle> {
    let mut ret = unsafe { mem::uninitialized() };
    let process = unsafe { k32::GetCurrentProcess() };
    if unsafe { k32::DuplicateHandle(process, handle, process, &mut ret,
                                     0, inherit as w::BOOL, w::DUPLICATE_SAME_ACCESS) } == w::FALSE
    {
        return Err(io::Error::last_os_error());
    }

    Ok(Handle::new(ret))
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}