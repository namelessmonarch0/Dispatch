//! The name this machine is known by.
//!
//! Federation labels every daemon's row with this, so two machines the user
//! is running Dispatch on can be told apart at a glance. The environment is
//! not trusted for it: `$HOSTNAME` is a bash-only convention that bash itself
//! does not export, and zsh, fish and most CI runners leave it unset
//! entirely — reading it would silently fall back to a placeholder on most
//! shells, exactly the situation naming a machine exists to avoid. Asking the
//! operating system directly gives the real name regardless of shell.

/// The hostname of the machine this process is running on.
///
/// Falls back to `"local"` when the operating system call fails, rather than
/// returning a `Result`: this is a label for a sidebar row, not something
/// callers need to handle failing.
pub fn hostname() -> String {
    imp::hostname().unwrap_or_else(|| "local".to_string())
}

#[cfg(unix)]
mod imp {
    /// POSIX only guarantees 255 bytes of hostname; this is generous
    /// headroom over that.
    const BUF_LEN: usize = 256;

    pub(super) fn hostname() -> Option<String> {
        let mut buf = [0 as libc::c_char; BUF_LEN];

        // SAFETY: buf is a valid, correctly-sized buffer for the duration of
        // the call, and gethostname writes at most buf.len() bytes into it.
        let result = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
        if result != 0 {
            return None;
        }

        // gethostname null-terminates on success but does not promise the
        // rest of the buffer is zeroed, so the name ends at the first NUL
        // rather than at whatever garbage might follow.
        let end = buf.iter().position(|&c| c == 0)?;
        let bytes: Vec<u8> = buf[..end].iter().map(|&c| c as u8).collect();
        String::from_utf8(bytes)
            .ok()
            .filter(|name| !name.is_empty())
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;

    /// `MAX_COMPUTERNAME_LENGTH` (15) plus the NUL terminator.
    const BUF_LEN: usize = 16;

    pub(super) fn hostname() -> Option<String> {
        let mut buf = [0u16; BUF_LEN];
        let mut len = BUF_LEN as u32;

        // SAFETY: buf is valid for `len` elements; on success GetComputerNameW
        // overwrites `len` with the number of characters written, excluding
        // the terminating NUL.
        let ok = unsafe { GetComputerNameW(buf.as_mut_ptr(), &mut len) };
        if ok == 0 {
            return None;
        }

        String::from_utf16(&buf[..len as usize])
            .ok()
            .filter(|name| !name.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hostname_is_never_empty_or_nul_bearing() {
        let name = hostname();

        assert!(!name.is_empty(), "a sidebar row needs something to show");
        assert!(
            !name.contains('\0'),
            "a NUL midstring means the platform call was not trimmed correctly"
        );
    }
}
