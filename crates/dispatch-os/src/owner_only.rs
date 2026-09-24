//! A security descriptor admitting one user, and reading one back.
//!
//! Shared by everything Dispatch makes on Windows that must be this user's
//! alone: the daemon's pipe, and a delegated task's file and its directory.

use windows_sys::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// A security descriptor admitting the user this process runs as, and
/// nobody else.
///
/// SDDL `O:<sid>D:P(A;;GA;;;<sid>)`: a protected DACL, so nothing is
/// inherited into it, whose one entry grants everything to this user.
/// The null descriptor it replaced on the daemon's pipe took the default
/// DACL, which also let Everyone and anonymous logons open the pipe for
/// reading.
///
/// The owner is named too, because clients refuse a pipe this user does
/// not own (see `connect` in `ipc`), and left to the default an elevated
/// daemon's pipe would be owned by the Administrators group instead.
pub(crate) struct OwnerOnly(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is never changed after it is built, and is freed
// exactly once, on drop.
unsafe impl Send for OwnerOnly {}
// SAFETY: as above -- shared use only ever reads it.
unsafe impl Sync for OwnerOnly {}

impl OwnerOnly {
    pub(crate) fn new() -> std::io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and outlives the call; on
        // success `descriptor` is a LocalAlloc'd descriptor that `Drop`
        // frees.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self(descriptor))
    }

    /// What `CreateNamedPipeW` takes: it points into `self`, so it is
    /// good only while this descriptor lives.
    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                .expect("a small struct"),
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from LocalAlloc via the conversion
        // above and is freed exactly once, here.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// The SID of the user this process runs as, as a string (`S-1-5-21-…`).
pub(crate) fn current_user_sid() -> std::io::Result<String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
    // closing; `raw` receives a token handle owned below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a handle this call just opened, owned from here on.
    let token = unsafe { OwnedHandle::from_raw_handle(raw as _) };

    let mut needed = 0u32;
    // SAFETY: a null buffer of length zero only asks how much is needed.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };

    // u64s, not bytes: TOKEN_USER holds a pointer and must be aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `needed` bytes.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: on success the buffer begins with a TOKEN_USER whose SID
    // points into the same buffer, which is alive until this returns.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        sid_string(user.User.Sid)
    }
}

/// `sid` as a string (`S-1-5-21-…`).
///
/// # Safety
///
/// `sid` must point at a valid SID that stays alive for the call.
pub(crate) unsafe fn sid_string(sid: PSID) -> std::io::Result<String> {
    let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: the caller vouches for `sid`; `text` receives a
    // LocalAlloc'd string freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: `text` is the NUL-terminated string the call returned.
    let string = unsafe {
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    // SAFETY: allocated by ConvertSidToStringSidW, freed exactly once.
    unsafe { LocalFree(text as HLOCAL) };

    Ok(string)
}

/// `ACCESS_ALLOWED_ACE_TYPE`, as the `u8` an ACE header carries.
#[cfg(test)]
pub(crate) const ACCESS_ALLOWED: u8 =
    windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE as u8;

/// One entry of a DACL, as a test compares it.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ace {
    /// The ACE type its header carries.
    pub(crate) kind: u8,
    /// The access it grants, denies, audits or labels, as stored.
    pub(crate) mask: u32,
    /// Whose entry it is, as a string.
    ///
    /// Empty, and `mask` zero, for a type not laid out as
    /// ACCESS_ALLOWED_ACE is: its SID is elsewhere, and reading it where
    /// that layout keeps one would read something else.
    pub(crate) sid: String,
}

/// The ACE types laid out as ACCESS_ALLOWED_ACE is: a header, a mask,
/// and the SID straight after.
#[cfg(test)]
const LAID_OUT_AS_ALLOWED: [u32; 4] = {
    use windows_sys::Win32::System::SystemServices::{
        ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, SYSTEM_AUDIT_ACE_TYPE,
        SYSTEM_MANDATORY_LABEL_ACE_TYPE,
    };
    [
        ACCESS_ALLOWED_ACE_TYPE,
        ACCESS_DENIED_ACE_TYPE,
        SYSTEM_AUDIT_ACE_TYPE,
        SYSTEM_MANDATORY_LABEL_ACE_TYPE,
    ]
};

/// A DACL as a test compares it.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Dacl {
    /// Whether it is protected: nothing a parent holds is inherited into it.
    pub(crate) protected: bool,
    /// Its entries, in order.
    pub(crate) entries: Vec<Ace>,
}

/// Each entry of the DACL on `handle`, a kernel object.
///
/// A NULL DACL -- no list at all, which admits everyone -- is an error,
/// so a test expecting entries fails on it rather than reading one.
#[cfg(test)]
pub(crate) fn dacl_of(handle: isize) -> std::io::Result<Vec<Ace>> {
    use windows_sys::Win32::Security::ACL;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: every out-pointer is valid; on success `descriptor` is
    // LocalAlloc'd and `dacl`, unless null, points into it.
    let status = unsafe {
        GetSecurityInfo(
            handle as HANDLE,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }

    // SAFETY: both came from the successful call above.
    unsafe { read_dacl(descriptor, dacl) }.map(|dacl| dacl.entries)
}

/// The DACL on the file or directory at `path`, and whether it is
/// protected.
#[cfg(test)]
pub(crate) fn dacl_of_path(path: &std::path::Path) -> std::io::Result<Dacl> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Security::ACL;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and outlives the call; every
    // out-pointer is valid; on success `descriptor` is LocalAlloc'd and
    // `dacl`, unless null, points into it.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }

    // SAFETY: both came from the successful call above.
    unsafe { read_dacl(descriptor, dacl) }
}

/// Reads `dacl` out of `descriptor`, then frees `descriptor`.
///
/// A NULL DACL -- no list at all, which admits everyone -- is an error, so
/// a test expecting entries fails on it rather than reading one.
///
/// # Safety
///
/// `descriptor` must be a LocalAlloc'd descriptor that nothing else frees,
/// and `dacl` null or its DACL.
#[cfg(test)]
unsafe fn read_dacl(
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *mut windows_sys::Win32::Security::ACL,
) -> std::io::Result<Dacl> {
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL_SIZE_INFORMATION, AclSizeInformation, GetAce,
        GetAclInformation, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
    };

    // Read inside a closure so the descriptor `dacl` points into is freed
    // on every path out, failures included.
    let read = (|| {
        if dacl.is_null() {
            return Err(std::io::Error::other(
                "the object has a NULL DACL, which admits everyone",
            ));
        }

        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: the caller vouches for `descriptor`; both out-pointers
        // are valid.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut size = ACL_SIZE_INFORMATION::default();
        // SAFETY: `dacl` is the non-null DACL the caller vouches for, and
        // `size` is an ACL_SIZE_INFORMATION of exactly the length passed.
        let sized = unsafe {
            GetAclInformation(
                dacl,
                (&raw mut size).cast(),
                u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).expect("small"),
                AclSizeInformation,
            )
        };
        if sized == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut entries = Vec::new();
        for index in 0..size.AceCount {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `index` is within the count the ACL reported.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(std::io::Error::last_os_error());
            }

            // SAFETY: GetAce pointed `ace` at an entry inside `dacl`, and
            // every entry starts with a header.
            let kind = unsafe { (*ace.cast::<ACE_HEADER>()).AceType };
            if !LAID_OUT_AS_ALLOWED.contains(&u32::from(kind)) {
                entries.push(Ace {
                    kind,
                    mask: 0,
                    sid: String::new(),
                });
                continue;
            }

            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
            // SAFETY: an entry of this type is laid out as
            // ACCESS_ALLOWED_ACE: its mask follows the header and its SID
            // starts at `SidStart`, inside the entry, inside `descriptor`,
            // which is freed only below.
            let (mask, sid) = unsafe {
                let sid = (&raw const (*allowed).SidStart) as PSID;
                ((*allowed).Mask, sid_string(sid))
            };
            entries.push(Ace {
                kind,
                mask,
                sid: sid?,
            });
        }
        Ok(Dacl {
            protected: control & SE_DACL_PROTECTED != 0,
            entries,
        })
    })();

    // SAFETY: the caller vouches it is LocalAlloc'd and freed only here;
    // nothing reads `dacl` after this.
    unsafe { LocalFree(descriptor as HLOCAL) };
    read
}
