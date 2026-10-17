//! Pagefile-backed shared memory sections between the app and the helper.
//!
//! The app creates every section and owns it: document sources (shared with the helper read-only) and
//! render targets (shared writable, then read back by the app). Section memory is charged to the system
//! commit when the app creates it, not to the helper's job, so the helper's memory cap bounds only
//! PDFium's own allocations (measured in step 0). The app sizes every section; the helper never chooses a
//! buffer size.
//!
//! Read-only sharing means the handle the helper receives grants `FILE_MAP_READ` only, so a mapping for
//! write through it fails. It is a guard against mistakes, not a privilege boundary: the helper runs as
//! the same user and could open a writable handle another way (see the module documentation of
//! `pdf_helper`).

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, OwnedHandle};

use windows::Win32::Foundation::{CloseHandle, HANDLE};

use crate::child_job::owned_handle;

pub(crate) const FILE_MAP_WRITE: u32 = 0x0002;
pub(crate) const FILE_MAP_READ: u32 = 0x0004;
const PAGE_READWRITE: u32 = 0x04;
const INVALID_HANDLE_VALUE: isize = -1;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(file: isize, attributes: *const c_void, protect: u32, size_high: u32, size_low: u32, name: *const u16) -> isize;
    fn MapViewOfFile(mapping: isize, access: u32, offset_high: u32, offset_low: u32, bytes: usize) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
}

/// A section the app created, with its full-access handle.
pub(crate) struct Section {
    handle: OwnedHandle,
    len: usize,
}

impl Section {
    /// A zero-filled section of exactly `len` bytes (at least one).
    pub(crate) fn create(len: usize) -> Result<Self, String> {
        if len == 0 {
            return Err("A shared section cannot be empty.".into());
        }
        let size = len as u64;
        let raw = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, (size >> 32) as u32, size as u32, std::ptr::null()) };
        if raw == 0 {
            return Err(format!("Could not create a {len}-byte shared section: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { handle: unsafe { owned_handle(HANDLE(raw as *mut c_void)) }, len })
    }

    /// A section holding a copy of `bytes`.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let section = Self::create(bytes.len())?;
        section.map(true)?.as_mut_slice().copy_from_slice(bytes);
        Ok(section)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn handle(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }

    /// Maps the whole section into this process.
    pub(crate) fn map(&self, writable: bool) -> Result<View, String> {
        unsafe { View::map(self.handle(), self.len, writable) }
    }
}

/// A mapped view of (the start of) a section. Unmapped on drop.
pub(crate) struct View {
    base: *mut u8,
    len: usize,
}

impl View {
    /// Maps `len` bytes of the section behind `handle`; fails if the handle's access or the section's size
    /// does not allow it.
    ///
    /// # Safety
    /// `handle` must be a section handle valid in this process.
    pub(crate) unsafe fn map(handle: HANDLE, len: usize, writable: bool) -> Result<Self, String> {
        if len == 0 {
            return Err("A shared section view cannot be empty.".into());
        }
        let access = if writable { FILE_MAP_WRITE | FILE_MAP_READ } else { FILE_MAP_READ };
        let base = unsafe { MapViewOfFile(handle.0 as isize, access, 0, 0, len) };
        if base.is_null() {
            return Err(format!("Could not map a {len}-byte shared section: {}", std::io::Error::last_os_error()));
        }
        Ok(Self { base: base.cast(), len })
    }

    /// Maps a section handle the app passed to the helper, then closes the handle (the view keeps the
    /// section alive). The handle is closed whether or not the mapping succeeds.
    ///
    /// # Safety
    /// `raw` must be a handle the app duplicated into this process for this request and that nothing
    /// else uses.
    pub(crate) unsafe fn map_passed(raw: u64, len: usize, writable: bool) -> Result<Self, String> {
        let handle = HANDLE(raw as usize as *mut c_void);
        let view = unsafe { Self::map(handle, len, writable) };
        let _ = unsafe { CloseHandle(handle) };
        view
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.base, self.len) }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.base, self.len) }
    }

    /// The view's bytes with an unbounded lifetime, for a PDFium document that borrows its source.
    ///
    /// # Safety
    /// The caller must drop every borrower before this view.
    pub(crate) unsafe fn detached_slice<'a>(&self) -> &'a [u8] {
        unsafe { std::slice::from_raw_parts(self.base, self.len) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe { UnmapViewOfFile(self.base as *const c_void) };
    }
}
