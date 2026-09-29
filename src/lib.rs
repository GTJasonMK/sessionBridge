pub mod bridge;
pub mod error;
pub mod git;
pub mod private_files;

#[cfg(not(target_os = "linux"))]
compile_error!("SessionBridge currently requires Linux");
