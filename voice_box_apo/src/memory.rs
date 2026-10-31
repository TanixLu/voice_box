//! Non-RT allocation capture and reference-counted page residency.
//!
//! Capturing construction through the DLL's allocator also covers private
//! allocations inside Denoiser and Rubato, without relying on their layouts.
//! Captured objects must keep their allocations fixed until they are dropped.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::BTreeMap,
    ffi::c_void,
    ops::{Deref, DerefMut},
    ptr,
    sync::{LazyLock, Mutex},
};

use windows::Win32::System::{
    Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
        PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
        VirtualLock, VirtualQuery, VirtualUnlock,
    },
    ProcessStatus::{PSAPI_WORKING_SET_EX_INFORMATION, QueryWorkingSetEx},
    SystemInformation::{GetSystemInfo, SYSTEM_INFO},
    Threading::{GetCurrentProcess, GetProcessWorkingSetSize, SetProcessWorkingSetSize},
};

type Capture = BTreeMap<usize, usize>;

thread_local! {
    // No TLS allocation or destructor is needed by the allocator itself.
    static CAPTURE: Cell<*mut Capture> = const { Cell::new(ptr::null_mut()) };
}

struct ApoAllocator;
#[global_allocator]
static ALLOCATOR: ApoAllocator = ApoAllocator;

fn record(action: impl FnOnce(&mut Capture)) {
    let _ = CAPTURE.try_with(|slot| {
        let capture = slot.replace(ptr::null_mut());
        if !capture.is_null() {
            // Disable recording while bookkeeping allocates/frees its own nodes.
            // The scope is thread-local and cannot overlap another capture.
            let capture_ref = unsafe { &mut *capture };
            action(capture_ref);
            slot.set(capture);
        }
    });
}

unsafe impl GlobalAlloc for ApoAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let allocation = unsafe { System.alloc(layout) };
        if !allocation.is_null() {
            record(|capture| {
                capture.insert(allocation as usize, layout.size());
            });
        }
        allocation
    }

    unsafe fn dealloc(&self, allocation: *mut u8, layout: Layout) {
        record(|capture| {
            capture.remove(&(allocation as usize));
        });
        unsafe { System.dealloc(allocation, layout) };
    }
}

struct CaptureScope;
impl Drop for CaptureScope {
    fn drop(&mut self) {
        CAPTURE.with(|slot| slot.set(ptr::null_mut()));
    }
}

fn capture<T>(create: impl FnOnce() -> T) -> (T, Capture) {
    let mut allocations = Capture::default();
    CAPTURE.with(|slot| {
        assert!(slot.get().is_null(), "nested resident construction");
        slot.set(&mut allocations);
    });
    let scope = CaptureScope;
    let value = create();
    drop(scope);
    (value, allocations)
}

/// Any page leases drop before the value, while all its allocations still exist.
/// Only constructed and destroyed on non-real-time control threads.
pub struct Resident<T> {
    _pages: Option<LockedPages>,
    value: Box<T>,
}

impl<T> Resident<T> {
    pub fn new(create: impl FnOnce() -> T, lock_pages: bool) -> windows::core::Result<Self> {
        let (value, pages) = if lock_pages {
            let (value, allocations) = capture(|| Box::new(create()));
            (value, Some(LockedPages::new(allocations)?))
        } else {
            (Box::new(create()), None)
        };
        Ok(Self {
            _pages: pages,
            value,
        })
    }
}

impl<T> Deref for Resident<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> DerefMut for Resident<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

static PAGE_SIZE: LazyLock<usize> = LazyLock::new(|| {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&mut info) };
    info.dwPageSize as usize
});

#[derive(Default)]
struct PageRegistry {
    pages: BTreeMap<usize, PageLock>,
    reserved_bytes: usize,
    last_minimum: usize,
    last_maximum: usize,
    host_maximum: usize,
}
struct PageLock {
    references: usize,
    owned: bool,
}
static PAGES: LazyLock<Mutex<PageRegistry>> = LazyLock::new(Mutex::default);

impl PageRegistry {
    fn adjust_working_set(&mut self, added: usize, released: usize) -> windows::core::Result<()> {
        // shortcut: host quota writes are detected between calls; coordinate writers if they can race.
        let process = unsafe { GetCurrentProcess() };
        let (mut minimum, mut maximum) = (0, 0);
        unsafe { GetProcessWorkingSetSize(process, &mut minimum, &mut maximum)? };
        // A host-written minimum owns the earlier allowance; only reclaim
        // increases made since that change. Track maximum changes separately.
        if minimum != self.last_minimum {
            self.reserved_bytes = 0;
        }
        if maximum != self.last_maximum {
            self.host_maximum = maximum;
        }
        let reclaimed = released.min(self.reserved_bytes);
        let required = minimum.checked_add(added).ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY)
        })? - reclaimed;
        let required_maximum = self.host_maximum.max(required);
        if required != minimum || required_maximum != maximum {
            unsafe { SetProcessWorkingSetSize(process, required, required_maximum)? };
        }
        self.last_minimum = required;
        self.last_maximum = required_maximum;
        self.reserved_bytes = self.reserved_bytes + added - reclaimed;
        Ok(())
    }
}

/// Heap allocations can share pages across APO instances. VirtualLock itself
/// does not count references; only the final lease may unlock a page we locked.
pub struct LockedPages {
    pages: Vec<usize>,
}

impl LockedPages {
    pub fn new(ranges: impl IntoIterator<Item = (usize, usize)>) -> windows::core::Result<Self> {
        let page_size = *PAGE_SIZE;
        let mut requested = BTreeMap::new();
        for (start, bytes) in ranges {
            if bytes != 0 {
                let last = start.checked_add(bytes - 1).ok_or_else(|| {
                    windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
                })?;
                requested
                    .extend((start / page_size..=last / page_size).map(|p| (p * page_size, false)));
            }
        }
        let mut lease = Self {
            pages: Vec::with_capacity(requested.len()),
        };
        let mut registry = PAGES.lock().unwrap_or_else(|e| e.into_inner());
        // shortcut: host locks on shared pages must stay stable during leases; coordinate if they can race.
        for (page, owned) in &mut requested {
            if !registry.pages.contains_key(page) {
                *owned = !page_is_locked(*page)?;
            }
        }
        // VirtualLock's quota depends on the process minimum working set.
        // Add headroom to the current limit, including later host reservations.
        let new_pages = requested.values().filter(|&&owned| owned).count();
        let mut unused_quota = new_pages * page_size;
        registry.adjust_working_set(unused_quota, 0)?;
        for (page, owned) in requested {
            if owned {
                if let Err(error) = unsafe { VirtualLock(page as *const c_void, page_size) } {
                    let _ = registry.adjust_working_set(0, unused_quota);
                    // Release the mutex before Drop rolls back acquired pages.
                    drop(registry);
                    return Err(error);
                }
                unused_quota -= page_size;
            }
            registry
                .pages
                .entry(page)
                .or_insert(PageLock {
                    references: 0,
                    owned,
                })
                .references += 1;
            lease.pages.push(page);
        }
        Ok(lease)
    }
}

fn page_is_locked(page: usize) -> windows::core::Result<bool> {
    let mut info = PSAPI_WORKING_SET_EX_INFORMATION {
        VirtualAddress: page as *mut c_void,
        ..Default::default()
    };
    unsafe {
        QueryWorkingSetEx(
            GetCurrentProcess(),
            (&mut info as *mut PSAPI_WORKING_SET_EX_INFORMATION).cast(),
            size_of_val(&info) as u32,
        )?;
    }
    // PSAPI_WORKING_SET_EX_BLOCK exposes Valid at bit 0 and Locked at bit 22.
    const VALID_AND_LOCKED: usize = 1 | (1 << 22);
    Ok(unsafe { info.VirtualAttributes.Flags } & VALID_AND_LOCKED == VALID_AND_LOCKED)
}

impl Drop for LockedPages {
    fn drop(&mut self) {
        if self.pages.is_empty() {
            return;
        }
        let mut registry = PAGES.lock().unwrap_or_else(|e| e.into_inner());
        let mut released = 0;
        for page in &self.pages {
            let lock = registry
                .pages
                .get_mut(page)
                .expect("UNREACHABLE: leased page should exist in the registry");
            lock.references -= 1;
            if lock.references == 0 {
                if lock.owned {
                    unsafe {
                        let _ = VirtualUnlock(*page as *const c_void, *PAGE_SIZE);
                    }
                    released += *PAGE_SIZE;
                }
                registry.pages.remove(page);
            }
        }
        let _ = registry.adjust_working_set(0, released);
    }
}

/// Include the loaded image's readable code/data, notably the denoiser's static
/// weights and Rubato's vtables. These aren't visible to allocation capture.
pub fn image_ranges() -> Vec<(usize, usize)> {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    unsafe {
        VirtualQuery(
            Some(image_ranges as *const () as *const c_void),
            &mut info,
            size_of_val(&info),
        );
    }
    let base = info.AllocationBase;
    let mut address = base as usize;
    let mut ranges = Vec::new();
    let readable = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    loop {
        let queried = unsafe {
            VirtualQuery(
                Some(address as *const c_void),
                &mut info,
                size_of_val(&info),
            )
        };
        if queried == 0 || info.AllocationBase != base {
            break;
        }
        if info.State == MEM_COMMIT
            && (info.Protect & readable).0 != 0
            && (info.Protect & PAGE_GUARD).0 == 0
        {
            ranges.push((address, info.RegionSize));
        }
        address += info.RegionSize;
    }
    ranges
}
