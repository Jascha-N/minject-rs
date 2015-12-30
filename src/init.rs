#![doc(hidden)]

use std::{mem, ptr};
use std::any::Any;

use {k32, w, serde_json};
use serde::Deserialize;

#[doc(hidden)]
pub fn __handle_init_panic(payload: Box<Any + Send>) -> (*const u8, usize) {
    let message = match payload.downcast_ref::<&'static str>() {
        Some(s) => *s,
        None => match payload.downcast_ref::<String>() {
            Some(s) => &s[..],
            None => "Box<Any>",
        }
    };

    if message.len() == 0 {
        return (ptr::null(), 0)
    }

    let result = unsafe {
        k32::VirtualAlloc(ptr::null_mut(),
                          mem::size_of_val(&message[..]) as w::SIZE_T,
                          w::MEM_COMMIT | w::MEM_RESERVE,
                          w::PAGE_READWRITE)
    } as *mut u8;
    if result.is_null() {
        return (ptr::null(), 0)
    }
    unsafe { ptr::copy_nonoverlapping(message.as_ptr(), result, message.len()); }

    (result as *const _, message.len())
}

#[doc(hidden)]
pub fn __deserialize<T: Deserialize>(bytes: &[u8]) -> serde_json::Result<T> {
    serde_json::from_slice(bytes)
}


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
        pub unsafe extern fn $fn_name(__user_data: *mut *const u8, __user_len: *mut usize) -> bool {
            fn __inner($($arg_name : $arg_type),*) $body

            ::std::panic::recover(|| {
                assert!(!__user_data.is_null() && !__user_len.is_null() && !(*__user_data).is_null());

                let slice = ::std::slice::from_raw_parts(*__user_data, *__user_len);
                let ($($temp_name,)*) = $crate::init::__deserialize(slice).expect("error deserializing arguments");
                __inner($($temp_name),*);

                true
            }).unwrap_or_else(|payload| {
                let (message, length) = $crate::init::__handle_init_panic(payload);

                *__user_data = message;
                *__user_len = length;

                false
            })
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
