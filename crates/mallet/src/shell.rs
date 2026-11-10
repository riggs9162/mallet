//! Windows file types: `.mallet` maps opening in Mallet when double-clicked, and Mallet in
//! the "Open with" menu of VMF files.
//!
//! Everything is written for the current user only, under
//! `HKEY_CURRENT_USER\Software\Classes`, the way per-user installers do it: no
//! administrator rights are needed and Hammer stays the default program for VMF files.
//!
//! On other platforms nothing here runs: the options page hides its file-type
//! section, and the registry stub refuses every call. The API stays compiled
//! so Windows behaviour is untouched, which is why dead-code is allowed there.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The program id `.mallet` files point to.
const MALLET_PROG_ID: &str = "Mallet.Map";
/// The program id VMF files list under "Open with".
const VMF_PROG_ID: &str = "Mallet.Vmf";
/// Where file types live for the current user.
const CLASSES: &str = r"Software\Classes";

/// Whether a file type is set up, and for which copy of Mallet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// Not set up.
    No,
    /// Set up for this copy of Mallet.
    ThisCopy,
    /// Set up for another copy, such as a debug build or an older folder.
    OtherCopy(PathBuf),
}

/// How both file types are set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTypes {
    /// `.mallet` files open in Mallet.
    pub mallet: Registration,
    /// VMF files list Mallet under "Open with".
    pub vmf_open_with: Registration,
}

/// How the file types are set up now.
pub fn file_types() -> FileTypes {
    let exe = current_exe();
    FileTypes {
        mallet: registration(CLASSES, MALLET_PROG_ID, ".mallet", exe.as_deref()),
        vmf_open_with: registration(CLASSES, VMF_PROG_ID, ".vmf", exe.as_deref()),
    }
}

/// Makes `.mallet` files open in this copy of Mallet, with its icon.
pub fn register_mallet() -> Result<(), String> {
    let exe = current_exe().ok_or("could not find Mallet's own program file")?;
    let root = CLASSES;
    register_prog_id(root, MALLET_PROG_ID, "Mallet Map", &exe)?;
    let extension = format!(r"{root}\.mallet");
    registry::set_string(&extension, "", MALLET_PROG_ID)?;
    registry::set_string(&extension, "Content Type", "application/x-mallet-map")?;
    registry::set_string(&format!(r"{extension}\OpenWithProgids"), MALLET_PROG_ID, "")?;
    registry::notify_file_types_changed();
    Ok(())
}

/// Stops `.mallet` files opening in Mallet.
pub fn unregister_mallet() -> Result<(), String> {
    let root = CLASSES;
    registry::delete_tree(&format!(r"{root}\.mallet"))?;
    registry::delete_tree(&format!(r"{root}\{MALLET_PROG_ID}"))?;
    registry::notify_file_types_changed();
    Ok(())
}

/// Lists this copy of Mallet under "Open with" for VMF files, leaving the default program
/// alone.
pub fn add_vmf_open_with() -> Result<(), String> {
    let exe = current_exe().ok_or("could not find Mallet's own program file")?;
    let root = CLASSES;
    register_prog_id(root, VMF_PROG_ID, "Valve Map File", &exe)?;
    registry::set_string(&format!(r"{root}\.vmf\OpenWithProgids"), VMF_PROG_ID, "")?;
    registry::notify_file_types_changed();
    Ok(())
}

/// Takes Mallet out of "Open with" for VMF files.
pub fn remove_vmf_open_with() -> Result<(), String> {
    let root = CLASSES;
    registry::delete_value(&format!(r"{root}\.vmf\OpenWithProgids"), VMF_PROG_ID)?;
    registry::delete_tree(&format!(r"{root}\{VMF_PROG_ID}"))?;
    registry::notify_file_types_changed();
    Ok(())
}

fn current_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
}

/// A program id that opens files with `exe` and shows its icon.
fn register_prog_id(root: &str, prog_id: &str, name: &str, exe: &Path) -> Result<(), String> {
    let key = format!(r"{root}\{prog_id}");
    let exe = plain_path(exe);
    registry::set_string(&key, "", name)?;
    registry::set_string(&format!(r"{key}\DefaultIcon"), "", &format!("\"{exe}\",0"))?;
    registry::set_string(&format!(r"{key}\shell\open\command"), "", &format!("\"{exe}\" \"%1\""))
}

/// Whether `extension` points at `prog_id` and that opens `exe`.
fn registration(root: &str, prog_id: &str, extension: &str, exe: Option<&Path>) -> Registration {
    let listed = registry::get_string(&format!(r"{root}\{extension}\OpenWithProgids"), prog_id).is_some()
        || registry::get_string(&format!(r"{root}\{extension}"), "").as_deref() == Some(prog_id);
    let command = registry::get_string(&format!(r"{root}\{prog_id}\shell\open\command"), "");
    let (true, Some(command)) = (listed, command) else {
        return Registration::No;
    };
    let program = command_program(&command);
    let same = exe.is_some_and(|exe| plain_path(exe).eq_ignore_ascii_case(&program));
    if same {
        Registration::ThisCopy
    } else {
        Registration::OtherCopy(PathBuf::from(program))
    }
}

/// The program of a command line such as `"C:\Mallet\mallet.exe" "%1"`.
fn command_program(command: &str) -> String {
    let command = command.trim();
    match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default().to_owned(),
        None => command.split_whitespace().next().unwrap_or_default().to_owned(),
    }
}

/// A path without the `\\?\` prefix `canonicalize` adds, which Explorer does not accept in
/// commands.
fn plain_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(text)
}

#[cfg(windows)]
mod registry {
    //! The few registry calls needed, straight from `advapi32` and `shell32`.

    use std::ffi::c_void;
    use std::ptr;

    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    const REG_SZ: u32 = 1;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegSetKeyValueW(
            key: isize,
            sub_key: *const u16,
            value_name: *const u16,
            kind: u32,
            data: *const c_void,
            size: u32,
        ) -> i32;
        fn RegGetValueW(
            key: isize,
            sub_key: *const u16,
            value_name: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
        fn RegDeleteTreeW(key: isize, sub_key: *const u16) -> i32;
        fn RegDeleteKeyValueW(key: isize, sub_key: *const u16, value_name: *const u16) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(event: i32, flags: u32, item1: *const c_void, item2: *const c_void);
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    fn check(status: i32, what: &str, key: &str) -> Result<(), String> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            let error = std::io::Error::from_raw_os_error(status);
            Err(format!("could not {what} HKEY_CURRENT_USER\\{key}: {error}"))
        }
    }

    /// Writes a string value, creating the key if needed. An empty name is the key's
    /// default value.
    pub fn set_string(key: &str, name: &str, value: &str) -> Result<(), String> {
        let data = wide(value);
        let size = u32::try_from(data.len() * 2).map_err(|_| "the value is too long".to_owned())?;
        // SAFETY: every pointer is to a live, NUL-terminated UTF-16 buffer, and `size` is
        // the byte length of `data`.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(key).as_ptr(),
                wide(name).as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                size,
            )
        };
        check(status, "write", key)
    }

    /// Reads a string value; `None` if the key or value is missing or is not a string.
    pub fn get_string(key: &str, name: &str) -> Option<String> {
        let (key, name) = (wide(key), wide(name));
        let mut size = 0u32;
        // SAFETY: a null data pointer asks only for the size, written to `size`.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let mut data = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size = u32::try_from(data.len() * 2).ok()?;
        // SAFETY: `data` holds `size` bytes, and the call writes at most that many.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let end = data.iter().position(|&unit| unit == 0).unwrap_or(data.len());
        Some(String::from_utf16_lossy(&data[..end]))
    }

    /// Deletes a key with everything under it. A missing key is fine.
    pub fn delete_tree(key: &str) -> Result<(), String> {
        // SAFETY: the key name is a live, NUL-terminated UTF-16 buffer. Deleting needs the
        // key itself gone too, which `RegDeleteTreeW` with a sub key name does.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(key).as_ptr()) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(status, "delete", key)
    }

    /// Deletes one value. A missing key or value is fine.
    pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
        // SAFETY: both names are live, NUL-terminated UTF-16 buffers.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(key).as_ptr(), wide(name).as_ptr()) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(status, "change", key)
    }

    /// Tells Explorer the file types changed, so icons and menus update at once.
    pub fn notify_file_types_changed() {
        // SAFETY: this event takes no items.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, ptr::null(), ptr::null()) };
    }
}

#[cfg(not(windows))]
mod registry {
    //! File types are only set up on Windows.

    const ONLY_WINDOWS: &str = "file types can only be set up on Windows";

    pub fn set_string(_key: &str, _name: &str, _value: &str) -> Result<(), String> {
        Err(ONLY_WINDOWS.to_owned())
    }

    pub fn get_string(_key: &str, _name: &str) -> Option<String> {
        None
    }

    pub fn delete_tree(_key: &str) -> Result<(), String> {
        Err(ONLY_WINDOWS.to_owned())
    }

    pub fn delete_value(_key: &str, _name: &str) -> Result<(), String> {
        Err(ONLY_WINDOWS.to_owned())
    }

    pub fn notify_file_types_changed() {}
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Works under a scratch key instead of the real file types, and removes it after.
    #[test]
    fn registers_and_reads_back_under_a_scratch_key() {
        let root = format!(r"Software\Mallet-test-{}\Classes", std::process::id());
        let exe = Path::new(r"C:\Tools\Mallet\mallet.exe");
        assert_eq!(
            registration(&root, MALLET_PROG_ID, ".mallet", Some(exe)),
            Registration::No
        );

        register_prog_id(&root, MALLET_PROG_ID, "Mallet Map", exe).unwrap();
        registry::set_string(&format!(r"{root}\.mallet"), "", MALLET_PROG_ID).unwrap();
        assert_eq!(
            registry::get_string(&format!(r"{root}\{MALLET_PROG_ID}\shell\open\command"), "").as_deref(),
            Some(r#""C:\Tools\Mallet\mallet.exe" "%1""#)
        );
        assert_eq!(
            registration(&root, MALLET_PROG_ID, ".mallet", Some(exe)),
            Registration::ThisCopy
        );
        let other = Path::new(r"D:\Other\mallet.exe");
        assert_eq!(
            registration(&root, MALLET_PROG_ID, ".mallet", Some(other)),
            Registration::OtherCopy(exe.to_owned())
        );

        // Open with for VMF files sits beside whatever program is the default.
        registry::set_string(&format!(r"{root}\.vmf"), "", "Hammer.Vmf").unwrap();
        register_prog_id(&root, VMF_PROG_ID, "Valve Map File", exe).unwrap();
        registry::set_string(&format!(r"{root}\.vmf\OpenWithProgids"), VMF_PROG_ID, "").unwrap();
        assert_eq!(
            registration(&root, VMF_PROG_ID, ".vmf", Some(exe)),
            Registration::ThisCopy
        );
        registry::delete_value(&format!(r"{root}\.vmf\OpenWithProgids"), VMF_PROG_ID).unwrap();
        assert_eq!(registration(&root, VMF_PROG_ID, ".vmf", Some(exe)), Registration::No);
        assert_eq!(
            registry::get_string(&format!(r"{root}\.vmf"), "").as_deref(),
            Some("Hammer.Vmf")
        );

        let scratch = format!(r"Software\Mallet-test-{}", std::process::id());
        registry::delete_tree(&scratch).unwrap();
        assert_eq!(registry::get_string(&format!(r"{root}\.mallet"), ""), None);
        registry::delete_tree(&scratch).unwrap();
    }

    #[test]
    fn reads_the_program_of_a_command() {
        assert_eq!(command_program(r#""C:\A B\mallet.exe" "%1""#), r"C:\A B\mallet.exe");
        assert_eq!(command_program(r"C:\mallet.exe %1"), r"C:\mallet.exe");
        assert_eq!(plain_path(Path::new(r"\\?\C:\x\mallet.exe")), r"C:\x\mallet.exe");
    }
}
