#![doc(hidden)]

use std::{mem, ptr};
use std::fmt::{self, Display, Formatter};
use std::error::Error;
use std::io::Read;

use {k32, w};
use bincode::{self, SizeLimit};
use bincode::serde::DeserializeResult;
use serde::{Serializer, Deserialize, Deserializer};

/// An error that can occur in a call to an initializer function.
#[derive(Debug, Serialize, Deserialize)]
pub enum InitError {
    /// A panic occurred.
    Panic(String),
    /// An argument could not be deserialized.
    Argument(String, String),
    /// Too many arguments were supplied.
    TooManyArguments
}

impl Display for InitError {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        match *self {
            InitError::Panic(ref message) => write!(formatter, "A panic occcured during initialization: {}", message),
            InitError::Argument(ref name, ref error) => write!(formatter, "Failed to deserialize argument '{}': {}", name, error),
            InitError::TooManyArguments => write!(formatter, "Too many arguments supplied to initializer function")
        }
    }
}

impl Error for InitError {
    fn description(&self) -> &str {
        "error during initializer"
    }
}

#[doc(hidden)]
pub fn __set_result(result: Result<(), InitError>, out_data: &mut *const u8, out_size: &mut usize) -> usize {
    *out_data = ptr::null();
    *out_size = 0;

    match result {
        Ok(()) => 1,
        Err(error) => {
            if let Ok(buffer) = bincode::serde::serialize(&error, SizeLimit::Infinite) {
                let size = mem::size_of_val(&buffer[..]);
                let data = unsafe {
                    k32::VirtualAlloc(ptr::null_mut(),
                                      size as w::SIZE_T,
                                      w::MEM_COMMIT | w::MEM_RESERVE,
                                      w::PAGE_READWRITE)
                } as *mut u8;
                if !data.is_null() {
                    unsafe { ptr::copy_nonoverlapping(buffer.as_ptr(), data, buffer.len()); }
                    *out_data = data;
                    *out_size = size;
                }
            }
            0
        }
    }

}

#[doc(hidden)]
pub fn __deserialize<R: Read, T: Deserialize>(reader: &mut R) -> DeserializeResult<T> {
    bincode::serde::deserialize_from(reader, SizeLimit::Infinite)
}

/// Creates a suitable initialization wrapper function around the given function.
///
/// The resulting public function is usable as an initialization function to
/// be called during code injection. Its signature is unspecified and is
/// subject to change.
///
/// Arguments of the wrapped function must implement `serde::Deserialize`.
/// The function can not return a value.
#[macro_export]
macro_rules! initializer {
    (parse: $(#[$fn_attr:meta])* fn $fn_name:ident ($($arg_name:ident : $arg_type:ty),*) { $($body:tt)* }) => {
        initializer!(make: ($($fn_attr)*) ($fn_name) ($($arg_name)*) ($($arg_type)*) ({ $($body)* }));
    };

    (make: ($($fn_attr:meta)*) ($fn_name:ident) ($($arg_name:ident)*) ($($arg_type:ty)*) ($body:block)) => {
        initializer!(gen_arg_names: (make_init)
                                    (($($fn_attr)*) ($fn_name) ($($arg_name)*) ($($arg_type)*) ($body))
                                    ($($arg_name)*));
    };

    (make_init: ($($temp_name:ident)*) ($($fn_attr:meta)*) ($fn_name:ident) ($($arg_name:ident)*) ($($arg_type:ty)*) ($body:block)) => {
        $(#[$fn_attr])*
        #[no_mangle]
        pub unsafe extern fn $fn_name(__data: *mut *const u8, __size: *mut usize) -> usize {
            fn __inner($($arg_name : $arg_type),*) $body

            unsafe fn __deserialize_and_invoke(data: *const u8, size: usize) -> Result<(), $crate::InitError> {
                assert!(!data.is_null());

                let slice = ::std::slice::from_raw_parts(data, size);
                let mut reader = ::std::io::Cursor::new(slice);
                $(
                    let $temp_name = try!($crate::init::__deserialize(&mut reader).map_err(|e| $crate::InitError::Argument(stringify!($arg_name).to_owned(), format!("{}", e))));
                )*

                match ::std::io::Read::read(&mut reader, &mut [0u8]) {
                    Ok(0) => { __inner($($temp_name),*); Ok(()) }
                    Ok(_) => Err($crate::InitError::TooManyArguments),
                    _ => unreachable!()
                }
            }

            if __data.is_null() || __size.is_null() {
                return 0;
            }

            let result = ::std::panic::recover(|| {
                __deserialize_and_invoke(*__data, *__size)
            }).unwrap_or_else(|payload| {
                let message = match payload.downcast::<&'static str>() {
                    Ok(s) => (*s).to_owned(),
                    Err(payload) => match payload.downcast::<String>() {
                        Ok(s) => *s,
                        Err(_) => "Box<Any>".to_owned()
                    }
                };

                Err($crate::InitError::Panic(message))
            });

            $crate::init::__set_result(result, &mut *__data, &mut *__size)
        }
    };

    (gen_arg_names: ($label:ident) ($($args:tt)*) ($($token:tt)*)) => {
        initializer!(gen_arg_names: ($label)
                                    ($($args)*)
                                    (
                                        __arg_0  __arg_1  __arg_2  __arg_3  __arg_4  __arg_5  __arg_6  __arg_7
                                        __arg_8  __arg_9  __arg_10 __arg_11 __arg_12 __arg_13 __arg_14 __arg_15
                                        __arg_16 __arg_17 __arg_18 __arg_19 __arg_20 __arg_21 __arg_22 __arg_23
                                        __arg_24 __arg_25
                                    )
                                    ($($token)*)
                                    ());
    };
    (gen_arg_names: ($label:ident) ($($args:tt)*) ($hd_name:tt $($tl_name:tt)*) ($hd:tt $($tl:tt)*) ($($acc:tt)*) ) => {
        initializer!(gen_arg_names: ($label) ($($args)*) ($($tl_name)*) ($($tl)*) ($($acc)* $hd_name));
    };
    (gen_arg_names: ($label:ident) ($($args:tt)*) ($($name:tt)*) () ($($acc:tt)*)) => {
        initializer!($label: ($($acc)*) $($args)*);
    };

    ($($t:tt)+) => {
        initializer!(parse: $($t)+);
    };
}