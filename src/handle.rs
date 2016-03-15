use std::{io, mem};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::os::windows::io::{RawHandle, FromRawHandle, AsRawHandle, IntoRawHandle};

use {w, k32};
use serde::{Deserialize, Deserializer};
use serde::de::{Visitor, Error};

#[derive(Debug)]
pub struct Handle(RawHandle);

impl Handle {
    pub fn new(handle: RawHandle) -> Handle {
        Handle(handle)
    }

    pub fn duplicate_from(handle: RawHandle, inherit: bool) -> io::Result<Handle> {
        let mut ret = unsafe { mem::uninitialized() };
        let process = unsafe { k32::GetCurrentProcess() };
        if unsafe { k32::DuplicateHandle(process, handle, process, &mut ret,
                                        0, inherit as w::BOOL, w::DUPLICATE_SAME_ACCESS) } == w::FALSE
        {
            return Err(io::Error::last_os_error());
        }

        Ok(Handle::new(ret))
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

impl IntoRawHandle for Handle {
    fn into_raw_handle(self) -> RawHandle {
        self.as_inner()
    }
}

impl FromRawHandle for Handle {
    unsafe fn from_raw_handle(handle: RawHandle) -> Handle {
        Handle::new(handle)
    }
}



unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}


/// A helper type for deserializing objects that can be constructed from a raw handle.
///
/// It can be used to receive and deserialize any type `T` that implements `FromRawHandle` in
/// an initializer function.
pub struct Shared<T>(T);

impl<T> Shared<T> {
    /// Unwraps this `Shared` and returns the underlying object.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: FromRawHandle> Deserialize for Shared<T> {
    fn deserialize<D>(deserializer: &mut D) -> Result<Shared<T>, D::Error>
    where D: Deserializer {
        deserializer.deserialize_usize(HandleVisitor(PhantomData))
    }
}

struct HandleVisitor<T>(PhantomData<Shared<T>>);

impl<T: FromRawHandle> Visitor for HandleVisitor<T> {
    type Value = Shared<T>;

    fn visit_usize<E>(&mut self, value: usize) -> Result<Shared<T>, E>
    where E: Error {
        Ok(Shared(unsafe { T::from_raw_handle(value as RawHandle) }))
    }
}