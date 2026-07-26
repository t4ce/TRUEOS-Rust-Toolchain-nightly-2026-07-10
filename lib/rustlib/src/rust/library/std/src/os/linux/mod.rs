//! Linux-specific definitions.

#![stable(feature = "raw_ext", since = "1.1.0")]
#![doc(cfg(target_os = "linux"))]

pub mod fs;
#[cfg(not(target_os = "trueos"))]
pub mod net;
#[cfg(not(target_os = "trueos"))]
pub mod process;
pub mod raw;
