//! Host virtual memory: a 4 GiB reservation with parts committed on demand.
//!
//! With the whole guest address space at one host base, the JIT turns a guest
//! access into `base + address`.
use std::ptr::NonNull;

/// Size of the guest's address space.
pub const ADDRESS_SPACE: usize = 1 << 32;
const PAGE: usize = 0x1000;

#[cfg(windows)]
mod os {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc,
        VirtualFree,
    };

    pub fn reserve(size: usize) -> *mut u8 {
        // SAFETY: asks the OS for a fresh range; touches no existing memory.
        unsafe { VirtualAlloc(std::ptr::null(), size, MEM_RESERVE, PAGE_NOACCESS).cast() }
    }

    pub fn commit(address: *mut u8, size: usize) -> bool {
        // SAFETY: `address..address + size` lies inside our reservation.
        !unsafe { VirtualAlloc(address.cast(), size, MEM_COMMIT, PAGE_READWRITE) }.is_null()
    }

    pub fn release(address: *mut u8, _size: usize) {
        // SAFETY: `address` is the base of a reservation owned by the caller.
        unsafe { VirtualFree(address.cast(), 0, MEM_RELEASE) };
    }
}

#[cfg(unix)]
mod os {
    pub fn reserve(size: usize) -> *mut u8 {
        // SAFETY: anonymous mapping at an address of the kernel's choice.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED { std::ptr::null_mut() } else { address.cast() }
    }

    pub fn commit(address: *mut u8, size: usize) -> bool {
        // SAFETY: `address..address + size` lies inside our reservation.
        unsafe { libc::mprotect(address.cast(), size, libc::PROT_READ | libc::PROT_WRITE) == 0 }
    }

    pub fn release(address: *mut u8, size: usize) {
        // SAFETY: the range is a mapping owned by the caller.
        unsafe { libc::munmap(address.cast(), size) };
    }
}

/// The reservation backing one guest address space.
#[derive(Debug)]
pub struct AddressSpace {
    base: NonNull<u8>,
}

// SAFETY: the reservation is plain memory owned exclusively by this value.
unsafe impl Send for AddressSpace {}
// SAFETY: shared access only reads through `base`; writes need `&mut`.
unsafe impl Sync for AddressSpace {}

impl AddressSpace {
    /// Reserves the address space. Nothing is accessible until [`Self::commit`].
    ///
    /// # Panics
    /// If the host cannot reserve 4 GiB of address space (64-bit hosts always can).
    #[allow(clippy::expect_used, reason = "only fails on hosts flora cannot run on")]
    pub fn new() -> Self {
        let base = NonNull::new(os::reserve(ADDRESS_SPACE))
            .expect("cannot reserve the guest address space");
        Self { base }
    }

    pub const fn base(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    /// Makes `[start, start + size)` readable and writable (zeroed on first commit).
    ///
    /// # Panics
    /// If the host is out of commit charge.
    #[allow(clippy::expect_used, reason = "running out of host memory is not recoverable")]
    pub fn commit(&self, start: u32, size: u32) {
        let first = start as usize & !(PAGE - 1);
        let last = (start as usize + size as usize + PAGE - 1) & !(PAGE - 1);
        // SAFETY: stays inside the 4 GiB reservation.
        let address = unsafe { self.base.as_ptr().add(first) };
        assert!(os::commit(address, last - first), "cannot commit {size:#x} bytes of guest memory");
    }
}

impl Default for AddressSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        os::release(self.base.as_ptr(), ADDRESS_SPACE);
    }
}
